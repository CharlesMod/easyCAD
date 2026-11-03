//! Frame cost while panning the sample enclosure: the UI pass (`EasyCadApp::frame`,
//! CPU) per frame, and the full frame with the GPU paint. Printed, and the UI pass
//! is asserted to fit a 144 Hz budget. `cargo test --release -p easycad --test pan_perf -- --nocapture`

use std::time::{Duration, Instant};

use easycad_app::EasyCadApp;
use egui_kittest::Harness;
use glam::Vec2;

fn stats(name: &str, v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let avg = v.iter().sum::<f64>() / v.len() as f64;
    let p95 = v[(v.len() * 95 / 100).min(v.len() - 1)];
    eprintln!("{name}: avg {avg:.2} ms, p95 {p95:.2} ms, max {:.2} ms", v[v.len() - 1]);
    p95
}

#[test]
fn panning_fits_a_144hz_frame() {
    let doc = easycad_app::sample::sensor_node();
    let mut h: Harness<'static, EasyCadApp> = Harness::builder()
        .with_size(egui::vec2(2560.0, 1440.0))
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .wgpu()
        .build_eframe(move |cc| EasyCadApp::new(cc, Some(doc)));
    for _ in 0..6 {
        h.run_steps(2);
        assert!(h.state_mut().settle(Duration::from_secs(240)));
    }
    h.run_steps(3);
    let _ = h.render();
    let (mut ui, mut full) = (Vec::new(), Vec::new());
    for i in 0..120 {
        let d = if (i / 30) % 2 == 0 { 6.0 } else { -6.0 };
        h.state_mut().camera.pan(Vec2::new(d, d * 0.5), 1400.0);
        let t0 = Instant::now();
        h.step();
        let t1 = Instant::now();
        if i % 4 == 0 {
            let _ = h.render();
            full.push(t1.elapsed().as_secs_f64() * 1000.0);
        }
        ui.push(h.state().frame_ms as f64);
        let _ = t0;
    }
    // the overlay itself, for a look: target/tmp/perf_overlay.png
    h.state_mut().show_perf = true;
    h.run_steps(2);
    if let Ok(img) = h.render() {
        let _ = img.save(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("perf_overlay.png"));
    }
    let p95 = stats("UI pass (frame_ms)", &mut ui);
    stats("step + GPU render + readback", &mut full);
    assert!(p95 < 1000.0 / 144.0 * 0.5, "the UI pass alone eats half a 144 Hz frame: p95 {p95:.2} ms");
}
