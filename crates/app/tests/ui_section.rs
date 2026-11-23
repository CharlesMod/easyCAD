//! The floating Section checkbox by the view cube's home button, at 480, 800
//! and 1440 px wide: it cuts the model in half (caps drawn), the card under the
//! cube picks the axis, drags or nudges the cut and flips the kept half, and
//! neither the checkbox nor the card covers the cube or its rotate buttons.
//! Snapshots land in `tests/snapshots/` (written, not compared).
//! Run with `cargo test -p easycad --test ui_section`.

use std::path::PathBuf;
use std::time::Duration;

use easycad_app::EasyCadApp;
use egui::{pos2, vec2, Rect};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

const H: f32 = 900.0;

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

    // the card: under the cube's rotate grid, inside the view
    let flip = h.get_by_label("Flip").rect();
    let x = h.get_by_label("X").rect();
    for r in [flip, x] {
        assert!(!r.intersects(cube), "{tag}: card control {r:?} over the cube {cube:?}");
        assert!(r.top() > cube.bottom(), "{tag}: card below the cube: {r:?}");
        assert!(r.left() >= vp.left() && r.right() <= vp.right(), "{tag}: {r:?} outside {vp:?}");
    }

    // axis, nudge, flip each recut the model
    let s0 = h.state().section.unwrap();
    let other = ["X", "Y", "Z"][(s0.axis + 1) % 3];
    h.get_by_label(other).click();
    settle(&mut h);
    let s1 = h.state().section.unwrap();
    assert_eq!(s1.axis, (s0.axis + 1) % 3, "{tag}: axis");
    assert!(!h.state().model.as_ref().unwrap().caps.is_empty());
    // the card's "+" (Parameters has its own "+ Add" row buttons)
    let plus = h
        .query_all_by_label("+")
        .find(|n| n.rect().top() > cube.bottom() && n.rect().center().y < flip.bottom() + 60.0)
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
    run(480.0, true);
    run(480.0, false);
}

#[test]
fn section_checkbox_at_800() {
    run(800.0, true);
}

#[test]
fn section_checkbox_at_1440() {
    run(1440.0, true);
}
