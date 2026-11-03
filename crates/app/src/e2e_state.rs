//! State dump for the click-through E2E scripts (`crates/app/e2e/`). When
//! `CASEWORK_STATE_FILE` is set, a JSON summary of what the user would see (mode,
//! open dialog, selection, pick stack, parameters, undo labels, camera, toasts) is
//! written there whenever it changes, so scripts can assert with `jq` next to
//! their screenshots. Off (and free) otherwise.

use serde_json::{json, Value};

use crate::app::{CaseworkApp, Mode};
use crate::dialogs::Body;

pub struct StateDump {
    path: std::path::PathBuf,
    last: String,
}

impl StateDump {
    pub fn from_env() -> Option<StateDump> {
        let p = std::env::var_os("CASEWORK_STATE_FILE")?;
        Some(StateDump {
            path: p.into(),
            last: String::new(),
        })
    }

    pub fn write(&mut self, app: &CaseworkApp) {
        let s = serde_json::to_string_pretty(&summary(app)).unwrap_or_default();
        if s == self.last {
            return;
        }
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, &s).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
        self.last = s;
    }
}

fn dialog(app: &CaseworkApp) -> Value {
    let Some(d) = &app.dialog else {
        return Value::Null;
    };
    let mut v = json!({
        "kind": d.kind.title(),
        "editing": d.editing.map(|f| f.0),
        "error": d.error,
        "picked": d.picked().len(),
    });
    match &d.body {
        Body::Fillet(f) => {
            v["sets"] = f
                .sets
                .iter()
                .map(|s| {
                    json!({
                        "label": s.label(),
                        "radius": s.radius.text,
                        "edges": s.edges.len(),
                        "skipped": s.skipped.len(),
                        "results": s.results.iter().flatten().map(|r| format!("{r:?}")).collect::<Vec<_>>(),
                    })
                })
                .collect();
        }
        Body::Joint(j) => {
            v["joint"] = json!({
                "kind": j.kind.label(),
                "z": j.z.text,
                "report": j.report.is_some(),
                "status": j.status.as_ref().map(|s| format!("{s:?}")),
            });
        }
        Body::Extrude(e) => {
            v["extrude"] = json!({
                "sketch": e.sketch.map(|s| s.0),
                "distance": e.distance.text,
                "op": format!("{:?}", e.op),
                "target": e.target.and_then(|b| app.model.as_ref()?.body(b).map(|b| b.name.clone())),
            });
        }
        _ => {}
    }
    v
}

fn mode(app: &CaseworkApp) -> Value {
    match &app.mode {
        Mode::Model => json!({ "name": "model" }),
        Mode::Sketch(s) => json!({
            "name": "sketch",
            "plane": s.plane_name(),
            "bbox": sketch_bbox(&s.sketch),
            "selection": s.selection.len(),
            "view": [s.center.x, s.center.y, s.scale as f64],
            "sketch": s.sketch.name,
            "tool": format!("{:?}", s.tool),
            "entities": s.sketch.entities.iter().filter(|e| !e.construction).count(),
            "constraints": s.sketch.constraints.len(),
            "profiles": s.profile_count(),
            "status": s.report.as_ref().map(|r| format!("{:?}", r.status)),
            "dof": s.report.as_ref().map(|r| r.dof),
            "input": s.input,
            "error": s.error,
        }),
        Mode::Parts(_) => json!({ "name": "parts" }),
        Mode::Drawing(d) => json!({
            "name": "drawing",
            "annotations": d.draft.as_ref().map(|x| x.annotations.len()),
            "views": d.draft.as_ref().map(|x| x.views.len()),
            "tool": d.tool,
            "picks": d.picks.len(),
            "computed": d.result.is_some(),
        }),
    }
}

/// Bounding box [min x, min y, max x, max y] of the non-construction geometry.
fn sketch_bbox(s: &casework_model::sketch::Sketch) -> Option<[f64; 4]> {
    let mut b: Option<[f64; 4]> = None;
    for e in s.entities.iter().filter(|e| !e.construction) {
        for p in crate::sketch_mode::geom_points(&e.geom) {
            let q = s.points[p.0 as usize];
            b = Some(match b {
                None => [q.x, q.y, q.x, q.y],
                Some(b) => [b[0].min(q.x), b[1].min(q.y), b[2].max(q.x), b[3].max(q.y)],
            });
        }
    }
    b.map(|b| b.map(|v| (v * 100.0).round() / 100.0))
}

pub fn summary(app: &CaseworkApp) -> Value {
    let labels = |keys: &[crate::pickstack::PickKey]| -> Vec<String> {
        match &app.model {
            Some(m) => keys
                .iter()
                .map(|k| {
                    let body = m.body(k.body).map(|b| b.name.clone()).unwrap_or_default();
                    format!("{} @ {body}", crate::ui_viewport::pick_label(m, k).0)
                })
                .collect(),
            None => Vec::new(),
        }
    };
    let stack: Vec<_> = app.stack.items.iter().map(|c| c.key).collect();
    let m = app.model.as_ref();
    let cam = &app.camera;
    json!({
        "doc": app.doc.data.name,
        "path": app.path.as_ref().map(|p| p.display().to_string()),
        "revision": app.doc.data.revision,
        "dirty": app.is_dirty(),
        "undo": app.doc.undo_label(),
        "redo": app.doc.redo_label(),
        "profile": app.doc.data.active_profile,
        "teardrops": app.doc.data.profile().teardrop.enabled,
        "teardrop_style": format!("{:?}", app.doc.data.profile().teardrop.style),
        "profiles": app.doc.data.processes.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        "params": app.doc.data.params.list.iter().map(|p| (p.name.clone(), Value::String(p.expr.0.clone()))).collect::<serde_json::Map<_, _>>(),
        "features": app.doc.data.features.iter().map(|f| {
            let st = app.feature_status(f.id).map(|s| match s {
                casework_model::regen::FeatureStatus::Ok => "ok".to_string(),
                other => format!("{other:?}"),
            });
            json!({ "name": f.name, "kind": f.kind.label(), "status": st })
        }).collect::<Vec<_>>(),
        "bodies": m.map(|m| m.bodies.iter().map(|b| b.body.name.clone()).collect::<Vec<_>>()),
        "hidden": m.map(|m| m.bodies.iter().filter(|b| !app.body_visible(b.body.id)).map(|b| b.body.name.clone()).collect::<Vec<_>>()),
        "model_id": m.map(|m| m.id),
        "regenerating": app.regenerating(),
        "mode": mode(app),
        "dialog": dialog(app),
        "selection": labels(&app.selection),
        "stack": { "items": labels(&stack), "index": app.stack.index },
        "rollback": app.rollback,
        "camera": {
            "rot": cam.rot.to_array(),
            "forward": cam.forward().to_array(),
            "up": cam.up().to_array(),
            "target": cam.target.to_array(),
            "distance": cam.distance,
            "ortho": cam.ortho,
            "animating": cam.animating(),
        },
        "viewport": [app.viewport_rect.left(), app.viewport_rect.top(), app.viewport_rect.right(), app.viewport_rect.bottom()],
        "teardropped": m.map(|m| m.result.process.teardropped.len()),
        "toasts": app.toasts.iter().map(|t| t.text.clone()).collect::<Vec<_>>(),
        "overlay": app.overlay_name(),
        "typing": app.typing,
    })
}
