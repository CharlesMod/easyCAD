//! Fillet / chamfer by face pick: `EdgeSelector::OfFace` dresses every edge that
//! bounds the face, and edges named twice (two faces, or a face and an edge) are
//! dressed once.

use easycad_model::document::Document;
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::occt::ShapeKind;
use easycad_model::regen::FeatureStatus;
use easycad_model::select::{EdgeSelector, FaceSelector};
use easycad_model::sketch::Sketch;
use glam::{DVec2, DVec3};

/// A 40 x 30 x 10 block, then a dress-up feature (fillet or chamfer) with `edges`.
fn dressed(chamfer: bool, edges: Vec<EdgeSelector>) -> (Document, BodyId, FeatureId) {
    let mut doc = Document::new("Bar");
    let mut s = Sketch::new("S", Plane::XY);
    s.rect(DVec2::ZERO, DVec2::new(40.0, 30.0));
    let s = doc.add_sketch(s);
    let f = doc.add_feature(
        "Block",
        FeatureKind::Extrude {
            sketch: s,
            profiles: ProfileSel::All,
            distance: "10".into(),
            symmetric: false,
            reverse: false,
            draft_deg: 0.0.into(),
            op: BoolOp::NewBody,
            target: None,
        },
    );
    let kind = if chamfer {
        FeatureKind::Chamfer { body: None, edges, distance: 1.0.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new() }
    } else {
        FeatureKind::Fillet { body: None, edges, radius: 1.0.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new() }
    };
    let d = doc.add_feature("Dress", kind);
    (doc, BodyId::of(f), d)
}

/// (face count, volume) of the dressed body; asserts the feature is Ok and the solid valid.
fn result(doc: &mut Document, body: BodyId, d: FeatureId) -> (u32, f64) {
    let r = doc.regenerate();
    assert_eq!(r.report(d).unwrap().status, FeatureStatus::Ok, "{:?}", r.report(d));
    let sh = r.body_by_id(body).unwrap().shape();
    assert!(sh.is_valid());
    assert_eq!(sh.count(ShapeKind::Solid), 1);
    (sh.count(ShapeKind::Face), sh.volume().unwrap())
}

fn top_and_bottom() -> Vec<EdgeSelector> {
    vec![EdgeSelector::OfFace(FaceSelector::top()), EdgeSelector::OfFace(FaceSelector::bottom())]
}

#[test]
fn two_opposite_faces_dress_all_eight_edges() {
    let (mut doc, b, d) = dressed(false, top_and_bottom());
    let (faces, vol) = result(&mut doc, b, d);
    assert_eq!(faces, 6 + 8, "8 edges filleted: one new face each");
    assert!(vol < 40.0 * 30.0 * 10.0);
    let (mut doc, b, d) = dressed(true, top_and_bottom());
    assert_eq!(result(&mut doc, b, d).0, 6 + 8);
}

#[test]
fn overlapping_face_and_edge_picks_dress_each_edge_once() {
    let (mut doc, b, d) = dressed(false, top_and_bottom());
    let (faces, vol) = result(&mut doc, b, d);
    // the top face again plus a top edge by hint: nothing new
    let mut edges = top_and_bottom();
    edges.push(EdgeSelector::OfFace(FaceSelector::top()));
    edges.push(EdgeSelector::Hint {
        midpoint: DVec3::new(20.0, 0.0, 10.0),
        tangent: DVec3::X,
        kind: None,
    });
    let (mut doc2, b2, d2) = dressed(false, edges);
    let (faces2, vol2) = result(&mut doc2, b2, d2);
    assert_eq!(faces2, faces);
    assert!((vol2 - vol).abs() < 1e-9, "same edges, same solid");

    let mut edges = top_and_bottom();
    edges.push(EdgeSelector::OfFace(FaceSelector::Extreme { normal: DVec3::NEG_X, which: easycad_model::select::Extremum::Max }));
    let (mut doc3, b3, d3) = dressed(false, edges);
    let (faces3, _) = result(&mut doc3, b3, d3);
    assert_eq!(faces3, 6 + 10 + 4, "10 distinct edges (the left face shares 2) and 4 corner blends");
}
