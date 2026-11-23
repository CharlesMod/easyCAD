//! Mesh bodies: import STL / OBJ / GLB, combine with solid bodies in Manifold
//! (cut / intersect with clearance at several positions, join), flat-bottom split,
//! move / copy, watertight STL export. The sample is a procedural lumpy ball (an
//! organic, AI-generated-looking blob) written in each format by the test.

use easycad_model::document::{DocData, Document};
use easycad_model::export;
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::meshbody::{self, MeshSolid, RawMesh};
use easycad_model::regen::FeatureStatus;
use easycad_model::sketch::Sketch;
use glam::{DVec2, DVec3};

fn lumpy_ball(c: DVec3, r: f64) -> MeshSolid {
    meshbody::sample_blob(c, r, 4)
}

fn obj_bytes(m: &MeshSolid) -> Vec<u8> {
    let mut s = String::from("# lumpy ball\no pile\n");
    for p in &m.positions {
        s += &format!("v {} {} {}\n", p.x, p.y, p.z);
    }
    for t in &m.tris {
        s += &format!("f {} {} {}\n", t[0] + 1, t[1] + 1, t[2] + 1);
    }
    s.into_bytes()
}

/// Minimal GLB with the mesh stored Y-up (file = (x, z, -y)).
fn glb_bytes_y_up(m: &MeshSolid) -> Vec<u8> {
    let mut bin = Vec::new();
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &m.positions {
        let f = [p.x as f32, p.z as f32, -p.y as f32];
        for k in 0..3 {
            lo[k] = lo[k].min(f[k]);
            hi[k] = hi[k].max(f[k]);
            bin.extend(f[k].to_le_bytes());
        }
    }
    let pos_len = bin.len();
    for t in &m.tris {
        for i in t {
            bin.extend(i.to_le_bytes());
        }
    }
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1}}]}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":{},"type":"VEC3","min":[{},{},{}],"max":[{},{},{}]}},{{"bufferView":1,"componentType":5125,"count":{},"type":"SCALAR"}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{}}},{{"buffer":0,"byteOffset":{},"byteLength":{}}}],"buffers":[{{"byteLength":{}}}]}}"#,
        m.positions.len(), lo[0], lo[1], lo[2], hi[0], hi[1], hi[2], m.tris.len() * 3, pos_len, pos_len, bin.len() - pos_len, bin.len()
    );
    let mut j = json.into_bytes();
    while j.len() % 4 != 0 {
        j.push(b' ');
    }
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let total = 12 + 8 + j.len() + 8 + bin.len();
    let mut out = Vec::new();
    out.extend(b"glTF");
    out.extend(2u32.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((j.len() as u32).to_le_bytes());
    out.extend(0x4E4F_534Au32.to_le_bytes());
    out.extend(j);
    out.extend((bin.len() as u32).to_le_bytes());
    out.extend(0x004E_4942u32.to_le_bytes());
    out.extend(bin);
    out
}

/// A box solid as an extrude: `lo`..`hi`.
fn block(doc: &mut Document, name: &str, lo: DVec3, hi: DVec3, op: BoolOp, target: Option<BodyId>) -> FeatureId {
    let mut s = Sketch::new(name, Plane::XY.offset(lo.z));
    s.rect(DVec2::new(lo.x, lo.y), DVec2::new(hi.x, hi.y));
    let s = doc.add_sketch(s);
    doc.add_feature(
        name,
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: (hi.z - lo.z).into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op, target },
    )
}

fn ok(doc: &mut Document) {
    let errs = doc.regenerate().errors();
    assert!(errs.is_empty(), "{errs:?}");
}

fn mesh_of(doc: &mut Document, id: BodyId) -> MeshSolid {
    let r = doc.regenerate();
    let b = r.body_by_id(id).unwrap_or_else(|| panic!("no body {id:?}"));
    match b.mesh_solid() {
        Some(m) => m.clone(),
        None => MeshSolid::from_shape(b.shape()).unwrap(),
    }
}

/// Volume of `a ∩ b`.
fn overlap(a: &MeshSolid, b: &MeshSolid) -> f64 {
    meshbody::boolean(a, std::slice::from_ref(b), BoolOp::Intersect).map(|m| m.volume()).unwrap_or(0.0)
}

fn boxm(lo: DVec3, hi: DVec3) -> MeshSolid {
    MeshSolid::from_shape(&easycad_model::occt::Shape::make_box(lo, hi - lo).unwrap()).unwrap()
}

fn edit_feature(doc: &mut Document, f: FeatureId, g: impl FnOnce(&mut FeatureKind)) {
    doc.edit("edit", |d| {
        g(&mut d.feature_mut(f).unwrap().kind);
        Ok::<_, ()>(())
    })
    .unwrap();
}

const PILE_C: DVec3 = DVec3::new(0.0, 0.0, 10.0);

fn pile_doc() -> (Document, BodyId, MeshSolid) {
    let ball = lumpy_ball(PILE_C, 10.0);
    let mut doc = Document::new("Pile");
    let (_, f) = doc.import_mesh("pile.stl", meshbody::write_stl_bytes(&ball), MeshUp::Z).unwrap();
    (doc, BodyId::of(f), ball)
}

#[test]
fn stl_obj_and_y_up_glb_import_to_the_same_solid() {
    let ball = lumpy_ball(PILE_C, 10.0);
    let v = ball.volume();
    assert!(v > 3000.0, "{v}");
    let mut doc = Document::new("Formats");
    let (_, a) = doc.import_mesh("pile.stl", meshbody::write_stl_bytes(&ball), MeshUp::Z).unwrap();
    let (_, b) = doc.import_mesh("pile.obj", obj_bytes(&ball), MeshUp::Z).unwrap();
    let (_, c) = doc.import_mesh("pile.glb", glb_bytes_y_up(&ball), MeshUp::Y).unwrap();
    ok(&mut doc);
    let r = doc.regenerate().clone();
    for f in [a, b, c] {
        let body = r.body_by_id(BodyId::of(f)).unwrap();
        assert!(body.is_mesh());
        assert_eq!(body.name, "pile");
        assert!((body.volume().unwrap() - v).abs() < 1e-3 * v, "{f:?}: {} vs {v}", body.volume().unwrap());
        let bb = body.bbox().unwrap();
        let want = ball.bbox();
        assert!(bb.min.distance(want.min) < 1e-3 && bb.max.distance(want.max) < 1e-3, "{f:?}: {bb:?} vs {want:?}");
        // display mesh: one pick face, same triangle count
        assert_eq!(body.mesh().unwrap().indices.len(), ball.tris.len());
    }
    // the mesh is not a solid body: features without a target skip it
    assert!(r.solids().count() == 3);
}

#[test]
fn import_scale_up_axis_rotation_and_translation_are_parametric() {
    let (mut doc, id, ball) = pile_doc();
    doc.set_param("s", "0.5").unwrap();
    doc.set_param("dz", "3").unwrap();
    let f = id.feature;
    edit_feature(&mut doc, f, |k| {
        if let FeatureKind::ImportMesh { scale, translate, rotate_deg, .. } = k {
            *scale = "s".into();
            translate[2] = "dz".into();
            *rotate_deg = DVec3::new(0.0, 0.0, 90.0);
        }
    });
    ok(&mut doc);
    let m = mesh_of(&mut doc, id);
    assert!((m.volume() - ball.volume() / 8.0).abs() < 1e-3 * m.volume());
    let bb = m.bbox();
    // scaled about the file origin: the centre goes from z=10 to 5, then +3
    assert!(((bb.min.z + bb.max.z) / 2.0 - (ball.bbox().min.z + ball.bbox().max.z) / 4.0 - 3.0).abs() < 1e-6);
    doc.set_param("dz", "13").unwrap();
    let m2 = mesh_of(&mut doc, id);
    assert!((m2.bbox().min.z - bb.min.z - 10.0).abs() < 1e-9);
}

#[test]
fn cut_with_clearance_clears_the_tool_at_both_positions() {
    let (mut doc, pile, _) = pile_doc();
    // a static post above the pile's side (pile spans z 0..20 at rest, x ±~11): the
    // pile only reaches it when it travels up
    let post = BodyId::of(block(&mut doc, "Post", DVec3::new(6.0, -20.0, 18.0), DVec3::new(20.0, 20.0, 30.0), BoolOp::NewBody, None));
    doc.set_param("stroke", "14").unwrap();
    let cut = doc.add_feature(
        "Snip",
        FeatureKind::MeshBoolean {
            target: Some(pile),
            tools: vec![post],
            op: BoolOp::Cut,
            clearance: 0.5.into(),
            positions: vec![[0.0.into(), 0.0.into(), 0.0.into()], [0.0.into(), 0.0.into(), "stroke".into()]],
            keep_tools: true,
        },
    );
    ok(&mut doc);
    assert_eq!(doc.regenerate().report(cut).unwrap().status, FeatureStatus::Ok);
    let m = mesh_of(&mut doc, pile);
    let (open, nonman) = m.edge_defects();
    assert_eq!((open, nonman), (0, 0));
    // the post grown by the clearance has rounded edges (a true offset); a box grown
    // by 0.28 fits inside it (corner √3·0.28 < 0.5). It does not touch the pile at
    // rest or at +14 (the pile moved up = the post moved down relative to it)
    let g = 0.28;
    let grown = boxm(DVec3::new(6.0 - g, -20.0 - g, 18.0 - g), DVec3::new(20.0 + g, 20.0 + g, 30.0 + g));
    assert!(overlap(&m, &grown) < 1e-6);
    assert!(overlap(&m.translated(DVec3::new(0.0, 0.0, 14.0)), &grown) < 1e-6);
    // clearing the post at +14 too takes more than a single cut
    let single = {
        let mut d2 = pile_doc().0;
        let p2 = BodyId::of(block(&mut d2, "Post", DVec3::new(6.0, -20.0, 18.0), DVec3::new(20.0, 20.0, 30.0), BoolOp::NewBody, None));
        d2.add_feature("Snip", FeatureKind::MeshBoolean { target: Some(pile), tools: vec![p2], op: BoolOp::Cut, clearance: 0.5.into(), positions: vec![], keep_tools: true });
        mesh_of(&mut d2, pile).volume()
    };
    assert!(m.volume() < single - 1.0, "{} vs {single}", m.volume());
    // the tool body is still there and still a solid
    let r = doc.regenerate();
    assert!(!r.body_by_id(post).unwrap().is_mesh());
    // keep_tools: false removes it
    edit_feature(&mut doc, cut, |k| {
        if let FeatureKind::MeshBoolean { keep_tools, .. } = k {
            *keep_tools = false;
        }
    });
    assert!(doc.regenerate().body_by_id(post).is_none());
}

#[test]
fn intersect_trims_into_a_cavity_with_clearance() {
    let (mut doc, pile, ball) = pile_doc();
    let cavity = BodyId::of(block(&mut doc, "Cavity", DVec3::new(-8.0, -7.0, 1.0), DVec3::new(8.0, 7.0, 40.0), BoolOp::NewBody, None));
    doc.add_feature(
        "Trim",
        FeatureKind::MeshBoolean { target: Some(pile), tools: vec![cavity], op: BoolOp::Intersect, clearance: 0.4.into(), positions: vec![], keep_tools: true },
    );
    ok(&mut doc);
    let m = mesh_of(&mut doc, pile);
    let bb = m.bbox();
    assert!(bb.min.x >= -7.6 - 1e-6 && bb.max.x <= 7.6 + 1e-6, "{bb:?}");
    assert!(bb.min.y >= -6.6 - 1e-6 && bb.max.y <= 6.6 + 1e-6, "{bb:?}");
    assert!(bb.min.z >= 1.4 - 1e-6, "{bb:?}");
    assert!(m.volume() < ball.volume() && m.volume() > 0.3 * ball.volume());
}

#[test]
fn peg_join_flat_bottom_and_watertight_stl_export() {
    let (mut doc, pile, ball) = pile_doc();
    // flat bottom at z = 2: keep what is above
    doc.add_feature("Flat bottom", FeatureKind::Split { body: Some(pile), plane: Plane::XY, new_name: String::new(), offset: 2.0.into(), keep: SplitKeep::Above });
    // a peg joined from a sketch extrude straight into the mesh body
    block(&mut doc, "Peg", DVec3::new(-2.0, -2.0, -4.0), DVec3::new(2.0, 2.0, 3.0), BoolOp::Join, Some(pile));
    ok(&mut doc);
    let r = doc.regenerate().clone();
    assert_eq!(r.bodies.len(), 1, "the split kept one side and the peg joined the pile");
    let body = r.body_by_id(pile).unwrap();
    assert!(body.is_mesh());
    let bb = body.bbox().unwrap();
    assert!((bb.min.z + 4.0).abs() < 1e-6, "{bb:?}");
    let v = body.volume().unwrap();
    // export, read back, check closed and the same volume
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pile.stl");
    export::write_stl(&[body], &path).unwrap();
    let back = meshbody::read_mesh(&std::fs::read(&path).unwrap(), "stl").unwrap();
    let welded = export::weld(&easycad_model::occt::Mesh {
        positions: back.positions.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(),
        indices: back.tris.clone(),
        ..Default::default()
    });
    assert_eq!(export::open_edges(&welded), 0);
    let (m, rep) = MeshSolid::repair(&back).unwrap();
    assert_eq!(rep.filled_holes + rep.flipped_triangles, 0);
    assert!((m.volume() - v).abs() < 1e-3 * v, "{} vs {v}", m.volume());
    assert!(v < ball.volume());
    // 3MF works too; STEP refuses a mesh-only selection with a clear message
    export::write_3mf(&[body], &dir.path().join("pile.3mf")).unwrap();
    let e = export::write_step(&[body], &dir.path().join("pile.step")).unwrap_err().to_string();
    assert!(e.contains("STL or 3MF"), "{e}");
}

#[test]
fn copy_to_a_second_position_and_snip_each() {
    let (mut doc, pile, _) = pile_doc();
    let post = BodyId::of(block(&mut doc, "Post", DVec3::new(6.0, -20.0, -5.0), DVec3::new(20.0, 20.0, 8.0), BoolOp::NewBody, None));
    let copy = doc.add_feature(
        "Pile at B",
        FeatureKind::TransformBody {
            body: pile,
            translate: [0.0.into(), 0.0.into(), 14.0.into()],
            rotate_deg: DVec3::ZERO,
            pivot: DVec3::ZERO,
            scale: 1.0.into(),
            copy: true,
            new_name: "Pile B".into(),
        },
    );
    ok(&mut doc);
    let r = doc.regenerate().clone();
    let (a, b) = (r.body_by_id(pile).unwrap().bbox().unwrap(), r.body(&"Pile B".to_string()).unwrap().bbox().unwrap());
    assert!((b.min.z - a.min.z - 14.0).abs() < 1e-9);
    assert_eq!(r.body("Pile B").unwrap().id, BodyId::of(copy));
    // move (not copy) a solid body too
    doc.add_feature(
        "Move post",
        FeatureKind::TransformBody { body: post, translate: [1.0.into(), 0.0.into(), 0.0.into()], rotate_deg: DVec3::new(0.0, 0.0, 90.0), pivot: DVec3::ZERO, scale: 1.0.into(), copy: false, new_name: String::new() },
    );
    ok(&mut doc);
    let pb = doc.regenerate().body_by_id(post).unwrap().bbox().unwrap();
    assert!((pb.min.x - (-20.0 + 1.0)).abs() < 1e-6 && (pb.min.y - 6.0).abs() < 1e-6, "{pb:?}");
}

#[test]
fn solid_only_features_refuse_mesh_bodies_and_bad_meshes_are_refused() {
    let (mut doc, pile, _) = pile_doc();
    let f = doc.add_feature("Shell", FeatureKind::Shell { body: Some(pile), open_faces: vec![], thickness: 1.0.into(), outward: false });
    match &doc.regenerate().report(f).unwrap().status {
        FeatureStatus::Error(e) => assert!(e.contains("mesh body"), "{e}"),
        s => panic!("{s:?}"),
    }
    // a fin (an edge shared by three triangles) cannot be repaired
    let mut raw = RawMesh {
        positions: vec![DVec3::ZERO, DVec3::X, DVec3::Y, DVec3::Z, DVec3::new(-1.0, -1.0, 0.5)],
        tris: vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3], [0, 1, 4]],
    };
    let e = MeshSolid::repair(&raw).unwrap_err().to_string();
    assert!(e.contains("not a closed solid"), "{e}");
    let mut d2 = Document::new("Bad");
    assert!(d2.import_mesh("bad.stl", meshbody::write_stl_bytes(&MeshSolid { positions: raw.positions.clone(), tris: raw.tris.clone() }), MeshUp::Z).is_err());
    assert!(d2.data.imports.is_empty() && d2.data.features.is_empty());
    // a missing triangle is filled (with a warning on the feature)
    raw.tris.pop();
    raw.tris.pop();
    let (m, rep) = MeshSolid::repair(&raw).unwrap();
    assert_eq!(rep.filled_holes, 1);
    assert!(m.volume() > 0.1);
    assert!(meshbody::read_mesh(b"hello", "ply").is_err());
}

#[test]
fn mesh_imports_save_reload_and_old_split_json_loads() {
    let (mut doc, pile, ball) = pile_doc();
    ok(&mut doc);
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("pile.ecad");
    doc.save_to_dir(&p).unwrap();
    let mut back = Document::load_from_dir(&p).unwrap();
    let v = back.regenerate().body_by_id(pile).unwrap().volume().unwrap();
    assert!((v - ball.volume()).abs() < 1e-3 * v);
    // a Split saved before `keep` existed
    let mut j: serde_json::Value = serde_json::from_str(&doc.data.to_json().unwrap()).unwrap();
    j["features"].as_array_mut().unwrap().push(serde_json::json!({
        "id": 999, "name": "Old split", "kind": {"Split": {"body": null, "plane": serde_json::to_value(Plane::XY).unwrap(), "new_name": "Top", "offset": "1"}}
    }));
    let d = DocData::from_json(&j.to_string()).unwrap();
    assert!(matches!(d.features.last().unwrap().kind, FeatureKind::Split { keep: SplitKeep::Both, .. }));
}
