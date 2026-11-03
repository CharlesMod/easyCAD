//! Patterns re-apply the source recipe; ribs "to body".

use std::f64::consts::PI;

use easycad_model::document::Document;
use easycad_model::feature::*;
use easycad_model::geom::{Axis, Plane};
use easycad_model::library::{BuiltinSource, HoleChoice, PartSource};
use easycad_model::params::Expr;
use easycad_model::regen::FeatureStatus;
use easycad_model::select::{EdgeSelector, FaceSelector};
use easycad_model::sketch::Sketch;
use glam::{DVec2, DVec3};

fn plate(doc: &mut Document, size: DVec2, t: f64) -> BodyId {
    let mut s = Sketch::new("P", Plane::XY);
    s.rect(-size / 2.0, size / 2.0);
    let s = doc.add_sketch(s);
    BodyId::of(doc.add_feature(
        "Plate",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: t.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ))
}

fn boss(points: Vec<DVec2>, b: BodyId) -> FeatureKind {
    FeatureKind::ScrewBoss { body: Some(b), plane: Plane::XY.offset(3.0), points, od: 7.0.into(), hole_dia: 2.5.into(), height: 8.0.into(), hole_depth: None, base_fillet: 1.0.into(), draft_deg: 0.0.into(), offset: 0.0.into() }
}

fn vol(doc: &mut Document, b: BodyId) -> f64 {
    let r = doc.regenerate();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    r.body_by_id(b).unwrap().shape().volume().unwrap()
}

#[test]
fn patterned_bosses_keep_their_fillets() {
    // reference: one feature with three bosses
    let mut a = Document::new("A");
    let pa = plate(&mut a, DVec2::new(80.0, 30.0), 3.0);
    a.add_feature("Bosses", boss(vec![DVec2::new(-15.0, 0.0), DVec2::new(0.0, 0.0), DVec2::new(15.0, 0.0)], pa));
    let want = vol(&mut a, pa);
    // pattern of a single boss
    let mut b = Document::new("B");
    let pb = plate(&mut b, DVec2::new(80.0, 30.0), 3.0);
    let f = b.add_feature("Boss", boss(vec![DVec2::new(-15.0, 0.0)], pb));
    let p = b.add_feature("Row", FeatureKind::Pattern { source: PatternSource::Features(vec![f]), kind: PatternKind::Linear { dir: DVec3::X, spacing: 15.0.into(), count: Expr::from(3) } });
    let got = vol(&mut b, pb);
    assert!((got - want).abs() < 1e-3, "patterned {got} vs reference {want}");
    let r = b.regenerate();
    assert_eq!(r.report(p).unwrap().status, FeatureStatus::Ok, "{:?}", r.report(p));
    // the plain (unfilleted) boss volume would be clearly less
    let plain = 80.0 * 30.0 * 3.0 + 3.0 * PI * (12.25 - 1.5625) * 8.0;
    assert!(got > plain + 3.0);
}

#[test]
fn mirrored_parts_are_copied_and_counted() {
    let mut doc = Document::new("M");
    let p = plate(&mut doc, DVec2::new(60.0, 30.0), 10.0);
    let insert = BuiltinSource.parts().into_iter().find(|x| x.id == "insert_m").unwrap();
    let r = doc.part_ref(&insert, "M3").unwrap();
    let f = doc.add_feature(
        "Insert",
        FeatureKind::InsertPart {
            part: r,
            placements: vec![PlacementRef::PointOnFace { face: FaceSelector::top(), point: DVec3::new(20.0, 5.0, 10.0) }],
            flip: false,
            offset: 0.0.into(),
            auto_hole: Some(AutoHole { choice: HoleChoice::Insert, depth: None, fit: None }),
            target: Some(p),
        },
    );
    let before = vol(&mut doc, p);
    doc.add_feature("Mirror", FeatureKind::Mirror { source: PatternSource::Features(vec![f]), plane: Plane::YZ });
    let after = vol(&mut doc, p);
    assert!((before - after - PI * 4.0 * 6.7).abs() < 1e-3, "second insert hole cut");
    let out = doc.regenerate().clone();
    let parts: Vec<_> = out.bodies.iter().filter(|b| b.part.is_some()).collect();
    assert_eq!(parts.len(), 2);
    let xs: Vec<f64> = parts.iter().map(|b| b.shape().bbox().unwrap().center().x).collect();
    assert!(xs.iter().any(|x| (x - 20.0).abs() < 1e-6) && xs.iter().any(|x| (x + 20.0).abs() < 1e-6), "{xs:?}");
    assert_eq!(doc.bom()[0].qty, 2, "BOM counts the mirrored insert");
    // a circular pattern of the mirror (nested) → 4 inserts
    let m = doc.data.features.last().unwrap().id;
    doc.add_feature("Ring", FeatureKind::Pattern { source: PatternSource::Features(vec![f, m]), kind: PatternKind::Circular { axis: Axis::Z, count: Expr::from(2), total_deg: 360.0.into() } });
    assert_eq!(doc.bom()[0].qty, 4);
}

#[test]
fn patterned_holes_get_fits_and_teardrops() {
    let mut doc = Document::new("H");
    doc.set_active_profile("FDM 0.4 generic").unwrap();
    let p = plate(&mut doc, DVec2::new(60.0, 20.0), 20.0);
    // a horizontal Ø5 hole along Y, patterned along X
    let mut h = Sketch::new("H", Plane::XZ.offset(-15.0));
    h.circle(DVec2::new(-20.0, 10.0), 2.5);
    let h = doc.add_sketch(h);
    let hole = doc.add_feature(
        "Hole",
        FeatureKind::Extrude { sketch: h, profiles: ProfileSel::All, distance: 30.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Cut, target: Some(p) },
    );
    doc.add_feature("Holes", FeatureKind::Pattern { source: PatternSource::Features(vec![hole]), kind: PatternKind::Linear { dir: DVec3::X, spacing: 20.0.into(), count: Expr::from(3) } });
    let out = doc.regenerate();
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert_eq!(out.process.teardropped.len(), 3, "{:?}", out.process);
    // a fillet whose selector picks over the whole body has no copy of its own
    let fillet = doc.add_feature("F", FeatureKind::Fillet { body: Some(p), edges: vec![EdgeSelector::Parallel { dir: DVec3::Z }], radius: 1.0.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new(), setback: false });
    let bad = doc.add_feature("Bad", FeatureKind::Pattern { source: PatternSource::Features(vec![fillet]), kind: PatternKind::Linear { dir: DVec3::X, spacing: 5.0.into(), count: Expr::from(2) } });
    assert!(matches!(doc.regenerate().report(bad).unwrap().status, FeatureStatus::Error(ref e) if e.contains("parallel to a direction over the whole body")), "{:?}", doc.regenerate().report(bad));
}

fn fillet(edges: Vec<EdgeSelector>, r: f64, b: BodyId) -> FeatureKind {
    FeatureKind::Fillet { body: Some(b), edges, radius: r.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new(), setback: false }
}

/// The top outer edge of a boss at `x` (top at z = 11, radius 3.5).
fn boss_top(x: f64) -> EdgeSelector {
    EdgeSelector::Circles { radius: 3.5, near: Some((DVec3::new(x, 0.0, 11.0), 0.5)) }
}

#[test]
fn patterned_fillets_follow_their_bosses() {
    // reference: three bosses, one fillet on all three top edges
    let mut a = Document::new("A");
    let pa = plate(&mut a, DVec2::new(80.0, 30.0), 3.0);
    a.add_feature("Bosses", boss(vec![DVec2::new(-15.0, 0.0), DVec2::new(0.0, 0.0), DVec2::new(15.0, 0.0)], pa));
    a.add_feature("Round", fillet(vec![boss_top(-15.0), boss_top(0.0), boss_top(15.0)], 1.0, pa));
    let want = vol(&mut a, pa);
    // a pattern of one boss and its fillet
    let mut b = Document::new("B");
    let pb = plate(&mut b, DVec2::new(80.0, 30.0), 3.0);
    let f = b.add_feature("Boss", boss(vec![DVec2::new(-15.0, 0.0)], pb));
    let r = b.add_feature("Round", fillet(vec![boss_top(-15.0)], 1.0, pb));
    let before = vol(&mut b, pb);
    let p = b.add_feature("Row", FeatureKind::Pattern { source: PatternSource::Features(vec![f, r]), kind: PatternKind::Linear { dir: DVec3::X, spacing: 15.0.into(), count: Expr::from(3) } });
    let got = vol(&mut b, pb);
    assert!((got - want).abs() < 1e-3, "patterned {got} vs reference {want}");
    let out = b.regenerate();
    assert_eq!(out.report(p).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(p));
    assert!(out.body_by_id(pb).unwrap().shape().is_valid());
    // the bosses alone (without their fillets) would weigh more
    let mut c = Document::new("C");
    let pc = plate(&mut c, DVec2::new(80.0, 30.0), 3.0);
    let f = c.add_feature("Boss", boss(vec![DVec2::new(-15.0, 0.0)], pc));
    c.add_feature("Row", FeatureKind::Pattern { source: PatternSource::Features(vec![f]), kind: PatternKind::Linear { dir: DVec3::X, spacing: 15.0.into(), count: Expr::from(3) } });
    let unrounded = vol(&mut c, pc);
    assert!(unrounded > got + 1e-3 && before < got);
}

#[test]
fn mirrored_chamfer_and_draft_follow_the_copy() {
    let mut doc = Document::new("M");
    let p = plate(&mut doc, DVec2::new(60.0, 30.0), 3.0);
    let base = vol(&mut doc, p);
    let f = doc.add_feature("Boss", boss(vec![DVec2::new(15.0, 0.0)], p));
    // a picked (hint) edge: the boss's top outer circle
    let shape = doc.regenerate().body_by_id(p).unwrap().shape().clone();
    let e = shape.edges().unwrap().into_iter().find(|e| e.circle.is_some_and(|(c, _, r)| (r - 3.5).abs() < 1e-6 && (c.z - 11.0).abs() < 1e-6)).unwrap();
    let ch = doc.add_feature("Chamfer", FeatureKind::Chamfer { body: Some(p), edges: vec![EdgeSelector::from_pick(&e)], distance: 0.8.into(), auto_reduce: false, sets: Vec::new(), skipped: Vec::new() });
    let one = vol(&mut doc, p) - base;
    let m = doc.add_feature("Mirror", FeatureKind::Mirror { source: PatternSource::Features(vec![f, ch]), plane: Plane::YZ });
    let two = vol(&mut doc, p) - base;
    assert!((two - 2.0 * one).abs() < 1e-3, "{two} vs 2 x {one}");
    let out = doc.regenerate();
    assert_eq!(out.report(m).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(m));

    // a drafted block, patterned with its draft: the copies taper like the original
    let mut d = Document::new("D");
    let p = plate(&mut d, DVec2::new(80.0, 30.0), 3.0);
    let mut s = Sketch::new("B", Plane::XY.offset(3.0));
    s.rect(DVec2::new(-30.0, -5.0), DVec2::new(-20.0, 5.0));
    let s = d.add_sketch(s);
    let block = d.add_feature("Block", FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 10.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Join, target: Some(p) });
    let face = FaceSelector::Hint { centroid: DVec3::new(-30.0, 0.0, 8.0), normal: DVec3::NEG_X, kind: None };
    let dr = d.add_feature("Draft", FeatureKind::Draft { body: Some(p), faces: vec![face], neutral: Plane::XY.offset(3.0), pull: DVec3::Z, angle_deg: 5.0.into(), offset: 0.0.into() });
    let v1 = vol(&mut d, p);
    let row = d.add_feature("Row", FeatureKind::Pattern { source: PatternSource::Features(vec![block, dr]), kind: PatternKind::Linear { dir: DVec3::X, spacing: 20.0.into(), count: Expr::from(3) } });
    let v3 = vol(&mut d, p);
    let block_v = v1 - 80.0 * 30.0 * 3.0;
    assert!(block_v < 1000.0 - 1.0, "the draft removes material: {block_v}");
    assert!((v3 - v1 - 2.0 * block_v).abs() < 1e-3, "each copy drafted: {v3} {v1} {block_v}");
    let out = d.regenerate();
    assert_eq!(out.report(row).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(row));
}

#[test]
fn patterning_a_shell_says_why_not() {
    let mut doc = Document::new("S");
    let p = plate(&mut doc, DVec2::new(40.0, 40.0), 20.0);
    let sh = doc.add_feature("Shell", FeatureKind::Shell { body: Some(p), open_faces: vec![FaceSelector::top()], thickness: 2.0.into(), outward: false });
    let bad = doc.add_feature("Ring", FeatureKind::Pattern { source: PatternSource::Features(vec![sh]), kind: PatternKind::Circular { axis: Axis::Z, count: Expr::from(4), total_deg: 360.0.into() } });
    let st = doc.regenerate().report(bad).unwrap().status.clone();
    assert!(matches!(st, FeatureStatus::Error(ref e) if e.contains("hollows the whole body") && e.contains("pattern the body")), "{st:?}");
    // mirroring the (shelled) body itself works
    let m = doc.add_feature("Mirror body", FeatureKind::Mirror { source: PatternSource::Body(p), plane: Plane::YZ.offset(20.0) });
    doc.edit("drop ring", |d| {
        d.feature_mut(bad).unwrap().suppressed = true;
        Ok::<_, ()>(())
    })
    .unwrap();
    let out = doc.regenerate();
    assert_eq!(out.report(m).unwrap().status, FeatureStatus::Ok, "{:?}", out.report(m));
    let bb = out.body_by_id(p).unwrap().shape().bbox().unwrap();
    assert!((bb.max.x - 60.0).abs() < 1e-6, "{bb:?}");
}

#[test]
fn rib_to_body() {
    let mut doc = Document::new("R");
    let p = plate(&mut doc, DVec2::new(40.0, 40.0), 10.0);
    let base = vol(&mut doc, p);
    // sketched at z = 30, growing down onto the plate top (z = 10)
    let mut s = Sketch::new("Rib", Plane::XY.offset(30.0));
    s.line(DVec2::new(-10.0, 0.0), DVec2::new(10.0, 0.0));
    let s = doc.add_sketch(s);
    let rib = doc.add_feature(
        "Rib",
        FeatureKind::Rib { sketch: s, thickness: 2.0.into(), height: 1.0.into(), draft_deg: 0.0.into(), flip: true, body: Some(p), extent: RibExtent::ToBody },
    );
    let v = vol(&mut doc, p);
    assert!((v - base - 20.0 * 2.0 * 20.0).abs() < 1e-3, "{}", v - base);
    let bb = doc.regenerate().body_by_id(p).unwrap().shape().bbox().unwrap();
    assert!((bb.max.z - 30.0).abs() < 1e-6);
    // growing away from the body never meets it
    doc.edit("unflip", |d| {
        if let FeatureKind::Rib { flip, .. } = &mut d.feature_mut(rib).unwrap().kind {
            *flip = false;
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    assert!(matches!(doc.regenerate().report(rib).unwrap().status, FeatureStatus::Error(ref e) if e.contains("never meets")));
    // with draft it still lands on the body
    doc.undo();
    doc.edit("draft", |d| {
        if let FeatureKind::Rib { draft_deg, .. } = &mut d.feature_mut(rib).unwrap().kind {
            *draft_deg = 1.0.into();
        }
        Ok::<_, ()>(())
    })
    .unwrap();
    let r = doc.regenerate();
    assert_eq!(r.report(rib).unwrap().status, FeatureStatus::Ok, "{:?}", r.report(rib));
    let v2 = r.body_by_id(p).unwrap().shape().volume().unwrap();
    assert!(v2 > base && v2 < v, "drafted rib {v2} between {base} and {v}");
}

#[test]
fn xcaf_pcb_import_keeps_names_and_colors() {
    use easycad_model::occt::{Shape, StepPart};
    use easycad_model::regen::MeshQuality;
    let board = Shape::make_box(DVec3::ZERO, DVec3::new(50.0, 30.0, 1.6)).unwrap();
    let usb = Shape::make_box(DVec3::new(-1.0, 10.0, 1.6), DVec3::new(7.0, 9.0, 3.2)).unwrap();
    let chip = Shape::make_box(DVec3::new(20.0, 10.0, 1.6), DVec3::new(8.0, 8.0, 1.2)).unwrap();
    let c = Shape::compound(&[&board, &usb, &chip]).unwrap();
    let green = [0.0, 0.45, 0.1];
    let step = c
        .to_step_string_named(&[
            StepPart { name: "Board".into(), color: Some(green) },
            StepPart { name: "J1 USB-C".into(), color: Some([0.75, 0.75, 0.75]) },
            StepPart { name: "U1".into(), color: Some([0.05, 0.05, 0.05]) },
        ])
        .unwrap();
    let mut doc = Document::new("PCB");
    let (_, f) = doc.import_step("pcb", step.clone().into_bytes(), true);
    let out = doc.regenerate().clone();
    let pcb = out.body_by_id(BodyId::of(f)).unwrap();
    assert!(pcb.reference, "a locked reference body");
    let app = pcb.appearance().unwrap();
    let names: Vec<&str> = app.parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["Board", "J1 USB-C", "U1"]);
    let fc = pcb.face_colors().unwrap();
    assert_eq!(fc.len(), 18);
    assert!(fc.iter().filter(|c| (c[1] - green[1]).abs() < 0.02 && c[0] < 0.02).count() == 6, "board faces are green");
    let vc = pcb.mesh_colors(MeshQuality::Display).unwrap();
    assert_eq!(vc.len(), pcb.mesh().unwrap().positions.len());
    assert!(vc.iter().any(|c| c[0] < 0.1 && c[1] < 0.1) && vc.iter().any(|c| c[1] > 0.4 && c[0] < 0.05));

    // as solids: one named body per part; STEP export keeps names and colours
    let mut doc2 = Document::new("PCB2");
    doc2.import_step("pcb", step.into_bytes(), false);
    let out2 = doc2.regenerate().clone();
    let names2: Vec<String> = out2.bodies.iter().map(|b| b.name.clone()).collect();
    assert_eq!(names2, vec!["Board", "J1 USB-C", "U1"]);
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("out.step");
    let refs: Vec<&easycad_model::regen::Body> = out.bodies.iter().collect();
    easycad_model::export::write_step(&refs, &p).unwrap();
    let (_, back) = Shape::read_step_xcaf(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(back.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), vec!["Board", "J1 USB-C", "U1"]);
    assert!(back[0].color.is_some_and(|c| c[1] > 0.4));
}
