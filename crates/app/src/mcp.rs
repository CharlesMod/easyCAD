//! The Claude bridge: an MCP server inside the running app (see `docs/MCP.md`).
//!
//! The HTTP server (`easycad_mcp::serve`, loopback only, bearer token) runs on its
//! own threads and hands each tool call to the UI thread through a channel. The
//! frame loop ([`poll`]) runs the calls one at a time, in order. A call is a small
//! state machine ([`Step`]): mutations go through `Document::edit` on the UI thread
//! (one undo step labelled "Claude: ...", shown at once in the viewport), then wait
//! for the regeneration worker's model of that revision before answering, so the
//! reply carries the real regen reports. Reads look at the same state the user sees,
//! so edits made by hand are visible on the next call; `user_changes` lists them.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use easycad_mcp::{self as proto, json_text, ToolResult};
use easycad_model::document::{DocData, DocError, Document, ExportFormat, ExportOptions};
use easycad_model::feature::{BodyId, Feature, FeatureId, FeatureKind, SketchId};
use easycad_model::geom::Plane;
use easycad_model::occt::{EdgeInfo, FaceInfo, Shape, SurfaceKind};
use easycad_model::params::{Expr, Values};
use easycad_model::regen::{FeatureReport, FeatureStatus};
use easycad_model::select::{EdgeSelector, Extremum, FaceSelector};
use easycad_model::sketch::{Constraint, EntityId, Geom, PointId, Sketch};
use glam::{DVec2, DVec3};
use serde_json::{json, Value};

use crate::app::{EasyCadApp, Mode};
use crate::worker::{MeshedBody, Purpose};

/// Undo labels of Claude's steps start with this.
pub const LABEL: &str = "Claude: ";
/// A call that has not finished after this long is answered with an error.
const JOB_TIMEOUT: Duration = Duration::from_secs(600);

/// One tool call from the server thread.
pub struct Call {
    pub name: String,
    pub args: Value,
    pub reply: Sender<ToolResult>,
}

/// The bridge state owned by the app.
pub struct Bridge {
    server: Option<proto::Server>,
    tx: Sender<Call>,
    rx: Receiver<Call>,
    queue: VecDeque<Job>,
    /// (doc id, revision) when Claude last looked at or changed the document.
    seen: Option<(String, u64)>,
    /// Tool calls answered (shown in Settings).
    pub calls: u64,
}

struct Job {
    call: Call,
    step: Option<Step>,
    since: Instant,
}

type Then = Box<dyn FnOnce(&mut EasyCadApp, &egui::Context) -> Step>;

/// What a call is waiting for.
enum Step {
    Done(ToolResult),
    /// The worker's model of the current document revision.
    AfterModel(Then),
    /// The camera animation to finish, then `n` more frames (so the viewport has drawn).
    AfterCamera(u32, Then),
    /// A background thread (export, interference, the screenshot callback).
    Recv(Receiver<ToolResult>),
}

impl Bridge {
    /// A bridge with no server (tests drive it through [`Bridge::submit`]).
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Bridge { server: None, tx, rx, queue: VecDeque::new(), seen: None, calls: 0 }
    }

    /// Start the HTTP server on 127.0.0.1 (`port`, or the next free one up to +10;
    /// 0 = any) and write the endpoint file the `easycad-mcp` shim reads. The token is
    /// kept from an earlier endpoint file so direct HTTP configurations keep working.
    pub fn start(ctx: &egui::Context, port: u16) -> Result<Self, String> {
        let mut b = Bridge::new();
        let token = proto::read_endpoint().map(|e| e.token).filter(|t| t.len() >= 32).unwrap_or_else(proto::new_token);
        let (tx, ctx2) = (b.tx.clone(), ctx.clone());
        let handler: proto::Handler = Arc::new(move |name: &str, args: Value| {
            let (rtx, rrx) = channel();
            tx.send(Call { name: name.to_string(), args, reply: rtx }).map_err(|_| "easyCAD is closing".to_string())?;
            ctx2.request_repaint();
            match rrx.recv_timeout(JOB_TIMEOUT + Duration::from_secs(5)) {
                Ok(r) => r,
                Err(_) => Err("easyCAD did not answer in time. Its window may be minimized or busy: ask the user to bring it to the front.".into()),
            }
        });
        let server = proto::serve(port, token, handler).map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
        proto::write_endpoint(&server.url(), &server.token).map_err(|e| format!("cannot write the endpoint file: {e}"))?;
        b.server = Some(server);
        Ok(b)
    }

    pub fn url(&self) -> Option<String> {
        self.server.as_ref().map(|s| s.url())
    }

    pub fn token(&self) -> Option<&str> {
        self.server.as_ref().map(|s| s.token.as_str())
    }

    /// Queue a call (tests, or anything in-process); the reply arrives once the
    /// frame loop has run it.
    pub fn submit(&self, name: &str, args: Value) -> Receiver<ToolResult> {
        let (rtx, rrx) = channel();
        let _ = self.tx.send(Call { name: name.into(), args, reply: rtx });
        rrx
    }

    /// Stop the server and remove the endpoint file.
    pub fn stop(&mut self) {
        if let Some(s) = self.server.take() {
            s.stop();
            proto::remove_endpoint();
        }
    }

    pub fn busy(&self) -> bool {
        !self.queue.is_empty()
    }
}

impl Default for Bridge {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop();
    }
}

impl EasyCadApp {
    /// Turn the bridge on (`port` 0 = any free port) or off.
    pub fn set_mcp(&mut self, ctx: &egui::Context, on: bool, port: u16) {
        if !on {
            if self.mcp.take().is_some() {
                self.toast("Claude bridge off");
            }
            return;
        }
        if self.mcp.as_ref().is_some_and(|b| b.server.is_some()) {
            return;
        }
        match Bridge::start(ctx, port) {
            Ok(b) => {
                self.toast(format!("Claude bridge on at {}", b.url().unwrap_or_default()));
                self.mcp = Some(b);
            }
            Err(e) => self.warn(format!("Claude bridge: {e}")),
        }
    }
}

// ------------------------------------------------------------------ frame loop

/// Run queued calls (called every frame).
pub fn poll(app: &mut EasyCadApp, ctx: &egui::Context) {
    let Some(b) = app.mcp.as_mut() else { return };
    while let Ok(call) = b.rx.try_recv() {
        b.queue.push_back(Job { call, step: None, since: Instant::now() });
    }
    loop {
        let Some(mut job) = app.mcp.as_mut().and_then(|b| b.queue.pop_front()) else { return };
        let step = match job.step.take() {
            Some(s) => s,
            None => {
                let args = std::mem::take(&mut job.call.args);
                start(app, ctx, &job.call.name, args)
            }
        };
        match advance(app, ctx, step) {
            Ok(r) => {
                let _ = job.call.reply.send(r);
                if let Some(b) = app.mcp.as_mut() {
                    b.calls += 1;
                }
            }
            Err(s) if job.since.elapsed() > JOB_TIMEOUT => {
                drop(s);
                let _ = job.call.reply.send(Err(format!("`{}` timed out (regeneration still running?)", job.call.name)));
            }
            Err(s) => {
                job.step = Some(s);
                if let Some(b) = app.mcp.as_mut() {
                    b.queue.push_front(job);
                }
                ctx.request_repaint_after(Duration::from_millis(15));
                return;
            }
        }
    }
}

/// Run a step as far as it goes this frame: `Ok` = finished, `Err` = still waiting.
fn advance(app: &mut EasyCadApp, ctx: &egui::Context, mut step: Step) -> Result<ToolResult, Step> {
    loop {
        step = match step {
            Step::Done(r) => return Ok(r),
            Step::AfterModel(f) => {
                if model_ready(app) {
                    f(app, ctx)
                } else {
                    return Err(Step::AfterModel(f));
                }
            }
            Step::AfterCamera(n, f) => {
                if app.camera.animating() || !model_ready(app) {
                    return Err(Step::AfterCamera(n, f));
                }
                if n == 0 {
                    f(app, ctx)
                } else {
                    ctx.request_repaint();
                    return Err(Step::AfterCamera(n - 1, f));
                }
            }
            Step::Recv(rx) => match rx.try_recv() {
                Ok(r) => return Ok(r),
                Err(TryRecvError::Empty) => return Err(Step::Recv(rx)),
                Err(TryRecvError::Disconnected) => return Ok(Err("the background job ended without a result".into())),
            },
        }
    }
}

/// The worker's model is of the document as it is now.
fn model_ready(app: &EasyCadApp) -> bool {
    !app.model_dirty
        && !app.worker.busy_with(Purpose::Model)
        && app.model.as_ref().is_some_and(|m| m.data.revision == app.doc.data.revision)
}

fn after_model(f: impl FnOnce(&mut EasyCadApp, &egui::Context) -> Step + 'static) -> Step {
    Step::AfterModel(Box::new(f))
}

fn done(v: Value) -> Step {
    Step::Done(Ok(vec![json_text(&v)]))
}

fn fail(msg: impl Into<String>) -> Step {
    Step::Done(Err(msg.into()))
}

fn start(app: &mut EasyCadApp, ctx: &egui::Context, name: &str, args: Value) -> Step {
    let r = match name {
        "get_document" => Ok(get_document(args)),
        "set_param" => set_param(app, args),
        "add_sketch" => add_sketch(app, args),
        "add_feature" => add_feature(app, args),
        "edit_feature" => edit_feature(app, args),
        "delete_feature" => delete_feature(app, args),
        "regenerate" => Ok(after_model(|app, _| done(regen_json(app)))),
        "measure" => Ok(measure(args)),
        "get_selection" => Ok(after_model(|app, _| done(selection_json(app)))),
        "interference" => Ok(interference(args)),
        "screenshot" => screenshot(app, ctx, args),
        "export" => export(args),
        "open_document" | "new_document" => open_or_new(app, name, args),
        "save_document" => save_document(app, args),
        "undo" => undo(app, args),
        other => Err(format!("unknown tool `{other}`")),
    };
    r.unwrap_or_else(fail)
}

// ------------------------------------------------------------------ shared helpers

/// Round for readable JSON (0.1 µm).
fn r(x: f64) -> f64 {
    let v = (x * 1e4).round() / 1e4;
    if v == 0.0 {
        0.0
    } else {
        v
    }
}

fn v3(v: DVec3) -> Value {
    json!([r(v.x), r(v.y), r(v.z)])
}

fn str_arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

fn bool_arg(args: &Value, k: &str) -> Option<bool> {
    args.get(k).and_then(Value::as_bool)
}

fn expr_arg(v: &Value) -> Option<Expr> {
    match v {
        Value::String(s) => Some(Expr::new(s.trim())),
        Value::Number(n) => n.as_f64().map(|x| Expr::new(format!("{x}"))),
        _ => None,
    }
}

/// Mutations are refused while the user is in the middle of something.
fn can_edit(app: &EasyCadApp) -> Result<(), String> {
    match &app.mode {
        Mode::Model => {}
        Mode::Sketch(_) => return Err("The user is editing a sketch in easyCAD. Ask them to finish it (Finish sketch) or cancel it, then retry.".into()),
        Mode::Parts(_) => return Err("The user has the parts library open. Ask them to close it, then retry.".into()),
        Mode::Drawing(_) => return Err("The user is on a drawing sheet. Ask them to go back to the model, then retry.".into()),
    }
    if let Some(d) = &app.dialog {
        return Err(format!(
            "The user has the {} dialog open in easyCAD. Ask them to finish (OK) or cancel it, then retry.",
            d.kind.title()
        ));
    }
    if app.wizard.is_some() {
        return Err("The user has the enclosure wizard open. Ask them to finish or close it, then retry.".into());
    }
    if !app.active.is_empty() {
        return Err("The user is editing a component in place. Ask them to return to the top level, then retry.".into());
    }
    Ok(())
}

fn check_revision(app: &EasyCadApp, args: &Value) -> Result<(), String> {
    if let Some(want) = args.get("expected_revision").and_then(Value::as_u64) {
        let now = app.doc.data.revision;
        if now != want {
            let mut msg = format!("The document is at revision {now}, not {want}: it changed since you looked.");
            if let Some(c) = user_changes(app) {
                msg += &format!(" Changes: {c}.");
            }
            return Err(msg + " Call get_document and decide again.");
        }
    }
    Ok(())
}

/// Edits the user made since Claude's last call (none on the first call).
fn user_changes(app: &EasyCadApp) -> Option<Value> {
    let (id, rev) = app.mcp.as_ref()?.seen.clone()?;
    if id != app.doc.data.doc_id {
        return Some(json!({"note": "the user opened a different document since your last call", "document": app.doc.data.name}));
    }
    if rev == app.doc.data.revision {
        return None;
    }
    let steps: Vec<&str> = app.doc.undo_labels().take_while(|l| !l.starts_with(LABEL)).take(20).collect();
    let mut v = json!({"revision_was": rev, "revision_now": app.doc.data.revision, "steps": steps});
    if steps.is_empty() {
        v["note"] = json!(format!(
            "the user undid or redid steps (last step now: {})",
            app.doc.undo_label().unwrap_or("none")
        ));
    }
    Some(v)
}

fn mark_seen(app: &mut EasyCadApp) {
    let s = (app.doc.data.doc_id.clone(), app.doc.data.revision);
    if let Some(b) = app.mcp.as_mut() {
        b.seen = Some(s);
    }
}

/// Apply an undoable edit labelled "Claude: ...", show it, and remember the
/// revision as seen. Returns the user changes found before the edit.
fn claude_edit<T>(
    app: &mut EasyCadApp,
    label: &str,
    f: impl FnOnce(&mut DocData) -> Result<T, String>,
) -> Result<(T, Option<Value>), String> {
    can_edit(app)?;
    let before = user_changes(app);
    let label = format!("{LABEL}{label}");
    let out = app.doc.edit(&label, f)?;
    app.params.buffers.clear();
    app.after_edit();
    claude_toast(app, label);
    mark_seen(app);
    Ok((out, before))
}

/// One "Claude: ..." toast at a time: a quick run of steps does not stack them over
/// the model.
fn claude_toast(app: &mut EasyCadApp, text: String) {
    app.toasts.retain(|t| !t.text.starts_with(LABEL));
    app.toast(text);
}

fn known_params(d: &DocData) -> String {
    let v: Vec<&str> = d.params.list.iter().map(|p| p.name.as_str()).collect();
    if v.is_empty() {
        "there are no parameters yet (create them with set_param)".into()
    } else {
        format!("known parameters: {}", v.join(", "))
    }
}

/// Add what usually fixes an error message.
fn hint(d: &DocData, msg: &str) -> String {
    let mut s = msg.to_string();
    if msg.contains("unknown parameter") {
        s += &format!(" ({})", known_params(d));
    }
    if msg.contains("could not be resolved") || msg.contains("no face") || msg.contains("no edge") {
        s += " (call measure with faces/edges to see the candidates and their selectors)";
    }
    s
}

// ------------------------------------------------------------------ reports

fn status_json(d: &DocData, s: &FeatureStatus) -> Value {
    match s {
        FeatureStatus::Ok => json!({"status": "ok"}),
        FeatureStatus::Suppressed => json!({"status": "suppressed"}),
        FeatureStatus::Warning(m) => json!({"status": "warning", "message": hint(d, m)}),
        FeatureStatus::Error(m) => json!({"status": "error", "message": hint(d, m)}),
        FeatureStatus::Unresolved(u) => json!({
            "status": "unresolved",
            "message": "a stored face/edge reference no longer matches the geometry confidently",
            "selector": u.selector,
            "best_candidate": u.best_candidate,
            "score": r(u.score),
            "fix": "edit_feature with the best_candidate (if it is the intended one) or a new selector from measure",
        }),
    }
}

fn is_failure(s: &FeatureStatus) -> bool {
    matches!(s, FeatureStatus::Error(_) | FeatureStatus::Unresolved(_))
}

fn report_json(d: &DocData, rep: &FeatureReport) -> Value {
    use easycad_model::occt::dressup::EdgeStatus;
    let f = d.feature(rep.feature);
    let mut o = json!({
        "id": rep.feature.0,
        "name": f.map(|f| f.name.as_str()).unwrap_or("?"),
        "type": f.map(|f| f.kind.label()).unwrap_or("?"),
    });
    for (k, v) in status_json(d, &rep.status).as_object().into_iter().flatten() {
        o[k] = v.clone();
    }
    if rep.rolled_back {
        o["rolled_back"] = json!(true);
    }
    if let Some(dr) = &rep.dressup {
        o["summary"] = json!(dr.summary());
        let sets = f.map(|f| f.kind.edge_sets()).unwrap_or_default();
        let mut edges = Vec::new();
        for oc in &dr.outcomes {
            let sel = rep
                .edge_requests
                .get(oc.request)
                .and_then(|q| sets.get(q.set).and_then(|s| s.edges.get(q.selector)).map(|e| json!({"set": q.set, "selector": e})));
            let e = match &oc.status {
                EdgeStatus::Applied | EdgeStatus::Smooth => continue,
                EdgeStatus::AppliedReduced { requested, applied } => {
                    json!({"edge": oc.edge, "result": "reduced", "requested": r(*requested), "applied": r(*applied)})
                }
                EdgeStatus::Failed { reason, max_size } => json!({
                    "edge": oc.edge,
                    "result": "failed",
                    "reason": reason,
                    "largest_workable_size": max_size.map(r),
                    "fix": match max_size {
                        Some(m) => format!("use a size ≤ {:.2} mm for this edge (a separate edge set), set auto_reduce, or add its selector to `skipped`", m),
                        None => "no size works on this edge: add its selector to `skipped` or change the geometry".to_string(),
                    },
                }),
            };
            let mut e = e;
            if let Some(s) = sel {
                e["from"] = s;
            }
            edges.push(e);
        }
        if !edges.is_empty() {
            o["edge_problems"] = json!(edges);
        }
    }
    if let Some(s) = &rep.sketch {
        o["sketch_solve"] = json!({"converged": s.converged, "status": s.status, "dof": s.dof});
    }
    if let Some(j) = &rep.joint {
        o["joint"] = serde_json::to_value(j).unwrap_or(Value::Null);
    }
    if !rep.skipped_edges.is_empty() {
        o["skipped_edges"] = json!(rep.skipped_edges.len());
    }
    if !rep.cached && rep.elapsed_ms > 0.0 {
        o["ms"] = json!(rep.elapsed_ms.round());
    }
    o
}

fn bodies_json(app: &EasyCadApp) -> Value {
    let Some(m) = &app.model else { return json!([]) };
    let v: Vec<Value> = m
        .bodies
        .iter()
        .map(|b| {
            let mut o = json!({"id": b.body.id, "name": b.body.name, "reference": b.body.reference});
            if let Ok(bb) = b.body.shape().bbox() {
                o["bbox"] = json!({"min": v3(bb.min), "max": v3(bb.max), "size": v3(bb.size())});
            }
            if let Some(e) = &b.error {
                o["mesh_error"] = json!(e);
            }
            o
        })
        .collect();
    json!(v)
}

/// After a change: the changed feature's report (if any), every problem, the bodies.
fn outcome_json(app: &EasyCadApp, focus: Option<FeatureId>) -> Value {
    let d = &app.doc.data;
    let mut o = json!({"revision": d.revision});
    let Some(m) = &app.model else { return o };
    let mut problems = Vec::new();
    let mut ok = 0;
    for rep in &m.result.reports {
        if Some(rep.feature) == focus {
            o["feature"] = report_json(d, rep);
            continue;
        }
        match rep.status {
            FeatureStatus::Ok | FeatureStatus::Suppressed => ok += 1,
            _ => problems.push(report_json(d, rep)),
        }
    }
    o["other_features_ok"] = json!(ok);
    if !problems.is_empty() {
        o["problems"] = json!(problems);
    }
    o["bodies"] = bodies_json(app);
    o
}

fn regen_json(app: &mut EasyCadApp) -> Value {
    let d = &app.doc.data;
    let Some(m) = &app.model else { return json!({"revision": d.revision, "features": []}) };
    let reports: Vec<Value> = m.result.reports.iter().map(|r| report_json(d, r)).collect();
    json!({
        "revision": d.revision,
        "features": reports,
        "recomputed": m.result.recomputed,
        "reused": m.result.reused,
        "elapsed_ms": m.elapsed.as_millis() as u64,
        "bodies": bodies_json(app),
    })
}

// ------------------------------------------------------------------ get_document

fn get_document(args: Value) -> Step {
    let defs = bool_arg(&args, "include_definitions").unwrap_or(true);
    after_model(move |app, _| {
        let v = document_json(app, defs);
        mark_seen(app);
        done(v)
    })
}

fn document_json(app: &EasyCadApp, defs: bool) -> Value {
    let d = &app.doc.data;
    let values = d.values().unwrap_or_default();
    let params: Vec<Value> = d
        .params
        .list
        .iter()
        .map(|p| {
            let mut o = json!({"name": p.name, "expr": p.expr.0, "value": values.get(&p.name).map(|v| r(*v))});
            if !p.comment.is_empty() {
                o["comment"] = json!(p.comment);
            }
            if let Some(f) = &p.expr.1 {
                o["fit"] = serde_json::to_value(f).unwrap_or(Value::Null);
            }
            o
        })
        .collect();
    let sketches: Vec<Value> = d
        .sketches
        .iter()
        .map(|s| {
            let sk = &s.sketch;
            let solve = sk.analyze(&values).map(|r| json!({"status": r.status, "dof": r.dof})).unwrap_or_else(|e| json!({"error": e.to_string()}));
            let prof = sk.profiles();
            json!({
                "id": s.id.0,
                "name": sk.name,
                "plane": plane_json(&sk.plane),
                "points": sk.points.len(),
                "entities": sk.entities.len(),
                "constraints": sk.constraints.len(),
                "solve": solve,
                "closed_regions": prof.profiles.len(),
                "open_chains": prof.open.len(),
                "used_by": d.features.iter().filter(|f| sketch_of(&f.kind) == Some(s.id)).map(|f| f.id.0).collect::<Vec<_>>(),
            })
        })
        .collect();
    let reports: HashMap<FeatureId, &FeatureReport> = app.model.iter().flat_map(|m| m.result.reports.iter()).map(|r| (r.feature, r)).collect();
    let features: Vec<Value> = d
        .features
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut o = json!({"index": i, "id": f.id.0, "name": f.name, "type": f.kind.label()});
            if f.suppressed {
                o["suppressed"] = json!(true);
            }
            if let Some(rep) = reports.get(&f.id) {
                let rj = report_json(d, rep);
                for k in ["status", "message", "summary", "edge_problems", "selector", "best_candidate", "fix", "rolled_back"] {
                    if let Some(v) = rj.get(k) {
                        o[k] = v.clone();
                    }
                }
            }
            if defs {
                o["kind"] = serde_json::to_value(&f.kind).unwrap_or(Value::Null);
            }
            o
        })
        .collect();
    let mode = match (&app.mode, &app.dialog) {
        (Mode::Sketch(_), _) => "editing a sketch".to_string(),
        (Mode::Parts(_), _) => "browsing the parts library".to_string(),
        (Mode::Drawing(_), _) => "on a drawing sheet".to_string(),
        (Mode::Model, Some(dl)) => format!("{} dialog open", dl.kind.title()),
        (Mode::Model, None) => "modeling".to_string(),
    };
    let mut o = json!({
        "name": d.name,
        "path": app.path.as_ref().map(|p| p.display().to_string()),
        "revision": d.revision,
        "unsaved_changes": d.revision != app.saved_revision,
        "active_process_profile": d.active_profile,
        "app": {"state": mode, "selection_count": app.selection.len(), "rollback_marker": d.rollback},
        "undo_history": app.doc.undo_labels().take(10).collect::<Vec<_>>(),
        "params": params,
        "sketches": sketches,
        "features": features,
        "bodies": bodies_json(app),
    });
    if let Some(c) = user_changes(app) {
        o["user_changes"] = c;
    }
    if !d.occurrences.is_empty() {
        o["assembly"] = json!({"occurrences": d.occurrences.len(), "note": "assembly documents: the tools edit the top-level part only"});
    }
    o
}

fn sketch_of(k: &FeatureKind) -> Option<SketchId> {
    match k {
        FeatureKind::Extrude { sketch, .. } | FeatureKind::Revolve { sketch, .. } | FeatureKind::Rib { sketch, .. } => Some(*sketch),
        _ => None,
    }
}

fn plane_json(p: &Plane) -> Value {
    for (n, q) in [("XY", Plane::XY), ("XZ", Plane::XZ), ("YZ", Plane::YZ)] {
        if p.x_dir.abs_diff_eq(q.x_dir, 1e-9) && p.normal.abs_diff_eq(q.normal, 1e-9) {
            let off = p.origin.dot(q.normal);
            if p.origin.abs_diff_eq(q.normal * off, 1e-9) {
                return if off.abs() < 1e-9 { json!(n) } else { json!({"plane": n, "offset": r(off)}) };
            }
        }
    }
    json!({"origin": v3(p.origin), "x_dir": v3(p.x_dir), "normal": v3(p.normal)})
}

// ------------------------------------------------------------------ set_param

fn set_param(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    check_revision(app, &args)?;
    let mut sets: Vec<(String, Expr, Option<String>)> = Vec::new();
    let mut one = |v: &Value| -> Result<(), String> {
        let name = str_arg(v, "name").ok_or("each parameter needs a `name`")?.trim().to_string();
        let expr = v.get("expr").and_then(expr_arg).ok_or_else(|| format!("parameter `{name}` needs an `expr` (string or number)"))?;
        sets.push((name, expr, str_arg(v, "comment").map(String::from)));
        Ok(())
    };
    if args.get("name").is_some() {
        one(&args)?;
    }
    for p in args.get("params").and_then(Value::as_array).into_iter().flatten() {
        one(p)?;
    }
    let deletes: Vec<String> = args.get("delete").and_then(Value::as_array).into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
    if sets.is_empty() && deletes.is_empty() {
        return Err("give `name` and `expr`, a `params` list, or `delete`".into());
    }
    for n in &deletes {
        let users = crate::adapters::users_of(&app.doc.data, n);
        if !users.is_empty() {
            return Err(format!("parameter `{n}` is still used by {}: change those first", users.join(", ")));
        }
        if app.doc.data.params.get(n).is_none() {
            return Err(format!("no parameter `{n}` ({})", known_params(&app.doc.data)));
        }
    }
    let label = match (sets.as_slice(), deletes.as_slice()) {
        ([(n, e, _)], []) => format!("set {n} = {}", e.0),
        ([], [n]) => format!("delete parameter {n}"),
        _ => {
            let names: Vec<&str> = sets.iter().map(|s| s.0.as_str()).chain(deletes.iter().map(String::as_str)).collect();
            format!("set {} parameters ({})", names.len(), names.join(", "))
        }
    };
    let (_, before) = claude_edit(app, &label, |d| {
        for n in &deletes {
            d.params.remove(n);
        }
        for (name, expr, comment) in sets {
            d.params.set(&name, expr).map_err(|e| format!("parameter `{name}`: {e}"))?;
            if let (Some(c), Some(p)) = (comment, d.params.list.iter_mut().find(|p| p.name == name)) {
                p.comment = c;
            }
        }
        d.values().map_err(|e| {
            let msg = format!("the parameters do not evaluate: {e}");
            if matches!(e, easycad_model::params::ExprError::Unknown(_)) {
                format!("{msg} ({})", known_params(d))
            } else {
                msg
            }
        })?;
        Ok(())
    })?;
    Ok(after_model(move |app, _| {
        let mut o = outcome_json(app, None);
        let values = app.doc.data.values().unwrap_or_default();
        o["params"] = json!(app.doc.data.params.list.iter().map(|p| json!({"name": p.name, "expr": p.expr.0, "value": values.get(&p.name).map(|v| r(*v))})).collect::<Vec<_>>());
        if let Some(b) = before {
            o["user_changes_before"] = b;
        }
        done(o)
    }))
}

// ------------------------------------------------------------------ add_sketch

/// Names given to sketch points / entities by `add_sketch`.
#[derive(Default)]
struct Names {
    points: BTreeMap<String, PointId>,
    entities: BTreeMap<String, EntityId>,
}

impl Names {
    fn point(&self, sk: &Sketch, s: &str) -> Result<PointId, String> {
        if let Some(n) = s.strip_prefix("#p").and_then(|n| n.parse::<u32>().ok()) {
            return if (n as usize) < sk.points.len() { Ok(PointId(n)) } else { Err(format!("no point #p{n}")) };
        }
        if let Some(p) = self.points.get(s) {
            return Ok(*p);
        }
        if let Some((ent, end)) = s.rsplit_once('.') {
            if let Ok(e) = self.entity(sk, ent) {
                let g = sk.entities[e.0 as usize].geom;
                let p = match (g, end) {
                    (Geom::Line { a, .. }, "a" | "start") => Some(a),
                    (Geom::Line { b, .. }, "b" | "end") => Some(b),
                    (Geom::Arc { center, .. } | Geom::Circle { center, .. }, "center") => Some(center),
                    (Geom::Arc { start, .. }, "start" | "a") => Some(start),
                    (Geom::Arc { end, .. }, "end" | "b") => Some(end),
                    _ => None,
                };
                if let Some(p) = p {
                    return Ok(p);
                }
            }
        }
        Err(format!("unknown point `{s}` (named points: {})", self.points.keys().cloned().collect::<Vec<_>>().join(", ")))
    }

    fn entity(&self, sk: &Sketch, s: &str) -> Result<EntityId, String> {
        if let Some(n) = s.strip_prefix("#e").and_then(|n| n.parse::<u32>().ok()) {
            return if (n as usize) < sk.entities.len() { Ok(EntityId(n)) } else { Err(format!("no entity #e{n}")) };
        }
        self.entities
            .get(s)
            .copied()
            .ok_or_else(|| format!("unknown line/arc/circle `{s}` (named: {})", self.entities.keys().cloned().collect::<Vec<_>>().join(", ")))
    }
}

fn vec2_arg(v: &Value, k: &str) -> Result<DVec2, String> {
    let a = v.get(k).and_then(Value::as_array).ok_or_else(|| format!("`{k}` must be [x, y]"))?;
    match (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) {
        (Some(x), Some(y)) if a.len() == 2 => Ok(DVec2::new(x, y)),
        _ => Err(format!("`{k}` must be two numbers [x, y]")),
    }
}

fn vec3_val(v: &Value) -> Option<DVec3> {
    let a = v.as_array()?;
    Some(DVec3::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?))
}

fn plane_arg(args: &Value) -> Result<Plane, String> {
    let p = match args.get("plane") {
        Some(Value::String(s)) => match s.to_ascii_uppercase().as_str() {
            "XY" | "TOP" => Plane::XY,
            "XZ" | "FRONT" => Plane::XZ,
            "YZ" | "RIGHT" => Plane::YZ,
            _ => return Err(format!("unknown plane `{s}`: use XY, XZ, YZ or {{origin, x_dir, normal}}")),
        },
        Some(o @ Value::Object(_)) => {
            let get = |k| o.get(k).and_then(vec3_val).ok_or_else(|| format!("plane `{k}` must be [x, y, z]"));
            let (origin, x, n) = (get("origin")?, get("x_dir")?, get("normal")?);
            if n.length() < 1e-9 || x.cross(n).length() < 1e-9 {
                return Err("plane `normal` must be non-zero and not parallel to `x_dir`".into());
            }
            Plane::new(origin, x, n)
        }
        _ => return Err("`plane` is required: \"XY\", \"XZ\", \"YZ\" or {origin, x_dir, normal}".into()),
    };
    Ok(match args.get("offset").and_then(Value::as_f64) {
        Some(o) => p.offset(o),
        None => p,
    })
}

fn dim(v: &Value, k: &str, values: &Values) -> Result<(Expr, f64), String> {
    let e = v.get(k).and_then(expr_arg).ok_or_else(|| format!("`{k}` (an expression) is required"))?;
    let x = e.eval(values).map_err(|err| format!("`{k}` = \"{}\": {err}", e.0))?;
    Ok((e, x))
}

/// Build a sketch from `add_sketch` arguments.
fn build_sketch(args: &Value, values: &Values) -> Result<(Sketch, Names), String> {
    let name = str_arg(args, "name").unwrap_or("Sketch").to_string();
    let mut sk = Sketch::new(name, plane_arg(args)?);
    let mut nm = Names::default();
    let elements = args.get("elements").and_then(Value::as_array).ok_or("`elements` must be a list")?;
    for (i, el) in elements.iter().enumerate() {
        let ty = str_arg(el, "type").ok_or_else(|| format!("element {i} needs a `type`"))?;
        let id = str_arg(el, "id").map(String::from).unwrap_or_else(|| format!("{ty}{i}"));
        let construction = bool_arg(el, "construction").unwrap_or(false);
        let ctx = |e: String| format!("element {i} ({ty} `{id}`): {e}");
        let first_entity = sk.entities.len();
        match ty {
            "rect" => {
                if el.get("center").is_some() {
                    let c = vec2_arg(el, "center").map_err(ctx)?;
                    let (w, gw) = dim(el, "width", values).map_err(ctx)?;
                    let (h, gh) = dim(el, "height", values).map_err(ctx)?;
                    let v = DVec2::new;
                    let p = [
                        sk.add_point(v(c.x - gw / 2.0, c.y - gh / 2.0)),
                        sk.add_point(v(c.x + gw / 2.0, c.y - gh / 2.0)),
                        sk.add_point(v(c.x + gw / 2.0, c.y + gh / 2.0)),
                        sk.add_point(v(c.x - gw / 2.0, c.y + gh / 2.0)),
                    ];
                    let l: Vec<EntityId> = (0..4).map(|k| sk.add_line(p[k], p[(k + 1) % 4])).collect();
                    sk.constrain(Constraint::Horizontal(l[0]));
                    sk.constrain(Constraint::Vertical(l[1]));
                    sk.constrain(Constraint::Horizontal(l[2]));
                    sk.constrain(Constraint::Vertical(l[3]));
                    sk.constrain(Constraint::DistanceX(p[0], p[1], w));
                    sk.constrain(Constraint::DistanceY(p[1], p[2], h));
                    let cp = sk.add_point(c);
                    let diag = sk.add_entity(Geom::Line { a: p[0], b: p[2] }, true);
                    sk.constrain(Constraint::Midpoint(cp, diag));
                    if bool_arg(el, "fix").unwrap_or(true) {
                        sk.constrain(Constraint::Fix(cp, c));
                    }
                    nm.points.insert(format!("{id}.center"), cp);
                    nm.entities.insert(format!("{id}.diagonal"), diag);
                    for (k, n) in ["bl", "br", "tr", "tl"].iter().enumerate() {
                        nm.points.insert(format!("{id}.{n}"), p[k]);
                    }
                    for (k, n) in ["bottom", "right", "top", "left"].iter().enumerate() {
                        nm.entities.insert(format!("{id}.{n}"), l[k]);
                    }
                } else {
                    let (a, b) = (vec2_arg(el, "min").map_err(ctx)?, vec2_arg(el, "max").map_err(ctx)?);
                    let l = sk.rect(a.min(b), a.max(b));
                    for (k, n) in ["bottom", "right", "top", "left"].iter().enumerate() {
                        nm.entities.insert(format!("{id}.{n}"), l[k]);
                        if let Geom::Line { a, .. } = sk.entities[l[k].0 as usize].geom {
                            nm.points.insert(format!("{id}.{}", ["bl", "br", "tr", "tl"][k]), a);
                        }
                    }
                }
            }
            "rounded_rect" => {
                let c = vec2_arg(el, "center").map_err(ctx)?;
                let (w, gw) = dim(el, "width", values).map_err(ctx)?;
                let (h, gh) = dim(el, "height", values).map_err(ctx)?;
                let (rr, gr) = dim(el, "radius", values).map_err(ctx)?;
                if gr <= 0.0 || 2.0 * gr >= gw.min(gh) {
                    return Err(ctx(format!("radius {gr} must be > 0 and less than half of the smaller side (use `rect` for sharp corners)")));
                }
                let e = sk.rounded_rect(c, w, h, rr, (gw, gh, gr));
                for (k, n) in ["bottom", "arc_br", "right", "arc_tr", "top", "arc_tl", "left", "arc_bl"].iter().enumerate() {
                    nm.entities.insert(format!("{id}.{n}"), e[k]);
                }
                nm.points.insert(format!("{id}.center"), PointId(sk.points.len() as u32 - 1));
            }
            "circle" => {
                let c = vec2_arg(el, "center").map_err(ctx)?;
                let (cons, rad) = if el.get("diameter").is_some() {
                    let (e, d) = dim(el, "diameter", values).map_err(ctx)?;
                    (Some((e, true)), d / 2.0)
                } else if el.get("radius").is_some() {
                    let (e, r) = dim(el, "radius", values).map_err(ctx)?;
                    (Some((e, false)), r)
                } else {
                    return Err(ctx("give `radius` or `diameter`".into()));
                };
                if rad <= 0.0 {
                    return Err(ctx(format!("radius {rad} must be positive")));
                }
                let e = sk.circle(c, rad);
                let cp = PointId(sk.points.len() as u32 - 1);
                if let Some((x, is_d)) = cons {
                    sk.constrain(if is_d { Constraint::Diameter(e, x) } else { Constraint::Radius(e, x) });
                }
                if bool_arg(el, "fix").unwrap_or(true) {
                    sk.constrain(Constraint::Fix(cp, c));
                }
                nm.entities.insert(id.clone(), e);
                nm.points.insert(format!("{id}.center"), cp);
            }
            "line" => {
                let e = sk.line(vec2_arg(el, "from").map_err(ctx)?, vec2_arg(el, "to").map_err(ctx)?);
                nm.entities.insert(id.clone(), e);
            }
            "arc" => {
                let c = sk.add_point(vec2_arg(el, "center").map_err(ctx)?);
                let s = sk.add_point(vec2_arg(el, "start").map_err(ctx)?);
                let e = sk.add_point(vec2_arg(el, "end").map_err(ctx)?);
                let a = sk.add_arc(c, s, e);
                nm.entities.insert(id.clone(), a);
            }
            "point" => {
                let p = sk.add_point(vec2_arg(el, "at").map_err(ctx)?);
                nm.points.insert(id.clone(), p);
            }
            other => return Err(format!("element {i}: unknown type `{other}` (rect, rounded_rect, circle, line, arc, point)")),
        }
        if construction {
            for e in first_entity..sk.entities.len() {
                sk.set_construction(EntityId(e as u32), true);
            }
        }
    }
    for (i, c) in args.get("constraints").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let ty = str_arg(c, "type").ok_or_else(|| format!("constraint {i} needs a `type`"))?;
        let ctx = |e: String| format!("constraint {i} ({ty}): {e}");
        let s = |k: &str| str_arg(c, k).ok_or_else(|| ctx(format!("`{k}` is required")));
        let pt = |k: &str| s(k).and_then(|n| nm.point(&sk, n).map_err(ctx));
        let en = |k: &str| s(k).and_then(|n| nm.entity(&sk, n).map_err(ctx));
        let val = || c.get("value").and_then(expr_arg).ok_or_else(|| ctx("`value` (an expression) is required".into()));
        use Constraint as C;
        let con = match ty {
            "coincident" => C::Coincident(pt("a")?, pt("b")?),
            "horizontal" => C::Horizontal(en("entity")?),
            "vertical" => C::Vertical(en("entity")?),
            "parallel" => C::Parallel(en("a")?, en("b")?),
            "perpendicular" => C::Perpendicular(en("a")?, en("b")?),
            "tangent" => C::Tangent(en("a")?, en("b")?),
            "equal" => C::Equal(en("a")?, en("b")?),
            "point_on_line" => C::PointOnLine(pt("point")?, en("entity")?),
            "point_on_circle" => C::PointOnCircle(pt("point")?, en("entity")?),
            "midpoint" => C::Midpoint(pt("point")?, en("entity")?),
            "symmetric" => C::Symmetric(pt("a")?, pt("b")?, en("line")?),
            "fix" => {
                let p = pt("point")?;
                let at = if c.get("at").is_some() { vec2_arg(c, "at").map_err(ctx)? } else { sk.point(p) };
                C::Fix(p, at)
            }
            "distance" => C::Distance(pt("a")?, pt("b")?, val()?),
            "distance_x" => C::DistanceX(pt("a")?, pt("b")?, val()?),
            "distance_y" => C::DistanceY(pt("a")?, pt("b")?, val()?),
            "point_line_distance" => C::PointLineDistance(pt("point")?, en("entity")?, val()?),
            "length" => C::Length(en("entity")?, val()?),
            "radius" => C::Radius(en("entity")?, val()?),
            "diameter" => C::Diameter(en("entity")?, val()?),
            "angle" => C::Angle(en("a")?, en("b")?, val()?),
            other => return Err(format!("constraint {i}: unknown type `{other}`")),
        };
        sk.constrain(con);
    }
    Ok((sk, nm))
}

fn add_sketch(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    check_revision(app, &args)?;
    let values = app.doc.data.values().map_err(|e| format!("the parameters do not evaluate: {e}"))?;
    let (mut sk, names) = build_sketch(&args, &values).map_err(|e| hint(&app.doc.data, &e))?;
    let rep = sk.solve(&values).map_err(|e| format!("sketch not added: {}", hint(&app.doc.data, &e.to_string())))?;
    use easycad_model::sketch::ConstraintStatus as S;
    if !rep.converged || rep.status == S::Conflicting {
        return Err(format!(
            "sketch not added: the solver did not converge (status {:?}, max residual {:.2e}). Constraints contradict each other or a dimension is impossible; remove or change one.",
            rep.status, rep.max_residual
        ));
    }
    let replace = args.get("replace").and_then(Value::as_u64).map(|n| SketchId(n as u32));
    if let Some(id) = replace {
        if app.doc.data.sketch(id).is_none() {
            return Err(format!("no sketch with id {}", id.0));
        }
    }
    let label = match replace {
        Some(_) => format!("edit sketch '{}'", sk.name),
        None => format!("add sketch '{}'", sk.name),
    };
    let profiles = sk.profiles();
    let regions: Vec<Value> = profiles
        .profiles
        .iter()
        .map(|p| {
            let pts: Vec<DVec2> = p
                .outer
                .iter()
                .flat_map(|s| match *s {
                    easycad_model::geom::Segment2::Line { a, b } => vec![a, b],
                    easycad_model::geom::Segment2::Arc { start, end, center, .. } => {
                        let rr = (start - center).length();
                        vec![start, end, center - DVec2::splat(rr), center + DVec2::splat(rr)]
                    }
                    easycad_model::geom::Segment2::Circle { center, radius } => vec![center - DVec2::splat(radius), center + DVec2::splat(radius)],
                })
                .collect();
            let lo = pts.iter().fold(DVec2::splat(f64::MAX), |a, b| a.min(*b));
            let hi = pts.iter().fold(DVec2::splat(f64::MIN), |a, b| a.max(*b));
            json!({"outer_segments": p.outer.len(), "holes": p.holes.len(), "approx_bounds": [[r(lo.x), r(lo.y)], [r(hi.x), r(hi.y)]]})
        })
        .collect();
    let open_chains = profiles.open.len();
    let mut warning = None;
    if rep.status == S::UnderConstrained {
        warning = Some(format!(
            "{} degrees of freedom left: the solver may move or resize this geometry when parameters change. Add dimensions or fix points for a robust parametric sketch.",
            rep.dof
        ));
    }
    if regions.is_empty() && open_chains == 0 {
        return Err("sketch not added: it has no closed regions or open chains".into());
    }
    let named: Vec<String> = names.points.keys().chain(names.entities.keys()).cloned().collect();
    let ((id, _), before) = claude_edit(app, &label, move |d| {
        Ok(match replace {
            Some(id) => {
                *d.sketch_mut(id).ok_or("sketch vanished")? = sk;
                (id, ())
            }
            None => (d.add_sketch(sk), ()),
        })
    })?;
    Ok(after_model(move |app, _| {
        let mut o = outcome_json(app, None);
        o["sketch_id"] = json!(id.0);
        o["solve"] = json!({"status": rep.status, "dof": rep.dof, "converged": rep.converged, "max_residual": rep.max_residual, "iterations": rep.iterations});
        o["closed_regions"] = json!(regions);
        o["open_chains"] = json!(open_chains);
        o["names"] = json!(named);
        if let Some(w) = warning {
            o["warning"] = json!(w);
        }
        if let Some(b) = before {
            o["user_changes_before"] = b;
        }
        o["next"] = json!(format!("use it with add_feature, e.g. {{\"Extrude\":{{\"sketch\":{},\"profiles\":\"All\",\"distance\":\"...\",\"op\":\"NewBody\"}}}}", id.0));
        done(o)
    }))
}

// ------------------------------------------------------------------ features

fn parse_kind(v: &Value) -> Result<FeatureKind, String> {
    serde_json::from_value::<FeatureKind>(v.clone()).map_err(|e| {
        let ty = v.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_default();
        format!(
            "invalid `kind` for {}: {e}. A feature is one key naming its type with its fields, e.g. {{\"Extrude\":{{\"sketch\":1,\"profiles\":\"All\",\"distance\":\"height\",\"op\":\"NewBody\"}}}}; dimensions are strings or numbers, vectors [x,y,z], ids plain integers, body ids {{\"feature\":n,\"index\":0}}.",
            if ty.is_empty() { "the feature".to_string() } else { format!("`{ty}`") }
        )
    })
}

/// References that must exist before the feature is added.
fn check_refs(d: &DocData, k: &FeatureKind) -> Result<(), String> {
    if let Some(s) = sketch_of(k) {
        if d.sketch(s).is_none() {
            let ids: Vec<String> = d.sketches.iter().map(|e| format!("{} '{}'", e.id.0, e.sketch.name)).collect();
            return Err(format!("sketch {} does not exist (sketches: {})", s.0, if ids.is_empty() { "none; add one with add_sketch".into() } else { ids.join(", ") }));
        }
    }
    let v = serde_json::to_value(k).unwrap_or(Value::Null);
    let mut bad = Vec::new();
    find_body_refs(&v, &mut |b: &Value| {
        if let Some(f) = b.get("feature").and_then(Value::as_u64) {
            if d.feature(FeatureId(f as u32)).is_none() {
                bad.push(f);
            }
        }
    });
    if !bad.is_empty() {
        return Err(format!("body reference to feature(s) {bad:?} that do not exist (bodies are named {{\"feature\": <id of the feature that made it>, \"index\": 0}})"));
    }
    Ok(())
}

fn find_body_refs(v: &Value, f: &mut dyn FnMut(&Value)) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if matches!(k.as_str(), "body" | "target") && x.is_object() {
                    f(x);
                }
                find_body_refs(x, f);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| find_body_refs(x, f)),
        _ => {}
    }
}

/// After an add / edit: report it, or take it back out when it failed.
fn settle_feature(id: FeatureId, rev: u64, keep: bool, was_ok: bool, before: Option<Value>, what: &'static str) -> Step {
    after_model(move |app, _| {
        let failed = app
            .model
            .as_ref()
            .and_then(|m| m.result.report(id))
            .map(|r| is_failure(&r.status))
            .unwrap_or(false);
        let mut o = outcome_json(app, Some(id));
        if let Some(b) = before {
            o["user_changes_before"] = b;
        }
        if failed && !keep && was_ok {
            if app.doc.data.revision == rev {
                let label = app.doc.discard_last().unwrap_or_default();
                app.params.buffers.clear();
                app.after_edit();
                app.toasts.retain(|t| !t.text.starts_with(LABEL));
                app.warn(format!("{label}: failed, taken back"));
                mark_seen(app);
                let msg = format!(
                    "The {what} failed, so it was taken back out (nothing left in the history). Report:\n{}",
                    serde_json::to_string_pretty(&o["feature"]).unwrap_or_default()
                );
                return fail(msg);
            }
            o["note"] = json!("the feature failed; the user changed the document meanwhile, so it was left in place");
        }
        done(o)
    })
}

fn add_feature(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    check_revision(app, &args)?;
    let name = str_arg(&args, "name").ok_or("`name` is required")?.to_string();
    let kind = parse_kind(args.get("kind").ok_or("`kind` is required")?)?;
    check_refs(&app.doc.data, &kind)?;
    let keep = bool_arg(&args, "keep_on_error").unwrap_or(false);
    let pos = args.get("position").and_then(Value::as_u64).map(|p| p as usize);
    let label = format!("add {} '{name}'", kind.label());
    let (id, before) = claude_edit(app, &label, move |d| {
        Ok(match pos {
            None => d.add_feature(&name, kind),
            Some(p) => {
                let p = p.min(d.features.len());
                let id = FeatureId(d.next_id);
                d.next_id += 1;
                d.features.insert(p, Feature { id, name, suppressed: false, kind });
                if let Some(rb) = d.rollback {
                    if p <= rb {
                        d.rollback = Some(rb + 1);
                    }
                }
                id
            }
        })
    })?;
    Ok(settle_feature(id, app.doc.data.revision, keep, true, before, "new feature"))
}

/// `{"Fillet": {"radius": "2"}}` on a fillet changes only `radius`.
fn merge_kind(old: &FeatureKind, patch: &Value) -> Result<FeatureKind, String> {
    let mut cur = serde_json::to_value(old).map_err(|e| e.to_string())?;
    if let (Some(co), Some(po)) = (cur.as_object_mut(), patch.as_object()) {
        if po.len() == 1 {
            let (k, v) = po.iter().next().unwrap();
            if let (Some(inner), Some(pv)) = (co.get_mut(k).and_then(Value::as_object_mut), v.as_object()) {
                for (f, x) in pv {
                    inner.insert(f.clone(), x.clone());
                }
                return parse_kind(&cur);
            }
        }
    }
    parse_kind(patch)
}

fn find_feature(d: &DocData, args: &Value) -> Result<FeatureId, String> {
    let id = args.get("id").and_then(Value::as_u64).ok_or("`id` (the feature id from get_document) is required")?;
    let id = FeatureId(id as u32);
    if d.feature(id).is_none() {
        let ids: Vec<String> = d.features.iter().map(|f| format!("{} '{}'", f.id.0, f.name)).collect();
        return Err(format!("no feature with id {} (features: {})", id.0, ids.join(", ")));
    }
    Ok(id)
}

fn edit_feature(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    check_revision(app, &args)?;
    let id = find_feature(&app.doc.data, &args)?;
    // the current status decides whether a failing edit is taken back
    Ok(after_model(move |app, _| {
        let r = (|| {
            can_edit(app)?;
            let was_ok = app.model.as_ref().and_then(|m| m.result.report(id)).map(|r| !is_failure(&r.status)).unwrap_or(true);
            let f = app.doc.data.feature(id).ok_or("the feature was deleted meanwhile")?.clone();
            let kind = match args.get("kind") {
                Some(k) => merge_kind(&f.kind, k)?,
                None => f.kind.clone(),
            };
            check_refs(&app.doc.data, &kind)?;
            let name = str_arg(&args, "name").map(String::from).unwrap_or(f.name.clone());
            let suppressed = bool_arg(&args, "suppressed").unwrap_or(f.suppressed);
            if kind == f.kind && name == f.name && suppressed == f.suppressed {
                return Err("nothing to change: give `kind`, `name` or `suppressed`".to_string());
            }
            let label = format!("edit {} '{}'", kind.label(), f.name);
            let keep = bool_arg(&args, "keep_on_error").unwrap_or(false);
            let (_, before) = claude_edit(app, &label, move |d| {
                let x = d.feature_mut(id).ok_or("feature vanished")?;
                x.kind = kind;
                x.name = name;
                x.suppressed = suppressed;
                Ok(())
            })?;
            Ok(settle_feature(id, app.doc.data.revision, keep, was_ok, before, "edit"))
        })();
        r.unwrap_or_else(fail)
    }))
}

fn delete_feature(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    check_revision(app, &args)?;
    let id = find_feature(&app.doc.data, &args)?;
    let f = app.doc.data.feature(id).cloned().ok_or("feature vanished")?;
    let label = format!("delete {} '{}'", f.kind.label(), f.name);
    let (_, before) = claude_edit(app, &label, move |d| {
        let pos = d.features.iter().position(|x| x.id == id).ok_or("feature vanished")?;
        d.features.remove(pos);
        if let Some(rb) = d.rollback {
            if pos < rb {
                d.rollback = Some(rb - 1);
            }
        }
        Ok(())
    })?;
    Ok(after_model(move |app, _| {
        let mut o = outcome_json(app, None);
        o["deleted"] = json!({"id": id.0, "name": f.name});
        if let Some(b) = before {
            o["user_changes_before"] = b;
        }
        done(o)
    }))
}

// ------------------------------------------------------------------ measure / selection

/// Selectors for a face: a semantic one when it picks exactly this face.
fn face_selector(faces: &[FaceInfo], i: usize, scale: f64) -> FaceSelector {
    let f = &faces[i];
    let snap = |n: DVec3| {
        let a = n.abs();
        let m = a.max_element();
        if m > 1.0 - 1e-6 {
            DVec3::new(if a.x == m { n.x.signum() } else { 0.0 }, if a.y == m { n.y.signum() } else { 0.0 }, if a.z == m { n.z.signum() } else { 0.0 })
        } else {
            DVec3::new(r(n.x), r(n.y), r(n.z))
        }
    };
    let unique = |s: &FaceSelector| s.resolve_infos(faces, scale).is_ok_and(|v| v == [i as u32]);
    if f.kind == SurfaceKind::Plane {
        for which in [Extremum::Max, Extremum::Min] {
            let s = FaceSelector::Extreme { normal: snap(f.normal), which };
            if unique(&s) {
                return s;
            }
        }
    }
    if f.kind == SurfaceKind::Cylinder {
        if let Some((_, axis)) = f.axis {
            let s = FaceSelector::Cylinders { radius: Some(r(f.radius)), axis: Some(snap(axis)) };
            if unique(&s) {
                return s;
            }
        }
    }
    FaceSelector::from_pick(f)
}

fn edge_selector(shape: &Shape, e: &EdgeInfo, fsel: &dyn Fn(usize) -> FaceSelector) -> EdgeSelector {
    let hint = EdgeSelector::from_pick(e);
    if let [a, b] = e.faces[..] {
        let (sa, sb) = (fsel(a as usize), fsel(b as usize));
        if !matches!(sa, FaceSelector::Hint { .. }) && !matches!(sb, FaceSelector::Hint { .. }) {
            let s = EdgeSelector::Between(sa, sb);
            if s.resolve(shape).is_ok_and(|v| v == [e.index]) {
                return s;
            }
        }
    }
    hint
}

fn face_json(app: &EasyCadApp, mb: &MeshedBody, i: usize, sel: &FaceSelector) -> Value {
    let f = &mb.faces[i];
    let mut o = json!({
        "face": i,
        "kind": format!("{:?}", f.kind),
        "area": r(f.area),
        "centroid": v3(f.centroid),
        "normal": v3(f.normal),
        "selector": sel,
    });
    if let Some((p, a)) = f.axis {
        o["axis"] = json!({"point": v3(p), "dir": v3(a)});
        o["radius"] = json!(r(f.radius));
    }
    if let Some(m) = &app.model {
        if let Some(org) = m.result.face_origin(mb.body.id, i as u32) {
            o["made_by"] = json!(org.label(&app.doc.data, Some(f)));
        }
    }
    o
}

fn edge_json(e: &EdgeInfo, sel: &EdgeSelector) -> Value {
    let mut o = json!({
        "edge": e.index,
        "kind": format!("{:?}", e.kind),
        "length": r(e.length),
        "midpoint": v3(e.midpoint),
        "from": v3(e.start),
        "to": v3(e.end),
        "faces": e.faces,
        "selector": sel,
    });
    if let Some((c, a, rad)) = e.circle {
        o["circle"] = json!({"center": v3(c), "axis": v3(a), "radius": r(rad)});
    }
    o
}

fn scale_of(mb: &MeshedBody) -> f64 {
    mb.body.shape().bbox().map(|b| b.size().length()).unwrap_or(1.0).max(1.0)
}

fn body_matches(mb: &MeshedBody, want: &Value) -> bool {
    match want {
        Value::Null => true,
        Value::String(s) => mb.body.name == *s,
        o @ Value::Object(_) => serde_json::from_value::<BodyId>(o.clone()).is_ok_and(|id| id == mb.body.id),
        _ => false,
    }
}

fn measure(args: Value) -> Step {
    after_model(move |app, _| {
        let Some(m) = app.model.clone() else { return fail("no model yet") };
        let want = args.get("body").cloned().unwrap_or(Value::Null);
        let (faces, edges) = (bool_arg(&args, "faces").unwrap_or(false), bool_arg(&args, "edges").unwrap_or(false));
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(80) as usize;
        let mut out = Vec::new();
        for mb in m.bodies.iter().filter(|b| body_matches(b, &want)) {
            let shape = mb.body.shape();
            let mut o = json!({"id": mb.body.id, "name": mb.body.name, "reference": mb.body.reference});
            if let Ok(bb) = shape.bbox() {
                o["bbox"] = json!({"min": v3(bb.min), "max": v3(bb.max), "size": v3(bb.size()), "center": v3(bb.center())});
            }
            if let Ok(v) = shape.volume() {
                o["volume_mm3"] = json!(r(v));
            }
            if let Ok(a) = shape.area() {
                o["area_mm2"] = json!(r(a));
            }
            o["face_count"] = json!(mb.faces.len());
            o["edge_count"] = json!(mb.edges.len());
            let scale = scale_of(mb);
            let fsel: Vec<FaceSelector> = if faces || edges { (0..mb.faces.len()).map(|i| face_selector(&mb.faces, i, scale)).collect() } else { Vec::new() };
            if faces {
                let v: Vec<Value> = (0..mb.faces.len()).take(limit).map(|i| face_json(app, mb, i, &fsel[i])).collect();
                o["faces"] = json!(v);
                if mb.faces.len() > limit {
                    o["faces_truncated"] = json!(format!("{} more (raise `limit`)", mb.faces.len() - limit));
                }
            }
            if edges {
                let fs = |i: usize| fsel.get(i).cloned().unwrap_or(FaceSelector::Hint { centroid: DVec3::ZERO, normal: DVec3::Z, kind: None });
                let v: Vec<Value> = mb.edges.iter().take(limit).map(|e| edge_json(e, &edge_selector(shape, e, &fs))).collect();
                o["edges"] = json!(v);
                if mb.edges.len() > limit {
                    o["edges_truncated"] = json!(format!("{} more (raise `limit`)", mb.edges.len() - limit));
                }
            }
            out.push(o);
        }
        if out.is_empty() {
            let names: Vec<&str> = m.bodies.iter().map(|b| b.body.name.as_str()).collect();
            return fail(format!("no body matches {want} (bodies: {})", if names.is_empty() { "none yet".into() } else { names.join(", ") }));
        }
        done(json!({"revision": app.doc.data.revision, "bodies": out}))
    })
}

fn selection_json(app: &EasyCadApp) -> Value {
    use crate::pickstack::PickKind;
    let Some(m) = &app.model else { return json!({"selection": []}) };
    let mut out = Vec::new();
    for k in &app.selection {
        let Some(mb) = m.meshed(k.body) else {
            out.push(json!({"body": k.body, "note": "a body of another component (assemblies)"}));
            continue;
        };
        let scale = scale_of(mb);
        let mut o = match k.kind {
            PickKind::Body => json!({"type": "body"}),
            PickKind::Face(i) if (i as usize) < mb.faces.len() => {
                let mut o = face_json(app, mb, i as usize, &face_selector(&mb.faces, i as usize, scale));
                o["type"] = json!("face");
                o
            }
            PickKind::Edge(i) if (i as usize) < mb.edges.len() => {
                let fs = |f: usize| face_selector(&mb.faces, f, scale);
                let e = &mb.edges[i as usize];
                let mut o = edge_json(e, &edge_selector(mb.body.shape(), e, &fs));
                o["type"] = json!("edge");
                o
            }
            _ => json!({"type": "stale"}),
        };
        o["body"] = json!({"id": mb.body.id, "name": mb.body.name});
        out.push(o);
    }
    let mut v = json!({"revision": app.doc.data.revision, "selection": out});
    if out_is_empty(&v) {
        v["note"] = json!("nothing selected: ask the user to click a face or edge in the viewport (Shift adds more)");
    }
    v
}

fn out_is_empty(v: &Value) -> bool {
    v["selection"].as_array().is_none_or(|a| a.is_empty())
}

// ------------------------------------------------------------------ interference / export

fn interference(args: Value) -> Step {
    after_model(move |app, _| {
        let Some(m) = app.model.clone() else { return fail("no model yet") };
        let include_reference = bool_arg(&args, "include_reference").unwrap_or(true);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            use easycad_model::assembly::{interference, AssemblyResult, InterferenceOptions};
            let opts = InterferenceOptions { include_reference, ..Default::default() };
            let flat;
            let res = match &m.asm {
                Some(a) => &*a.result,
                None => {
                    flat = AssemblyResult::flat(m.result.clone());
                    &flat
                }
            };
            let rep = interference(&m.data, res, &opts);
            let pairs: Vec<Value> = rep
                .pairs
                .iter()
                .map(|p| {
                    let mut o = json!({"a": p.a.label, "b": p.b.label, "a_body": p.a.body, "b_body": p.b.body, "volume_mm3": r(p.volume)});
                    if let Some(c) = &p.common {
                        if let Ok(bb) = c.0.shape().bbox() {
                            o["overlap_bbox"] = json!({"min": v3(bb.min), "max": v3(bb.max)});
                        }
                    }
                    o
                })
                .collect();
            let failed: Vec<Value> = rep.failed.iter().map(|(a, b, e)| json!({"a": a.label, "b": b.label, "error": e})).collect();
            let v = json!({
                "interfering_pairs": pairs,
                "pairs_tested_exactly": rep.tested,
                "pairs_culled_by_bbox": rep.culled,
                "failed": failed,
                "verdict": if rep.pairs.is_empty() { "no interference" } else { "bodies overlap" },
            });
            let _ = tx.send(Ok(vec![json_text(&v)]));
        });
        Step::Recv(rx)
    })
}

fn export(args: Value) -> Result<Step, String> {
    let path = std::path::PathBuf::from(str_arg(&args, "path").ok_or("`path` is required")?);
    if !path.is_absolute() {
        return Err("`path` must be absolute".into());
    }
    if !path.parent().is_some_and(|p| p.is_dir()) {
        return Err(format!("the folder of {} does not exist", path.display()));
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let fmt = str_arg(&args, "format").map(str::to_lowercase).unwrap_or(ext);
    let format = match fmt.as_str() {
        "stl" => ExportFormat::Stl,
        "3mf" => ExportFormat::ThreeMf,
        "step" | "stp" => ExportFormat::Step,
        other => return Err(format!("unknown format `{other}`: stl, 3mf or step")),
    };
    let include_reference = bool_arg(&args, "include_reference").unwrap_or(false);
    let orient = bool_arg(&args, "orient_to_plate").unwrap_or(format != ExportFormat::Step);
    Ok(after_model(move |app, _| {
        let (data, blobs, library) = (app.doc.data.clone(), app.doc.blobs.clone(), app.doc.library.clone());
        let opts = ExportOptions { profile: Some(data.active_profile.clone()), orient_to_plate: orient, include_reference };
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut doc = Document::new(&data.name);
            doc.data = data;
            doc.blobs = blobs;
            doc.library = library;
            let r = doc
                .export(format, &path, &opts)
                .map(|_| {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    vec![json_text(&json!({"exported": path.display().to_string(), "bytes": size, "process_profile": opts.profile}))]
                })
                .map_err(|e: DocError| format!("export failed: {e}"));
            let _ = tx.send(r);
        });
        Step::Recv(rx)
    }))
}

// ------------------------------------------------------------------ screenshot

/// "top front right" → a view-cube region; "iso" → the default isometric view.
fn parse_view(s: &str) -> Result<Option<crate::camera::CubeRegion>, String> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() || s == "current" {
        return Ok(None);
    }
    if s == "iso" || s == "isometric" || s == "home" {
        return Ok(Some(crate::camera::CubeRegion([1, -1, 1])));
    }
    let mut c = [0i8; 3];
    for w in s.split(|ch: char| ch.is_whitespace() || ch == '-' || ch == '_' || ch == ',').filter(|w| !w.is_empty()) {
        let (i, v) = match w {
            "front" => (1, -1),
            "back" | "rear" => (1, 1),
            "left" => (0, -1),
            "right" => (0, 1),
            "top" => (2, 1),
            "bottom" => (2, -1),
            _ => return Err(format!("unknown view word `{w}`: use front/back/left/right/top/bottom, iso, or current")),
        };
        if c[i] != 0 {
            return Err(format!("view `{s}` names the same axis twice"));
        }
        c[i] = v;
    }
    Ok(Some(crate::camera::CubeRegion(c)))
}

fn screenshot(app: &mut EasyCadApp, ctx: &egui::Context, args: Value) -> Result<Step, String> {
    let view = parse_view(str_arg(&args, "view").unwrap_or("current"))?;
    let fit = bool_arg(&args, "fit").unwrap_or(view.is_some());
    let max_w = args.get("max_width").and_then(Value::as_u64).unwrap_or(1024).clamp(128, 4096) as u32;
    if !crate::widgets::window_visible(ctx) {
        return Err("the easyCAD window is minimized or hidden, so nothing is drawn: ask the user to bring it to the front".into());
    }
    if !matches!(app.mode, Mode::Model) {
        return Err("the app is not showing the 3D model (sketch, parts or drawing screen is open)".into());
    }
    Ok(after_model(move |app, ctx| {
        // Claude's own step toasts would cover the model in its picture
        app.toasts.retain(|t| !t.text.starts_with(LABEL));
        let aspect = app.viewport_size.x / app.viewport_size.y.max(1.0);
        match view {
            Some(reg) => {
                let rot = app.camera.region_rot(reg);
                app.camera.animate_rot(rot, app.now, aspect, fit);
            }
            None if fit => {
                if let Some(bb) = app.model.as_ref().and_then(|m| m.bbox) {
                    app.camera.fit_sphere(bb.center().as_vec3(), (bb.size().length() * 0.5) as f32, aspect);
                }
            }
            None => {}
        }
        ctx.request_repaint();
        let label = view.map(|v| v.label()).unwrap_or_else(|| "current view".into());
        Step::AfterCamera(
            2,
            Box::new(move |app, ctx| {
                let rect = app.viewport_rect;
                let ppp = ctx.pixels_per_point();
                let rev = app.doc.data.revision;
                let (tx, rx) = channel();
                let cb = egui::ScreenshotCallback::new(move |img: Arc<egui::ColorImage>| {
                    let _ = tx.send(encode_shot(&img, rect, ppp, max_w).map(|(png, w, h)| {
                        vec![proto::image(&png), proto::text(format!("{label}, {w}×{h} px, document revision {rev}"))]
                    }));
                });
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(cb));
                ctx.request_repaint();
                Step::Recv(rx)
            }),
        )
    }))
}

/// Crop the window image to the viewport, downscale, encode PNG.
fn encode_shot(img: &egui::ColorImage, rect: egui::Rect, ppp: f32, max_w: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let [iw, ih] = img.size;
    let full = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(iw as f32, ih as f32));
    let px = if rect.is_positive() { egui::Rect::from_min_max((rect.min.to_vec2() * ppp).to_pos2(), (rect.max.to_vec2() * ppp).to_pos2()).intersect(full) } else { full };
    let (x0, y0) = (px.min.x.max(0.0) as usize, px.min.y.max(0.0) as usize);
    let (w, h) = ((px.width() as usize).min(iw - x0), (px.height() as usize).min(ih - y0));
    if w < 8 || h < 8 {
        return Err("the viewport is too small to capture".into());
    }
    let mut buf = image::RgbaImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let c = img.pixels[(y0 + y) * iw + x0 + x];
            buf.put_pixel(x as u32, y as u32, image::Rgba([c.r(), c.g(), c.b(), 255]));
        }
    }
    let buf = if w as u32 > max_w {
        let nh = (h as f64 * max_w as f64 / w as f64).round().max(1.0) as u32;
        image::imageops::resize(&buf, max_w, nh, image::imageops::FilterType::Triangle)
    } else {
        buf
    };
    let (ow, oh) = buf.dimensions();
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(buf)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("PNG encoding failed: {e}"))?;
    Ok((png, ow, oh))
}

// ------------------------------------------------------------------ documents

fn open_or_new(app: &mut EasyCadApp, tool: &str, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    let unsaved = app.doc.data.revision != app.saved_revision && !app.doc.data.features.is_empty();
    if unsaved && !bool_arg(&args, "discard_unsaved").unwrap_or(false) {
        return Err(format!(
            "'{}' has unsaved changes. Ask the user whether to save it (save_document) or discard them (discard_unsaved: true).",
            app.doc.data.name
        ));
    }
    if tool == "open_document" {
        let p = std::path::PathBuf::from(str_arg(&args, "path").ok_or("`path` (a .ecad folder) is required")?);
        if !p.join("doc.json").is_file() {
            return Err(format!("{} is not a .ecad document folder (no doc.json inside)", p.display()));
        }
        app.open_path(&p)?;
        claude_toast(app, format!("{LABEL}opened {}", p.display()));
    } else {
        let name = str_arg(&args, "name").unwrap_or("Untitled").trim().to_string();
        let s = crate::settings::Settings::load();
        let profile = s.default_profile.or(s.last_profile).unwrap_or_default();
        app.create_doc(&name, &profile);
        claude_toast(app, format!("{LABEL}new document '{name}'"));
    }
    Ok(after_model(|app, _| {
        let v = document_json(app, false);
        mark_seen(app);
        done(v)
    }))
}

fn save_document(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    let p = match (str_arg(&args, "path"), &app.path) {
        (Some(p), _) => {
            let p = std::path::PathBuf::from(p);
            if !p.is_absolute() {
                return Err("`path` must be absolute".into());
            }
            p
        }
        (None, Some(p)) => p.clone(),
        (None, None) => return Err("this document was never saved: give an absolute `path` (a .ecad folder name)".into()),
    };
    let saved = app.save_to(p)?;
    claude_toast(app, format!("{LABEL}saved {}", saved.display()));
    Ok(done(json!({"saved": saved.display().to_string(), "revision": app.doc.data.revision})))
}

fn undo(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    let label = app.doc.undo_label().ok_or("nothing to undo")?.to_string();
    if !label.starts_with(LABEL) && !bool_arg(&args, "allow_user_steps").unwrap_or(false) {
        return Err(format!("the last step is the user's (\"{label}\"), not yours, so it was not undone"));
    }
    app.undo();
    mark_seen(app);
    Ok(after_model(move |app, _| {
        let mut o = outcome_json(app, None);
        o["undone"] = json!(label);
        o["next_undo"] = json!(app.doc.undo_label());
        done(o)
    }))
}
