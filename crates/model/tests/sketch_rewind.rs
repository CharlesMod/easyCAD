//! Rewinding the history head to a gap: before everything, or right after a
//! sketch (its consumer and later features rolled back, the sketch stays live).

use easycad_model::document::Document;
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::sketch::Sketch;
use glam::DVec2;

fn sk(doc: &mut Document, name: &str) -> SketchId {
    let mut s = Sketch::new(name, Plane::XY);
    s.rect(DVec2::ZERO, DVec2::new(10.0, 5.0));
    doc.add_sketch(s)
}
fn ext(doc: &mut Document, s: SketchId) -> FeatureId {
    doc.add_feature(
        "Block",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 5.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    )
}

#[test]
fn head_at_gap_zero_is_an_empty_model() {
    let mut doc = Document::new("t");
    let s = sk(&mut doc, "S1");
    let f = ext(&mut doc, s);
    doc.set_rollback(Some(0));
    assert_eq!(doc.data.rollback, Some(0));
    assert_eq!(doc.data.head_pos(), 0);
    let out = doc.regenerate().clone();
    assert!(out.report(f).unwrap().rolled_back);
    assert!(out.body_by_id(BodyId::of(f)).is_none(), "nothing is built");
    doc.undo();
    assert_eq!(doc.data.rollback, None);
}

#[test]
fn head_after_a_sketch_rolls_back_its_consumer_but_keeps_the_sketch() {
    let mut doc = Document::new("t");
    let s1 = sk(&mut doc, "S1");
    let f1 = ext(&mut doc, s1);
    let s2 = sk(&mut doc, "S2");
    let f2 = ext(&mut doc, s2);
    assert!(!doc.data.sketch_visible(s2), "consumed: hidden by default");
    assert_eq!(doc.data.first_consumer(s2), Some(1));
    doc.roll_back_to_sketch(s2);
    assert_eq!(doc.data.rollback, Some(1));
    assert_eq!(doc.undo_label(), Some("Roll back to 1"));
    let out = doc.regenerate().clone();
    assert!(!out.report(f1).unwrap().rolled_back);
    assert!(out.report(f2).unwrap().rolled_back, "its extrude is rolled back");
    assert!(doc.data.sketch(s2).is_some(), "the sketch is still there");
    assert!(doc.data.sketch_visible(s2), "and drawn: nothing live consumes it now");
    assert!(!doc.data.sketch_visible(s1), "s1's extrude is live: still hidden");
    // later features go in after the head, and the sketch can be edited
    doc.data.sketch_mut(s2).unwrap().name = "Renamed".into();
    doc.set_rollback(None);
    assert!(!doc.data.sketch_visible(s2));
}

#[test]
fn an_unconsumed_sketch_sits_after_every_feature() {
    let mut doc = Document::new("t");
    let s1 = sk(&mut doc, "S1");
    ext(&mut doc, s1);
    let s2 = sk(&mut doc, "Loose");
    assert_eq!(doc.data.first_consumer(s2), None);
    doc.set_rollback(Some(0));
    doc.roll_back_to_sketch(s2);
    assert_eq!(doc.data.rollback, None, "after the loose sketch = rolled forward");
}
