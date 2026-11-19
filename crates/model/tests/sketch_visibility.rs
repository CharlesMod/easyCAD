//! Sketch visibility in the 3D view: shown by default, hidden once a feature
//! consumes it, a manual choice sticks, old files (no flag) load.

use easycad_model::document::{DocData, Document};
use easycad_model::feature::{BoolOp, FeatureKind, ProfileSel};
use easycad_model::geom::Plane;
use easycad_model::sketch::Sketch;
use glam::DVec2;

fn doc_with_sketch() -> (Document, easycad_model::feature::SketchId) {
    let mut doc = Document::new("t");
    let mut s = Sketch::new("S", Plane::XY);
    s.rect(DVec2::ZERO, DVec2::new(10.0, 5.0));
    let id = doc.add_sketch(s);
    (doc, id)
}

fn extrude(doc: &mut Document, s: easycad_model::feature::SketchId) {
    doc.add_feature(
        "Block",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 5.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    );
}

#[test]
fn visible_until_consumed_and_the_toggle_sticks() {
    let (mut doc, s) = doc_with_sketch();
    assert!(doc.data.sketch_visible(s), "unused sketch is shown");
    extrude(&mut doc, s);
    assert!(doc.data.sketch_consumed(s));
    assert!(!doc.data.sketch_visible(s), "consumed sketch is hidden by default");
    doc.data.set_sketch_visible(s, Some(true));
    assert!(doc.data.sketch_visible(s), "manual show");
    // regenerating / a later edit does not flip it back
    extrude(&mut doc, s);
    assert!(doc.data.sketch_visible(s));
    doc.data.set_sketch_visible(s, Some(false));
    assert!(!doc.data.sketch_visible(s));
}

#[test]
fn an_unused_sketch_can_be_hidden() {
    let (mut doc, s) = doc_with_sketch();
    doc.data.set_sketch_visible(s, Some(false));
    assert!(!doc.data.sketch_visible(s));
}

#[test]
fn old_files_without_the_flag_load_and_round_trip() {
    let (doc, s) = doc_with_sketch();
    let json = doc.data.to_json().unwrap();
    assert!(!json.contains("\"visible\""), "default is not written");
    let back = DocData::from_json(&json).unwrap();
    assert!(back.sketch_visible(s));
    let mut d2 = back.clone();
    d2.set_sketch_visible(s, Some(false));
    let again = DocData::from_json(&d2.to_json().unwrap()).unwrap();
    assert!(!again.sketch_visible(s));
}
