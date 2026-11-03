//! Engine features requested by the GUI (docs/GUI_NEEDS.md): expression-driven
//! planes, the saved rollback marker, multi-set fillets with skipped edges and
//! request mapping, default fits, standoff exclusions / gussets / lid holes.

mod common;

use casework_model::document::{DocData, Document};
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::process::{Fit, FitClass, FitRole};
use casework_model::regen::FeatureStatus;
use casework_model::select::{EdgeSelector, FaceSelector};
use casework_model::sketch::Sketch;
use glam::{DVec2, DVec3};

fn block(doc: &mut Document, w: f64, d: f64, h: f64) -> BodyId {
    let mut s = Sketch::new("S", Plane::XY);
    s.rect(DVec2::new(-w / 2.0, -d / 2.0), DVec2::new(w / 2.0, d / 2.0));
    let s = doc.add_sketch(s);
    BodyId::of(doc.add_feature(
        "Block",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: h.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ))
}

#[test]
fn split_plane_follows_a_parameter() {
    let mut doc = Document::new("Split");
    doc.set_param("cut", "10").unwrap();
    let b = block(&mut doc, 40.0, 30.0, 20.0);
    let f = doc.add_feature("Split", FeatureKind::Split { body: Some(b), plane: Plane::XY, new_name: "Top".into(), offset: "cut".into() });
    let z = |doc: &mut Document| doc.regenerate().body_by_id(BodyId::of(f)).unwrap().shape().bbox().unwrap().min.z;
    assert!((z(&mut doc) - 10.0).abs() < 1e-6);
    doc.set_param("cut", "15").unwrap();
    assert!((z(&mut doc) - 15.0).abs() < 1e-6);
}

#[test]
fn rollback_marker_is_saved_undoable_and_inserts_there() {
    let mut doc = Document::new("Rollback");
    let b = block(&mut doc, 40.0, 30.0, 20.0);
    let ch = doc.add_feature(
        "Chamfer",
        FeatureKind::Chamfer { body: Some(b), edges: vec![EdgeSelector::OfFace(FaceSelector::top())], distance: 1.0.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new() },
    );
    let full = doc.regenerate().body_by_id(b).unwrap().shape().volume().unwrap();
    doc.set_rollback(Some(1));
    assert_eq!(doc.undo_label(), Some("Roll back to 1"));
    let out = doc.regenerate().clone();
    assert!((out.body_by_id(b).unwrap().shape().volume().unwrap() - 24000.0).abs() < 1e-6);
    let rep = out.report(ch).unwrap();
    assert!(rep.rolled_back && rep.status == FeatureStatus::Suppressed);
    // a new feature goes in at the marker, which moves past it
    let f = doc.add_feature("Split", FeatureKind::Split { body: Some(b), plane: Plane::XY.offset(5.0), new_name: "Top".into(), offset: 0.0.into() });
    assert_eq!(doc.data.features.iter().map(|x| x.id).collect::<Vec<_>>()[1], f);
    assert_eq!(doc.data.rollback, Some(2));
    // saved
    let back = DocData::from_json(&doc.data.to_json().unwrap()).unwrap();
    assert_eq!(back.rollback, Some(2));
    doc.undo();
    doc.set_rollback(None);
    assert_eq!(doc.data.rollback, None);
    assert!((doc.regenerate().body_by_id(b).unwrap().shape().volume().unwrap() - full).abs() < 1e-6);
    doc.undo();
    assert_eq!(doc.data.rollback, Some(1));
}

#[test]
fn fillet_sets_skipped_edges_and_request_mapping() {
    let mut doc = Document::new("Sets");
    let b = block(&mut doc, 40.0, 30.0, 20.0);
    let skip = EdgeSelector::Hint { midpoint: DVec3::new(20.0, 15.0, 10.0), tangent: DVec3::Z, kind: None };
    let f = doc.add_feature(
        "Fillet",
        FeatureKind::Fillet {
            body: Some(b),
            edges: vec![EdgeSelector::OfFace(FaceSelector::top())],
            radius: 2.0.into(),
            auto_reduce: false,
            sets: vec![EdgeSet { edges: vec![EdgeSelector::Parallel { dir: DVec3::Z }], size: 1.0.into(), auto_reduce: true }],
            skipped: vec![skip],
            setback: false,
        },
    );
    let out = doc.regenerate().clone();
    let rep = out.report(f).unwrap();
    assert!(matches!(rep.status, FeatureStatus::Ok | FeatureStatus::Warning(_)), "{:?}", rep.status);
    let dr = rep.dressup.as_ref().unwrap();
    assert_eq!(dr.outcomes.len(), 7, "4 top edges + 3 of 4 vertical edges");
    assert_eq!(rep.skipped_edges.len(), 1);
    assert_eq!(rep.edge_requests.len(), 7);
    let per_set = |s: usize| dr.outcomes.iter().filter(|o| rep.edge_requests[o.request].set == s).count();
    assert_eq!((per_set(0), per_set(1)), (4, 3));
    assert!(dr.applied_count() >= 6, "{}", dr.summary());
    // the skipped corner stays sharp: its vertical edge is still a full-height line
    let shape = out.body_by_id(b).unwrap().shape();
    assert!(shape.edges().unwrap().iter().any(|e| (e.midpoint - DVec3::new(20.0, 15.0, 9.0)).length() < 1.5 && e.tangent.z.abs() > 0.99));
    // edge_sets lists both sets in order
    let kind = &doc.data.feature(f).unwrap().kind;
    assert_eq!(kind.edge_sets().len(), 2);
    // everything skipped is an error that says so
    doc.edit("skip all", |d| {
        if let FeatureKind::Fillet { skipped, .. } = &mut d.feature_mut(f).unwrap().kind {
            *skipped = vec![EdgeSelector::Parallel { dir: DVec3::X }, EdgeSelector::Parallel { dir: DVec3::Y }, EdgeSelector::Parallel { dir: DVec3::Z }];
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    let out = doc.regenerate();
    assert!(matches!(&out.report(f).unwrap().status, FeatureStatus::Error(e) if e.contains("skipped")), "{:?}", out.report(f).unwrap().status);
}

#[test]
fn default_fits_per_field() {
    let boss = FeatureKind::ScrewBoss { body: None, plane: Plane::XY, points: vec![], od: 6.0.into(), hole_dia: 2.5.into(), height: 5.0.into(), hole_depth: None, base_fillet: 0.0.into(), draft_deg: 0.0.into(), offset: 0.0.into() };
    assert_eq!(boss.default_fit("hole_dia"), Some(Fit::hole(FitClass::Snug)));
    assert_eq!(boss.default_fit("od"), None);
    let (doc, _) = common::enclosure_doc();
    let joint = &doc.data.features.iter().find(|f| matches!(f.kind, FeatureKind::PartingJoint { .. })).unwrap().kind;
    assert_eq!(joint.default_fit("kind.width").map(|f| f.role), Some(FitRole::Pin));
    assert_eq!(joint.default_fit("extra_depth").map(|f| f.role), Some(FitRole::Gap));
}

/// Closed 100 × 80 × 30 box (2 mm walls, floor top z = 2, lid z 28–30) with five
/// board holes; one is excluded, the rest get gussets and lid holes.
#[test]
fn standoff_exclusions_gussets_and_lid_holes() {
    let mut doc = Document::new("Standoffs");
    let b = block(&mut doc, 100.0, 80.0, 30.0);
    let mut c = Sketch::new("Cavity", Plane::XY.offset(2.0));
    c.rect(DVec2::new(-48.0, -38.0), DVec2::new(48.0, 38.0));
    let c = doc.add_sketch(c);
    doc.add_feature(
        "Cavity",
        FeatureKind::Extrude { sketch: c, profiles: ProfileSel::All, distance: 26.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Cut, target: Some(b) },
    );
    let holes = [(-30.0, -25.0), (30.0, -25.0), (30.0, 25.0), (-30.0, 25.0), (40.0, -30.0)];
    let mut dxf = String::from("  0\nSECTION\n  2\nHEADER\n  9\n$INSUNITS\n 70\n     4\n  0\nENDSEC\n  0\nSECTION\n  2\nENTITIES\n");
    for (x, y) in holes {
        dxf += &format!("  0\nCIRCLE\n  8\nHoles\n 10\n{x}\n 20\n{y}\n 30\n0.0\n 40\n1.5\n");
    }
    dxf += "  0\nENDSEC\n  0\nEOF\n";
    let imp = doc.import_dxf("board.dxf", dxf.into_bytes());
    let kind = |exclude: Vec<DVec2>, gussets: Option<Gussets>, lid_holes: Option<LidHoles>| FeatureKind::StandoffsFromPcb {
        body: Some(b),
        source: PcbSource::Dxf { import: imp, layers: vec!["Holes".into()], min_dia: 2.5, max_dia: 4.0, offset: DVec2::ZERO },
        plane: Plane::XY,
        od: 7.0.into(),
        hole_dia: 2.5.into(),
        height: 10.0.into(),
        base_fillet: 0.0.into(),
        draft_deg: 0.0.into(),
        offset: 2.0.into(),
        exclude,
        gussets,
        lid_holes,
    };
    let f = doc.add_feature("Standoffs", kind(vec![DVec2::new(-30.0, 25.0)], None, None));
    let out = doc.regenerate().clone();
    assert_eq!(out.report(f).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(f).unwrap().status);
    let shape = out.body_by_id(b).unwrap().shape().clone();
    let bosses = |s: &casework_model::occt::Shape| s.faces().unwrap().iter().filter(|f| (f.radius - 3.5).abs() < 1e-6).count();
    assert_eq!(bosses(&shape), 4, "one of five holes excluded");
    let v0 = shape.volume().unwrap();

    let g = Gussets { thickness: 1.2.into(), height: None, max_length: 25.0.into() };
    let l = LidHoles { body: None, hole_dia: 3.2.into(), cbore_dia: 6.0.into(), cbore_depth: 1.0.into() };
    doc.edit("gussets", |d| {
        d.feature_mut(f).unwrap().kind = kind(vec![DVec2::new(-30.0, 25.0)], Some(g), Some(l));
        Ok::<_, ()>(())
    })
    .unwrap();
    let out = doc.regenerate().clone();
    let rep = out.report(f).unwrap();
    assert_eq!(rep.status, FeatureStatus::Ok, "{:?}", rep.status);
    let shape = out.body_by_id(b).unwrap().shape();
    assert!(shape.is_valid());
    let faces = shape.faces().unwrap();
    let lid = |r: f64| faces.iter().filter(|f| (f.radius - r).abs() < 0.02 && f.centroid.z > 27.0).count();
    // hole Ø3.2 (+ fit allowance, nominal profile: none) and counterbore Ø6 in the lid
    assert_eq!(lid(1.6), 4, "lid holes");
    assert_eq!(lid(3.0), 4, "counterbores");
    // gussets add material: 4 ribs 1.2 thick, 6 high (60 % of 10), 8..13 mm long
    let v1 = shape.volume().unwrap();
    let lid_removed = 4.0 * std::f64::consts::PI * (1.6f64.powi(2) * 1.0 + 3.0f64.powi(2) * 1.0);
    let ribs = v1 - v0 + lid_removed;
    assert!(ribs > 4.0 * 1.2 * 6.0 * 4.0 && ribs < 4.0 * 1.2 * 6.0 * 13.0, "{ribs}");
}

#[test]
fn profile_edits_keep_their_place() {
    let mut doc = Document::new("Profiles");
    let names: Vec<String> = doc.data.processes.iter().map(|p| p.name.clone()).collect();
    let mut p = doc.data.processes[0].clone();
    p.xy_comp = 0.07;
    doc.update_profile("Fit allowances", p);
    assert_eq!(doc.undo_label(), Some("Fit allowances"));
    assert_eq!(doc.data.processes.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), names);
    assert_eq!(doc.data.processes[0].xy_comp, 0.07);
}
