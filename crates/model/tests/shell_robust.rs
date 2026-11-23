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
fn impossible_shell_reports_largest_thickness_and_parameter() {
    // a 20 mm wall in a body 34 mm across cannot be built; the error gives the
    // largest thickness that works and names the parameter to lower
    let (mut doc, _, shell) = cart(37.82, 10.87, 1.2);
    doc.set_param("wall", "20").unwrap();
    doc.edit("wall param", |d| {
        if let Some(f) = d.features.iter_mut().find(|f| f.id == shell) {
            if let FeatureKind::Shell { thickness, .. } = &mut f.kind {
                *thickness = "wall".into();
            }
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    let r = doc.regenerate();
    let rep = r.reports.iter().find(|x| x.feature == shell).unwrap();
    let FeatureStatus::Error(msg) = &rep.status else { panic!("built a 20 mm wall: {:?}", rep.note) };
    eprintln!("{msg}\n({:.0} ms)", rep.elapsed_ms);
    assert!(!msg.contains("fillet radius"), "{msg}");
    assert!(msg.contains("largest workable thickness: ") && msg.contains("set `wall` to at most "), "{msg}");
    // following the advice works: one parameter edit re-shells
    let max: f64 = msg.split("set `wall` to at most ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
    assert!(max > 5.0 && max < 20.0, "{max}");
    doc.set_param("wall", &*format!("{max}")).unwrap();
    let r = doc.regenerate();
    let rep = r.reports.iter().find(|x| x.feature == shell).unwrap();
    assert!(matches!(rep.status, FeatureStatus::Ok), "{:?}", rep.status);
}
