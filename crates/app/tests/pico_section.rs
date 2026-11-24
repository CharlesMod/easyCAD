//! The real Pico W case STEP (kept outside the repo, `EASYCAD_PICO_STEP=<file>`):
//! with a section plane through the case, every body the plane crosses comes out of
//! the worker's cut either as a proper cut or flagged as clipped at the plane, and
//! no kept mesh pokes through the plane.

use easycad_app::worker::{clip_mesh, cut_body_ex, pokes_through};
use easycad_model::assembly::ComponentId;
use easycad_model::document::Document;
use easycad_model::regen::MeshQuality;
use glam::DVec3;

#[test]
fn nothing_pokes_through_the_plane_on_the_pico_case() {
    let Ok(p) = std::env::var("EASYCAD_PICO_STEP") else { return };
    let mut doc = Document::new("pico");
    doc.import_step_assembly("pico", std::fs::read(p).unwrap(), ComponentId::ROOT, false).unwrap();
    let ar = doc.regenerate_assembly().clone();
    let boxes: Vec<_> = ar.instances.iter().map(|i| i.body.bbox().ok().map(|b| (i, b))).collect();
    let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
    for (i, b) in boxes.iter().flatten() {
        for c in 0..8 {
            let q = i.transform.affine().transform_point3(DVec3::new(
                if c & 1 == 0 { b.min.x } else { b.max.x },
                if c & 2 == 0 { b.min.y } else { b.max.y },
                if c & 4 == 0 { b.min.z } else { b.max.z },
            ));
            lo = lo.min(q);
            hi = hi.max(q);
        }
    }
    let centre = (lo + hi) / 2.0;
    for normal in [DVec3::X, DVec3::Y, DVec3::Z, DVec3::new(1.0, 1.0, 0.3).normalize()] {
        let (mut crossed, mut clipped) = (0, 0);
        for (i, _) in boxes.iter().flatten() {
            let inv = i.transform.inverse().affine();
            let (o, n) = (inv.transform_point3(centre), inv.transform_vector3(normal));
            let Some((mb, _cap, was_clipped)) = cut_body_ex(&i.body, o, n, MeshQuality::Display) else { continue };
            crossed += 1;
            clipped += was_clipped as usize;
            if was_clipped { let m = i.body.mesh_quality(MeshQuality::Display).unwrap(); let ds: Vec<f64> = m.positions.iter().map(|p| (DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64) - o).dot(n)).collect(); println!("   range {:?} .. {:?} solids {}", ds.iter().cloned().fold(f64::MAX, f64::min), ds.iter().cloned().fold(f64::MIN, f64::max), i.body.shape().count(easycad_model::occt::ShapeKind::Solid)); println!("   clipped {normal:?} {} -> {:?}", i.body.name, i.body.section_view(o, n, MeshQuality::Display).err()); }
            if let Some(m) = &mb.mesh {
                assert!(!pokes_through(m, o, n), "{} pokes through the plane {normal:?}", i.body.name);
            }
        }
        println!("{normal:?}: {crossed} bodies cut, {clipped} clipped at the plane");
    }
}

/// The render-time fallback: a triangle straddling the plane keeps only its back part.
#[test]
fn clipping_a_mesh_keeps_the_back_side_only() {
    use easycad_model::occt::Mesh;
    let m = Mesh {
        positions: vec![[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        indices: vec![[0, 1, 2]],
        tri_face: vec![7],
        edges: vec![],
    };
    let (o, n) = (DVec3::ZERO, DVec3::X);
    let c = clip_mesh(&m, o, n).expect("part of it is behind the plane");
    assert!(!pokes_through(&c, o, n));
    assert!(c.positions.iter().any(|p| p[0] < -0.5), "the back part is still there");
    assert!(c.tri_face.iter().all(|&f| f == 7));
    assert!(clip_mesh(&m, DVec3::new(-5.0, 0.0, 0.0), n).is_none(), "wholly in front: nothing kept");
    assert!(pokes_through(&m, o, n), "the uncut triangle does poke through");
}
