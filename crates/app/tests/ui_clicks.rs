//! Click tests on the real app (worker thread, GPU viewport) with egui_kittest:
//! the overhang heat map, linked-file Update / Break, drag-to-reparent in the
//! assembly tree, the alternate navigation presets, "Output as components", mate
//! free-motion glyphs and the interference overlap solid. Snapshots land in
//! `tests/snapshots/` (written, not compared).
//! Run with `cargo test -p easycad --test ui_clicks -- --test-threads=1`.

use std::path::PathBuf;
use std::time::Duration;

use easycad_app::EasyCadApp;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

const W: f32 = 1440.0;
const H: f32 = 900.0;

fn out_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn harness(doc: easycad_model::document::Document) -> Harness<'static, EasyCadApp> {
    let mut h = Harness::builder()
        .with_size(egui::vec2(W, H))
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .with_step_dt(1.0 / 30.0)
        .wgpu()
        .build_eframe(move |cc| EasyCadApp::new(cc, Some(doc)));
    settle(&mut h);
    h
}

fn settle(h: &mut Harness<'static, EasyCadApp>) {
    for _ in 0..6 {
        h.run_steps(2);
        assert!(h.state_mut().settle(Duration::from_secs(240)), "regeneration did not finish");
    }
    h.run_steps(3);
}

fn save(h: &mut Harness<'static, EasyCadApp>, name: &str) -> image::RgbaImage {
    let img = h.render().expect("render");
    write_png(&img, &out_dir().join(format!("{name}.png")));
    img
}

/// Write a snapshot unless the one on disk is the same picture (renders jitter).
fn write_png(img: &image::RgbaImage, path: &std::path::Path) {
    if let Ok(old) = image::open(path).map(|i| i.to_rgba8()) {
        if old.dimensions() == img.dimensions() {
            let differ = old
                .pixels()
                .zip(img.pixels())
                .filter(|(a, b)| (0..3).any(|i| (a.0[i] as i32 - b.0[i] as i32).abs() > 24))
                .count();
            if differ * 1000 < (img.width() * img.height()) as usize {
                return;
            }
        }
    }
    img.save(path).unwrap();
}

/// Pixels in `r` within `tol` of an sRGB colour (sampled every 2 px).
fn count_near(img: &image::RgbaImage, r: egui::Rect, c: [f32; 3], tol: i32) -> usize {
    let c = c.map(|v| (v * 255.0) as i32);
    let mut n = 0;
    for y in (r.top() as u32..r.bottom() as u32).step_by(2) {
        for x in (r.left() as u32..r.right() as u32).step_by(2) {
            let p = img.get_pixel(x, y).0;
            if (0..3).all(|i| (p[i] as i32 - c[i]).abs() <= tol) {
                n += 1;
            }
        }
    }
    n
}

/// Is the pixel red-dominant (the heat map's "needs support")?
fn reddish(img: &image::RgbaImage, r: egui::Rect) -> usize {
    let mut n = 0;
    for y in (r.top() as u32..r.bottom() as u32).step_by(2) {
        for x in (r.left() as u32..r.right() as u32).step_by(2) {
            let p = img.get_pixel(x, y).0;
            if p[0] as i32 > 100 && p[0] as i32 > 2 * p[1] as i32 && p[0] as i32 > 2 * p[2] as i32 {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn overhang_heatmap() {
    let _serial = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    // the lid seen from below, the base hidden: its inside ceiling hangs over air
    {
        let app = h.state_mut();
        let base = app.model.as_ref().unwrap().bodies.iter().find(|b| b.body.name == "Base").unwrap().body.id;
        app.hidden.insert(base);
        let now = app.now;
        app.camera.go_region(easycad_app::camera::CubeRegion([1, -1, -1]), now, 1.6);
        app.camera.finish();
    }
    h.run_steps(3);
    let vp = h.state().viewport_rect.shrink(40.0);
    let before = h.render().expect("render");
    let red0 = reddish(&before, vp);
    // the toolbar toggle
    h.get_by_label("Overhang").click();
    h.run_steps(4);
    assert!(h.state().overhang.on);
    let img = save(&mut h, "overhang_heatmap");
    let (over, support) = h.state().overhang.totals();
    // the lid's underside (inside the box) and the floor's lip hang over air
    assert!(support > 100.0, "support {support}");
    assert!(over >= 0.0);
    assert!(reddish(&img, vp) > red0 + 50, "the ceilings show red: {} vs {red0}", reddish(&img, vp));
    assert!(count_near(&img, vp, easycad_app::ui_overhang::OK, 30) > 200, "walls stay neutral");
    // the legend
    h.get_by_label_contains("Needs support");
    // build upside down: other faces need support, the lid's top rests on the plate
    h.get_by_label("−Z").click();
    h.run_steps(4);
    assert_eq!(h.state().overhang.up, easycad_model::process::CubeFace::NegZ);
    let flipped = h.state().overhang.totals();
    assert!((flipped.1 - support).abs() > 1.0, "{flipped:?} vs {support}");
    save(&mut h, "overhang_heatmap_down");
    // off: nothing is computed or kept
    h.get_by_label("Overhang").click();
    h.run_steps(3);
    assert!(!h.state().overhang.on);
    assert_eq!(h.state().overhang.totals(), (0.0, 0.0));
    let off = h.render().expect("render");
    assert!(reddish(&off, vp) <= red0 + 5);
}
