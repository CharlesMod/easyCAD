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
use easycad_model::feature::{BodyId, Feature, FeatureId, FeatureKind, MeshUp, SketchId};
use easycad_model::geom::Plane;
use easycad_model::occt::{EdgeInfo, FaceInfo, Shape, SurfaceKind};
use easycad_model::params::{Expr, Values};
use easycad_model::positions::{PoseMove, PoseTarget, PositionId};
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
    /// Claude's own regeneration of the document (`Purpose::Agent`): what every
    /// tool reads, whatever the user's viewport shows.
    pub model: Option<Arc<crate::worker::Output>>,
    /// Claude's own view (camera, visibility, section, style).
    pub view: AgentView,
    /// Claude's offscreen renderer (its own mesh cache and target).
    gpu: Option<crate::viewport::gpu::ViewportGpu>,
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
    /// A screenshot (camera, display overrides, capture, put back).
    Shot(Box<Shot>),
    /// A background thread (export, interference, the screenshot callback).
    Recv(Receiver<ToolResult>),
}

impl Bridge {
    /// A bridge with no server (tests drive it through [`Bridge::submit`]).
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Bridge { server: None, tx, rx, queue: VecDeque::new(), seen: None, calls: 0, model: None, view: AgentView::default(), gpu: None }
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
            Step::Shot(sh) => return sh.advance(app).map_err(Step::Shot),
            Step::Recv(rx) => match rx.try_recv() {
                Ok(r) => return Ok(r),
                Err(TryRecvError::Empty) => return Err(Step::Recv(rx)),
                Err(TryRecvError::Disconnected) => return Ok(Err("the background job ended without a result".into())),
            },
        }
    }
}

/// Claude's model is of the document as it is now (else ask the worker for it).
/// It is the whole committed document: the user's dialog previews, rollback-to-edit,
/// section view and active component do not change it.
fn model_ready(app: &mut EasyCadApp) -> bool {
    let (rev, id) = (app.doc.data.revision, app.doc.data.doc_id.clone());
    let busy = app.worker.busy_with(Purpose::Agent);
    let Some(b) = app.mcp.as_ref() else { return false };
    // a position deleted meanwhile: back to the modeled pose
    let pose = b.view.position.filter(|p| app.doc.data.position(*p).is_some());
    if b.model.as_ref().is_some_and(|m| m.data.revision == rev && m.data.doc_id == id && m.pose_requested == pose) {
        return true;
    }
    if !busy {
        app.worker.submit_full(crate::worker::Request {
            id: 0,
            purpose: Purpose::Agent,
            data: app.doc.data.clone(),
            blobs: app.doc.blobs.clone(),
            library: app.doc.library.clone(),
            prefixes: Vec::new(),
            drawing: None,
            quality: app.settings.mesh.quality(),
            section: None,
            active: Vec::new(),
            explode: 0.0,
            cut: None,
            pose,
        });
    }
    false
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
    if matches!(name, "interference" | "screenshot" | "measure") {
        if let Some(p) = args.get("position") {
            match resolve_position(&app.doc.data, p) {
                Ok(p) => {
                    if let Some(b) = app.mcp.as_mut() {
                        b.view.position = p;
                    }
                }
                Err(e) => return fail(e),
            }
        }
    }
    let r = match name {
        "get_document" => Ok(get_document(args)),
        "set_param" => set_param(app, args),
        "add_sketch" => add_sketch(app, args),
        "add_feature" => add_feature(app, args),
        "import_mesh" => import_mesh(app, args),
        "edit_feature" => edit_feature(app, args),
        "delete_feature" => delete_feature(app, args),
        "regenerate" => Ok(after_model(|app, _| done(regen_json(app)))),
        "measure" => Ok(measure(args)),
        "get_selection" => Ok(after_model(|app, _| done(selection_json(app)))),
        "interference" => Ok(interference(args)),
        "clearance" => Ok(clearance(args)),
        "screenshot" => screenshot(app, ctx, args),
        "view_state" => Ok(view_state(args)),
        "list_positions" => Ok(after_model(|app, _| done(positions_json(app)))),
        "add_position" => add_position(app, args),
        "edit_position" => edit_position(app, args),
        "apply_position" => apply_position(app, args),
        "render_sketch" => crate::mcp_sketch::render_sketch(app, &args).map(|v| Step::Done(Ok(v))),
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
pub(crate) fn r(x: f64) -> f64 {
    let v = (x * 1e4).round() / 1e4;
    if v == 0.0 {
        0.0
    } else {
        v
    }
}

pub(crate) fn v3(v: DVec3) -> Value {
    json!([r(v.x), r(v.y), r(v.z)])
}

pub(crate) fn str_arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

pub(crate) fn bool_arg(args: &Value, k: &str) -> Option<bool> {
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
/// Claude edits the document while the user is modelling or has a sketch open (the
/// sketch session writes back only its own sketch entry when they finish it, so
/// Claude's other edits survive; `add_sketch` refuses to replace that very sketch).
fn can_edit(app: &EasyCadApp) -> Result<(), String> {
    match &app.mode {
        Mode::Model | Mode::Sketch(_) => {}
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
    refresh_open_sketch(app);
    claude_toast(app, label);
    mark_seen(app);
    Ok((out, before))
}

/// The user's open sketch, if any: (its document id when it edits an existing
/// sketch, its name).
fn open_sketch(app: &EasyCadApp) -> Option<(Option<SketchId>, String)> {
    match &app.mode {
        Mode::Sketch(s) => Some((s.id, s.sketch.name.clone())),
        _ => None,
    }
}

/// The open sketch session evaluates its dimensions with the parameters it read
/// when it opened: give it the ones Claude may just have changed.
fn refresh_open_sketch(app: &mut EasyCadApp) {
    if let Mode::Sketch(s) = &mut app.mode {
        s.values = crate::adapters::values(&app.doc.data);
        s.names = app.doc.data.params.list.iter().map(|p| p.name.clone()).collect();
    }
}

/// Refuses a step that would take away the sketch the user has open.
fn keeps_open_sketch(app: &EasyCadApp) -> bool {
    match open_sketch(app) {
        Some((Some(id), _)) => app.doc.data.sketch(id).is_some(),
        _ => true,
    }
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
    let Some(m) = agent_model(app) else { return json!([]) };
    let v: Vec<Value> = m
        .bodies
        .iter()
        .map(|b| {
            let mut o = json!({"id": b.body.id, "name": b.body.name, "reference": b.body.reference});
            if b.body.is_mesh() {
                o["mesh_body"] = json!(true);
            }
            if let Ok(bb) = b.body.bbox() {
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
    let Some(m) = agent_model(app) else { return o };
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
    let Some(m) = agent_model(app) else { return json!({"revision": d.revision, "features": []}) };
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
    let am = agent_model(app);
    let reports: HashMap<FeatureId, &FeatureReport> = am.iter().flat_map(|m| m.result.reports.iter()).map(|r| (r.feature, r)).collect();
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
        (Mode::Sketch(s), _) => match s.id {
            Some(id) => format!("editing sketch '{}' (id {}); you can edit everything else, render_sketch shows their draft", s.sketch.name, id.0),
            None => format!("drawing a new sketch '{}'; you can edit the document meanwhile, render_sketch shows their draft", s.sketch.name),
        },
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

pub(crate) fn plane_json(p: &Plane) -> Value {
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
        if let Some((Some(open), name)) = open_sketch(app) {
            if open == id {
                return Err(format!(
                    "The user has sketch '{name}' (id {}) open in easyCAD: replacing it now would be overwritten when they finish. Add a new sketch instead, or ask them to finish it first.",
                    id.0
                ));
            }
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
                if k == "tools" {
                    x.as_array().into_iter().flatten().filter(|t| t.is_object()).for_each(&mut *f);
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
        let failed = agent_model(app).and_then(|m| m.result.report(id).map(|r| is_failure(&r.status))).unwrap_or(false);
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

fn expr3_arg(args: &Value, k: &str) -> Result<[Expr; 3], String> {
    match args.get(k) {
        None | Some(Value::Null) => Ok([Expr::new("0"), Expr::new("0"), Expr::new("0")]),
        Some(Value::Array(a)) if a.len() == 3 => {
            let e: Vec<Expr> = a.iter().filter_map(expr_arg).collect();
            <[Expr; 3]>::try_from(e).map_err(|_| format!("`{k}` must be [x, y, z] of numbers or expressions"))
        }
        _ => Err(format!("`{k}` must be [x, y, z]")),
    }
}

/// Read a mesh file from disk into the document as a mesh body (`ImportMesh`).
fn import_mesh(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    check_revision(app, &args)?;
    let path = std::path::PathBuf::from(str_arg(&args, "path").ok_or("`path` is required")?);
    if !path.is_absolute() {
        return Err("`path` must be absolute".into());
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if !easycad_model::meshbody::MESH_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!("`{}` is not a mesh file (stl, obj, glb, gltf)", path.display()));
    }
    let data = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let up = match str_arg(&args, "up").map(str::to_ascii_uppercase).as_deref() {
        Some("Y") => MeshUp::Y,
        Some("Z") => MeshUp::Z,
        None if ext == "glb" || ext == "gltf" => MeshUp::Y,
        None => MeshUp::Z,
        Some(o) => return Err(format!("`up` is Z or Y, not {o}")),
    };
    let scale = args.get("scale").and_then(expr_arg).unwrap_or_else(|| Expr::new("1"));
    let rotate_deg = match args.get("rotate_deg") {
        None | Some(Value::Null) => DVec3::ZERO,
        Some(v) => vec3_val(v).ok_or("`rotate_deg` must be [x, y, z] in degrees")?,
    };
    let translate = expr3_arg(&args, "translate")?;
    let reference = bool_arg(&args, "reference").unwrap_or(false);
    let file = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| format!("mesh.{ext}"));
    // the feature takes the import's file name; a `name` renames it
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Mesh".into());
    let name = str_arg(&args, "name").map(str::to_string).unwrap_or(stem);
    let before = user_changes(app);
    let label = format!("{LABEL}import mesh '{file}'");
    let (_, id) = app
        .doc
        .import_mesh_as(&label, &file, &name, data, |import| FeatureKind::ImportMesh { import, scale, up, rotate_deg, translate, reference })
        .map_err(|e| e.to_string())?;
    app.params.buffers.clear();
    app.after_edit();
    app.fit_pending = true;
    claude_toast(app, label);
    mark_seen(app);
    Ok(settle_feature(id, app.doc.data.revision, false, true, before, "mesh import"))
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
            let was_ok = agent_model(app).and_then(|m| m.result.report(id).map(|r| !is_failure(&r.status))).unwrap_or(true);
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

fn face_json(app: &EasyCadApp, m: &crate::worker::Output, mb: &MeshedBody, i: usize, sel: &FaceSelector) -> Value {
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
    if let Some(org) = m.result.face_origin(mb.body.id, i as u32) {
        o["made_by"] = json!(org.label(&app.doc.data, Some(f)));
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
    mb.body.bbox().map(|b| b.size().length()).unwrap_or(1.0).max(1.0)
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
        let Some(m) = agent_model(app) else { return fail("no model yet") };
        let want = args.get("body").cloned().unwrap_or(Value::Null);
        let (faces, edges) = (bool_arg(&args, "faces").unwrap_or(false), bool_arg(&args, "edges").unwrap_or(false));
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(80) as usize;
        let mut out = Vec::new();
        for mb in m.bodies.iter().filter(|b| body_matches(b, &want)) {
            let shape = mb.body.shape();
            let mut o = json!({"id": mb.body.id, "name": mb.body.name, "reference": mb.body.reference});
            if let Ok(bb) = mb.body.bbox() {
                o["bbox"] = json!({"min": v3(bb.min), "max": v3(bb.max), "size": v3(bb.size()), "center": v3(bb.center())});
            }
            if let Ok(v) = mb.body.volume() {
                o["volume_mm3"] = json!(r(v));
            }
            if let Some(ms) = mb.body.mesh_solid() {
                let (open, nonmanifold) = ms.edge_defects();
                o["mesh"] = json!({"triangles": ms.tris.len(), "watertight": open == 0 && nonmanifold == 0});
            }
            if let Ok(a) = shape.area() {
                o["area_mm2"] = json!(r(a));
            }
            o["face_count"] = json!(mb.faces.len());
            o["edge_count"] = json!(mb.edges.len());
            let scale = scale_of(mb);
            let fsel: Vec<FaceSelector> = if faces || edges { (0..mb.faces.len()).map(|i| face_selector(&mb.faces, i, scale)).collect() } else { Vec::new() };
            if faces {
                let v: Vec<Value> = (0..mb.faces.len()).take(limit).map(|i| face_json(app, &m, mb, i, &fsel[i])).collect();
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

/// The user's selection, described on Claude's (whole, uncut) model: what the user
/// clicked in their viewport (maybe cut by their section view, maybe rolled back by
/// an open dialog) is matched by geometry.
fn selection_json(app: &EasyCadApp) -> Value {
    use crate::pickstack::PickKind;
    let (Some(um), Some(m)) = (&app.model, agent_model(app)) else { return json!({"selection": []}) };
    let mut out = Vec::new();
    for k in &app.selection {
        let (Some(umb), Some(mb)) = (um.meshed(k.body), m.meshed(k.body)) else {
            out.push(json!({"body": k.body, "note": "a body of another component (assemblies), or one that no longer exists"}));
            continue;
        };
        let scale = scale_of(mb);
        let mut o = match k.kind {
            PickKind::Body => json!({"type": "body"}),
            PickKind::Face(i) if (i as usize) < umb.faces.len() => {
                match FaceSelector::from_pick(&umb.faces[i as usize]).resolve_infos(&mb.faces, scale).ok().and_then(|v| v.first().copied()) {
                    Some(j) => {
                        let mut o = face_json(app, &m, mb, j as usize, &face_selector(&mb.faces, j as usize, scale));
                        o["type"] = json!("face");
                        o
                    }
                    None => json!({"type": "face", "note": "a face of the user's section cut, not of the model"}),
                }
            }
            PickKind::Edge(i) if (i as usize) < umb.edges.len() => {
                let fs = |f: usize| face_selector(&mb.faces, f, scale);
                match EdgeSelector::from_pick(&umb.edges[i as usize]).resolve(mb.body.shape()).ok().and_then(|v| v.first().copied()) {
                    Some(j) => match mb.edges.iter().find(|e| e.index == j) {
                        Some(e) => {
                            let mut o = edge_json(e, &edge_selector(mb.body.shape(), e, &fs));
                            o["type"] = json!("edge");
                            o
                        }
                        None => json!({"type": "stale"}),
                    },
                    None => json!({"type": "edge", "note": "an edge of the user's section cut, not of the model"}),
                }
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
        let Some(m) = agent_model(app) else { return fail("no model yet") };
        let include_reference = bool_arg(&args, "include_reference").unwrap_or(true);
        if let Some(e) = &m.pose_error {
            return fail(format!("the position could not be applied: {e}"));
        }
        let pos_name = position_name(&m.data, m.pose);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            use easycad_model::assembly::{interference, AssemblyResult, InterferenceOptions};
            let flat;
            // a plain part: its bodies against each other (they share the root path)
            let (res, within_occurrence) = match &m.asm {
                Some(a) => (&*a.result, false),
                None => {
                    flat = AssemblyResult::flat(m.result.clone());
                    (&flat, true)
                }
            };
            let opts = InterferenceOptions { include_reference, within_occurrence, ..Default::default() };
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
                "position": pos_name,
            });
            let _ = tx.send(Ok(vec![json_text(&v)]));
        });
        Step::Recv(rx)
    })
}

/// One side of a `clearance` question: bodies, or faces / edges picked by selector.
struct Side {
    /// (index into the model's bodies, part, label)
    parts: Vec<(usize, easycad_model::occt::SubShape, String)>,
}

fn clearance_side(m: &crate::worker::Output, v: &Value, what: &str) -> Result<Side, String> {
    use easycad_model::occt::SubShape;
    let names = || m.bodies.iter().map(|b| format!("'{}'", b.body.name)).collect::<Vec<_>>().join(", ");
    let (body, face, edge) = match v {
        Value::String(_) => (v.clone(), None, None),
        Value::Object(o) if o.contains_key("feature") => (v.clone(), None, None),
        Value::Object(o) => (
            o.get("body").cloned().unwrap_or(Value::Null),
            o.get("face").cloned(),
            o.get("edge").cloned(),
        ),
        _ => return Err(format!("`{what}`: a body name, a body id {{\"feature\":..,\"index\":..}}, or {{\"body\":..,\"face\":<selector>}} / {{\"body\":..,\"edge\":<selector>}}")),
    };
    let fsel = face
        .map(|f| serde_json::from_value::<FaceSelector>(f).map_err(|e| format!("`{what}.face` is not a face selector: {e}")))
        .transpose()?;
    let esel = edge
        .map(|f| serde_json::from_value::<EdgeSelector>(f).map_err(|e| format!("`{what}.edge` is not an edge selector: {e}")))
        .transpose()?;
    let mut parts = Vec::new();
    let mut matched_body = false;
    for (bi, mb) in m.bodies.iter().enumerate().filter(|(_, b)| body_matches(b, &body)) {
        matched_body = true;
        let name = &mb.body.name;
        let shape = mb.body.shape();
        if let Some(fs) = &fsel {
            for i in fs.resolve(shape).unwrap_or_default() {
                parts.push((bi, SubShape::Face(i), format!("'{name}' face {i}")));
            }
        } else if let Some(es) = &esel {
            for i in es.resolve(shape).unwrap_or_default() {
                parts.push((bi, SubShape::Edge(i), format!("'{name}' edge {i}")));
            }
        } else {
            parts.push((bi, SubShape::Whole, format!("'{name}'")));
        }
    }
    if !matched_body {
        return Err(format!("`{what}`: no body {body} (bodies: {})", names()));
    }
    if parts.is_empty() {
        return Err(format!("`{what}`: the selector matched nothing on those bodies; check it with screenshot highlight or measure faces/edges"));
    }
    Ok(Side { parts })
}

fn clearance(args: Value) -> Step {
    after_model(move |app, _| {
        let Some(m) = agent_model(app) else { return fail("no model yet") };
        let Some(a) = args.get("a") else { return fail("`a` is required: a body name, or {\"body\":..,\"face\":<selector>}") };
        let a = match clearance_side(&m, a, "a") {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        let a_bodies: Vec<usize> = a.parts.iter().map(|p| p.0).collect();
        let b = match args.get("b").filter(|v| !v.is_null()) {
            Some(v) => match clearance_side(&m, v, "b") {
                Ok(s) => s,
                Err(e) => return fail(e),
            },
            None => Side {
                parts: m
                    .bodies
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !a_bodies.contains(i))
                    .map(|(i, mb)| (i, easycad_model::occt::SubShape::Whole, format!("'{}'", mb.body.name)))
                    .collect(),
            },
        };
        if b.parts.is_empty() {
            return fail("there is no other body to measure to");
        }
        if a.parts.len() * b.parts.len() > 400 {
            return fail(format!("{} × {} pairs is too many: narrow the selectors", a.parts.len(), b.parts.len()));
        }
        let all_others = args.get("b").is_none_or(Value::is_null);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut rows = Vec::new();
            let mut errors = Vec::new();
            for (ai, ap, al) in &a.parts {
                for (bi, bp, bl) in &b.parts {
                    if ai == bi && (*ap == easycad_model::occt::SubShape::Whole || *bp == easycad_model::occt::SubShape::Whole) {
                        continue;
                    }
                    match m.bodies[*ai].body.shape().min_distance(*ap, m.bodies[*bi].body.shape(), *bp) {
                        Ok(d) => rows.push((d, al.clone(), bl.clone())),
                        Err(e) => errors.push(json!({"a": al, "b": bl, "error": e.to_string()})),
                    }
                }
            }
            rows.sort_by(|x, y| x.0.distance.total_cmp(&y.0.distance));
            let row = |(d, al, bl): &(easycad_model::occt::Distance, String, String)| {
                let mut o = json!({
                    "a": al, "b": bl, "distance_mm": r(d.distance),
                    "point_on_a": v3(d.on_self), "point_on_b": v3(d.on_other),
                });
                if d.distance > 1e-7 {
                    o["direction_a_to_b"] = v3((d.on_other - d.on_self).normalize());
                } else {
                    o["touching"] = json!(true);
                }
                o
            };
            let Some(best) = rows.first() else {
                let _ = tx.send(Err(format!("no distance could be computed: {}", json!(errors))));
                return;
            };
            let mut v = json!({"min_distance_mm": r(best.0.distance), "closest": row(best)});
            if best.0.distance <= 1e-7 {
                v["note"] = json!("they touch or overlap (distance 0): use interference for the overlap volume");
            }
            if rows.len() > 1 {
                let label = if all_others { "by_body" } else { "pairs" };
                v[label] = json!(rows.iter().take(12).map(row).collect::<Vec<_>>());
                if rows.len() > 12 {
                    v["more_pairs"] = json!(rows.len() - 12);
                }
            }
            if !errors.is_empty() {
                v["failed"] = json!(errors);
            }
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

// ------------------------------------------------------------------ Claude's view (offscreen)

/// Highlight colours of Claude's screenshots.
pub const SHOT_FACE: [f32; 4] = [1.0, 0.36, 0.08, 0.9];
pub const SHOT_BODY: [f32; 4] = [1.0, 0.36, 0.08, 0.6];
const SHOT_EDGE: [u8; 3] = [255, 92, 20];

/// A section plane of Claude's view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AgentSection {
    /// 0 = X, 1 = Y, 2 = Z.
    pub axis: usize,
    /// Plane position along the axis (mm).
    pub pos: f64,
    /// Keep the side above the plane (else the side below).
    pub keep_above: bool,
}

impl AgentSection {
    /// Plane point and normal; what stays is behind the normal.
    fn plane(&self) -> (DVec3, DVec3) {
        let mut n = DVec3::ZERO;
        n[self.axis] = if self.keep_above { -1.0 } else { 1.0 };
        let mut o = DVec3::ZERO;
        o[self.axis] = self.pos;
        (o, n)
    }
}

/// Claude's own view of the model: camera, visibility, section, style. It lasts
/// across calls and has nothing to do with the user's viewport.
#[derive(Clone)]
pub struct AgentView {
    pub camera: crate::camera::Camera,
    /// The camera has been placed (else the first picture fits an iso view).
    pub placed: bool,
    pub view_name: String,
    pub hidden: std::collections::HashSet<BodyId>,
    pub section: Option<AgentSection>,
    pub display: crate::viewport::gpu::DisplayMode,
    /// Claude's position (animation state), independent of the user's.
    pub position: Option<PositionId>,
}

impl Default for AgentView {
    fn default() -> Self {
        AgentView {
            camera: crate::camera::Camera::default(),
            placed: false,
            view_name: "Iso".into(),
            hidden: Default::default(),
            section: None,
            display: crate::viewport::gpu::DisplayMode::ShadedEdges,
            position: None,
        }
    }
}

/// Per picture: what to paint orange and how to draw the rest.
#[derive(Clone, Debug, Default)]
struct ShotStyle {
    faces: Vec<(BodyId, u32)>,
    edges: Vec<(BodyId, u32)>,
    bodies: Vec<BodyId>,
    ghost: bool,
    xray: bool,
}

impl ShotStyle {
    fn alpha(&self, b: BodyId) -> Option<f32> {
        if self.xray {
            return Some(0.32);
        }
        let involved = self.bodies.contains(&b) || self.faces.iter().any(|f| f.0 == b) || self.edges.iter().any(|e| e.0 == b);
        (self.ghost && !involved).then_some(0.16)
    }
    fn through(&self) -> bool {
        self.ghost || self.xray
    }
}

/// "top front right" → a view-cube region; "iso" → the default isometric view.
fn parse_view(s: &str) -> Result<Option<crate::camera::CubeRegion>, String> {
    let s = s.trim().to_ascii_lowercase();
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
            _ => return Err(format!("unknown view word `{w}`: use front/back/left/right/top/bottom, iso, current (your last view) or user (the user's view)")),
        };
        if c[i] != 0 {
            return Err(format!("view `{s}` names the same axis twice"));
        }
        c[i] = v;
    }
    if c == [0, 0, 0] {
        return Err("empty view name".into());
    }
    Ok(Some(crate::camera::CubeRegion(c)))
}

#[derive(Clone)]
enum ViewSpec {
    Named(crate::camera::CubeRegion, String),
    Current,
    User,
}

fn view_spec(s: &str) -> Result<ViewSpec, String> {
    let l = s.trim().to_ascii_lowercase();
    match l.as_str() {
        "" | "current" => Ok(ViewSpec::Current),
        "user" | "user's" => Ok(ViewSpec::User),
        _ => {
            let iso = matches!(l.as_str(), "iso" | "isometric" | "home");
            let r = parse_view(&l)?.expect("named");
            Ok(ViewSpec::Named(r, if iso { "Iso".into() } else { r.label() }))
        }
    }
}

/// What to highlight, as asked.
#[derive(Default)]
struct HighlightReq {
    faces: Vec<FaceSelector>,
    edges: Vec<EdgeSelector>,
    features: Vec<FeatureId>,
    bodies: Vec<Value>,
    on: Value,
}

impl HighlightReq {
    fn is_empty(&self) -> bool {
        self.faces.is_empty() && self.edges.is_empty() && self.features.is_empty() && self.bodies.is_empty()
    }
}

/// Bodies to draw: the agent model's, or their cut halves plus the caps.
struct Scene {
    bodies: Vec<MeshedBody>,
    caps: Vec<(Arc<easycad_model::occt::Mesh>, glam::DAffine3)>,
}

enum Phase {
    Model,
    Cut(Receiver<Scene>),
}

pub struct Shot {
    views: Vec<ViewSpec>,
    single: bool,
    fit: Option<bool>,
    max_w: u32,
    hl: HighlightReq,
    ghost: Option<bool>,
    xray: bool,
    args: Value,
    phase: Phase,
}

fn body_ref_matches(id: BodyId, name: &str, want: &Value) -> bool {
    match want {
        Value::String(s) => name == s,
        o @ Value::Object(_) => serde_json::from_value::<BodyId>(o.clone()).is_ok_and(|b| b == id),
        _ => false,
    }
}

fn parse_section(s: &Value) -> Result<Option<(usize, Option<f64>, bool)>, String> {
    if s.is_null() || s == &json!(false) {
        return Ok(None);
    }
    let axis = match str_arg(s, "axis").map(str::to_ascii_lowercase).as_deref() {
        Some("x") => 0,
        Some("y") => 1,
        Some("z") => 2,
        _ => return Err("`section.axis` must be \"x\", \"y\" or \"z\" (or `section: null` to remove the cut)".into()),
    };
    let keep = match str_arg(s, "keep").unwrap_or("below") {
        "below" | "min" | "-" => false,
        "above" | "max" | "+" => true,
        k => return Err(format!("`section.keep` must be \"below\" or \"above\", not `{k}`")),
    };
    Ok(Some((axis, s.get("at").and_then(Value::as_f64), keep)))
}

fn screenshot(app: &mut EasyCadApp, _ctx: &egui::Context, args: Value) -> Result<Step, String> {
    if app.wgpu.is_none() {
        return Err("screenshots need easyCAD's GPU renderer, which is not available in this window".into());
    }
    let mut views = Vec::new();
    let single = args.get("views").is_none();
    if let Some(a) = args.get("views").and_then(Value::as_array) {
        for v in a {
            views.push(view_spec(v.as_str().ok_or("`views` must be a list of view names")?)?);
        }
        if views.is_empty() || views.len() > 6 {
            return Err("`views` takes 1 to 6 views".into());
        }
    }
    // validate the persistent changes before anything moves
    if let Some(s) = args.get("section") {
        parse_section(s)?;
    }
    if let Some(d) = str_arg(&args, "display") {
        display_mode(d)?;
    }
    let sec = args.get("section").map(parse_section).transpose()?.flatten();
    if single {
        let v = match str_arg(&args, "view") {
            Some(v) => v.to_string(),
            // a new cut: look straight at it
            None => match sec {
                Some((a, _, above)) => [["right", "left"], ["back", "front"], ["top", "bottom"]][a][above as usize].to_string(),
                None => "current".into(),
            },
        };
        views.push(view_spec(&v)?);
    }
    let mut hl = HighlightReq::default();
    if let Some(h) = args.get("highlight") {
        let list = |k: &str| h.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
        for f in list("faces") {
            hl.faces.push(serde_json::from_value(f.clone()).map_err(|e| format!("highlight face selector {f}: {e}"))?);
        }
        for e in list("edges") {
            hl.edges.push(serde_json::from_value(e.clone()).map_err(|er| format!("highlight edge selector {e}: {er}"))?);
        }
        for f in list("features") {
            hl.features.push(FeatureId(f.as_u64().ok_or("`highlight.features` takes feature ids")? as u32));
        }
        hl.bodies = list("bodies");
        hl.on = h.get("body").cloned().unwrap_or(Value::Null);
    }
    Ok(Step::Shot(Box::new(Shot {
        views,
        single,
        fit: bool_arg(&args, "fit"),
        max_w: args.get("max_width").and_then(Value::as_u64).unwrap_or(1024).clamp(128, 4096) as u32,
        hl,
        ghost: bool_arg(&args, "ghost"),
        xray: bool_arg(&args, "xray").unwrap_or(false),
        args,
        phase: Phase::Model,
    })))
}

fn display_mode(s: &str) -> Result<crate::viewport::gpu::DisplayMode, String> {
    use crate::viewport::gpu::DisplayMode as D;
    match s {
        "shaded" => Ok(D::Shaded),
        "edges" | "shaded+edges" | "shaded_edges" => Ok(D::ShadedEdges),
        "wireframe" => Ok(D::Wireframe),
        d => Err(format!("`display` must be \"shaded\", \"edges\" or \"wireframe\", not `{d}`")),
    }
}

/// The agent model (Claude's regeneration of the whole document).
fn agent_model(app: &EasyCadApp) -> Option<Arc<crate::worker::Output>> {
    app.mcp.as_ref()?.model.clone()
}

/// Apply the persistent view changes of a screenshot call (hide, show, isolate,
/// section, display, reset) to Claude's view.
fn apply_view_args(app: &mut EasyCadApp, args: &Value) -> Result<(), String> {
    let m = agent_model(app).ok_or("no model yet")?;
    let names: Vec<(BodyId, String)> = m.result.bodies.iter().map(|b| (b.id, b.name.clone())).collect();
    let find = |want: &Value| -> Result<Vec<BodyId>, String> {
        let v: Vec<BodyId> = names.iter().filter(|(id, n)| body_ref_matches(*id, n, want)).map(|b| b.0).collect();
        if v.is_empty() {
            Err(format!("no body {want} (bodies: {})", names.iter().map(|b| b.1.as_str()).collect::<Vec<_>>().join(", ")))
        } else {
            Ok(v)
        }
    };
    let list = |k: &str| args.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let mut v = app.mcp.as_ref().map(|b| b.view.clone()).unwrap_or_default();
    if bool_arg(args, "reset_view").unwrap_or(false) {
        v = AgentView::default();
    }
    if args.get("show") == Some(&json!("all")) {
        v.hidden.clear();
    }
    for s in list("show") {
        for b in find(&s)? {
            v.hidden.remove(&b);
        }
    }
    for h in list("hide") {
        v.hidden.extend(find(&h)?);
    }
    let iso = list("isolate");
    if !iso.is_empty() {
        let mut keep = Vec::new();
        for i in &iso {
            keep.extend(find(i)?);
        }
        v.hidden = names.iter().map(|b| b.0).filter(|b| !keep.contains(b)).collect();
    }
    if let Some(s) = args.get("section") {
        v.section = match parse_section(s)? {
            None => None,
            Some((axis, at, keep_above)) => {
                let bb = m.bbox.ok_or("the model is empty")?;
                let (lo, hi) = (bb.min[axis], bb.max[axis]);
                let pos = at.unwrap_or((lo + hi) / 2.0);
                if pos <= lo || pos >= hi {
                    return Err(format!("section at {pos} is outside the model ({lo:.2} .. {hi:.2} along {})", ["x", "y", "z"][axis]));
                }
                Some(AgentSection { axis, pos, keep_above })
            }
        };
    }
    if let Some(d) = str_arg(args, "display") {
        v.display = display_mode(d)?;
    }
    if let Some(b) = app.mcp.as_mut() {
        b.view = v;
    }
    Ok(())
}

/// Claude's view state as JSON (in every screenshot reply and from `view_state`).
fn view_json(app: &EasyCadApp) -> Value {
    let Some(b) = app.mcp.as_ref() else { return Value::Null };
    let v = &b.view;
    let names: HashMap<BodyId, String> = b.model.iter().flat_map(|m| m.result.bodies.iter()).map(|x| (x.id, x.name.clone())).collect();
    let mut hidden: Vec<String> = v.hidden.iter().map(|id| names.get(id).cloned().unwrap_or_else(|| format!("{id:?}"))).collect();
    hidden.sort();
    json!({
        "camera": if v.placed { v.view_name.clone() } else { "not placed yet (first picture: iso, fitted)".into() },
        "hidden_bodies": hidden,
        "section": v.section.map(|s| {
            let axis = ["x", "y", "z"][s.axis];
            let keep = if s.keep_above { "above" } else { "below" };
            json!({"axis": axis, "at": r(s.pos), "keep": keep})
        }),
        "display": match v.display {
            crate::viewport::gpu::DisplayMode::Shaded => "shaded",
            crate::viewport::gpu::DisplayMode::ShadedEdges => "edges",
            crate::viewport::gpu::DisplayMode::Wireframe => "wireframe",
        },
        "position": position_name(&app.doc.data, v.position),
    })
}

fn view_state(args: Value) -> Step {
    after_model(move |app, _| {
        if bool_arg(&args, "reset").unwrap_or(false) {
            if let Some(b) = app.mcp.as_mut() {
                b.view = AgentView::default();
            }
        }
        done(json!({"claude_view": view_json(app), "note": "Claude's own view: the user's camera, visibility and section never change it, and it never changes theirs"}))
    })
}

impl Shot {
    fn advance(mut self: Box<Self>, app: &mut EasyCadApp) -> Result<ToolResult, Box<Self>> {
        loop {
            match &mut self.phase {
                Phase::Model => {
                    if !model_ready(app) {
                        return Err(self);
                    }
                    if let Err(e) = apply_view_args(app, &self.args) {
                        return Ok(Err(e));
                    }
                    let m = agent_model(app).expect("ready");
                    let v = app.mcp.as_ref().map(|b| b.view.clone()).unwrap_or_default();
                    let visible: Vec<MeshedBody> = m.bodies.iter().filter(|b| !v.hidden.contains(&b.body.id)).cloned().collect();
                    match v.section {
                        None => return Ok(self.render(app, Scene { bodies: visible, caps: Vec::new() })),
                        Some(s) => {
                            let (o, n) = s.plane();
                            let q = app.settings.mesh.quality();
                            let (tx, rx) = channel();
                            std::thread::spawn(move || {
                                let mut sc = Scene { bodies: Vec::new(), caps: Vec::new() };
                                for b in visible {
                                    match crate::worker::cut_body(&b.body, o, n, q) {
                                        Some((mb, cap)) => {
                                            sc.bodies.push(mb);
                                            if let Some(c) = cap {
                                                sc.caps.push((c, glam::DAffine3::from_translation(n * 0.05)));
                                            }
                                        }
                                        None => sc.bodies.push(b),
                                    }
                                }
                                let _ = tx.send(sc);
                            });
                            self.phase = Phase::Cut(rx);
                        }
                    }
                }
                Phase::Cut(rx) => match rx.try_recv() {
                    Ok(sc) => return Ok(self.render(app, sc)),
                    Err(TryRecvError::Empty) => return Err(self),
                    Err(TryRecvError::Disconnected) => return Ok(Err("the section cut failed".into())),
                },
            }
        }
    }

    /// Resolve the highlights on the uncut bodies and map them onto the scene.
    fn style(&self, m: &crate::worker::Output, scene: &Scene, cut: bool) -> Result<(ShotStyle, Vec<String>), String> {
        let mut st = ShotStyle { xray: self.xray, ..Default::default() };
        let mut notes = Vec::new();
        let names: Vec<(BodyId, String)> = m.result.bodies.iter().map(|b| (b.id, b.name.clone())).collect();
        for want in &self.hl.bodies {
            let v: Vec<BodyId> = names.iter().filter(|(id, n)| body_ref_matches(*id, n, want)).map(|b| b.0).collect();
            if v.is_empty() {
                return Err(format!("no body {want} to highlight"));
            }
            st.bodies.extend(v);
        }
        let shown = |id: BodyId| scene.bodies.iter().find(|b| b.body.id == id);
        let mut nf = 0;
        let mut ne = 0;
        if !self.hl.faces.is_empty() || !self.hl.edges.is_empty() || !self.hl.features.is_empty() {
            let targets: Vec<&easycad_model::regen::Body> = m.result.bodies.iter().filter(|b| self.hl.on.is_null() || body_ref_matches(b.id, &b.name, &self.hl.on)).collect();
            if targets.is_empty() {
                return Err(format!("no body {} to highlight on", self.hl.on));
            }
            for b in targets {
                let shape = b.shape();
                let (Ok(fi), Ok(ei)) = (shape.faces(), shape.edges()) else { continue };
                let scale = shape.bbox().map(|x| x.size().length()).unwrap_or(1.0).max(1.0);
                let mut faces: Vec<&FaceInfo> = Vec::new();
                for s in &self.hl.faces {
                    faces.extend(s.resolve_infos(&fi, scale).unwrap_or_default().into_iter().map(|i| &fi[i as usize]));
                }
                for (i, f) in fi.iter().enumerate() {
                    if m.result.face_origin(b.id, i as u32).and_then(|o| o.feature).is_some_and(|id| self.hl.features.contains(&id)) {
                        faces.push(f);
                    }
                }
                let mut edges: Vec<&EdgeInfo> = Vec::new();
                for s in &self.hl.edges {
                    for i in s.resolve(shape).unwrap_or_default() {
                        edges.extend(ei.iter().find(|e| e.index == i));
                    }
                }
                nf += faces.len();
                ne += edges.len();
                let Some(mb) = shown(b.id) else {
                    if !faces.is_empty() || !edges.is_empty() {
                        notes.push(format!("{} is hidden in your view, so its highlights are not drawn", b.name));
                    }
                    continue;
                };
                for f in faces {
                    if cut {
                        st.faces.extend(FaceSelector::from_pick(f).resolve_infos(&mb.faces, scale).unwrap_or_default().into_iter().map(|i| (b.id, i)));
                    } else {
                        st.faces.push((b.id, f.index));
                    }
                }
                for e in edges {
                    if cut {
                        st.edges.extend(EdgeSelector::from_pick(e).resolve(mb.body.shape()).unwrap_or_default().into_iter().map(|i| (b.id, i)));
                    } else {
                        st.edges.push((b.id, e.index));
                    }
                }
            }
            if nf == 0 && ne == 0 {
                return Err("the highlight matched no face or edge on any body: check the selectors with measure (faces/edges)".into());
            }
        }
        st.faces.sort_unstable();
        st.faces.dedup();
        st.edges.sort_unstable();
        st.edges.dedup();
        if !self.hl.is_empty() {
            notes.insert(0, format!("highlighted (orange): {nf} faces, {ne} edges, {} bodies", st.bodies.len()));
        }
        st.ghost = self.ghost.unwrap_or(!self.hl.is_empty());
        Ok((st, notes))
    }

    fn render(&mut self, app: &mut EasyCadApp, scene: Scene) -> ToolResult {
        let m = agent_model(app).ok_or("no model yet")?;
        let view = app.mcp.as_ref().map(|b| b.view.clone()).unwrap_or_default();
        let (style, notes) = self.style(&m, &scene, view.section.is_some())?;
        let n = self.views.len();
        let cols = match n {
            1 => 1,
            2 | 4 => 2,
            _ => 3,
        } as u32;
        let gap = if n > 1 { 4 } else { 0 };
        let cell_w = ((self.max_w - gap * (cols - 1)) / cols).max(64);
        let cell_h = ((cell_w as f32) * if n > 1 { 0.75 } else { 0.66 }).round() as u32;
        // what to fit: the bodies drawn
        let mut bb: Option<(DVec3, DVec3)> = None;
        for b in &scene.bodies {
            if let Ok(x) = b.body.shape().bbox() {
                bb = Some(match bb {
                    Some((lo, hi)) => (lo.min(x.min), hi.max(x.max)),
                    None => (x.min, x.max),
                });
            }
        }
        let mut images = Vec::new();
        let mut labels = Vec::new();
        for spec in self.views.clone() {
            let mut cam = view.camera.clone();
            cam.cancel_anim();
            let aspect = cell_w as f32 / cell_h as f32;
            let (label, fit) = match &spec {
                ViewSpec::Named(reg, label) => {
                    // from iso, so the result does not depend on the last view (the
                    // view cube rolls a face that is already faced)
                    cam.rot = crate::camera::iso_rot();
                    cam.rot = cam.region_rot(*reg);
                    (label.clone(), self.fit.unwrap_or(true))
                }
                ViewSpec::Current if !view.placed => {
                    cam.rot = crate::camera::iso_rot();
                    ("Iso".to_string(), true)
                }
                ViewSpec::Current => (view.view_name.clone(), self.fit.unwrap_or(false)),
                ViewSpec::User => {
                    cam = app.camera.clone();
                    cam.cancel_anim();
                    ("The user's view".to_string(), self.fit.unwrap_or(false))
                }
            };
            if fit {
                if let Some((lo, hi)) = bb {
                    cam.fit_sphere(((lo + hi) * 0.5).as_vec3(), ((hi - lo).length() * 0.5) as f32, aspect);
                }
            }
            let img = self.render_one(app, &cam, [cell_w, cell_h], &scene, &style, view.display, bb)?;
            let mut img = img;
            let caption = match view.section {
                Some(s) => format!("{label} - section {}={:.1}", ["x", "y", "z"][s.axis], s.pos),
                None => label.clone(),
            };
            draw_caption(&mut img, &caption);
            images.push(img);
            labels.push(label.clone());
            if self.single {
                if let Some(b) = app.mcp.as_mut() {
                    b.view.camera = cam;
                    b.view.placed = true;
                    b.view.view_name = label;
                }
            }
        }
        let rows = (n as u32).div_ceil(cols);
        let mut sheet = image::RgbaImage::from_pixel(cols * cell_w + gap * (cols - 1), rows * cell_h + gap * (rows - 1), image::Rgba([12, 13, 15, 255]));
        for (k, c) in images.iter().enumerate() {
            let (x, y) = ((k as u32 % cols) * (cell_w + gap), (k as u32 / cols) * (cell_h + gap));
            image::imageops::overlay(&mut sheet, c, x as i64, y as i64);
        }
        let (w, h) = sheet.dimensions();
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(sheet)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| format!("PNG encoding failed: {e}"))?;
        let mut text = if n > 1 {
            format!("{n} views, {cols} per row, left to right then down: {} ({w}×{h} px)", labels.join(", "))
        } else {
            format!("{} ({w}×{h} px)", labels[0])
        };
        text += &format!(", document revision {}", app.doc.data.revision);
        if let Some(s) = view.section {
            text += &format!("\nsection: plane {}={:.2}, keeping the {} side (cut faces tinted orange)", ["x", "y", "z"][s.axis], s.pos, if s.keep_above { "upper" } else { "lower" });
        }
        for note in notes {
            text += &format!("\n{note}");
        }
        text += &format!("\nyour view (kept for the next picture; the user's view is separate): {}", view_json(app));
        Ok(vec![proto::image(&png), proto::text(text)])
    }

    #[allow(clippy::too_many_arguments)]
    fn render_one(
        &self,
        app: &mut EasyCadApp,
        cam: &crate::camera::Camera,
        size: [u32; 2],
        scene: &Scene,
        style: &ShotStyle,
        mode: crate::viewport::gpu::DisplayMode,
        bb: Option<(DVec3, DVec3)>,
    ) -> Result<image::RgbaImage, String> {
        use crate::viewport::gpu::{srgba, DrawBody, FaceHighlight, Frame3D, Grid};
        let sz = glam::Vec2::new(size[0] as f32, size[1] as f32);
        let mut bodies = Vec::new();
        for b in &scene.bodies {
            let Some(mesh) = &b.mesh else { continue };
            let mut color = crate::ui_viewport::body_color(b.body.reference, b.body.part.is_some());
            if b.colors.is_some() {
                color = [1.0, 1.0, 1.0, if b.body.reference { 0.6 } else { 1.0 }];
            }
            let mut translucent = b.body.reference;
            if let Some(a) = style.alpha(b.body.id) {
                color[3] = color[3].min(a);
                translucent = true;
            }
            bodies.push(DrawBody { mesh: mesh.clone(), color, translucent, colors: b.colors.clone(), model: glam::Mat4::IDENTITY });
        }
        for (cap, xf) in &scene.caps {
            bodies.push(DrawBody { mesh: cap.clone(), color: srgba(crate::theme::ORANGE_SOFT), translucent: false, colors: None, model: crate::ui_viewport::to_mat4(*xf) });
        }
        let mesh_of = |id: BodyId| scene.bodies.iter().find(|b| b.body.id == id).and_then(|b| b.mesh.clone());
        let mut highlights = Vec::new();
        for &(b, f) in &style.faces {
            if let Some(mesh) = mesh_of(b) {
                highlights.push(FaceHighlight { mesh, face: f, color: SHOT_FACE, xray: style.through(), model: glam::Mat4::IDENTITY });
            }
        }
        for &b in &style.bodies {
            if let Some(mesh) = mesh_of(b) {
                highlights.push(FaceHighlight { mesh, face: u32::MAX, color: SHOT_BODY, xray: style.through(), model: glam::Mat4::IDENTITY });
            }
        }
        let grid = bb.map(|(lo, hi)| {
            let s = hi - lo;
            Grid { z: lo.z as f32, spacing: 10.0, extent: (s.x.max(s.y) as f32 * 1.5).max(50.0), center: [((lo.x + hi.x) / 2.0) as f32, ((lo.y + hi.y) / 2.0) as f32] }
        });
        let light = (cam.up() * 0.8 + cam.right() * -0.35 - cam.forward() * 0.6).normalize();
        let view_proj = cam.view_proj(sz);
        let frame = Frame3D {
            size_px: size,
            view_proj,
            eye: cam.eye(),
            forward: cam.forward(),
            ortho: cam.ortho,
            light,
            background: srgba(crate::theme::VIEWPORT),
            mode,
            edge_color: srgba(crate::theme::EDGE_DARK),
            bodies,
            highlights,
            grid,
        };
        let rs = app.wgpu.clone().ok_or("no GPU")?;
        let b = app.mcp.as_mut().ok_or("bridge off")?;
        let gpu = b.gpu.get_or_insert_with(|| crate::viewport::gpu::ViewportGpu::new(&rs.device, &rs.adapter, rs.target_format));
        let px = gpu.render_to_image(&rs.device, &rs.queue, &frame)?;
        let mut img = image::RgbaImage::from_raw(size[0], size[1], px).ok_or("bad image size")?;
        for p in img.pixels_mut() {
            p.0[3] = 255;
        }
        // highlighted edges: thick lines drawn over everything
        for &(bid, e) in &style.edges {
            let Some(mesh) = mesh_of(bid) else { continue };
            for pl in mesh.edges.iter().filter(|p| p.edge == e) {
                let pts: Vec<Option<(f32, f32)>> = pl
                    .points
                    .iter()
                    .map(|p| {
                        let c = view_proj * glam::Vec4::new(p[0], p[1], p[2], 1.0);
                        (c.w > 1e-6).then(|| ((c.x / c.w * 0.5 + 0.5) * sz.x, (0.5 - c.y / c.w * 0.5) * sz.y))
                    })
                    .collect();
                for w in pts.windows(2) {
                    if let (Some(a), Some(b)) = (w[0], w[1]) {
                        thick_line(&mut img, a, b, 2.2, SHOT_EDGE);
                    }
                }
            }
        }
        Ok(img)
    }
}

pub(crate) fn thick_line(img: &mut image::RgbaImage, a: (f32, f32), b: (f32, f32), r: f32, c: [u8; 3]) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let len = ((b.0 - a.0).hypot(b.1 - a.1)).max(1.0);
    let steps = (len * 2.0).min(20_000.0) as i32;
    for k in 0..=steps {
        let t = k as f32 / steps.max(1) as f32;
        let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let ri = r.ceil() as i32;
        for dy in -ri..=ri {
            for dx in -ri..=ri {
                let (px, py) = (x as i32 + dx, y as i32 + dy);
                if px >= 0 && py >= 0 && px < w && py < h && ((dx * dx + dy * dy) as f32) <= r * r {
                    img.put_pixel(px as u32, py as u32, image::Rgba([c[0], c[1], c[2], 255]));
                }
            }
        }
    }
}

/// 5×7 glyphs (rows top to bottom, 5 bits each, MSB left) for captions and
/// labels; letters print upper case.
pub(crate) fn glyph(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '=' => [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '\'' => [0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        '*' => [0x00, 0x15, 0x0E, 0x1F, 0x0E, 0x15, 0x00],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x0C, 0x04, 0x08],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        '>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        '?' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '^' => [0x04, 0x0A, 0x11, 0x00, 0x00, 0x00, 0x00],
        '°' => [0x0C, 0x12, 0x12, 0x0C, 0x00, 0x00, 0x00],
        _ => [0; 7],
    }
}

/// Text in the 5×7 font at `scale`, its top-left at (x, y), on an optional box
/// (3 px padding). Returns the box's size.
pub(crate) fn draw_text(img: &mut image::RgbaImage, x: i32, y: i32, scale: u32, text: &str, fg: [u8; 3], bg: Option<[u8; 3]>) -> (u32, u32) {
    let s = scale.max(1);
    let (cw, ch) = (6 * s, 7 * s);
    let n = text.chars().count() as u32;
    let (bw, bh) = (n * cw - s + 6, ch + 6);
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let mut put = |px: i32, py: i32, c: [u8; 3]| {
        if px >= 0 && py >= 0 && px < iw && py < ih {
            img.put_pixel(px as u32, py as u32, image::Rgba([c[0], c[1], c[2], 255]));
        }
    };
    if let Some(bg) = bg {
        for py in y..y + bh as i32 {
            for px in x..x + bw as i32 {
                put(px, py, bg);
            }
        }
    }
    for (i, c) in text.chars().enumerate() {
        let g = glyph(c);
        let (x0, y0) = (x + 3 + (i as u32 * cw) as i32, y + 3);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..5u32 {
                if bits & (0x10 >> col) != 0 {
                    for dy in 0..s {
                        for dx in 0..s {
                            put(x0 + (col * s + dx) as i32, y0 + (row as u32 * s + dy) as i32, fg);
                        }
                    }
                }
            }
        }
    }
    (bw, bh)
}

/// A caption in the top-left corner, on a dark box.
pub(crate) fn draw_caption(img: &mut image::RgbaImage, text: &str) {
    draw_text(img, 8, 8, 2, text, [230, 232, 235], Some([34, 38, 44]));
}

// ------------------------------------------------------------------ documents

fn open_or_new(app: &mut EasyCadApp, tool: &str, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    if let Some((_, name)) = open_sketch(app) {
        return Err(format!(
            "The user is editing sketch '{name}' in easyCAD. Ask them to finish or cancel it before switching documents."
        ));
    }
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
    if !keeps_open_sketch(app) {
        app.redo();
        return Err(format!(
            "undoing \"{label}\" would remove the sketch the user has open, so it was not undone. Ask them to finish or cancel it first."
        ));
    }
    refresh_open_sketch(app);
    mark_seen(app);
    Ok(after_model(move |app, _| {
        let mut o = outcome_json(app, None);
        o["undone"] = json!(label);
        o["next_undo"] = json!(app.doc.undo_label());
        done(o)
    }))
}

// ------------------------------------------------------------------ positions

fn position_name(d: &DocData, p: Option<PositionId>) -> Value {
    match p.and_then(|p| d.position(p)) {
        Some(p) => json!(p.name),
        None => json!("as modeled"),
    }
}

/// A position by name or id; null / "modeled" / "as modeled" = none.
fn resolve_position(d: &DocData, v: &Value) -> Result<Option<PositionId>, String> {
    let known = || d.positions.iter().map(|p| format!("{} '{}'", p.id.0, p.name)).collect::<Vec<_>>().join(", ");
    match v {
        Value::Null => Ok(None),
        Value::String(s) if matches!(s.trim().to_ascii_lowercase().as_str(), "" | "modeled" | "as modeled" | "none") => Ok(None),
        Value::String(s) => d.position_named(s.trim()).map(|p| Some(p.id)).ok_or_else(|| format!("no position named `{s}` (positions: {})", if d.positions.is_empty() { "none yet".into() } else { known() })),
        Value::Number(n) => {
            let id = PositionId(n.as_u64().unwrap_or(u64::MAX) as u32);
            d.position(id).map(|_| Some(id)).ok_or_else(|| format!("no position {n} (positions: {})", known()))
        }
        _ => Err("`position` is a name, an id or null".into()),
    }
}

fn target_json(d: &DocData, m: Option<&crate::worker::Output>, t: &PoseTarget) -> Value {
    match t {
        PoseTarget::Body(id) => json!({"body": m.and_then(|m| m.body(*id)).map(|b| b.name.clone()), "body_id": id}),
        PoseTarget::Occurrence(p) => {
            let names: Vec<String> = p.iter().filter_map(|o| d.occurrence(*o).map(|o| o.name.clone())).collect();
            json!({"occurrence": names.join(" / "), "occurrence_path": p})
        }
    }
}

fn positions_json(app: &EasyCadApp) -> Value {
    let d = &app.doc.data;
    let m = agent_model(app);
    let vals = d.values().ok();
    let list: Vec<Value> = d
        .positions
        .iter()
        .map(|p| {
            let moves: Vec<Value> = p
                .moves
                .iter()
                .map(|mv| {
                    let mut o = target_json(d, m.as_deref(), &mv.target);
                    o["translate"] = json!([mv.translate[0].0, mv.translate[1].0, mv.translate[2].0]);
                    o["angle_deg"] = json!(mv.angle_deg.0);
                    o["axis"] = v3(mv.axis);
                    o["pivot"] = v3(mv.pivot);
                    o
                })
                .collect();
            let mut o = json!({"id": p.id.0, "name": p.name, "moves": moves});
            if let Some(Err(e)) = vals.as_ref().map(|v| p.evaluate(v)) {
                o["error"] = json!(e);
            }
            o
        })
        .collect();
    json!({
        "positions": list,
        "claude_position": position_name(d, app.mcp.as_ref().and_then(|b| b.view.position)),
        "user_position": position_name(d, app.pose),
        "user_playing": app.positions.play.is_some(),
        "revision": d.revision,
    })
}

/// One move from JSON: `body` (name or {feature,index}) or `occurrence` (name or
/// path of ids), `translate` [x,y,z], `angle_deg`, `axis` [x,y,z], `pivot` [x,y,z].
fn parse_move(app: &EasyCadApp, v: &Value) -> Result<PoseMove, String> {
    let d = &app.doc.data;
    let m = agent_model(app).or_else(|| app.model.clone());
    let target = if let Some(b) = v.get("body") {
        let bodies: Vec<(BodyId, String)> = m.as_ref().map(|m| m.result.bodies.iter().map(|b| (b.id, b.name.clone())).collect()).unwrap_or_default();
        let id = bodies
            .iter()
            .find(|(id, n)| body_ref_matches(*id, n, b))
            .map(|x| x.0)
            .ok_or_else(|| format!("no body {b} (bodies: {})", bodies.iter().map(|b| b.1.as_str()).collect::<Vec<_>>().join(", ")))?;
        PoseTarget::Body(id)
    } else if let Some(o) = v.get("occurrence") {
        match o {
            Value::String(name) => {
                let occ = d.occurrences.iter().find(|x| &x.name == name).ok_or_else(|| format!("no top-level occurrence named `{name}`"))?;
                PoseTarget::Occurrence(vec![occ.id])
            }
            p => PoseTarget::Occurrence(serde_json::from_value(p.clone()).map_err(|e| format!("bad occurrence path: {e}"))?),
        }
    } else if let Some(t) = v.get("target") {
        serde_json::from_value(t.clone()).map_err(|e| format!("bad target: {e}"))?
    } else {
        return Err("a move needs `body` (name or {\"feature\":n,\"index\":0}) or `occurrence`".into());
    };
    let mut mv = PoseMove::new(target);
    if let Some(t) = v.get("translate") {
        mv.translate = expr3_arg(&json!({"t": t}), "t")?;
    }
    if let Some(a) = v.get("angle_deg") {
        mv.angle_deg = expr_arg(a).ok_or("`angle_deg` is a number or an expression")?;
    }
    if let Some(a) = v.get("axis") {
        mv.axis = vec3_val(a).ok_or("`axis` is [x, y, z]")?;
        if mv.axis.length() < 1e-9 {
            return Err("`axis` must not be zero".into());
        }
    }
    if let Some(p) = v.get("pivot") {
        mv.pivot = vec3_val(p).ok_or("`pivot` is [x, y, z] (a point on the rotation axis)")?;
    }
    Ok(mv)
}

fn parse_moves(app: &EasyCadApp, v: Option<&Value>) -> Result<Vec<PoseMove>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a.iter().map(|m| parse_move(app, m)).collect(),
        Some(_) => Err("`moves` is a list of moves".into()),
    }
}

/// Check every position still evaluates (unknown parameters etc.).
fn check_position(d: &DocData, id: PositionId) -> Result<(), String> {
    let vals = d.values().map_err(|e| e.to_string())?;
    d.position(id).ok_or("position vanished")?.evaluate(&vals).map(|_| ())
}

fn add_position(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    check_revision(app, &args)?;
    let name = str_arg(&args, "name").ok_or("`name` is required")?.trim().to_string();
    if name.is_empty() {
        return Err("`name` must not be empty".into());
    }
    if app.doc.data.position_named(&name).is_some() {
        return Err(format!("there is already a position named `{name}`: use edit_position"));
    }
    let mut moves = match args.get("copy_from") {
        Some(v) => resolve_position(&app.doc.data, v)?.and_then(|p| app.doc.data.position(p)).map(|p| p.moves.clone()).unwrap_or_default(),
        None => Vec::new(),
    };
    for mv in parse_moves(app, args.get("moves"))? {
        moves.retain(|m| m.target != mv.target);
        moves.push(mv);
    }
    let (id, before) = claude_edit(app, &format!("add position '{name}'"), |d| {
        let id = d.add_position(&name);
        d.positions.last_mut().unwrap().moves = moves;
        check_position(d, id)?;
        Ok(id)
    })?;
    if bool_arg(&args, "apply").unwrap_or(true) {
        if let Some(b) = app.mcp.as_mut() {
            b.view.position = Some(id);
        }
    }
    Ok(after_model(move |app, _| {
        let mut o = positions_json(app);
        o["added"] = json!(id.0);
        if let Some(b) = before {
            o["user_changes_before"] = b;
        }
        done(o)
    }))
}

fn edit_position(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    can_edit(app)?;
    check_revision(app, &args)?;
    let id = resolve_position(&app.doc.data, args.get("position").ok_or("`position` is required (name or id)")?)?.ok_or("`position` names no position")?;
    let old = app.doc.data.position(id).cloned().ok_or("no such position")?;
    if bool_arg(&args, "delete").unwrap_or(false) {
        claude_edit(app, &format!("delete position '{}'", old.name), |d| {
            d.positions.retain(|p| p.id != id);
            Ok(())
        })?;
        if app.pose == Some(id) {
            app.go_to_position(None);
        }
        return Ok(after_model(|app, _| done(positions_json(app))));
    }
    let rename = str_arg(&args, "name").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(n) = &rename {
        if app.doc.data.position_named(n).is_some_and(|p| p.id != id) {
            return Err(format!("there is already a position named `{n}`"));
        }
    }
    let replace = args.get("moves").map(|_| parse_moves(app, args.get("moves"))).transpose()?;
    let set = parse_moves(app, args.get("set"))?;
    let mut remove = Vec::new();
    for v in args.get("remove").and_then(Value::as_array).into_iter().flatten() {
        remove.push(parse_move(app, &json!({"body": v})).or_else(|_| parse_move(app, &json!({"occurrence": v})))?.target);
    }
    let (_, before) = claude_edit(app, &format!("edit position '{}'", old.name), |d| {
        let p = d.positions.iter_mut().find(|p| p.id == id).ok_or("position vanished")?;
        if let Some(n) = rename {
            p.name = n;
        }
        if let Some(r) = replace {
            p.moves = r;
        }
        for mv in set {
            match p.moves.iter_mut().find(|m| m.target == mv.target) {
                Some(m) => *m = mv,
                None => p.moves.push(mv),
            }
        }
        p.moves.retain(|m| !remove.contains(&m.target));
        check_position(d, id)
    })?;
    Ok(after_model(move |app, _| {
        let mut o = positions_json(app);
        if let Some(b) = before {
            o["user_changes_before"] = b;
        }
        done(o)
    }))
}

/// Switch Claude's view (default), the user's window, or both to a position.
fn apply_position(app: &mut EasyCadApp, args: Value) -> Result<Step, String> {
    let p = resolve_position(&app.doc.data, args.get("position").unwrap_or(&Value::Null))?;
    let who = str_arg(&args, "for").unwrap_or("claude").to_ascii_lowercase();
    match who.as_str() {
        "claude" | "both" | "user" => {}
        o => return Err(format!("`for` is claude, user or both, not {o}")),
    }
    if who != "user" {
        if let Some(b) = app.mcp.as_mut() {
            b.view.position = p;
        }
    }
    if who != "claude" {
        app.positions.play = None;
        app.go_to_position(p);
    }
    Ok(after_model(move |app, _| {
        let m = agent_model(app);
        if let Some(e) = m.as_ref().and_then(|m| m.pose_error.clone()) {
            return fail(format!("the position could not be applied: {e}"));
        }
        let mut o = positions_json(app);
        o["bodies"] = bodies_json(app);
        done(o)
    }))
}
