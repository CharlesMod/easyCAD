//! Collapsible side panes on the real app (GPU viewport) with egui_kittest: the
//! edge tab, the rail, the Ctrl/Cmd+[ ] \ chords, the slide, the viewport taking
//! the freed width without stretching the model, and the state kept across a
//! restart. Snapshots land in `tests/snapshots/` (written, not compared).
//! Run with `cargo test -p easycad --test ui_panes`.

use std::path::PathBuf;
use std::time::Duration;

use easycad_app::EasyCadApp;
use egui::{Key, Modifiers};
use egui_kittest::Harness;

const W: f32 = 1440.0;
const H: f32 = 900.0;

fn harness(config: &std::path::Path) -> Harness<'static, EasyCadApp> {
    // own settings and recovery dirs: never the user's
    std::env::set_var("XDG_CONFIG_HOME", config);
    std::env::set_var("XDG_DATA_HOME", config.join("data"));
    let mut h = Harness::builder()
        .with_size(egui::vec2(W, H))
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .with_step_dt(1.0 / 30.0)
        .wgpu()
        .build_eframe(|cc| EasyCadApp::new(cc, Some(easycad_app::sample::sensor_node())));
    for _ in 0..6 {
        h.run_steps(2);
        assert!(h.state_mut().settle(Duration::from_secs(240)), "regeneration did not finish");
    }
    h.run_steps(10);
    h
}

fn snap(h: &mut Harness<'static, EasyCadApp>, name: &str) {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    std::fs::create_dir_all(&d).unwrap();
    h.render().expect("render").save(d.join(format!("{name}.png"))).unwrap();
}

fn click_at(h: &mut Harness<'static, EasyCadApp>, p: egui::Pos2) {
    h.hover_at(p);
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
}

fn chord(h: &mut Harness<'static, EasyCadApp>, k: Key) {
    h.key_press_modifiers(Modifiers::COMMAND, k);
    h.step();
}

/// The model's bounding box on screen (px), from the camera at the viewport's size.
fn model_px(app: &EasyCadApp) -> egui::Vec2 {
    let m = app.model.as_ref().unwrap();
    let (mut lo, mut hi) = (glam::Vec2::splat(f32::MAX), glam::Vec2::splat(f32::MIN));
    let size = glam::vec2(app.viewport_size.x, app.viewport_size.y);
    for b in &m.bodies {
        let Some(mesh) = b.mesh.as_ref() else { continue };
        for p in mesh.positions.iter().step_by(7) {
            if let Some(s) = app.camera.project(glam::Vec3::from_array(*p), size) {
                lo = lo.min(s);
                hi = hi.max(s);
            }
        }
    }
    egui::vec2(hi.x - lo.x, hi.y - lo.y)
}

fn saved(config: &std::path::Path) -> serde_json::Value {
    let t = std::fs::read_to_string(config.join("easycad/settings.json")).unwrap();
    serde_json::from_str(&t).unwrap()
}

#[test]
fn side_panes_collapse_expand_and_persist() {
    let config = std::env::temp_dir().join(format!("easycad-panes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&config);
    let mut h = harness(&config);
    let full = h.state().viewport_rect;
    let px0 = model_px(h.state());
    assert!(full.left() > 200.0 && full.right() < W - 200.0, "both panes open: {full:?}");
    snap(&mut h, "panes_open");

    // the tab on the viewport's left edge closes the browser, with a slide
    // (the harness turns animations off; egui's default time back on)
    h.ctx.all_styles_mut(|s| s.animation_time = 1.0 / 12.0);
    let p = egui::pos2(full.left() + 6.0, full.center().y);
    h.hover_at(p);
    h.step();
    let mut lefts = Vec::new();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
        lefts.push(h.state().viewport_rect.left());
    }
    for _ in 0..20 {
        h.step();
        lefts.push(h.state().viewport_rect.left());
    }
    let between = lefts.iter().filter(|&&l| l < full.left() - 1.0 && l > easycad_app::panes::RAIL + 1.0).count();
    assert!(between >= 1, "slides through in-between widths: {lefts:?}");
    assert!(lefts.windows(2).all(|w| w[1] <= w[0]), "monotonic: {lefts:?}");
    eprintln!("viewport left edge per frame: {lefts:?}");
    let r = h.state().viewport_rect;
    assert!((r.left() - easycad_app::panes::RAIL).abs() < 2.0, "left on its rail: {r:?}");
    assert_eq!(r.right(), full.right());
    assert!(h.state().settings.left_pane_collapsed);
    assert_eq!(saved(&config)["left_pane_collapsed"], true);

    // Cmd/Ctrl+] closes Parameters
    chord(&mut h, Key::CloseBracket);
    h.run_steps(20);
    let r = h.state().viewport_rect;
    assert!((r.right() - (W - easycad_app::panes::RAIL)).abs() < 2.0, "right on its rail: {r:?}");
    assert_eq!(r.height(), full.height());
    // same height, same vertical field of view: the model keeps its size and shape
    let px1 = model_px(h.state());
    assert!((px1 - px0).length() < 1.5, "model on screen {px0:?} -> {px1:?}");
    snap(&mut h, "panes_closed");

    // a restart keeps them closed
    drop(h);
    let mut h = harness(&config);
    let r = h.state().viewport_rect;
    assert!(r.left() < 20.0 && r.right() > W - 20.0, "closed after restart: {r:?}");

    // the left rail opens the browser again
    click_at(&mut h, egui::pos2(7.0, r.top() + 24.0));
    h.run_steps(20);
    assert!(!h.state().settings.left_pane_collapsed);
    assert!((h.state().viewport_rect.left() - full.left()).abs() < 2.0);

    // Cmd/Ctrl+\ closes both (one was open), again opens both
    chord(&mut h, Key::Backslash);
    h.run_steps(20);
    let r = h.state().viewport_rect;
    assert!(r.left() < 20.0 && r.right() > W - 20.0, "both closed: {r:?}");
    chord(&mut h, Key::Backslash);
    h.run_steps(20);
    assert_eq!(h.state().viewport_rect, full);
    // a feature dialog folds the browser for itself only, as before
    h.key_press(Key::E);
    h.run_steps(20);
    assert!(h.state().dialog.is_some());
    assert!(h.state().viewport_rect.left() < 20.0, "browser folded for the dialog");
    assert!(!h.state().settings.left_pane_collapsed, "the user's choice is untouched");
    h.key_press(Key::Escape);
    h.run_steps(20);
    assert!(h.state().dialog.is_none());
    assert_eq!(h.state().viewport_rect, full);

    // Cmd/Ctrl+[ toggles the browser
    chord(&mut h, Key::OpenBracket);
    h.run_steps(20);
    assert!(h.state().settings.left_pane_collapsed);

    // the sketch screen's panes follow the same choice and chords
    h.key_press(Key::S);
    h.run_steps(20);
    assert!(matches!(h.state().mode, easycad_app::app::Mode::Sketch(_)));
    chord(&mut h, Key::CloseBracket);
    h.run_steps(20);
    assert!(h.state().settings.right_pane_collapsed);
    snap(&mut h, "panes_sketch_closed");
    chord(&mut h, Key::OpenBracket);
    chord(&mut h, Key::CloseBracket);
    h.run_steps(20);
    assert!(!h.state().settings.left_pane_collapsed && !h.state().settings.right_pane_collapsed);
    let _ = std::fs::remove_dir_all(&config);
}
