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
