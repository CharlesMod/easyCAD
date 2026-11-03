//! Modelled threads: validity, volume against the basic profile, mesh size.

use std::f64::consts::PI;
use std::time::Instant;

use occt::{DVec3, Shape, Thread};

fn tri_count(s: &Shape) -> usize {
    s.tessellate(0.05, 12.0).unwrap().indices.len()
}

/// Volume enclosed by a fine mesh (divergence theorem), as a cross-check.
fn mesh_volume(s: &Shape) -> f64 {
    let m = s.tessellate(0.002, 2.0).unwrap();
    let p = |i: u32| DVec3::from(m.positions[i as usize].map(f64::from));
    m.indices.iter().map(|t| p(t[0]).dot(p(t[1]).cross(p(t[2]))) / 6.0).sum()
}

#[test]
fn m3_rod_has_the_basic_profile() {
    let t = Thread::parse("M3").unwrap();
    let l = 10.0;
    let t0 = Instant::now();
    let rod = Shape::thread_rod(&t, l, 0.0).unwrap();
    let built = t0.elapsed();
    assert!(rod.is_valid());
    assert_eq!(rod.solids().unwrap().len(), 1);
    let bb = rod.bbox().unwrap();
    assert!((bb.min.z).abs() < 1e-6 && (bb.max.z - l).abs() < 1e-6, "{bb:?}");
    assert!((bb.size().x - t.major).abs() < 0.02 && (bb.size().y - t.major).abs() < 0.02, "{bb:?}");
    // The basic profile, per pitch: the core D1 plus a trapezoid (P/8 crest, 3P/4
    // root, 5H/8 high). A section through the axis cuts it exactly ...
    let p = t.pitch;
    let (r1, depth) = (t.minor() / 2.0, 5.0 * t.h() / 8.0);
    let (a, b) = (0.75 * p, p / 8.0);
    let area = 0.5 * (a + b) * depth;
    let sec = rod.section(DVec3::ZERO, DVec3::Y).unwrap().area().unwrap();
    let want_sec = 2.0 * (r1 * l + l / p * area);
    assert!((sec - want_sec).abs() / want_sec < 1e-4, "axial section {sec} vs basic profile {want_sec}");
    // ... and revolved at its centroid radius gives the volume (Pappus). The
    // kernel's Gauss integration over the long helical B-spline faces reads about
    // 2 % high, the fine mesh a little low.
    let rc = r1 + depth * (a + 2.0 * b) / (3.0 * (a + b));
    let want = PI * r1 * r1 * l + 2.0 * PI * rc * area * l / p;
    let (v, vm) = (rod.volume().unwrap(), mesh_volume(&rod));
    assert!((v - want).abs() / want < 0.03, "volume {v} vs basic profile {want}");
    assert!((vm - want).abs() / want < 0.015, "mesh volume {vm} vs basic profile {want}");
    let tris = tri_count(&rod);
    eprintln!("M3x10 thread: {:.0} ms, {v:.2} mm³, {tris} triangles at display quality", built.as_secs_f64() * 1e3);
    assert!(tris < 120_000, "{tris} triangles");
}

#[test]
fn threaded_hole_and_inch_thread() {
    // an M4 threaded hole: a block minus the rod (the hole's void is the basic
    // external thread)
    let t = Thread::parse("M4").unwrap();
    let block = Shape::make_box(DVec3::new(-5.0, -5.0, 0.0), DVec3::new(10.0, 10.0, 8.0)).unwrap();
    let rod = Shape::thread_rod(&t, 8.0, 0.0).unwrap();
    let holed = block.cut(&rod).unwrap();
    assert!(holed.is_valid());
    let dv = block.volume().unwrap() - holed.volume().unwrap();
    assert!((dv - rod.volume().unwrap()).abs() < 1e-3, "{dv}");
    // a radial clearance grows the tool
    let loose = Shape::thread_rod(&t, 8.0, 0.1).unwrap();
    assert!(loose.volume().unwrap() > rod.volume().unwrap() + 1.0);
    assert!(Shape::thread_rod(&t, 8.0, 0.5).is_err(), "offset wider than the profile");

    let t = Thread::parse("#4-40").unwrap();
    let rod = Shape::thread_rod(&t, 0.5 * 25.4, 0.0).unwrap();
    assert!(rod.is_valid());
    let bb = rod.bbox().unwrap();
    assert!((bb.size().x - 0.112 * 25.4).abs() < 0.02, "{bb:?}");
}
