//! The floating Section checkbox by the view cube's home button, at 480, 800
//! and 1440 px wide: it cuts the model in half (caps drawn), the bar above the
//! view bar picks the axis, drags or nudges the cut and flips the kept half, and
//! neither the checkbox nor the bar covers the cube, its rotate buttons or the view bar.
//! Snapshots land in `tests/snapshots/` (written, not compared).
//! Run with `cargo test -p easycad --test ui_section`.

use std::path::PathBuf;
use std::time::Duration;

use easycad_app::EasyCadApp;
use egui::{pos2, vec2, Rect};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

const H: f32 = 900.0;

/// The tests in this file run one at a time: each points the app at its own
/// settings folder through XDG_CONFIG_HOME / XDG_DATA_HOME, which the whole
/// process shares, and the panes remember the side opened last process-wide.
/// Run in parallel, one test's app loaded another's settings (both panes closed).
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn config(tag: &str) -> PathBuf {
    let c = std::env::temp_dir().join(format!("easycad-section-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&c);
    c
}

fn harness(w: f32, config: &std::path::Path) -> Harness<'static, EasyCadApp> {
    std::env::set_var("XDG_CONFIG_HOME", config);
    std::env::set_var("XDG_DATA_HOME", config.join("data"));
    let mut h = Harness::builder()
        .with_size(egui::vec2(w, H))
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .with_step_dt(1.0 / 30.0)
        .wgpu()
        .build_eframe(|cc| EasyCadApp::new(cc, Some(easycad_app::sample::sensor_node())));
    settle(&mut h);
    h
}

fn settle(h: &mut Harness<'static, EasyCadApp>) {
    for _ in 0..6 {
        h.run_steps(2);
        assert!(h.state_mut().settle(Duration::from_secs(240)), "regeneration did not finish");
    }
    h.run_steps(6);
}

fn snap(h: &mut Harness<'static, EasyCadApp>, name: &str) {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    std::fs::create_dir_all(&d).unwrap();
    h.render().expect("render").save(d.join(format!("{name}.png"))).unwrap();
}

/// The cube with its home button and rotate grid, as `ui_viewport::cube` places it
/// (no dialog open: top right of the viewport).
fn cube_rect(app: &EasyCadApp) -> Rect {
    let vp = app.viewport_rect;
    let w = 88.0;
    Rect::from_min_size(pos2(vp.right() - 16.0 - w, vp.top() + 12.0), vec2(w, easycad_app::viewcube::widget_height(w)))
        .expand(2.0)
}

fn run(w: f32, panes: bool) {
    let tag = format!("{}{}", w as u32, if panes { "" } else { "_closed" });
    let cfg = config(&tag);
    if !panes {
        std::fs::create_dir_all(cfg.join("easycad")).unwrap();
        std::fs::write(cfg.join("easycad/settings.json"), r#"{"left_pane_collapsed":true,"right_pane_collapsed":true}"#).unwrap();
    }
    let mut h = harness(w, &cfg);
    let cube = cube_rect(h.state());
    let vp = h.state().viewport_rect;

    // the checkbox: left of the cube, level with its home button, inside the view
    let cb = h.get_by_label("Section").rect();
    assert!(!cb.intersects(cube), "{tag}: checkbox {cb:?} over the cube {cube:?}");
    assert!(cb.left() >= vp.left() && cb.right() <= vp.right(), "{tag}: checkbox {cb:?} outside {vp:?}");
    assert!((cb.center().y - (cube.top() + 10.0)).abs() < 14.0, "{tag}: by the home button: {cb:?}");
    assert!(h.state().section.is_none());
    // nor over the pick filters (their pills or their » menu)
    for f in ["Bodies", "Faces", "Edges", "Sketches", "More filters"] {
        for node in h.query_all_by_label(f) {
            assert!(!node.rect().intersects(cb), "{tag}: checkbox over {f}: {:?}", node.rect());
            assert!(!node.rect().intersects(cube), "{tag}: {f} over the cube");
        }
    }

    h.get_by_label("Section").click();
    settle(&mut h);
    {
        let app = h.state();
        assert!(app.section.is_some(), "{tag}: section on");
        assert!(!app.model.as_ref().unwrap().caps.is_empty(), "{tag}: cut faces capped");
    }
    snap(&mut h, &format!("section_{tag}"));

    // the bar: directly above the view bar, inside the viewport, clear of the cube
    let flip = h.get_by_label("Flip").rect();
    let x = h.get_by_label("X").rect();
    let view_bar = ["Overhang", "More view options"]
        .iter()
        .find_map(|l| h.query_by_label(l).map(|n| n.rect()))
        .expect("view bar");
    for r in [flip, x] {
        assert!(!r.intersects(cube), "{tag}: bar control {r:?} over the cube {cube:?}");
        assert!(r.bottom() <= view_bar.top(), "{tag}: bar above the view bar: {r:?} vs {view_bar:?}");
        assert!(r.left() >= vp.left() && r.right() <= vp.right(), "{tag}: {r:?} outside {vp:?}");
        assert!(r.top() > vp.bottom() - 54.0 - 90.0, "{tag}: bar sits just above the view bar: {r:?}");
    }
    assert!((flip.right() + x.left() - 2.0 * vp.center().x).abs() < 120.0 || w < 800.0, "{tag}: bar centred");

    // axis, nudge, flip each recut the model
    let s0 = h.state().section.unwrap();
    let other = ["X", "Y", "Z"][(s0.axis + 1) % 3];
    h.get_by_label(other).click();
    settle(&mut h);
    let s1 = h.state().section.unwrap();
    assert_eq!(s1.axis, (s0.axis + 1) % 3, "{tag}: axis");
    assert!(!h.state().model.as_ref().unwrap().caps.is_empty());
    // the bar's "+" (Parameters has its own "+ Add" row buttons)
    let plus = h
        .query_all_by_label("+")
        .find(|n| n.rect().bottom() <= view_bar.top() && n.rect().top() > view_bar.top() - 120.0)
        .expect("nudge button");
    plus.click();
    h.run_steps(2);
    let s2 = h.state().section.unwrap();
    assert!(s2.at > s1.at && s2.at - s1.at < 0.1, "{tag}: nudge {} -> {}", s1.at, s2.at);
    h.get_by_label("Flip").click();
    settle(&mut h);
    assert_ne!(h.state().section.unwrap().flip, s2.flip, "{tag}: flip");
    let cut = h.state().section_cut().unwrap();
    eprintln!("{tag}: cut at {:?} normal {:?}", cut.0, cut.1);
    snap(&mut h, &format!("section_{tag}_moved"));

    // off again: whole model back
    h.get_by_label("Section").click();
    settle(&mut h);
    assert!(h.state().section.is_none());
    assert!(h.state().model.as_ref().unwrap().caps.is_empty(), "{tag}: no caps when off");
    let _ = std::fs::remove_dir_all(&cfg);
}

#[test]
fn section_checkbox_at_480() {
    let _serial = serial();
    run(480.0, true);
    run(480.0, false);
}

#[test]
fn section_checkbox_at_800() {
    let _serial = serial();
    run(800.0, true);
}

#[test]
fn section_checkbox_at_1440() {
    let _serial = serial();
    run(1440.0, true);
}

/// Section turned on from the app side (the view bar's toggle, the shortcut and
/// MCP all end in `toggle_section`) shows the same bar; Y in it cuts across Y.
#[test]
fn section_bar_follows_the_toggle_and_y_cuts_across_y() {
    let _g = serial();
    let cfg = config("bar_y");
    let mut h = harness(800.0, &cfg);
    assert!(h.query_by_label("Flip").is_none(), "no bar while section is off");
    h.state_mut().toggle_section();
    settle(&mut h);
    assert!(h.query_by_label("Flip").is_some(), "bar shown with section on");
    h.get_by_label("Y").click();
    settle(&mut h);
    assert_eq!(h.state().section.unwrap().axis, 1, "Y picked");
    assert!(!h.state().model.as_ref().unwrap().caps.is_empty());
    snap(&mut h, "section_bar_y");
    h.state_mut().toggle_section();
    settle(&mut h);
    assert!(h.query_by_label("Flip").is_none(), "bar gone with section off");
    let _ = std::fs::remove_dir_all(&cfg);
}
