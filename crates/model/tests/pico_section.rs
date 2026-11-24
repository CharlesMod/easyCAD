//! Section view of a real, many-body STEP assembly kept outside the repo
//! (`EASYCAD_PICO_STEP=<file>`, the Adafruit Pico W case): every body the plane
//! crosses is either cut by the B-rep boolean or flagged for render clipping.

use easycad_model::assembly::{ComponentId, Transform};
use easycad_model::document::Document;
use easycad_model::regen::MeshQuality;
use glam::DVec3;

fn world_bbox(t: &Transform, b: &easycad_model::occt::BBox) -> (DVec3, DVec3) {
    let a = t.affine();
    let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
    for c in 0..8 {
        let p = a.transform_point3(DVec3::new(
            if c & 1 == 0 { b.min.x } else { b.max.x },
            if c & 2 == 0 { b.min.y } else { b.max.y },
            if c & 4 == 0 { b.min.z } else { b.max.z },
        ));
        lo = lo.min(p);
        hi = hi.max(p);
    }
    (lo, hi)
}

#[test]
fn every_body_the_plane_crosses_is_cut_or_flagged() {
    let Ok(p) = std::env::var("EASYCAD_PICO_STEP") else { return };
    let data = std::fs::read(p).unwrap();
    let mut doc = Document::new("pico");
    doc.import_step_assembly("pico", data, ComponentId::ROOT, false).unwrap();
    let ar = doc.regenerate_assembly().clone();
    let mut all: Option<(DVec3, DVec3)> = None;
    let mut boxes = Vec::new();
    for i in &ar.instances {
        let Ok(b) = i.body.bbox() else { continue };
        let (lo, hi) = world_bbox(&i.transform, &b);
        all = Some(all.map_or((lo, hi), |(a, z)| (a.min(lo), z.max(hi))));
        boxes.push((lo, hi));
    }
    let (lo, hi) = all.unwrap();
    let centre = (lo + hi) / 2.0;
    println!("{} instances, world bbox {lo:?} .. {hi:?}", ar.instances.len());
    let mut unresolved = Vec::new();
    for normal in [DVec3::X, DVec3::Y, DVec3::Z] {
        let (mut crossed, mut cut, mut failed) = (0, 0, 0);
        for (i, (blo, bhi)) in ar.instances.iter().zip(&boxes).map(|(i, b)| (i, *b)) {
            if blo.dot(normal).min(bhi.dot(normal)) >= centre.dot(normal) || bhi.dot(normal) <= centre.dot(normal) {
                continue;
            }
            crossed += 1;
            let inv = i.transform.inverse().affine();
            let (o, n) = (inv.transform_point3(centre), inv.transform_vector3(normal));
            match i.body.section_view(o, n, MeshQuality::Display) {
                Ok(s) if s.kept.is_some() => {
                    cut += 1;
                    if s.cap.is_none() {
                        println!("  {normal:?}: {} cut but no cap", i.body.name);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    failed += 1;
                    println!("  {normal:?}: {} FAILED: {e} (valid input: {})", i.body.name, i.body.shape().is_valid());
                    unresolved.push((normal, i.body.name.clone()));
                }
            }
        }
        println!("normal {normal:?}: {crossed} crossed, {cut} cut, {failed} failed");
    }
    assert!(unresolved.is_empty(), "bodies the boolean could not cut: {unresolved:?}");
}
