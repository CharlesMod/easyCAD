//! Edge picks outrank the faces they border (fillet / chamfer dialogs pick both).

use easycad_model::feature::{BodyId, FeatureId};
use easycad_model::occt::Shape;
use easycad_model::pick::{EdgeTolerance, HitKind, Picker};
use glam::{DVec3, Vec3};

fn body() -> BodyId {
    BodyId::of(FeatureId(1))
}

#[test]
fn box_edge_beats_the_faces_it_borders() {
    let s = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 10.0)).unwrap();
    let mesh = s.tessellate(0.1, 10.0).unwrap();
    let p = Picker::new([(body(), &mesh)]);
    let edges = s.edges().unwrap();
    // top edge along x at y = 0, z = 10
    let top = edges
        .iter()
        .find(|e| (e.midpoint - DVec3::new(20.0, 0.0, 10.0)).length() < 1e-6)
        .unwrap()
        .index;
    let tol = EdgeTolerance::ortho(0.3);
    // straight down, 0.2 inside the top face
    let hits = p.pick_all(Vec3::new(20.0, 0.2, 50.0), Vec3::NEG_Z, tol);
    assert_eq!(hits[0].kind, HitKind::Edge(top), "{hits:?}");
    // from the side: 0.1 outside the edge, ray tilted so it runs into the front wall
    let dir = Vec3::new(0.0, 0.5, -1.0);
    let o = Vec3::new(20.0, -0.1, 10.0) - dir * 10.0;
    let hits = p.pick_all(o, dir, tol);
    assert_eq!(hits[0].kind, HitKind::Edge(top), "{hits:?}");
    // outside tolerance the face wins
    let hits = p.pick_all(Vec3::new(20.0, 0.6, 50.0), Vec3::NEG_Z, tol);
    assert!(matches!(hits[0].kind, HitKind::Face(_)), "{hits:?}");
}

#[test]
fn back_edge_never_beats_the_front_face() {
    let s = Shape::make_box(DVec3::ZERO, DVec3::new(40.0, 30.0, 10.0)).unwrap();
    let mesh = s.tessellate(0.1, 10.0).unwrap();
    let p = Picker::new([(body(), &mesh)]);
    // a ray tilted so it passes 0.24 from the bottom edge (y = 0, z = 0) but enters
    // through the front wall at z = 8: the wall borders that edge, yet the
    // edge is ~8 deeper, so the wall must stay first
    let dir = Vec3::new(0.0, 0.03, -1.0).normalize();
    let o = Vec3::new(20.0, 0.0, 8.0) - dir * 40.0;
    let bottom = s.edges().unwrap().iter().find(|e| (e.midpoint - DVec3::new(20.0, 0.0, 0.0)).length() < 1e-6).unwrap().index;
    let wall = |hits: &[easycad_model::pick::Hit]| hits.iter().position(|h| matches!(h.kind, HitKind::Face(f) if f == 2)).unwrap();
    let edge = |hits: &[easycad_model::pick::Hit]| hits.iter().position(|h| h.kind == HitKind::Edge(bottom)).expect("bottom edge listed");
    let hits = p.pick_all(o, dir, EdgeTolerance::ortho(0.3));
    assert!(edge(&hits) > wall(&hits), "{hits:?}");
    let hits = p.pick_all(o, dir, EdgeTolerance::perspective(5.0, 0.8, 800.0));
    assert!(edge(&hits) > wall(&hits), "{hits:?}");
    // straight down a face-interior point: back (bottom) edges only after the top face
    let hits = p.pick_all(
        Vec3::new(20.0, 0.1, 50.0),
        Vec3::NEG_Z,
        EdgeTolerance::ortho(0.3),
    );
    let bottom = hits
        .iter()
        .position(|h| matches!(h.kind, HitKind::Edge(_)) && h.t > 45.0)
        .unwrap();
    assert!(
        hits[..bottom]
            .iter()
            .any(|h| matches!(h.kind, HitKind::Face(_))),
        "{hits:?}"
    );
}

/// A boss on a plate with a big root fillet, meshed coarsely: the tessellated
/// fillet face sits in front of the exact tangent edges.
#[test]
fn concave_root_edge_beats_the_coarse_fillet_face() {
    let plate = Shape::make_box(DVec3::ZERO, DVec3::new(60.0, 60.0, 5.0)).unwrap();
    let boss = Shape::make_box(DVec3::new(20.0, 20.0, 5.0), DVec3::new(20.0, 20.0, 20.0)).unwrap();
    let fused = plate.fuse(&boss).unwrap();
    let edges = fused.edges().unwrap();
    // the root: straight edges at z = 5 along the boss (x = 20..40, y = 20)
    let root = edges
        .iter()
        .find(|e| (e.midpoint - DVec3::new(30.0, 20.0, 5.0)).length() < 1e-6)
        .unwrap()
        .index;
    let s = fused.fillet_raw(&[root], &[8.0]).unwrap();
    let mesh = s.tessellate(3.0, 60.0).unwrap();
    let p = Picker::new([(body(), &mesh)]);
    let faces = s.faces().unwrap();
    let einfo = s.edges().unwrap();
    // tangent edges of the fillet: straight, along x, bordering a cylinder face
    let tangents: Vec<_> = einfo
        .iter()
        .filter(|e| {
            e.tangent.x.abs() > 0.99 && e.faces.iter().any(|f| faces[*f as usize].radius > 7.0)
        })
        .collect();
    assert_eq!(tangents.len(), 2, "{tangents:?}");
    let tol = EdgeTolerance::ortho(0.4);
    let mut exercised = 0;
    for e in tangents {
        // look from the open side, tilted 20 / 45 / 70 degrees down, 0.2 off the edge
        // in the view plane (towards the fillet and away from it)
        for deg in [20.0f32, 45.0, 70.0] {
            let (sn, cs) = deg.to_radians().sin_cos();
            let dir = Vec3::new(0.0, cs, -sn);
            let perp = Vec3::new(0.0, sn, cs);
            for off in [perp * 0.2, -perp * 0.2] {
                let m = e.midpoint.as_vec3();
                let o = m + off - dir * 100.0;
                let hits = p.pick_all(o, dir, tol);
                let edge = hits.iter().find(|h| h.kind == HitKind::Edge(e.index));
                let Some(edge) = edge else { continue };
                // is a bordering face genuinely nearer than the exact edge?
                if hits.iter().any(|h| {
                    matches!(h.kind, HitKind::Face(f) if e.faces.contains(&f))
                        && h.t < edge.t - 1e-4
                }) {
                    exercised += 1;
                }
                assert_eq!(
                    hits[0].kind,
                    HitKind::Edge(e.index),
                    "dir {dir:?} off {off:?}: {hits:?}"
                );
            }
        }
    }
    assert!(
        exercised > 0,
        "no ray had the tessellated face in front of the edge"
    );
}
