//! Delete Face: remove faces and let the neighbours heal the gap.

use easycad_model::document::{DocData, Document};
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::occt::{ShapeKind, SurfaceKind};
use easycad_model::regen::FeatureStatus;
use easycad_model::select::{EdgeKind, EdgeSelector, Extremum, FaceKind, FaceSelector};
use easycad_model::sketch::Sketch;
use glam::{DVec2, DVec3};

fn extrude(doc: &mut Document, name: &str, plane: Plane, build: impl FnOnce(&mut Sketch), d: f64, op: BoolOp) -> FeatureId {
    let mut s = Sketch::new(name, plane);
    build(&mut s);
    let s = doc.add_sketch(s);
    doc.add_feature(
        name,
        FeatureKind::Extrude {
            sketch: s,
            profiles: ProfileSel::All,
            distance: d.into(),
            symmetric: false,
            reverse: false,
            draft_deg: 0.0.into(),
            op,
            target: None,
        },
    )
}

fn hint(c: [f64; 3], n: [f64; 3], kind: FaceKind) -> FaceSelector {
    FaceSelector::Hint { centroid: DVec3::from_array(c), normal: DVec3::from_array(n), kind: Some(kind) }
}

/// (planar faces, all faces, volume) after regen; asserts the body is one valid solid.
fn stats(doc: &mut Document, body: BodyId, feat: FeatureId) -> (u32, u32, f64) {
    let r = doc.regenerate();
    assert_eq!(r.report(feat).unwrap().status, FeatureStatus::Ok, "{:?}", r.report(feat));
    let sh = r.body_by_id(body).unwrap().shape();
    assert!(sh.is_valid());
    assert_eq!(sh.count(ShapeKind::Solid), 1);
    let planar = sh.faces().unwrap().iter().filter(|f| f.kind == SurfaceKind::Plane).count() as u32;
    (planar, sh.count(ShapeKind::Face), sh.volume().unwrap())
}

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs()
}

fn filleted_box() -> (Document, BodyId, FeatureId) {
    let mut doc = Document::new("F");
    let f = extrude(&mut doc, "Block", Plane::XY, |s| { s.rect(DVec2::ZERO, DVec2::new(40.0, 30.0)); }, 10.0, BoolOp::NewBody);
    let edge = EdgeSelector::Hint { midpoint: DVec3::new(40.0, 30.0, 5.0), tangent: DVec3::Z, kind: Some(EdgeKind::Line) };
    doc.add_feature("Fillet", FeatureKind::Fillet { body: None, edges: vec![edge], radius: 2.0.into(), auto_reduce: false, sets: vec![], skipped: vec![] });
    let d = doc.add_feature("Del", FeatureKind::DeleteFace { body: None, faces: vec![FaceSelector::Cylinders { radius: Some(2.0), axis: None }] });
    (doc, BodyId::of(f), d)
}

#[test]
fn deleting_a_fillet_face_brings_the_sharp_edge_back() {
    let (mut doc, b, d) = filleted_box();
    let (planar, all, vol) = stats(&mut doc, b, d);
    assert_eq!((planar, all), (6, 6));
    assert!(rel(vol, 40.0 * 30.0 * 10.0) < 1e-6, "{vol}");
}

#[test]
fn deleting_a_bosss_faces_gives_the_plain_plate() {
    let mut doc = Document::new("B");
    let f = extrude(&mut doc, "Plate", Plane::XY, |s| { s.rect(DVec2::ZERO, DVec2::new(60.0, 60.0)); }, 5.0, BoolOp::NewBody);
    extrude(&mut doc, "Boss", Plane::XY.offset(5.0), |s| { s.rect(DVec2::new(20.0, 20.0), DVec2::new(40.0, 40.0)); }, 10.0, BoolOp::Join);
    let faces = vec![
        FaceSelector::Extreme { normal: DVec3::Z, which: Extremum::Max },
        hint([40.0, 30.0, 10.0], [1.0, 0.0, 0.0], FaceKind::Plane),
        hint([20.0, 30.0, 10.0], [-1.0, 0.0, 0.0], FaceKind::Plane),
        hint([30.0, 40.0, 10.0], [0.0, 1.0, 0.0], FaceKind::Plane),
        hint([30.0, 20.0, 10.0], [0.0, -1.0, 0.0], FaceKind::Plane),
    ];
    let d = doc.add_feature("Del", FeatureKind::DeleteFace { body: None, faces });
    let (planar, all, vol) = stats(&mut doc, BodyId::of(f), d);
    assert_eq!((planar, all), (6, 6));
    assert!(rel(vol, 60.0 * 60.0 * 5.0) < 1e-6, "{vol}");
}

#[test]
fn deleting_a_hole_wall_closes_the_hole() {
    let mut doc = Document::new("H");
    let f = extrude(&mut doc, "Plate", Plane::XY, |s| { s.rect(DVec2::ZERO, DVec2::new(60.0, 40.0)); }, 5.0, BoolOp::NewBody);
    extrude(&mut doc, "Hole", Plane::XY, |s| { s.circle(DVec2::new(30.0, 20.0), 6.0); }, 5.0, BoolOp::Cut);
    let d = doc.add_feature("Del", FeatureKind::DeleteFace { body: None, faces: vec![FaceSelector::Cylinders { radius: Some(6.0), axis: None }] });
    let (planar, all, vol) = stats(&mut doc, BodyId::of(f), d);
    assert_eq!((planar, all), (6, 6));
    assert!(rel(vol, 60.0 * 40.0 * 5.0) < 1e-6, "{vol}");
}

#[test]
fn a_face_that_cannot_heal_is_a_clean_error() {
    let mut doc = Document::new("X");
    let f = extrude(&mut doc, "Block", Plane::XY, |s| { s.rect(DVec2::ZERO, DVec2::new(40.0, 30.0)); }, 10.0, BoolOp::NewBody);
    let d = doc.add_feature("Del", FeatureKind::DeleteFace { body: None, faces: vec![FaceSelector::top()] });
    let r = doc.regenerate();
    let rep = r.report(d).unwrap();
    assert_ne!(rep.status, FeatureStatus::Ok, "{rep:?}");
    // the body before the failed feature is untouched and valid
    let sh = r.body_by_id(BodyId::of(f)).unwrap().shape();
    assert!(sh.is_valid());
    assert_eq!(sh.count(ShapeKind::Face), 6);
    // the shape-level call is an Err too
    let e = sh.remove_faces(&[0]).err().expect("must fail");
    assert!(e.to_string().contains("can't be removed"), "{e}");
}

#[test]
fn delete_face_round_trips_through_save_and_load() {
    let (mut doc, b, d) = filleted_box();
    let json = doc.data.to_json().unwrap();
    let back = DocData::from_json(&json).unwrap();
    assert_eq!(back, doc.data);
    assert_eq!(back.to_json().unwrap(), json);
    let mut doc2 = Document::new("loaded");
    doc2.data = back;
    let (planar, _, vol) = stats(&mut doc2, b, d);
    assert_eq!(planar, 6);
    assert!(rel(vol, 12000.0) < 1e-6);
    let _ = &mut doc;
    assert_eq!(FeatureKind::DeleteFace { body: None, faces: vec![] }.label(), "Delete face");
}
