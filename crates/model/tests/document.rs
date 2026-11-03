//! Undo/redo, regeneration caching, sync-friendly persistence, failure isolation,
//! and picking on a real tessellation.

use casework_model::document::{DocData, Document};
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::pick::{EdgeTolerance, HitKind, Picker};
use casework_model::regen::FeatureStatus;
use casework_model::select::{EdgeSelector, FaceSelector};
use casework_model::sketch::{Constraint, Sketch};
use glam::{DVec2, DVec3, Vec3};

fn block_doc() -> (Document, BodyId, FeatureId) {
    let mut doc = Document::new("Block");
    doc.set_param("w", "40").unwrap();
    doc.set_param("t", "w / 4").unwrap();
    let mut s = Sketch::new("S", Plane::XY);
    let l = s.rect(DVec2::ZERO, DVec2::new(40.0, 30.0));
    s.constrain(Constraint::Fix(casework_model::sketch::PointId(0), DVec2::ZERO));
    s.constrain(Constraint::Length(l[0], "w".into()));
    s.constrain(Constraint::Length(l[1], 30.0.into()));
    let s = doc.add_sketch(s);
    let f = doc.add_feature(
        "Block",
        FeatureKind::Extrude {
            sketch: s,
            profiles: ProfileSel::All,
            distance: "t".into(),
            symmetric: false,
            reverse: false,
            draft_deg: 0.0.into(),
            op: BoolOp::NewBody,
            target: None,
        },
    );
    doc.add_feature(
        "Round",
        FeatureKind::Fillet { body: None, edges: vec![EdgeSelector::OfFace(FaceSelector::top())], radius: 2.0.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new(), setback: false },
    );
    (doc, BodyId::of(f), f)
}

fn vol(doc: &mut Document, b: BodyId) -> f64 {
    doc.regenerate().body_by_id(b).unwrap().shape().volume().unwrap()
}

#[test]
fn undo_redo_hits_the_prefix_cache() {
    let (mut doc, body, _) = block_doc();
    let r = doc.regenerate();
    assert_eq!((r.recomputed, r.reused), (2, 0));
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let v40 = vol(&mut doc, body);
    let bb = doc.regenerate().body_by_id(body).unwrap().shape().bbox().unwrap();
    assert!((bb.size().z - 10.0).abs() < 1e-6, "t = w/4");

    let rev0 = doc.data.revision;
    doc.set_param("w", "60").unwrap();
    assert!(doc.data.revision > rev0);
    let r = doc.regenerate();
    assert_eq!((r.recomputed, r.reused), (2, 0));
    let v60 = vol(&mut doc, body);
    assert!(v60 > v40 * 1.5);

    assert_eq!(doc.undo_label(), Some("Set w"));
    assert_eq!(doc.undo().as_deref(), Some("Set w"));
    let r = doc.regenerate();
    assert_eq!((r.recomputed, r.reused), (0, 2), "undo is served from the cache");
    assert!(r.reports.iter().all(|r| r.cached));
    assert!((vol(&mut doc, body) - v40).abs() < 1e-9);
    let rev_after_undo = doc.data.revision;
    assert!(rev_after_undo > rev0 + 1, "revision keeps increasing on undo");

    doc.redo().unwrap();
    let r = doc.regenerate();
    assert_eq!((r.recomputed, r.reused), (0, 2));
    assert!((vol(&mut doc, body) - v60).abs() < 1e-9);

    // appending a feature only computes the new one
    doc.add_feature("Chamfer", FeatureKind::Chamfer { body: None, edges: vec![EdgeSelector::OfFace(FaceSelector::bottom())], distance: 1.0.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new() });
    let r = doc.regenerate();
    assert_eq!((r.recomputed, r.reused), (1, 2));
    // the mesh of a cached body is computed once and shared
    let b = doc.regenerate().body_by_id(body).unwrap().clone();
    let m1 = b.mesh().unwrap();
    let m2 = doc.regenerate().body_by_id(body).unwrap().mesh().unwrap();
    assert!(std::sync::Arc::ptr_eq(&m1, &m2));
}

#[test]
fn failing_feature_is_isolated() {
    let (mut doc, body, _) = block_doc();
    let bad = doc.add_feature(
        "Huge fillet",
        FeatureKind::Fillet { body: None, edges: vec![EdgeSelector::Parallel { dir: DVec3::Z }], radius: 50.0.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new(), setback: false },
    );
    let ok = doc.add_feature(
        "Shell",
        FeatureKind::Shell { body: None, open_faces: vec![FaceSelector::bottom()], thickness: 1.0.into(), outward: false },
    );
    let r = doc.regenerate();
    assert!(matches!(r.report(bad).unwrap().status, FeatureStatus::Error(_)), "{:?}", r.report(bad));
    assert_eq!(r.report(ok).unwrap().status, FeatureStatus::Ok, "{:?}", r.report(ok));
    assert!(r.body_by_id(body).unwrap().shape().is_valid());

    // a bad parameter expression is an error, not a panic, and the edit is rejected
    assert!(doc.set_param("w", "w + 1").is_err());
    assert!(doc.set_param("w", "(").is_err());
    assert_eq!(doc.data.params.get("w").unwrap().expr.0, "40");
}

#[test]
fn folder_format_is_sync_friendly() {
    let (mut doc, _, _) = block_doc();
    let dxf = "0\nSECTION\n2\nENTITIES\n0\nCIRCLE\n8\nH\n10\n0\n20\n0\n40\n1\n0\nENDSEC\n0\nEOF\n";
    doc.import_dxf("holes.dxf", dxf.as_bytes().to_vec());
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("Block.cwk");
    doc.save_to_dir(&p).unwrap();
    let json1 = std::fs::read_to_string(p.join("doc.json")).unwrap();
    doc.save_to_dir(&p).unwrap();
    assert_eq!(json1, std::fs::read_to_string(p.join("doc.json")).unwrap(), "deterministic output");
    assert!(json1.contains("\n  \"doc_id\""), "pretty printed");
    // blobs are content-addressed and not embedded
    let refs = doc.blob_refs();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0], casework_model::document::sha256_hex(dxf.as_bytes()));
    assert!(p.join("blobs").join(&refs[0]).exists());
    assert!(!json1.contains("CIRCLE"));

    let back = Document::load_from_dir(&p).unwrap();
    assert_eq!(back.data.doc_id, doc.data.doc_id);
    assert_eq!(back.data.revision, doc.data.revision);
    assert_eq!(back.dxf(back.data.imports[0].id).unwrap().circles(&[]).len(), 1);
    assert!(uuid::Uuid::parse_str(&back.data.doc_id).is_ok());
    // a missing blob is reported
    std::fs::remove_file(p.join("blobs").join(&refs[0])).unwrap();
    assert!(Document::load_from_dir(&p).is_err());
    // JSON round trip of the data alone
    assert_eq!(DocData::from_json(&json1).unwrap(), doc.data);
}

#[test]
fn pick_stack_on_real_mesh() {
    let (mut doc, body, _) = block_doc();
    let b = doc.regenerate().body_by_id(body).unwrap().clone();
    let mesh = b.mesh().unwrap();
    let picker = Picker::new([(body, &*mesh)]);
    let faces = b.shape().faces().unwrap();
    // straight down through the middle: top face, then bottom face
    let hits = picker.pick_all(Vec3::new(20.0, 15.0, 50.0), Vec3::NEG_Z, EdgeTolerance::ortho(0.05));
    let face_hits: Vec<u32> = hits.iter().filter_map(|h| match h.kind {
        HitKind::Face(f) => Some(f),
        _ => None,
    }).collect();
    assert_eq!(face_hits.len(), 2, "{hits:?}");
    assert!(faces[face_hits[0] as usize].normal.z > 0.99);
    assert!(faces[face_hits[1] as usize].normal.z < -0.99);
    assert!((hits[0].t - 40.0).abs() < 1e-3);
    // a horizontal ray along x through the side walls, grazing an edge region
    let hits = picker.pick_all(Vec3::new(-10.0, 15.0, 5.0), Vec3::X, EdgeTolerance::ortho(0.05));
    assert_eq!(hits.len(), 2);
    assert!(hits[0].t < hits[1].t);
    // perspective tolerance catches the bottom edge at y = 0 seen from the side
    let tol = EdgeTolerance::perspective(4.0, 0.8, 800.0);
    let hits = picker.pick_all(Vec3::new(20.0, -100.0, 0.05), Vec3::Y, tol);
    assert!(matches!(hits[0].kind, HitKind::Edge(_)), "{hits:?}");
}

#[test]
fn revolve_draft_chamfer_circular_pattern() {
    use casework_model::geom::Axis;
    let mut doc = Document::new("Misc");
    // revolve a 2×4 rectangle at r = 5..7 around Z → tube
    let mut s = Sketch::new("Prof", Plane::XZ);
    // XZ plane: local x = world X, local y = world Z (normal -Y)
    s.rect(DVec2::new(5.0, 0.0), DVec2::new(7.0, 4.0));
    let s = doc.add_sketch(s);
    let tube = doc.add_feature(
        "Tube",
        FeatureKind::Revolve { sketch: s, profiles: ProfileSel::All, axis: Axis::Z, angle_deg: 360.0.into(), op: BoolOp::NewBody, target: None },
    );
    let tv = std::f64::consts::PI * (49.0 - 25.0) * 4.0;
    assert!((vol(&mut doc, BodyId::of(tube)) - tv).abs() < 1e-6);

    // a block next to it, drafted and chamfered, then a circular pattern of a peg
    let mut s = Sketch::new("B", Plane::XY);
    s.rect(DVec2::new(20.0, -5.0), DVec2::new(30.0, 5.0));
    let s = doc.add_sketch(s);
    let blk = doc.add_feature(
        "Block",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 10.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    );
    let blk = BodyId::of(blk);
    doc.add_feature(
        "Draft",
        FeatureKind::Draft {
            body: Some(blk),
            faces: vec![FaceSelector::AllWithNormal { normal: DVec3::X }, FaceSelector::AllWithNormal { normal: DVec3::NEG_X }],
            neutral: Plane::XY,
            pull: DVec3::Z,
            angle_deg: 5.0.into(),
            offset: 0.0.into(),
        },
    );
    let v = vol(&mut doc, blk);
    let want = 1000.0 - 10.0 * 10.0 * 10.0 * 5f64.to_radians().tan(); // two faces × ½·h²·tan·depth
    assert!((v - want).abs() < 1e-3, "{v} vs {want}");
    doc.add_feature(
        "Chamfer",
        FeatureKind::Chamfer { body: Some(blk), edges: vec![EdgeSelector::Between(FaceSelector::top(), FaceSelector::AllWithNormal { normal: DVec3::Y })], distance: 1.0.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new() },
    );
    let v2 = vol(&mut doc, blk);
    assert!(v2 < v && v - v2 < 10.0);

    let mut s = Sketch::new("Peg", Plane::XY.offset(4.0));
    s.circle(DVec2::new(6.0, 0.0), 0.5);
    let s = doc.add_sketch(s);
    let peg = doc.add_feature(
        "Peg",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 3.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Join, target: Some(BodyId::of(tube)) },
    );
    let pat = doc.add_feature(
        "Pegs",
        FeatureKind::Pattern { source: PatternSource::Features(vec![peg]), kind: PatternKind::Circular { axis: Axis::Z, count: 6.0.into(), total_deg: 360.0.into() } },
    );
    let r = doc.regenerate();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    assert_eq!(r.report(pat).unwrap().status, FeatureStatus::Ok);
    let v = vol(&mut doc, BodyId::of(tube));
    assert!((v - (tv + 6.0 * std::f64::consts::PI * 0.25 * 3.0)).abs() < 1e-4, "{v}");
}

#[test]
fn selector_threshold_reports_unresolved_after_drastic_edit() {
    use casework_model::select::Selector;
    let (mut doc, body, block) = block_doc();
    // pick the +X side face in the viewport (a stored hint, not a semantic rule)
    let face = {
        let out = doc.regenerate();
        let faces = out.body_by_id(body).unwrap().shape().faces().unwrap();
        faces.into_iter().find(|f| f.normal.x > 0.99).unwrap()
    };
    let hint = FaceSelector::from_pick(&face);
    let shell = doc.add_feature("Shell", FeatureKind::Shell { body: None, open_faces: vec![hint.clone()], thickness: 1.0.into(), outward: false });
    assert_eq!(doc.regenerate().report(shell).unwrap().status, FeatureStatus::Ok);

    // a dimension edit moves the face (w 40 → 50): still resolves
    doc.set_param("w", "50").unwrap();
    assert_eq!(doc.regenerate().report(shell).unwrap().status, FeatureStatus::Ok);

    // drastic edit: the outline becomes a circle, so the planar side face is gone
    let sk = match &doc.data.feature(block).unwrap().kind {
        FeatureKind::Extrude { sketch, .. } => *sketch,
        _ => unreachable!(),
    };
    doc.edit("Circle outline", |d| {
        let s = d.sketch_mut(sk).unwrap();
        *s = Sketch::new("S", Plane::XY);
        s.circle(DVec2::new(20.0, 15.0), 15.0);
        Ok::<_, ()>(())
    })
    .unwrap();
    let st = doc.regenerate().report(shell).unwrap().status.clone();
    let FeatureStatus::Unresolved(u) = st else { panic!("expected Unresolved, got {st:?}") };
    let casework_model::regen::Unresolved { selector, best_candidate, score } = *u;
    assert_eq!(selector, Selector::Face(hint));
    assert!(score > 1.0);
    let best = best_candidate.expect("a candidate is offered");
    assert!(matches!(best, Selector::Face(FaceSelector::Hint { .. })));
    // the shell was not applied to a wrong face: the body is still the solid cylinder
    let v = doc.regenerate().body_by_id(body).unwrap().shape().volume().unwrap();
    // (solid cylinder minus the r2 top fillet; a shell would remove far more)
    let solid = std::f64::consts::PI * 225.0 * 12.5;
    assert!(v < solid && v > solid - 100.0, "{v}");

    // accepting the candidate re-binds the reference and the feature applies
    assert!(doc.accept_candidate(shell, &selector, &best));
    assert_eq!(doc.undo_label(), Some("Re-pick reference"));
    let out = doc.regenerate();
    assert_eq!(out.report(shell).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(shell));
    assert!(out.body_by_id(body).unwrap().shape().volume().unwrap() < v);

    // a semantic selector that matches nothing is also Unresolved (no candidate)
    let f = doc.add_feature("No such face", FeatureKind::Shell { body: None, open_faces: vec![FaceSelector::AllWithNormal { normal: DVec3::X }], thickness: 1.0.into(), outward: false });
    assert!(matches!(doc.regenerate().report(f).unwrap().status, FeatureStatus::Unresolved(ref u) if u.best_candidate.is_none()));
}

/// GUI_NEEDS #5: a shell with no open faces is a closed hollow solid (outer skin +
/// internal void), never just the inner offset surface; too thick is an error.
#[test]
fn shell_without_open_faces_is_closed_hollow() {
    let (mut doc, body, _) = block_doc();
    let solid = doc.regenerate().body_by_id(body).unwrap().shape().volume().unwrap();
    let f = doc.add_feature("Hollow", FeatureKind::Shell { body: None, open_faces: vec![], thickness: 1.0.into(), outward: false });
    let r = doc.regenerate();
    assert_eq!(r.report(f).unwrap().status, FeatureStatus::Ok, "{:?}", r.report(f));
    let s = r.body_by_id(body).unwrap().shape();
    assert!(s.is_valid());
    assert_eq!(s.count(casework_model::occt::ShapeKind::Solid), 1);
    assert_eq!(s.count(casework_model::occt::ShapeKind::Shell), 2);
    let size = s.bbox().unwrap().size();
    assert!((size.x - 40.0).abs() < 1e-6 && (size.z - 10.0).abs() < 1e-6, "{size:?}");
    let v = s.volume().unwrap();
    assert!(v > 0.0 && v < 0.5 * solid, "{v} vs {solid}");
    doc.edit("thick", |d| {
        if let FeatureKind::Shell { thickness, .. } = &mut d.feature_mut(f).unwrap().kind {
            *thickness = 6.0.into();
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    let r = doc.regenerate();
    match &r.report(f).unwrap().status {
        FeatureStatus::Error(e) => assert!(e.contains("no open faces"), "{e}"),
        other => panic!("{other:?}"),
    }
}
