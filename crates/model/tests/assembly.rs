//! Assemblies: components, occurrences, in-context references, mates,
//! interference, BOM, export and migration of flat documents.

mod common;

use casework_model::assembly::*;
use casework_model::document::{DocData, Document};
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::sketch::Sketch;
use glam::{DQuat, DVec2, DVec3};

fn block(doc: &mut Document, comp: ComponentId, name: &str, size: (&str, &str, &str), at: DVec2) -> FeatureId {
    let mut s = Sketch::new(name, Plane::XY);
    s.rounded_rect(at, size.0, size.1, "0.5", (10.0, 10.0, 0.5));
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
    let p = dir.path().join("Old.cwk");
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
    use casework_model::library::PartSource;
    let shcs = casework_model::library::BuiltinSource.parts().into_iter().find(|p| p.id == "iso4762").unwrap();
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
        .add_feature_in(lid, "Fit to base", FeatureKind::ContextRef { source: vec![ob], body: None, op: BoolOp::Cut, target: Some(plate), offset: 0.0.into(), reference: false })
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
        .add_feature_in(base, "Back-ref", FeatureKind::ContextRef { source: vec![ol], body: None, op: BoolOp::NewBody, target: None, offset: 0.0.into(), reference: true })
        .unwrap();
    let r = doc.regenerate_assembly().clone();
    let cyc: Vec<_> = r.issues.iter().filter(|i| i.message.contains("in-context dependency cycle")).collect();
    assert_eq!(cyc.len(), 1, "{:?}", r.issues);
    assert!(cyc[0].message.contains("Base") && cyc[0].message.contains("Lid"), "{}", cyc[0].message);
    match &r.component(base).unwrap().regen.report(f).unwrap().status {
        casework_model::regen::FeatureStatus::Error(e) => assert!(e.contains("cycle"), "{e}"),
        other => panic!("{other:?}"),
    }
    // breaking the cycle (suppressing the back-reference) clears it
    doc.edit_component(base, "suppress", |c| c.features.iter_mut().find(|x| x.id == f).unwrap().suppressed = true).unwrap();
    let r = doc.regenerate_assembly();
    assert!(r.issues.is_empty() && r.errors().is_empty(), "{:?}", r.errors());
}
