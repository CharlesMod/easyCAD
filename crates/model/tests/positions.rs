//! Positions (animation states): named rigid poses of bodies, evaluated from
//! parameters, applied to regenerated bodies and assemblies, interpolated for the
//! snap animation, saved with the document. A small cart: a tub, a lid hinged on
//! the x axis through (y 22, z 56.5), and a pile on a carriage with a 14 mm stroke.

use easycad_model::assembly::{interference, AssemblyResult, InterferenceOptions};
use easycad_model::document::{DocData, Document};
use easycad_model::feature::*;
use easycad_model::geom::Plane;
use easycad_model::positions::{Pose, PoseMove, PoseTarget, Rigid};
use easycad_model::sketch::Sketch;
use glam::{DVec2, DVec3};

fn block(doc: &mut Document, name: &str, lo: DVec3, hi: DVec3) -> BodyId {
    let mut s = Sketch::new(name, Plane::XY.offset(lo.z));
    s.rect(DVec2::new(lo.x, lo.y), DVec2::new(hi.x, hi.y));
    let s = doc.add_sketch(s);
    BodyId::of(doc.add_feature(
        name,
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: (hi.z - lo.z).into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ))
}

const HINGE: DVec3 = DVec3::new(0.0, 22.0, 56.5);

struct Cart {
    doc: Document,
    lid: BodyId,
    pile: BodyId,
}

/// Tub walls up to z 56, the lid on top (z 56.5..58.5, hinge at the back, y 22),
/// the pile inside reaching z 50 at rest.
fn cart() -> Cart {
    let mut doc = Document::new("Cart");
    doc.set_param("stroke", "14").unwrap();
    block(&mut doc, "Tub floor", DVec3::new(-20.0, -22.0, 0.0), DVec3::new(20.0, 22.0, 2.0));
    let lid = block(&mut doc, "Lid", DVec3::new(-20.0, -22.0, 56.5), DVec3::new(20.0, 22.0, 58.5));
    let pile = block(&mut doc, "Pile", DVec3::new(-10.0, -10.0, 20.0), DVec3::new(10.0, 10.0, 50.0));
    let lid_open = |deg: &str| PoseMove { angle_deg: deg.into(), axis: DVec3::X, pivot: HINGE, ..PoseMove::new(PoseTarget::Body(lid)) };
    let pile_up = |dz: &str| PoseMove { translate: [0.0.into(), 0.0.into(), dz.into()], ..PoseMove::new(PoseTarget::Body(pile)) };
    doc.edit("positions", |d| {
        d.add_position("closed");
        let p = d.add_position("peeking");
        d.positions.iter_mut().find(|x| x.id == p).unwrap().moves = vec![pile_up("stroke / 2"), lid_open("-25")];
        let v = d.add_position("very full");
        d.positions.iter_mut().find(|x| x.id == v).unwrap().moves = vec![pile_up("stroke"), lid_open("-100")];
        let j = d.add_position("jammed");
        d.positions.iter_mut().find(|x| x.id == j).unwrap().moves = vec![pile_up("stroke")];
        Ok::<_, ()>(())
    })
    .unwrap();
    Cart { doc, lid, pile }
}

fn posed_bbox(c: &mut Cart, name: &str, id: BodyId) -> easycad_model::occt::BBox {
    let p = c.doc.data.position_named(name).unwrap().id;
    let pose = c.doc.data.pose(p).unwrap();
    let r = c.doc.regenerate().clone();
    let bodies = pose.apply(&r.bodies).unwrap();
    bodies.iter().find(|b| b.id == id).unwrap().bbox().unwrap()
}

#[test]
fn positions_move_bodies_from_parameters_and_leave_the_rest() {
    let mut c = cart();
    let (pile, lid) = (c.pile, c.lid);
    let rest = c.doc.regenerate().body_by_id(c.pile).unwrap().bbox().unwrap();
    // closed: nothing moves
    let closed = posed_bbox(&mut c, "closed", pile);
    assert!(closed.min.distance(rest.min) < 1e-9);
    // peeking: half the stroke
    assert!((posed_bbox(&mut c, "peeking", pile).min.z - (rest.min.z + 7.0)).abs() < 1e-9);
    // very full follows the parameter
    assert!((posed_bbox(&mut c, "very full", pile).max.z - 64.0).abs() < 1e-9);
    c.doc.set_param("stroke", "12").unwrap();
    assert!((posed_bbox(&mut c, "very full", pile).max.z - 62.0).abs() < 1e-9);
    // the lid turns about the hinge: the hinge edge stays, the front edge rises
    let lid_bb = posed_bbox(&mut c, "very full", lid);
    assert!(lid_bb.max.z > 58.5 + 30.0, "{lid_bb:?}");
    assert!(lid_bb.min.y > 21.9, "the lid swings back over the hinge: {lid_bb:?}");
    // the history is untouched
    let r = c.doc.regenerate();
    assert!((r.body_by_id(lid).unwrap().bbox().unwrap().max.z - 58.5).abs() < 1e-9);
}

#[test]
fn interference_in_each_position() {
    let mut c = cart();
    let r = c.doc.regenerate().clone();
    let check = |c: &Cart, name: &str| {
        let pose = c.doc.data.pose(c.doc.data.position_named(name).unwrap().id).unwrap();
        let ar = pose.apply_assembly(&AssemblyResult::flat(r.clone())).unwrap();
        let rep = interference(&c.doc.data, &ar, &InterferenceOptions { within_occurrence: true, ..Default::default() });
        rep.pairs.iter().map(|p| (p.a.label.clone(), p.b.label.clone(), p.volume)).collect::<Vec<_>>()
    };
    assert!(check(&c, "closed").is_empty());
    assert!(check(&c, "peeking").is_empty());
    assert!(check(&c, "very full").is_empty(), "{:?}", check(&c, "very full"));
    // the pile at full stroke under a shut lid collides
    let jam = check(&c, "jammed");
    assert_eq!(jam.len(), 1, "{jam:?}");
    assert!(jam[0].0.contains("Lid") || jam[0].1.contains("Lid"));
    assert!((jam[0].2 - 20.0 * 20.0 * 2.0).abs() < 1e-3, "{jam:?}");
}

#[test]
fn tween_interpolates_translation_and_angle() {
    let mut c = cart();
    let d = &c.doc.data;
    let closed = d.pose(d.position_named("closed").unwrap().id).unwrap();
    let full = d.pose(d.position_named("very full").unwrap().id).unwrap();
    let half = Pose::lerp(&closed, &full, 0.5);
    let pile = half.body(c.pile);
    assert!((pile.translate.z - 7.0).abs() < 1e-9);
    let lid = half.body(c.lid);
    assert!((lid.angle_deg + 50.0).abs() < 1e-9 && lid.pivot == HINGE);
    // the hinge line is fixed by the move at any angle
    let p = HINGE + DVec3::X * 5.0;
    assert!(lid.affine().transform_point3(p).distance(p) < 1e-9);
    assert!(Rigid::lerp(&Rigid::IDENTITY, &Rigid::IDENTITY, 0.3).is_identity());
    // moved meshes are carried over, not re-tessellated: same triangles, moved
    let r = c.doc.regenerate().clone();
    let b = r.body_by_id(c.pile).unwrap();
    let m0 = b.mesh().unwrap();
    let moved = full.apply(std::slice::from_ref(b)).unwrap().remove(0);
    let m1 = moved.mesh().unwrap();
    assert_eq!(m0.indices, m1.indices);
    assert!((m1.positions[0][2] - m0.positions[0][2] - 14.0).abs() < 1e-4);
}

#[test]
fn positions_save_undo_and_old_documents_load() {
    let c = cart();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cart.ecad");
    c.doc.save_to_dir(&path).unwrap();
    let back = Document::load_from_dir(&path).unwrap();
    assert_eq!(back.data.positions.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["closed", "peeking", "very full", "jammed"]);
    assert_eq!(back.data.positions, c.doc.data.positions);
    // a document from before positions
    let mut j: serde_json::Value = serde_json::from_str(&c.doc.data.to_json().unwrap()).unwrap();
    j.as_object_mut().unwrap().remove("positions");
    assert!(DocData::from_json(&j.to_string()).unwrap().positions.is_empty());
    // a bad expression is reported, not applied
    let mut d = c.doc.data.clone();
    let id = d.position_named("peeking").unwrap().id;
    d.positions.iter_mut().find(|p| p.id == id).unwrap().moves[0].translate[2] = "strok".into();
    let e = d.pose(id).unwrap_err();
    assert!(e.contains("peeking") && e.contains("strok"), "{e}");
}
