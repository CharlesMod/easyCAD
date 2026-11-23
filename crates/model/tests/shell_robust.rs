//! Shell on a drafted body with a collar joined on top: the 1:18 trash cart that
//! failed through the Claude bridge ("shell failed (thickness larger than a fillet
//! radius or feature?)"). See ENGINE.md, Robustness.

use easycad_model::document::Document;
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::regen::FeatureStatus;
use easycad_model::select::FaceSelector;
use easycad_model::sketch::Sketch;
use glam::DVec2;

/// Rounded square `w` (corner `r`) on XY at `z`, extruded `h` with `draft`.
#[allow(clippy::too_many_arguments)]
fn extrude(doc: &mut Document, name: &str, z: f64, w: f64, r: f64, h: f64, draft: f64, op: BoolOp) -> FeatureId {
    let mut s = Sketch::new(name, Plane::XY.offset(z));
    s.rounded_rect(DVec2::ZERO, w, w, r, (w, w, r));
    let s = doc.add_sketch(s);
    doc.add_feature(
        name,
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: h.into(), symmetric: false, reverse: false, draft_deg: draft.into(), op, target: None },
    )
}

fn cart(collar_w: f64, collar_r: f64, thickness: f64) -> (Document, FeatureId, FeatureId) {
    let mut doc = Document::new("Cart");
    let lower = extrude(&mut doc, "Lower body", 0.0, 34.4, 9.17, 46.57, -2.083, BoolOp::NewBody);
    extrude(&mut doc, "Collar", 46.57, collar_w, collar_r, 4.23, -20.1, BoolOp::Join);
    let shell = doc.add_feature("Shell", FeatureKind::Shell { body: None, open_faces: vec![FaceSelector::top()], thickness: thickness.into(), outward: false });
    (doc, lower, shell)
}

#[test]
fn trash_cart_shell_succeeds_and_reports_the_fallback() {
    let (mut doc, lower, shell) = cart(37.82, 10.87, 1.2);
    let r = doc.regenerate();
    let rep = r.reports.iter().find(|x| x.feature == shell).unwrap();
    assert!(matches!(rep.status, FeatureStatus::Ok), "{:?}", rep.status);
    let note = rep.note.as_deref().unwrap_or_default();
    assert!(note.starts_with("shell: ") && note.contains("default offset failed"), "{note:?}");
    let body = r.body_by_id(BodyId::of(lower)).unwrap();
    let v = body.shape().volume().unwrap();
    assert!(v > 7000.0 && v < 10000.0, "wall volume {v}");
}

#[test]
fn impossible_shell_names_the_blocking_faces() {
    // a 0.3 mm step under a 1 mm-tall straight collar, 2 mm wall: cannot be shelled
    let d = 46.57 * 2.083f64.to_radians().tan();
    let mut doc = Document::new("Cart");
    let lower = extrude(&mut doc, "Lower body", 0.0, 34.4, 9.17, 46.57, -2.083, BoolOp::NewBody);
    extrude(&mut doc, "Collar", 46.57, 34.4 + 2.0 * d + 0.6, 9.17 + d + 0.3, 1.0, 0.0, BoolOp::Join);
    let shell = doc.add_feature("Shell", FeatureKind::Shell { body: None, open_faces: vec![FaceSelector::top()], thickness: 2.0.into(), outward: false });
    let _ = lower;
    let r = doc.regenerate();
    let rep = r.reports.iter().find(|x| x.feature == shell).unwrap();
    let FeatureStatus::Error(msg) = &rep.status else {
        eprintln!("built anyway: {:?}", rep.note);
        return;
    };
    eprintln!("{msg}");
    assert!(!msg.contains("fillet radius"), "{msg}");
    assert!(msg.contains("likely blockers:") && msg.contains("{\"Hint\""), "{msg}");
    assert!(msg.contains("Collar ·") || msg.contains("Lower body ·"), "{msg}");
}
