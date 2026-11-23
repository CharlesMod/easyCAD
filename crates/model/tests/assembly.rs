//! Assemblies: components, occurrences, in-context references, mates,
//! interference, BOM, export and migration of flat documents.

mod common;

use easycad_model::assembly::*;
use easycad_model::document::{DocData, Document};
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::sketch::Sketch;
use glam::{DQuat, DVec2, DVec3};

fn block(doc: &mut Document, comp: ComponentId, name: &str, size: (&str, &str, &str), at: DVec2) -> FeatureId {
    let mut s = Sketch::new(name, Plane::XY);
    let g = |e: &str| e.parse::<f64>().unwrap_or(10.0);
    s.rounded_rect(at, size.0, size.1, "0.5", (g(size.0), g(size.1), 0.5));
    let sk = doc.add_sketch_in(comp, s).unwrap();
    doc.add_feature_in(
        comp,
        name,
        FeatureKind::Extrude { sketch: sk, profiles: ProfileSel::All, distance: size.2.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    )
    .unwrap()
}

#[test]
fn old_flat_document_migrates_to_a_root_component() {
    let (mut doc, _) = common::enclosure_doc();
    let vols: Vec<f64> = doc.regenerate().bodies.iter().map(|b| b.shape().volume().unwrap()).collect();
    // what a format-1 file looks like: no assembly fields at all
    let mut v: serde_json::Value = serde_json::from_str(&doc.data.to_json().unwrap()).unwrap();
    let o = v.as_object_mut().unwrap();
    for k in ["components", "occurrences", "mates"] {
        assert!(o.remove(k).is_some(), "{k}");
    }
    o.insert("format".into(), 1.into());
    let old = serde_json::to_string_pretty(&v).unwrap();
    let data = DocData::from_json(&old).unwrap();
    assert_eq!(data.format, 2);
    assert!(data.components.is_empty() && data.occurrences.is_empty() && data.mates.is_empty());
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("Old.ecad");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("doc.json"), old).unwrap();
    let mut back = Document::load_from_dir(&p).unwrap();
    let vols2: Vec<f64> = back.regenerate().bodies.iter().map(|b| b.shape().volume().unwrap()).collect();
    assert_eq!(vols, vols2);
    // saving writes format 2, deterministically
    back.save_to_dir(&p).unwrap();
    let j1 = std::fs::read_to_string(p.join("doc.json")).unwrap();
    assert!(j1.contains("\"format\": 2"));
    back.save_to_dir(&p).unwrap();
    assert_eq!(j1, std::fs::read_to_string(p.join("doc.json")).unwrap());
    // a newer format is refused
    assert!(DocData::from_json(&j1.replace("\"format\": 2", "\"format\": 3")).is_err());
}

#[test]
fn tree_editing_and_nesting_cycles() {
    let mut doc = Document::new("Asm");
    let a = doc.add_component("A");
    let b = doc.add_component("B");
    block(&mut doc, a, "A body", ("10", "10", "5"), DVec2::ZERO);
    let oa = doc.add_occurrence(ComponentId::ROOT, a, "A:1", Transform::translation(DVec3::X * 50.0)).unwrap();
    let ob = doc.add_occurrence(a, b, "B:1", Transform::new(DQuat::from_rotation_z(1.0), DVec3::Z * 5.0)).unwrap();
    assert_eq!(doc.data.component_at(&[oa, ob]).unwrap(), b);
    let w = doc.data.world_transform(&[oa, ob]).unwrap();
    assert!((w.point(DVec3::ZERO) - DVec3::new(50.0, 0.0, 5.0)).length() < 1e-12);
    // B inside A inside B: refused
    let e = doc.add_occurrence(b, a, "loop", Transform::IDENTITY).unwrap_err();
    assert!(e.to_string().contains("nesting cycle"), "{e}");
    assert!(doc.add_occurrence(a, a, "self", Transform::IDENTITY).is_err());
    // local params shadow document params
    doc.set_param("wall", "2").unwrap();
    doc.set_component_param(a, "wall", "wall_a").unwrap_err(); // unknown ref: rejected, nothing changed
    assert!(doc.data.component(a).unwrap().params.list.is_empty());
    doc.set_component_param(a, "wall", "3").unwrap();
    let v = doc.data.component_view(a).unwrap().values().unwrap();
    assert_eq!(v["wall"], 3.0);
    assert_eq!(doc.data.values().unwrap()["wall"], 2.0);
    // undo restores the tree
    doc.remove_occurrence(ob).unwrap();
    assert!(doc.data.occurrence(ob).is_none());
    doc.undo();
    assert!(doc.data.occurrence(ob).is_some());
    // transform rows round trip
    let t = Transform::new(DQuat::from_euler(glam::EulerRot::XYZ, 0.3, -0.2, 1.1), DVec3::new(1.0, 2.0, 3.0));
    let t2 = Transform::from_rows(&t.to_rows());
    assert!((t.point(DVec3::ONE) - t2.point(DVec3::ONE)).length() < 1e-12);
}

fn screw_component(doc: &mut Document) -> ComponentId {
    use easycad_model::library::PartSource;
    let shcs = easycad_model::library::BuiltinSource.parts().into_iter().find(|p| p.id == "iso4762").unwrap();
    doc.add_part_component(&shcs, "M3x10").unwrap()
}

/// Bracket (plate + 4 screws) placed twice: 2-level nesting, one screw component
/// instanced 8 times.
pub fn bracket_assembly() -> (Document, ComponentId, ComponentId, [OccurrenceId; 2]) {
    let mut doc = Document::new("Rack");
    let screw = screw_component(&mut doc);
    let bracket = doc.add_component("Bracket");
    block(&mut doc, bracket, "Plate", ("40", "30", "4"), DVec2::ZERO);
    for (k, (x, y)) in [(-15.0, -10.0), (15.0, -10.0), (15.0, 10.0), (-15.0, 10.0)].into_iter().enumerate() {
        doc.add_occurrence(bracket, screw, &format!("Screw:{}", k + 1), Transform::translation(DVec3::new(x, y, 4.0))).unwrap();
    }
    let a = doc.add_occurrence(ComponentId::ROOT, bracket, "Bracket:1", Transform::IDENTITY).unwrap();
    let b = doc.add_occurrence(ComponentId::ROOT, bracket, "Bracket:2", Transform::new(DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2), DVec3::new(100.0, 0.0, 0.0))).unwrap();
    (doc, bracket, screw, [a, b])
}

#[test]
fn nested_components_with_instanced_screws() {
    let (mut doc, bracket, screw, [_a, b]) = bracket_assembly();
    let r = doc.regenerate_assembly().clone();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    // one regeneration per component, shared by every occurrence
    assert_eq!(r.component(screw).unwrap().regen.bodies.len(), 1);
    let screws: Vec<_> = r.instances.iter().filter(|i| i.component == screw).collect();
    assert_eq!(screws.len(), 8);
    assert!(screws.iter().all(|i| i.path.len() == 2 && i.body.reference));
    let g0 = &screws[0].body.geom;
    assert!(screws.iter().all(|i| std::sync::Arc::ptr_eq(&i.body.geom, g0)), "instances share geometry");
    assert_eq!(r.instances.iter().filter(|i| i.component == bracket).count(), 2);
    // world placement: Bracket:2 is rotated 90° about Z and moved to x = 100
    let s = r.instances.iter().find(|i| i.component == screw && i.path[0] == b && doc.data.occurrence(i.path[1]).unwrap().name == "Screw:2").unwrap();
    let c = s.world_shape().unwrap().bbox().unwrap().center();
    assert!((c.x - 110.0).abs() < 0.2 && (c.y - 15.0).abs() < 0.2, "{c:?}");
    // all 8 screw positions are distinct
    let mut cs: Vec<DVec3> = screws.iter().map(|i| i.transform.point(DVec3::ZERO)).collect();
    cs.dedup_by(|p, q| (*p - *q).length() < 1e-6);
    assert_eq!(cs.len(), 8);
}

/// Base (open box) and a lid whose plug is cut by the base in context: changing
/// `wall` shrinks the plug; editing the lid does not regenerate the base or the board.
fn base_lid_doc() -> (Document, [ComponentId; 3], [OccurrenceId; 3], FeatureId) {
    let mut doc = Document::new("Box");
    for (n, v) in [("w", "60"), ("d", "40"), ("h", "20"), ("wall", "2")] {
        doc.set_param(n, v).unwrap();
    }
    let base = doc.add_component("Base");
    let mut s = Sketch::new("Outer", Plane::XY);
    s.rounded_rect(DVec2::ZERO, "w", "d", "1", (60.0, 40.0, 1.0));
    let sk = doc.add_sketch_in(base, s).unwrap();
    let body = BodyId::of(
        doc.add_feature_in(base, "Body", FeatureKind::Extrude { sketch: sk, profiles: ProfileSel::All, distance: "h".into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None })
            .unwrap(),
    );
    let mut s = Sketch::new("Cavity", Plane::XY.offset(2.0));
    s.rounded_rect(DVec2::ZERO, "w - 2*wall", "d - 2*wall", "0.5", (56.0, 36.0, 0.5));
    let sk = doc.add_sketch_in(base, s).unwrap();
    doc.add_feature_in(base, "Cavity", FeatureKind::Extrude { sketch: sk, profiles: ProfileSel::All, distance: "h".into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Cut, target: Some(body) })
        .unwrap();

    let lid = doc.add_component("Lid");
    let mut s = Sketch::new("Plate", Plane::XY.offset(20.0));
    s.rounded_rect(DVec2::ZERO, "w", "d", "1", (60.0, 40.0, 1.0));
    let sk = doc.add_sketch_in(lid, s).unwrap();
    let plate = BodyId::of(
        doc.add_feature_in(lid, "Plate", FeatureKind::Extrude { sketch: sk, profiles: ProfileSel::All, distance: "2".into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None })
            .unwrap(),
    );
    let mut s = Sketch::new("Plug", Plane::XY.offset(15.0));
    s.rounded_rect(DVec2::ZERO, "w", "d", "1", (60.0, 40.0, 1.0));
    let sk = doc.add_sketch_in(lid, s).unwrap();
    doc.add_feature_in(lid, "Plug", FeatureKind::Extrude { sketch: sk, profiles: ProfileSel::All, distance: "5".into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::Join, target: Some(plate) })
        .unwrap();

    let board = doc.add_component("Board");
    block(&mut doc, board, "PCB", ("30", "20", "1.6"), DVec2::ZERO);

    let at = Transform::translation(DVec3::new(100.0, 0.0, 0.0));
    let ob = doc.add_occurrence(ComponentId::ROOT, base, "Base:1", at).unwrap();
    let ol = doc.add_occurrence(ComponentId::ROOT, lid, "Lid:1", at).unwrap();
    let op = doc.add_occurrence(ComponentId::ROOT, board, "Board:1", Transform::translation(DVec3::new(100.0, 0.0, 5.0))).unwrap();
    let ctx = doc
        .add_feature_in(lid, "Fit to base", FeatureKind::ContextRef { source: vec![ob], body: None, op: BoolOp::Cut, target: Some(plate), offset: 0.0.into(), reference: false, root: None })
        .unwrap();
    (doc, [base, lid, board], [ob, ol, op], ctx)
}

#[test]
fn lid_in_context_follows_the_base() {
    let (mut doc, [base, lid, board], _, ctx) = base_lid_doc();
    let r = doc.regenerate_assembly().clone();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let lid_vol = |r: &AssemblyResult| r.component(lid).unwrap().regen.bodies[0].shape().volume().unwrap();
    let v2 = lid_vol(&r);
    let plate = 60.0 * 40.0 * 2.0;
    assert!((v2 - plate - 56.0 * 36.0 * 5.0).abs() < 0.01 * v2, "{v2}");
    // thicker walls: the plug shrinks with the cavity
    doc.set_param("wall", "3").unwrap();
    let r = doc.regenerate_assembly().clone();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let v3 = lid_vol(&r);
    assert!((v2 - v3 - 5.0 * (56.0 * 36.0 - 54.0 * 34.0)).abs() < 0.02 * (v2 - v3), "{v2} -> {v3}");
    assert_eq!(r.component(board).unwrap().regen.recomputed, 0, "the board does not depend on wall");
    // editing only the lid leaves the base and the board cached
    doc.edit_component(lid, "rename", |c| c.features.iter_mut().find(|f| f.id == ctx).unwrap().name = "Fit".into()).unwrap();
    let r = doc.regenerate_assembly().clone();
    assert_eq!(r.component(base).unwrap().regen.recomputed, 0);
    assert_eq!(r.component(board).unwrap().regen.recomputed, 0);
    assert!(r.component(lid).unwrap().regen.recomputed >= 1);
    // the lid instance sits on the base in world space
    let li = r.instances.iter().find(|i| i.component == lid).unwrap();
    let bb = li.world_shape().unwrap().bbox().unwrap();
    assert!((bb.min.x - 70.0).abs() < 1e-6 && (bb.min.z - 15.0).abs() < 1e-6, "{bb:?}");
}

#[test]
fn in_context_cycle_is_reported() {
    let (mut doc, [base, _lid, _], [_, ol, _], _) = base_lid_doc();
    let f = doc
        .add_feature_in(base, "Back-ref", FeatureKind::ContextRef { source: vec![ol], body: None, op: BoolOp::NewBody, target: None, offset: 0.0.into(), reference: true, root: None })
        .unwrap();
    let r = doc.regenerate_assembly().clone();
    let cyc: Vec<_> = r.issues.iter().filter(|i| i.message.contains("in-context dependency cycle")).collect();
    assert_eq!(cyc.len(), 1, "{:?}", r.issues);
    assert!(cyc[0].message.contains("Base") && cyc[0].message.contains("Lid"), "{}", cyc[0].message);
    match &r.component(base).unwrap().regen.report(f).unwrap().status {
        easycad_model::regen::FeatureStatus::Error(e) => assert!(e.contains("cycle"), "{e}"),
        other => panic!("{other:?}"),
    }
    // breaking the cycle (suppressing the back-reference) clears it
    doc.edit_component(base, "suppress", |c| c.features.iter_mut().find(|x| x.id == f).unwrap().suppressed = true).unwrap();
    let r = doc.regenerate_assembly();
    assert!(r.issues.is_empty() && r.errors().is_empty(), "{:?}", r.errors());
}

/// Base box (grounded), a hinged lid (revolute) and a slider block on top.
fn hinge_doc() -> (Document, [OccurrenceId; 3], [MateId; 2]) {
    let mut doc = Document::new("Hinge");
    let base = doc.add_component("Base");
    block(&mut doc, base, "Box", ("60", "40", "20"), DVec2::ZERO);
    let lid = doc.add_component("Lid");
    block(&mut doc, lid, "Plate", ("60", "40", "2"), DVec2::ZERO);
    let slide = doc.add_component("Slide");
    block(&mut doc, slide, "Block", ("10", "10", "5"), DVec2::ZERO);
    let ob = doc.add_occurrence(ComponentId::ROOT, base, "Base:1", Transform::IDENTITY).unwrap();
    doc.edit_occurrence(ob, "ground", |o| o.grounded = true).unwrap();
    // start somewhere arbitrary: the solver puts them in place
    let ol = doc.add_occurrence(ComponentId::ROOT, lid, "Lid:1", Transform::new(DQuat::from_rotation_y(0.4), DVec3::new(7.0, -30.0, 80.0))).unwrap();
    let os = doc.add_occurrence(ComponentId::ROOT, slide, "Slide:1", Transform::translation(DVec3::new(-50.0, 50.0, 3.0))).unwrap();
    // hinge axis: the base's top back edge (y = 20, z = 20) and the lid's bottom back edge
    let hinge = doc.add_mate(
        "Hinge",
        MateKind::Revolute,
        MateConnector::new(vec![ob], DVec3::new(0.0, 20.0, 20.0), DVec3::X),
        MateConnector::new(vec![ol], DVec3::new(0.0, 20.0, 0.0), DVec3::X),
        false,
    );
    // slider along X on the base top
    let slider = doc.add_mate(
        "Rail",
        MateKind::Slider,
        MateConnector::new(vec![ob], DVec3::new(-20.0, 0.0, 20.0), DVec3::X),
        MateConnector::point(vec![os], DVec3::ZERO).clone_with_z(DVec3::X),
        false,
    );
    (doc, [ob, ol, os], [hinge, slider])
}

trait WithZ {
    fn clone_with_z(&self, z: DVec3) -> Self;
}
impl WithZ for MateConnector {
    fn clone_with_z(&self, z: DVec3) -> Self {
        MateConnector::new(self.occurrence.clone(), self.origin, z)
    }
}

fn world_bbox(doc: &mut Document, occ: OccurrenceId) -> easycad_model::occt::BBox {
    let r = doc.regenerate_assembly();
    assert!(r.errors().is_empty(), "{:?} {:?}", r.errors(), r.instances.iter().map(|i| i.path.clone()).collect::<Vec<_>>());
    r.instances_of(&[occ]).next().unwrap().world_shape().unwrap().bbox().unwrap()
}

#[test]
fn revolute_hinge_and_slider() {
    let (mut doc, [_ob, ol, os], [hinge, slider]) = hinge_doc();
    let rep = doc.solve_mates();
    assert!(rep.ok(), "{rep:?}");
    assert_eq!(rep.moved.len(), 2);
    assert_eq!(doc.undo_label(), Some("Solve mates"));
    // closed: the lid lies on the base top
    let bb = world_bbox(&mut doc, ol);
    assert!((bb.min.z - 20.0).abs() < 1e-6 && (bb.max.z - 22.0).abs() < 1e-6, "{bb:?}");
    assert!((bb.min.x + 30.0).abs() < 1e-6 && (bb.min.y + 20.0).abs() < 1e-6, "{bb:?}");
    // open 90°: the lid stands up behind the hinge
    doc.edit_mate(hinge, "open", |m| m.angle = (-90.0).into()).unwrap();
    assert!(doc.solve_mates().ok());
    let bb = world_bbox(&mut doc, ol);
    assert!((bb.size().z - 40.0).abs() < 1e-6 && (bb.size().y - 2.0).abs() < 1e-6, "{bb:?}");
    assert!((bb.min.z - 20.0).abs() < 1e-6, "{bb:?}");
    // the hinge's free direction is reported in world coordinates
    let rep = doc.solve_mates();
    let hm = rep.mates.iter().find(|m| m.mate == hinge).unwrap();
    assert!(matches!(hm.free[..], [FreeDof::Rotate { axis, .. }] if axis.length() > 0.99), "{:?}", hm.free);
    // joint limits clamp the drive: asked for -120°, stopped at -90°
    doc.edit_mate(hinge, "limit", |m| {
        m.angle_limits = Some((-90.0, 0.0));
        m.angle = (-120.0).into();
    })
    .unwrap();
    let rep = doc.solve_mates();
    assert!(rep.mates.iter().find(|m| m.mate == hinge).unwrap().limited);
    let bb = world_bbox(&mut doc, ol);
    assert!((bb.size().z - 40.0).abs() < 1e-6, "clamped at -90°: {bb:?}");
    // the slider block runs along the rail by its drive value
    let x0 = world_bbox(&mut doc, os).min.x;
    doc.edit_mate(slider, "slide", |m| m.distance = "15".into()).unwrap();
    assert!(doc.solve_mates().ok());
    let x1 = world_bbox(&mut doc, os).min.x;
    assert!((x1 - x0 - 15.0).abs() < 1e-6, "{x0} -> {x1}");
    let bb = world_bbox(&mut doc, os);
    assert!((bb.min.z - 20.0).abs() < 1e-6 && bb.center().y.abs() < 1e-6, "{bb:?}");
    // solving again changes nothing
    assert!(doc.solve_mates().moved.is_empty());
}

#[test]
fn over_constrained_unsolved_planar_snap_and_explode() {
    let (mut doc, [ob, ol, os], _) = hinge_doc();
    assert!(doc.solve_mates().ok());
    // a rigid mate that contradicts the hinge
    let bad = doc.add_mate(
        "Bad",
        MateKind::Rigid,
        MateConnector::point(vec![ob], DVec3::new(0.0, 0.0, 30.0)),
        MateConnector::point(vec![ol], DVec3::ZERO),
        false,
    );
    let rep = doc.solve_mates();
    let st = &rep.mates.iter().find(|m| m.mate == bad).unwrap().status;
    assert!(matches!(st, MateStatus::OverConstrained(m) if m.contains("off by")), "{st:?}");
    doc.edit_mate(bad, "off", |m| m.suppressed = true).unwrap();
    assert!(doc.solve_mates().ok());
    // two loose parts mated only to each other: unsolved, reported as floating
    let loose = doc.add_component("Loose");
    block(&mut doc, loose, "L", ("5", "5", "5"), DVec2::ZERO);
    let o1 = doc.add_occurrence(ComponentId::ROOT, loose, "Loose:1", Transform::IDENTITY).unwrap();
    let o2 = doc.add_occurrence(ComponentId::ROOT, loose, "Loose:2", Transform::IDENTITY).unwrap();
    doc.add_mate("Pair", MateKind::Ball, MateConnector::point(vec![o1], DVec3::ZERO), MateConnector::point(vec![o2], DVec3::ZERO), false);
    let rep = doc.solve_mates();
    assert!(!rep.ok());
    assert!(rep.mates.iter().any(|m| matches!(&m.status, MateStatus::Unsolved(r) if r.contains("grounded"))), "{rep:?}");
    assert!(rep.floating.contains(&o1) && rep.floating.contains(&o2));
    // snap to a planar face: put Loose:1 on the base top face (faces touching)
    let base_shape = doc.regenerate_assembly().instances_of(&[ob]).next().unwrap().body.shape().clone();
    let top = base_shape.faces().unwrap().into_iter().max_by(|a, b| a.centroid.z.total_cmp(&b.centroid.z)).unwrap();
    let loose_shape = doc.regenerate_assembly().instances_of(&[o1]).next().unwrap().body.shape().clone();
    let bottom = loose_shape.faces().unwrap().into_iter().min_by(|a, b| a.centroid.z.total_cmp(&b.centroid.z)).unwrap();
    doc.add_mate("On top", MateKind::Planar, MateConnector::from_face(vec![ob], &top), MateConnector::from_face(vec![o1], &bottom), true);
    let rep = doc.solve_mates();
    assert_eq!(rep.mates.iter().find(|m| m.name == "On top").unwrap().status, MateStatus::Solved, "{rep:?}");
    assert_eq!(rep.mates.iter().find(|m| m.name == "Pair").unwrap().status, MateStatus::Solved, "now reachable through Loose:1");
    assert!(rep.ok() && rep.floating.is_empty());
    let bb = world_bbox(&mut doc, o1);
    assert!((bb.min.z - 20.0).abs() < 1e-6 && (bb.max.z - 25.0).abs() < 1e-6, "{bb:?}");
    // explode is display only
    let before = doc.regenerate_assembly().instances_of(&[os]).next().unwrap().transform;
    doc.edit_occurrence(os, "explode", |o| o.explode = DVec3::Z * 30.0).unwrap();
    let r = doc.regenerate_assembly();
    let i = r.instances_of(&[os]).next().unwrap();
    assert_eq!(i.transform, before);
    assert!((i.display.translation - before.translation - DVec3::Z * 30.0).length() < 1e-9);
}

#[test]
fn interference_detected_then_cleared() {
    let (mut doc, [ob, ol, os], [hinge, _]) = hinge_doc();
    assert!(doc.solve_mates().ok());
    // closed, the lid sits on the slider block: found
    let rep = doc.interference(&InterferenceOptions::default());
    assert_eq!(rep.pairs.len(), 1);
    assert!(rep.pairs[0].a.path == vec![ol] || rep.pairs[0].b.path == vec![ol]);
    doc.edit_mate(hinge, "open", |m| m.angle = (-90.0).into()).unwrap();
    assert!(doc.solve_mates().ok());
    let rep = doc.interference(&InterferenceOptions::default());
    assert!(rep.pairs.is_empty(), "touching is not interfering: {:?}", rep.pairs);
    assert!(rep.culled > 0);
    // sink the slider block 2 mm into the base top
    doc.edit_occurrence(os, "sink", |o| o.transform.translation.z -= 2.0).unwrap();
    let rep = doc.interference(&InterferenceOptions::default());
    assert_eq!(rep.pairs.len(), 1, "{:?}", rep.pairs);
    let p = &rep.pairs[0];
    assert!((p.volume - 10.0 * 10.0 * 2.0).abs() < 1.0, "{}", p.volume);
    // the overlap solid, in world coordinates, to draw (GUI_NEEDS #19)
    let ov = p.common.as_ref().unwrap();
    assert!((ov.0.shape().volume().unwrap() - p.volume).abs() < 1e-6);
    assert!((ov.0.shape().bbox().unwrap().size().z - 2.0).abs() < 1e-3);
    assert!(!ov.0.mesh().unwrap().indices.is_empty());
    let paths = [p.a.path.clone(), p.b.path.clone()];
    assert!(paths.contains(&vec![ob]) && paths.contains(&vec![os]));
    assert!(p.a.label.contains(':') && p.b.label.contains(':'), "{} / {}", p.a.label, p.b.label);
    // the mates put it back: cleared
    assert!(doc.solve_mates().ok());
    assert!(doc.interference(&InterferenceOptions::default()).pairs.is_empty());
    // hidden occurrences are skipped
    doc.edit_occurrence(os, "sink", |o| o.transform.translation.z -= 2.0).unwrap();
    doc.edit_occurrence(os, "hide", |o| o.visible = false).unwrap();
    assert!(doc.interference(&InterferenceOptions::default()).pairs.is_empty());
}

#[test]
fn bom_rolls_up_quantities() {
    let (mut doc, bracket, screw, _) = bracket_assembly();
    doc.edit_component(bracket, "process", |c| c.process = Some("FDM 0.4 generic".into())).unwrap();
    let bom = doc.bom_tree();
    let ind: Vec<(usize, &str, usize, usize)> = bom.indented.iter().map(|r| (r.level, r.name.as_str(), r.qty, r.total)).collect();
    assert_eq!(ind[0], (0, "Bracket", 2, 2), "{ind:?}");
    assert_eq!(ind[1].0, 1);
    assert_eq!((ind[1].2, ind[1].3), (4, 8), "{ind:?}");
    assert_eq!(bom.indented[1].component, Some(screw));
    assert_eq!(bom.indented[0].kind, BomKind::Made { process: "FDM 0.4 generic".into() });
    assert!(matches!(&bom.indented[1].kind, BomKind::Purchased { part, size, .. } if part == "iso4762" && size == "M3x10"));
    assert_eq!(bom.flat.len(), 2);
    assert_eq!((bom.flat[0].name.as_str(), bom.flat[0].total), ("Bracket", 2));
    assert_eq!(bom.flat[1].total, 8);
    // an occurrence excluded from the BOM drops out with its children
    let first = doc.data.occurrences[0].id;
    doc.edit_occurrence(first, "exclude", |o| o.in_bom = false).unwrap();
    let bom = doc.bom_tree();
    assert_eq!(bom.flat.iter().map(|r| r.total).collect::<Vec<_>>(), vec![1, 4]);
}

fn pcb_step() -> String {
    use easycad_model::occt::{Shape, StepLink, StepNode, StepTree};
    let t = |x: f64, y: f64, z: f64| [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, y, 0.0, 0.0, 1.0, z];
    let board = Shape::make_box(DVec3::ZERO, DVec3::new(50.0, 30.0, 1.6)).unwrap();
    let chip = Shape::make_box(DVec3::ZERO, DVec3::new(5.0, 5.0, 1.0)).unwrap();
    let usb = Shape::make_box(DVec3::ZERO, DVec3::new(8.0, 6.0, 3.0)).unwrap();
    StepTree {
        nodes: vec![
            StepNode {
                name: "MainBoard".into(),
                children: vec![
                    StepLink { child: 1, name: "PCB".into(), matrix: t(0.0, 0.0, 0.0) },
                    StepLink { child: 2, name: "U1".into(), matrix: t(10.0, 10.0, 1.6) },
                    StepLink { child: 2, name: "U2".into(), matrix: t(30.0, 10.0, 1.6) },
                    StepLink { child: 3, name: "J1".into(), matrix: t(0.0, 12.0, 1.6) },
                ],
                ..Default::default()
            },
            StepNode { name: "Board".into(), color: Some([0.0, 0.5, 0.1]), shape: Some(board), children: vec![] },
            StepNode { name: "QFN".into(), color: Some([0.1, 0.1, 0.1]), shape: Some(chip), children: vec![] },
            StepNode { name: "USB-C".into(), color: Some([0.8, 0.8, 0.8]), shape: Some(usb), children: vec![] },
        ],
        roots: vec![0],
    }
    .to_step_string()
    .unwrap()
}

#[test]
fn pcb_step_is_a_locked_reference_component_with_sub_parts() {
    let mut doc = Document::new("Enclosure");
    let (pcb, occ) = doc.import_step_assembly("Controller", pcb_step().into_bytes(), ComponentId::ROOT, true).unwrap();
    let c = doc.data.component(pcb).unwrap().clone();
    assert!(c.locked && c.purchased.is_some());
    assert_eq!(c.name, "Controller");
    let kids: Vec<&str> = c.children.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(kids, ["PCB", "U1", "U2", "J1"]);
    // U1 and U2 are one component instanced twice
    assert_eq!(c.children[1].component, c.children[2].component);
    let r = doc.regenerate_assembly().clone();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let parts: Vec<_> = r.instances.iter().filter(|i| i.path.first() == Some(&occ)).collect();
    assert_eq!(parts.len(), 4);
    assert!(parts.iter().all(|i| i.body.reference));
    let u2 = parts.iter().find(|i| doc.data.occurrence(*i.path.last().unwrap()).unwrap().name == "U2").unwrap();
    let bb = u2.world_shape().unwrap().bbox().unwrap();
    assert!((bb.min - DVec3::new(30.0, 10.0, 1.6)).length() < 1e-6, "{bb:?}");
    assert!(u2.body.appearance().unwrap().parts[0].color.is_some());
    // locked: no features; BOM: one purchased line, not expanded
    let e = doc.add_feature_in(pcb, "x", FeatureKind::Split { keep: Default::default(), body: None, plane: Plane::XY, new_name: "x".into(), offset: 0.0.into() }).unwrap_err();
    assert!(e.to_string().contains("locked"), "{e}");
    let bom = doc.bom_tree();
    assert_eq!(bom.indented.len(), 1);
    assert!(matches!(bom.indented[0].kind, BomKind::Purchased { ref source, .. } if source == "STEP"));
    // saved with its blob, reloads and regenerates
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("E.ecad");
    doc.save_to_dir(&p).unwrap();
    let mut back = Document::load_from_dir(&p).unwrap();
    assert_eq!(back.regenerate_assembly().instances.len(), 4);
}

#[test]
fn step_assembly_export_round_trips_tree_and_transforms() {
    let (mut doc, _, _, [_a, b]) = bracket_assembly();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("rack.step");
    doc.export_assembly(AssemblyFormat::Step, &p, &AssemblyExportOptions { include_reference: true, include_hidden: false }).unwrap();
    let data = std::fs::read_to_string(&p).unwrap();
    let tree = easycad_model::occt::StepTree::read(&data).unwrap();
    let root = &tree.nodes[tree.roots[0]];
    assert_eq!(root.name, "Rack");
    let occ: Vec<&str> = root.children.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(occ, ["Bracket:1", "Bracket:2"]);
    let br = &tree.nodes[root.children[0].child];
    assert_eq!(br.name, "Bracket");
    let names: Vec<&str> = br.children.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Plate", "Screw:1", "Screw:2", "Screw:3", "Screw:4"]);
    // re-import: same tree, same transforms
    let mut doc2 = Document::new("Back");
    let (top, _) = doc2.import_step_assembly("Rack", data.into_bytes(), ComponentId::ROOT, false).unwrap();
    let t = doc2.data.component(top).unwrap().clone();
    assert_eq!(t.children.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), ["Bracket:1", "Bracket:2"]);
    let want = doc.data.occurrence(b).unwrap().transform;
    let got = t.children[1].transform;
    assert!((want.translation - got.translation).length() < 1e-9 && want.rotation.angle_between(got.rotation) < 1e-9, "{want:?} vs {got:?}");
    let r1 = doc.regenerate_assembly().clone();
    let r2 = doc2.regenerate_assembly().clone();
    assert!(r2.errors().is_empty(), "{:?}", r2.errors());
    assert_eq!(r1.instances.len(), r2.instances.len());
    let centers = |r: &AssemblyResult| {
        let mut v: Vec<[i64; 3]> = r.instances.iter().map(|i| i.world_shape().unwrap().bbox().unwrap().center()).map(|c| [(c.x * 1e3).round() as i64, (c.y * 1e3).round() as i64, (c.z * 1e3).round() as i64]).collect();
        v.sort();
        v
    };
    assert_eq!(centers(&r1), centers(&r2));
    // non-reference re-import: solids are printable (not reference)
    assert!(r2.instances.iter().any(|i| !i.body.reference));
}

#[test]
fn three_mf_and_stl_per_component() {
    let (mut doc, [_, _lid, _], _, _) = base_lid_doc();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("box.3mf");
    doc.export_assembly(AssemblyFormat::ThreeMf, &p, &AssemblyExportOptions::default()).unwrap();
    let mut z = zip::ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut z.by_name("3D/3dmodel.model").unwrap(), &mut xml).unwrap();
    let x = roxmltree::Document::parse(&xml).unwrap();
    let objects: Vec<_> = x.descendants().filter(|n| n.has_tag_name("object")).map(|n| n.attribute("name").unwrap().to_string()).collect();
    assert_eq!(objects, ["Base", "Lid", "Board"]);
    let items: Vec<_> = x.descendants().filter(|n| n.has_tag_name("item")).collect();
    assert_eq!(items.len(), 3);
    let tr: Vec<f64> = items[0].attribute("transform").unwrap().split(' ').map(|v| v.parse().unwrap()).collect();
    assert_eq!(tr.len(), 12);
    assert!((tr[9] - 100.0).abs() < 1e-9, "translation x in m30: {tr:?}");
    // a second occurrence of the lid becomes a second build item of the same object
    let lid_c = doc.data.components.iter().find(|c| c.name == "Lid").unwrap().id;
    doc.add_occurrence(ComponentId::ROOT, lid_c, "Lid:2", Transform::translation(DVec3::new(0.0, 100.0, 0.0))).unwrap();
    doc.export_assembly(AssemblyFormat::ThreeMf, &p, &AssemblyExportOptions::default()).unwrap();
    let mut z = zip::ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut z.by_name("3D/3dmodel.model").unwrap(), &mut xml).unwrap();
    let x = roxmltree::Document::parse(&xml).unwrap();
    assert_eq!(x.descendants().filter(|n| n.has_tag_name("object")).count(), 3);
    let items: Vec<_> = x.descendants().filter(|n| n.has_tag_name("item")).map(|n| n.attribute("objectid").unwrap().to_string()).collect();
    assert_eq!(items.iter().filter(|i| *i == "2").count(), 2, "{items:?}");
    // STL per component, in each component's own frame
    let out = dir.path().join("stl");
    let files = doc.export_assembly(AssemblyFormat::StlPerComponent, &out, &AssemblyExportOptions::default()).unwrap();
    let names: Vec<String> = files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["Base.stl", "Lid.stl", "Board.stl"]);
    for f in &files {
        let b = std::fs::read(f).unwrap();
        let n = u32::from_le_bytes(b[80..84].try_into().unwrap()) as usize;
        assert!(n > 0 && b.len() == 84 + 50 * n);
    }
}

#[test]
fn parting_joint_outputs_base_and_lid_components_in_context() {
    let (mut doc, _) = common::enclosure_doc();
    let joint = doc.data.features.iter().find(|f| f.name == "Joint").unwrap().id;
    let (enc, occ) = doc.move_root_into_component("Enclosure").unwrap();
    assert!(doc.data.features.is_empty() && doc.data.component(enc).unwrap().features.len() == 5);
    let [(base, ob), (lid, ol)] = doc.split_to_components(&[occ], joint, Some(("Base", "Lid"))).unwrap();
    let r = doc.regenerate_assembly().clone();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let src = r.component(enc).unwrap().regen.clone();
    let v = |c: ComponentId, r: &AssemblyResult| r.component(c).unwrap().regen.solids().map(|b| b.shape().volume().unwrap()).sum::<f64>();
    assert!((v(base, &r) - src.body("Body").unwrap().shape().volume().unwrap()).abs() < 1e-6);
    assert!((v(lid, &r) - src.body("Lid").unwrap().shape().volume().unwrap()).abs() < 1e-6);
    // the master is hidden and out of the BOM; Base and Lid are made parts
    assert!(r.instances.iter().filter(|i| i.path == vec![occ]).all(|i| !i.visible && !i.in_bom));
    assert!(r.instances_of(&[ob]).next().unwrap().visible && r.instances_of(&[ol]).next().is_some());
    let names: Vec<String> = doc.bom_tree().flat.iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, ["Base", "Lid"]);
    // a parameter edit flows through the master into both halves
    let v_lid = v(lid, &r);
    doc.set_param("split", "18").unwrap();
    let r = doc.regenerate_assembly().clone();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    assert!(v(lid, &r) > v_lid * 1.02, "{} vs {v_lid}", v(lid, &r));
    // splitting something that is not a split is refused
    let body_f = doc.data.component(enc).unwrap().features[0].id;
    assert!(doc.split_to_components(&[occ], body_f, None).is_err());
}

#[test]
fn linked_component_status_update_and_break() {
    let dir = tempfile::tempdir().unwrap();
    let mut part = Document::new("Knob");
    part.set_param("t", "5").unwrap();
    block(&mut part, ComponentId::ROOT, "Knob", ("20", "20", "t"), DVec2::ZERO);
    let lib = dir.path().join("lib").join("Knob.ecad");
    std::fs::create_dir_all(&lib).unwrap();
    part.save_to_dir(&lib).unwrap();

    let host_dir = dir.path().join("proj");
    std::fs::create_dir_all(&host_dir).unwrap();
    let mut doc = Document::new("Panel");
    let k = doc.link_component(&lib, &host_dir).unwrap();
    assert!(matches!(&doc.data.component(k).unwrap().source, ComponentSource::Linked { path, .. } if path == "../lib/Knob.ecad"));
    doc.add_occurrence(ComponentId::ROOT, k, "Knob:1", Transform::IDENTITY).unwrap();
    let vol = |doc: &mut Document| doc.component_result(k).unwrap().bodies[0].shape().volume().unwrap();
    assert!((vol(&mut doc) - 2000.0).abs() < 5.0, "{}", vol(&mut doc));
    assert_eq!(doc.link_status(k, &host_dir), LinkStatus::UpToDate);
    // locked while linked
    assert!(doc.set_component_param(k, "t", "6").is_err());
    // the file changes: out of date until updated
    part.set_param("t", "8").unwrap();
    part.save_to_dir(&lib).unwrap();
    assert_eq!(doc.link_status(k, &host_dir), LinkStatus::OutOfDate);
    assert!((vol(&mut doc) - 2000.0).abs() < 5.0, "the cached copy is used until updated");
    doc.update_link(k, &host_dir).unwrap();
    assert_eq!(doc.link_status(k, &host_dir), LinkStatus::UpToDate);
    assert!((vol(&mut doc) - 3200.0).abs() < 8.0, "{}", vol(&mut doc));
    // the host saves and reopens without the library (embedded cache)
    let hp = host_dir.join("Panel.ecad");
    doc.save_to_dir(&hp).unwrap();
    std::fs::remove_dir_all(dir.path().join("lib")).unwrap();
    let mut back = Document::load_from_dir(&hp).unwrap();
    assert!(matches!(back.link_status(k, &host_dir), LinkStatus::Missing(_)));
    assert!((vol(&mut back) - 3200.0).abs() < 8.0);
    // break the link: editable, no longer tracked
    back.break_link(k).unwrap();
    assert_eq!(back.link_status(k, &host_dir), LinkStatus::NotLinked);
    back.set_component_param(k, "t", "2").unwrap();
    assert!((vol(&mut back) - 800.0).abs() < 3.0, "{}", vol(&mut back));
}

/// Mates on nested occurrences: a sub-assembly mated through one of its parts moves
/// rigidly; a part mated inside a fixed sub-assembly moves within it.
#[test]
fn mates_on_nested_occurrences() {
    let (mut doc, _bracket, _screw, [a, b]) = bracket_assembly();
    let table = doc.add_component("Table");
    block(&mut doc, table, "Top", ("200", "200", "10"), DVec2::ZERO);
    let ot = doc.add_occurrence(ComponentId::ROOT, table, "Table:1", Transform::translation(DVec3::new(0.0, 0.0, -50.0))).unwrap();
    doc.edit_occurrence(ot, "ground", |o| o.grounded = true).unwrap();
    let screws_of = |doc: &Document, occ: OccurrenceId| doc.data.component(doc.data.occurrence(occ).unwrap().component).unwrap().children.clone();
    let s1 = screws_of(&doc, a)[0].id;
    // Bracket:1 (ungrounded) is mated through its screw Screw:1: the whole bracket moves
    let target = DVec3::new(-60.0, 40.0, -40.0);
    doc.add_mate("Screw on table", MateKind::Rigid, MateConnector::point(vec![ot], target - DVec3::new(0.0, 0.0, -50.0)), MateConnector::point(vec![a, s1], DVec3::ZERO), false);
    let before_local = doc.data.occurrence(s1).unwrap().transform;
    let rep = doc.solve_mates();
    assert!(rep.ok(), "{rep:?}");
    assert_eq!(rep.moved, vec![a]);
    assert_eq!(doc.data.occurrence(s1).unwrap().transform, before_local, "the screw stays put inside its bracket");
    let w = doc.data.world_transform(&[a, s1]).unwrap();
    assert!((w.point(DVec3::ZERO) - target).length() < 1e-9);
    // Bracket:2 is grounded: a mate on its Screw:2 moves the screw inside the bracket
    // (which moves it in every Bracket occurrence: it is part of the definition)
    doc.edit_occurrence(b, "ground", |o| o.grounded = true).unwrap();
    let s2 = screws_of(&doc, b)[1].id;
    doc.add_mate("Screw 2 in slot", MateKind::Slider, MateConnector::new(vec![b], DVec3::new(15.0, -10.0, 4.0), DVec3::X), MateConnector::new(vec![b, s2], DVec3::ZERO, DVec3::X), false);
    let m = doc.data.mates.last().unwrap().id;
    doc.edit_mate(m, "slide", |m| m.distance = "-5".into()).unwrap();
    let rep = doc.solve_mates();
    assert!(rep.ok(), "{rep:?}");
    assert!(rep.moved.contains(&s2) && !rep.moved.contains(&b));
    let local = doc.data.occurrence(s2).unwrap().transform;
    assert!((local.translation - DVec3::new(10.0, -10.0, 4.0)).length() < 1e-9, "{local:?}");
    // Screw:1 of Bracket:1 still sits on its target (bracket moved again? no: unchanged)
    let w = doc.data.world_transform(&[a, s1]).unwrap();
    assert!((w.point(DVec3::ZERO) - target).length() < 1e-9);
}

/// Linked `.ecad` files that are assemblies themselves: the tree comes along with
/// stable ids across updates, and in-context features inside resolve relative to
/// where the linked component is placed.
#[test]
fn linked_assembly_files() {
    let dir = tempfile::tempdir().unwrap();
    let (mut rack, _, _, [_, b2]) = bracket_assembly();
    let rack_dir = dir.path().join("Rack.ecad");
    rack.save_to_dir(&rack_dir).unwrap();
    let (boxdoc, ..) = base_lid_doc();
    let box_dir = dir.path().join("Box.ecad");
    boxdoc.save_to_dir(&box_dir).unwrap();

    let mut doc = Document::new("Cabinet");
    let r = doc.link_component(&rack_dir, dir.path()).unwrap();
    let o1 = doc.add_occurrence(ComponentId::ROOT, r, "Rack:1", Transform::IDENTITY).unwrap();
    let o2 = doc.add_occurrence(ComponentId::ROOT, r, "Rack:2", Transform::translation(DVec3::Z * 100.0)).unwrap();
    let res = doc.regenerate_assembly().clone();
    assert!(res.errors().is_empty(), "{:?}", res.errors());
    assert_eq!(res.instances.len(), 2 * 2 * 5, "2 racks × 2 brackets × (plate + 4 screws)");
    assert!(doc.data.components.iter().filter(|c| c.owner == Some(r)).all(|c| c.locked));
    // a mate path into the linked tree
    let b2_here = doc.data.component(r).unwrap().children[1].id;
    let w_before = doc.data.world_transform(&[o2, b2_here]).unwrap();
    // the file changes (Bracket:2 moves): out of date, update keeps the ids
    rack.edit_occurrence(b2, "move", |o| o.transform.translation.x = 150.0).unwrap();
    rack.save_to_dir(&rack_dir).unwrap();
    assert_eq!(doc.link_status(r, dir.path()), LinkStatus::OutOfDate);
    let ids_before: Vec<u32> = doc.data.components.iter().map(|c| c.id.0).collect();
    doc.update_link(r, dir.path()).unwrap();
    let ids_after: Vec<u32> = doc.data.components.iter().map(|c| c.id.0).collect();
    assert_eq!(ids_before, ids_after);
    assert_eq!(doc.data.component(r).unwrap().children[1].id, b2_here);
    let w_after = doc.data.world_transform(&[o2, b2_here]).unwrap();
    assert!((w_after.translation.x - w_before.translation.x - 50.0).abs() < 1e-9);
    assert!(doc.regenerate_assembly().errors().is_empty());
    let _ = o1;

    // a linked file with in-context features, placed away from the origin
    let bx = doc.link_component(&box_dir, dir.path()).unwrap();
    doc.add_occurrence(ComponentId::ROOT, bx, "Box:1", Transform::new(DQuat::from_rotation_z(0.5), DVec3::new(500.0, 0.0, 0.0))).unwrap();
    let res = doc.regenerate_assembly().clone();
    assert!(res.errors().is_empty(), "{:?}", res.errors());
    let lid = doc.data.components.iter().find(|c| c.owner == Some(bx) && c.name == "Lid").unwrap().id;
    let v = res.component(lid).unwrap().regen.bodies[0].shape().volume().unwrap();
    assert!((v - 60.0 * 40.0 * 2.0 - 56.0 * 36.0 * 5.0).abs() < 0.01 * v, "{v}");
    // break the link: everything becomes editable
    doc.break_link(bx).unwrap();
    assert!(doc.data.components.iter().filter(|c| c.owner == Some(bx)).all(|c| !c.locked));
    doc.set_component_param(lid, "wall", "3").unwrap();
    assert!(doc.regenerate_assembly().errors().is_empty(), "the file's params still apply");
}

#[test]
fn in_context_frame_is_selectable() {
    let (mut doc, [base, lid, _], [ob, ol, _], _) = base_lid_doc();
    // a second lid occurrence elsewhere: noted, still built in Lid:1's frame
    let ol2 = doc.add_occurrence(ComponentId::ROOT, lid, "Lid:2", Transform::translation(DVec3::new(0.0, 300.0, 0.0))).unwrap();
    let r = doc.regenerate_assembly().clone();
    assert!(r.errors().is_empty());
    assert!(r.notes.iter().any(|n| n.contains("Lid:1")), "{:?}", r.notes);
    let v1 = r.component(lid).unwrap().regen.bodies[0].shape().volume().unwrap();
    // choose Lid:2 as the context: it is away from the base, so nothing is cut
    doc.edit_component(lid, "context", |c| c.context = Some(vec![ol2])).unwrap();
    let r = doc.regenerate_assembly().clone();
    let v2 = r.component(lid).unwrap().regen.bodies[0].shape().volume().unwrap();
    assert!(v2 > v1 + 1000.0, "{v1} {v2}");
    assert!(r.notes.iter().any(|n| n.contains("Lid:2")), "{:?}", r.notes);
    // an invalid context is an issue
    doc.edit_component(lid, "context", |c| c.context = Some(vec![ob])).unwrap();
    assert!(doc.regenerate_assembly().issues.iter().any(|i| i.message.contains("context occurrence")));
    let _ = (base, ol);
}

/// Drawings, export and BOM of the whole assembly: views of the assembly / one
/// component / one occurrence, hidden lines between parts, dimensions on placed
/// edges, item balloons and a BOM table from the assembly BOM.
#[test]
fn assembly_drawings_exports_and_bom() {
    use easycad_model::document::{ExportFormat, ExportOptions};
    use easycad_model::drawing::*;
    use easycad_model::process::CubeFace;
    let (mut doc, bracket, screw, [a, b]) = bracket_assembly();
    let r = doc.regenerate_assembly().clone();
    // purchased BOM spans the assembly: 8 screws (4 per bracket × 2)
    let bom = doc.bom();
    assert_eq!(bom.len(), 1, "{bom:?}");
    assert_eq!(bom[0].qty, 8);
    assert_eq!(doc.data.bom(&r), bom);
    let flat = doc.data.bom_tree(&r).flat;
    assert_eq!(flat.len(), 2);

    let dr = doc.add_drawing("Assembly", Sheet::new(PaperSize::A3, PaperOrientation::Landscape));
    let (top, front, comp, occ) = doc
        .edit_drawing(dr, "views", |d| {
            let top = d.add_view("Top", ViewOrientation::Face(CubeFace::PosZ), DVec2::new(120.0, 220.0), 1.0);
            let front = d.add_view("Front", ViewOrientation::Face(CubeFace::NegY), DVec2::new(120.0, 120.0), 1.0);
            let comp = d.add_view("Bracket", ViewOrientation::Face(CubeFace::PosZ), DVec2::new(300.0, 220.0), 1.0);
            let occ = d.add_view("Bracket:2", ViewOrientation::Face(CubeFace::PosZ), DVec2::new(300.0, 120.0), 1.0);
            d.view_mut(top).unwrap().scope = Scope::Assembly;
            d.view_mut(front).unwrap().scope = Scope::Assembly;
            d.view_mut(comp).unwrap().scope = Scope::Component(bracket);
            d.view_mut(occ).unwrap().scope = Scope::Occurrence(vec![b]);
            (top, front, comp, occ)
        })
        .unwrap();
    let res = doc.drawing_result(dr).unwrap();
    for v in &res.views {
        assert!(v.error.is_none(), "{:?}", v.error);
    }
    let ext = |id| {
        let v = res.view(id).unwrap();
        v.max - v.min
    };
    // bracket 1: 40 × 30 at the origin; bracket 2: turned 90° at x = 100 (30 × 40)
    let e = ext(top);
    assert!((e.x - 135.0).abs() < 0.1 && (e.y - 40.0).abs() < 0.1, "{e}");
    // a component or an occurrence is drawn in its own frame
    for id in [comp, occ] {
        let e = ext(id);
        assert!((e.x - 40.0).abs() < 0.1 && (e.y - 30.0).abs() < 0.1, "{e}");
    }
    let ov = res.view(occ).unwrap();
    assert!(ov.edges.iter().any(|e| e.path.len() == 1), "screws inside the occurrence have relative paths");

    // front view: the screw shanks inside the plates (z 0..4) are hidden by the plates
    let fv = res.view(front).unwrap();
    let zmin = r.instances.iter().map(|i| i.world_shape().unwrap().bbox().unwrap().min.z).fold(f64::INFINITY, f64::min);
    let to_model = |p: DVec2| DVec2::new(p.x - fv.min.x - 20.0, p.y - fv.min.y + zmin);
    let in_plate = |m: DVec2| m.y > 0.6 && m.y < 3.4 && [-15.0, 15.0, 90.0, 110.0].iter().any(|x| (m.x - x).abs() < 1.6);
    let pts = |vis: bool| fv.edges.iter().filter(move |e| e.visible == vis).flat_map(|e| e.points.windows(2).flat_map(|w| [w[0], (w[0] + w[1]) / 2.0, w[1]])).map(to_model);
    assert_eq!(pts(true).filter(|m| in_plate(*m)).count(), 0, "screw geometry inside a plate is drawn visible");
    assert!(pts(false).any(in_plate), "no hidden screw lines inside the plates");

    // a dimension on a placed edge (bracket 2's long straight side: 40 − 2·0.5)
    let tv = res.view(top).unwrap();
    let long = tv
        .edges
        .iter()
        .filter(|e| e.visible && e.path == vec![b] && e.points.len() == 2)
        .max_by(|p, q| p.points[0].distance(p.points[1]).total_cmp(&q.points[0].distance(q.points[1])))
        .unwrap();
    let dref = DimRef::from_view_edge(long).unwrap();
    assert!(matches!(&dref, DimRef::Placed { path, .. } if *path == vec![b]));
    let (dim, b1, b2, table) = doc
        .edit_drawing(dr, "annotate", |d| {
            let dim = d.annotate(AnnotationKind::Linear { view: top, a: dref.clone(), b: None, kind: LinearKind::Aligned, offset: 8.0 });
            let s1 = doc_child(&r, a, screw);
            let b1 = d.annotate(AnnotationKind::ItemBalloon { view: top, path: vec![a], body: None, position: DVec2::new(60.0, 260.0) });
            let b2 = d.annotate(AnnotationKind::ItemBalloon { view: top, path: s1, body: None, position: DVec2::new(80.0, 260.0) });
            let t = d.annotate(AnnotationKind::BomTable { position: DVec2::new(20.0, 80.0), indented: true });
            (dim, b1, b2, t)
        })
        .unwrap();
    let res = doc.drawing_result(dr).unwrap();
    let st = |id| res.annotation(id).unwrap();
    assert_eq!(st(dim).status, AnnotationStatus::Ok, "{:?}", st(dim).status);
    assert!((st(dim).value.unwrap() - 39.0).abs() < 0.05, "{:?}", st(dim).value);
    assert_eq!(st(b1).text, "1", "{:?}", st(b1).status);
    assert_eq!(st(b2).text, "2", "{:?}", st(b2).status);
    assert_eq!(st(table).status, AnnotationStatus::Ok);
    let texts: Vec<&str> = st(table).prims.iter().filter_map(|p| if let Prim::Text { text, .. } = p { Some(text.as_str()) } else { None }).collect();
    assert!(texts.contains(&"Bracket") && texts.iter().any(|t| t.contains("cap screw")), "{texts:?}");
    // the dimension follows bracket 2 when it moves, and goes dangling when hidden
    doc.edit_occurrence(b, "move", |o| o.transform.translation.y = 40.0).unwrap();
    let res2 = doc.drawing_result(dr).unwrap();
    assert_eq!(res2.annotation(dim).unwrap().status, AnnotationStatus::Ok);
    doc.edit_occurrence(b, "hide", |o| o.visible = false).unwrap();
    let res3 = doc.drawing_result(dr).unwrap();
    assert!(matches!(res3.annotation(dim).unwrap().status, AnnotationStatus::Dangling(_)));
    doc.undo();
    doc.undo();
    // without the assembly, an assembly view says so instead of drawing nothing silently
    let root = doc.regenerate().clone();
    let plain = compute(&doc.data, &root, &[], doc.data.drawing(dr).unwrap(), &mut DrawingCache::default());
    assert!(plain.view(top).unwrap().error.as_deref().unwrap_or("").contains("assembly"));
    let dir = tempfile::tempdir().unwrap();
    doc.export_drawing(dr, &dir.path().join("asm.svg")).unwrap();

    // Document::export covers the assembly: STEP with references = 2 plates + 8 screws
    let p = dir.path().join("asm.step");
    doc.export(ExportFormat::Step, &p, &ExportOptions { include_reference: true, ..Default::default() }).unwrap();
    let back = easycad_model::occt::Shape::read_step(&p).unwrap();
    assert_eq!(back.solids().unwrap().len(), 10);
    let bb = back.bbox().unwrap();
    assert!((bb.max.x - 115.0).abs() < 0.1, "placed in world: {bb:?}");
    // STL: printable bodies only (the screws are purchased reference bodies)
    let p = dir.path().join("asm.stl");
    doc.export(ExportFormat::Stl, &p, &ExportOptions::default()).unwrap();
    let mesh_bytes = std::fs::metadata(&p).unwrap().len();
    // one component: 1 plate + 4 screws
    let p = dir.path().join("bracket.step");
    doc.export_scoped(ExportFormat::Step, &p, &ExportOptions { include_reference: true, ..Default::default() }, &Scope::Component(bracket)).unwrap();
    assert_eq!(easycad_model::occt::Shape::read_step(&p).unwrap().solids().unwrap().len(), 5);
    let p = dir.path().join("bracket.stl");
    doc.export_scoped(ExportFormat::Stl, &p, &ExportOptions::default(), &Scope::Component(bracket)).unwrap();
    let one = std::fs::metadata(&p).unwrap().len();
    assert_eq!((mesh_bytes - 84) / 50, 2 * ((one - 84) / 50), "two plates vs one");
}

/// Path of the first screw occurrence inside bracket occurrence `a`.
fn doc_child(r: &AssemblyResult, a: OccurrenceId, screw: ComponentId) -> OccPath {
    r.instances.iter().find(|i| i.component == screw && i.path[0] == a).unwrap().path.clone()
}

/// GUI_NEEDS #17: the Base / Lid of `split_to_components` keep the frame they were
/// split in (`Component.context_frame`), so a mate moves the lid.
#[test]
fn split_outputs_move_with_their_mates() {
    let (mut doc, _) = common::enclosure_doc();
    let joint = doc.data.features.iter().find(|f| f.name == "Joint").unwrap().id;
    let (_enc, occ) = doc.move_root_into_component("Enclosure").unwrap();
    let [(base, ob), (lid, ol)] = doc.split_to_components(&[occ], joint, Some(("Base", "Lid"))).unwrap();
    assert!(doc.data.component(lid).unwrap().context_frame.is_some());
    doc.edit_occurrence(ob, "ground", |o| o.grounded = true).unwrap();
    let bb0 = world_bbox(&mut doc, ol);
    // hinge along the back top edge of the lid (y = +30, z = 30), parallel to x
    let edge = DVec3::new(0.0, 30.0, 30.0);
    let a = MateConnector::new(vec![ob], edge, DVec3::X);
    let b = MateConnector::new(vec![ol], edge, DVec3::X);
    let hinge = doc.add_mate("Hinge", MateKind::Revolute, a, b, false);
    assert!(doc.solve_mates().ok());
    let closed = world_bbox(&mut doc, ol);
    assert!((closed.min - bb0.min).length() < 1e-6 && (closed.max - bb0.max).length() < 1e-6, "closed lid stays: {closed:?} vs {bb0:?}");
    doc.edit_mate(hinge, "open", |m| m.angle = (90.0).into()).unwrap();
    assert!(doc.solve_mates().ok());
    let open = world_bbox(&mut doc, ol);
    // turned 90° about the hinge: its 60 mm depth now stands along z (or hangs)
    assert!((open.size().z - closed.size().y).abs() < 1e-3, "{open:?} vs {closed:?}");
    assert!((open.size().y - closed.size().z).abs() < 1e-3, "{open:?} vs {closed:?}");
    // the base did not move, and both still regenerate cleanly
    let r = doc.regenerate_assembly().clone();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let _ = base;
}

/// GUI_NEEDS #20: the root's rollback marker does not truncate components; each
/// component has its own.
#[test]
fn rollback_is_per_component() {
    let (mut doc, [_base, lid, _], [_, _ol, _], _) = base_lid_doc();
    let n_lid = doc.data.component(lid).unwrap().features.len();
    assert!(n_lid >= 2);
    let v_full = doc.regenerate_assembly().component(lid).unwrap().regen.bodies[0].shape().volume().unwrap();
    // a root marker (the root has no features of its own here: add one)
    block(&mut doc, ComponentId::ROOT, "Root block", ("5", "5", "5"), DVec2::new(200.0, 0.0));
    block(&mut doc, ComponentId::ROOT, "Root block 2", ("5", "5", "5"), DVec2::new(220.0, 0.0));
    doc.set_rollback(Some(1));
    let r = doc.regenerate_assembly().clone();
    let lid_r = &r.component(lid).unwrap().regen;
    assert!(lid_r.reports.iter().all(|x| !x.rolled_back), "the lid is not rolled back by the root marker");
    assert!((lid_r.bodies[0].shape().volume().unwrap() - v_full).abs() < 1e-6);
    // the lid's own marker, before its in-context cut (the last feature)
    doc.set_component_rollback(lid, Some(n_lid - 1)).unwrap();
    let r = doc.regenerate_assembly().clone();
    let lid_r = &r.component(lid).unwrap().regen;
    assert!(lid_r.reports.last().unwrap().rolled_back && !lid_r.reports[0].rolled_back);
    assert!(lid_r.bodies[0].shape().volume().unwrap() > v_full + 1.0);
    assert_eq!(doc.data.component(lid).unwrap().rollback, Some(n_lid - 1));
    doc.undo();
    assert_eq!(doc.data.component(lid).unwrap().rollback, None);
}

/// A plain multi-body part: its bodies are tested against each other only with
/// `within_occurrence` (they all share the root path).
#[test]
fn interference_between_bodies_of_one_part() {
    let mut doc = Document::new("Boxes");
    for (name, x) in [("A", 0.0), ("B", 5.0), ("C", 40.0)] {
        let mut sk = Sketch::new(name, Plane::XY);
        sk.rect(DVec2::new(x, 0.0), DVec2::new(x + 10.0, 10.0));
        let s = doc.add_sketch(sk);
        doc.add_feature(name, FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: "10".into(), symmetric: false, reverse: false, draft_deg: "0".into(), op: BoolOp::NewBody, target: None });
    }
    let r = AssemblyResult::flat(doc.regenerate().clone());
    assert_eq!(r.instances.len(), 3);
    let none = interference(&doc.data, &r, &InterferenceOptions::default());
    assert!(none.pairs.is_empty() && none.tested == 0);
    let rep = interference(&doc.data, &r, &InterferenceOptions { within_occurrence: true, ..Default::default() });
    assert_eq!(rep.pairs.len(), 1, "{:?}", rep.pairs.iter().map(|p| (&p.a.label, &p.b.label)).collect::<Vec<_>>());
    assert!((rep.pairs[0].volume - 500.0).abs() < 1e-3, "{}", rep.pairs[0].volume);
    assert_eq!((rep.tested, rep.culled), (1, 2));
}
