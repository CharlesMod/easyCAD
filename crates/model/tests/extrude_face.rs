//! Extrude a body's planar face: out of the body joins, into it cuts.

use easycad_model::document::Document;
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::regen::FeatureStatus;
use easycad_model::select::FaceSelector;
use easycad_model::sketch::Sketch;
use glam::{DVec2, DVec3};
use std::f64::consts::PI;

const R: f64 = 4.0;
const H: f64 = 10.0;

fn cylinder() -> (Document, BodyId) {
    let mut doc = Document::new("Cyl");
    let mut s = Sketch::new("C", Plane::XY);
    s.circle(DVec2::ZERO, R);
    let s = doc.add_sketch(s);
    let f = doc.add_feature(
        "Cyl",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: H.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    );
    (doc, BodyId::of(f))
}

fn add(doc: &mut Document, body: BodyId, d: f64, op: BoolOp) -> FeatureId {
    doc.add_feature("Face extrude", FeatureKind::ExtrudeFace { face: FaceSelector::top(), distance: d.into(), op, target: Some(body) })
}

fn vol(doc: &mut Document, b: BodyId) -> f64 {
    doc.regenerate().body_by_id(b).unwrap().shape().volume().unwrap()
}

fn solids(doc: &mut Document) -> usize {
    doc.regenerate().bodies.len()
}

#[test]
fn top_face_out_joins() {
    let (mut doc, body) = cylinder();
    let v0 = vol(&mut doc, body);
    let f = add(&mut doc, body, 5.0, BoolOp::Join);
    let r = doc.regenerate();
    assert_eq!(r.report(f).unwrap().status, FeatureStatus::Ok, "{:?}", r.report(f));
    let v = vol(&mut doc, body);
    assert!((v - v0 - PI * R * R * 5.0).abs() < 1e-6, "{v} {v0}");
    assert_eq!(solids(&mut doc), 1);
    let sh = doc.regenerate().body_by_id(body).unwrap().shape().clone();
    assert_eq!(sh.count(occt::ShapeKind::Solid), 1);
    assert_eq!(sh.faces().unwrap().len(), 3, "unified into one cylinder + two caps");
}

#[test]
fn top_face_in_cuts_the_same_body() {
    let (mut doc, body) = cylinder();
    let v0 = vol(&mut doc, body);
    let f = add(&mut doc, body, -3.0, BoolOp::Cut);
    let r = doc.regenerate();
    assert_eq!(r.report(f).unwrap().status, FeatureStatus::Ok, "{:?}", r.report(f));
    let v = vol(&mut doc, body);
    assert!((v0 - v - PI * R * R * 3.0).abs() < 1e-6, "{v} {v0}");
    assert_eq!(solids(&mut doc), 1);
    let sh = doc.regenerate().body_by_id(body).unwrap().shape().clone();
    assert_eq!(sh.faces().unwrap().len(), 3);
}

#[test]
fn cut_targets_the_faced_body_not_the_last_one() {
    let (mut doc, body) = cylinder();
    // a second, separate body made after the cylinder
    let mut s = Sketch::new("B", Plane::XY);
    s.rect(DVec2::new(20.0, 0.0), DVec2::new(30.0, 10.0));
    let s = doc.add_sketch(s);
    let other = doc.add_feature(
        "Box",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 5.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    );
    let other = BodyId::of(other);
    let (v0, o0) = (vol(&mut doc, body), vol(&mut doc, other));
    // no explicit target: the face selector's body is the last solid, so pin it
    add(&mut doc, body, -3.0, BoolOp::Cut);
    assert!((v0 - vol(&mut doc, body) - PI * R * R * 3.0).abs() < 1e-6);
    assert!((o0 - vol(&mut doc, other)).abs() < 1e-9);
}

#[test]
fn face_extrude_round_trips_through_save_and_load() {
    let (mut doc, body) = cylinder();
    add(&mut doc, body, -3.0, BoolOp::Cut);
    let want = vol(&mut doc, body);
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("C.ecad");
    doc.save_to_dir(&p).unwrap();
    let mut back = Document::load_from_dir(&p).unwrap();
    assert_eq!(back.data, doc.data);
    assert!((vol(&mut back, body) - want).abs() < 1e-9);
}

#[test]
fn non_planar_face_is_a_clear_error() {
    let (mut doc, body) = cylinder();
    let f = doc.add_feature(
        "Bad",
        FeatureKind::ExtrudeFace { face: FaceSelector::Cylinders { radius: None, axis: None }, distance: 2.0.into(), op: BoolOp::Join, target: Some(body) },
    );
    let r = doc.regenerate();
    match &r.report(f).unwrap().status {
        FeatureStatus::Error(m) => assert!(m.contains("not planar"), "{m}"),
        s => panic!("{s:?}"),
    }
    let _ = DVec3::X;
}
