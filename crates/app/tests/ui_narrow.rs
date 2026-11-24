//! The window at 480, 800 and 1440 px wide (480 is the minimum: half a laptop
//! screen): the toolbars fit, every tool stays reachable (a button, or a row of
//! the » overflow menu), and nothing in the bars is cut off at the window edge.
//! Snapshots land in `tests/snapshots/` (written, not compared).
//! Run with `cargo test -p easycad --test ui_narrow`.

use std::path::PathBuf;
use std::time::Duration;

use easycad_app::EasyCadApp;
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

/// Every toolbar tool, by the name its button or menu row carries.
const TOOLS: [&str; 24] = [
    "Sketch", "Extrude", "Revolve", "Fillet", "Chamfer", "Shell", "Draft", "Split", "Pattern",
    "Mirror", "Boss", "Rib", "Standoffs", "Joint", "Parts", "Component", "Insert", "Mate", "Move",
    "Interfere", "BOM", "Import", "Export", "Drawing",
];

fn config(tag: &str) -> PathBuf {
    let c = std::env::temp_dir().join(format!("easycad-narrow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&c);
    c
}

/// The bars' widgets (labelled nodes) lie inside the window.
fn inside_window(h: &Harness<'static, EasyCadApp>, w: f32, names: &[&str]) {
    for n in names {
        for node in h.query_all_by_label(n) {
            let r = node.rect();
            assert!(r.left() >= -0.5 && r.right() <= w + 0.5, "{n} cut off at {w}: {r:?}");
        }
    }
}

fn at_width(w: f32, panes_open: bool) {
    let tag = format!("{}{}", w as u32, if panes_open { "" } else { "_closed" });
    let cfg = config(&tag);
    if !panes_open {
        std::fs::create_dir_all(cfg.join("easycad")).unwrap();
        std::fs::write(
            cfg.join("easycad/settings.json"),
            r#"{"left_pane_collapsed":true,"right_pane_collapsed":true}"#,
        )
        .unwrap();
    }
    let mut h = harness(w, &cfg);
    snap(&mut h, &format!("narrow_{tag}"));
    inside_window(&h, w, &TOOLS);
    inside_window(&h, w, &["Overhang", "Shaded", "Shaded + edges", "Wireframe", "Bodies", "Faces", "Edges", "Sketches"]);

    // every tool is a button now or a row of the overflow menu
    let shown: Vec<&str> = TOOLS.iter().copied().filter(|t| h.query_by_label(t).is_some()).collect();
    let missing: Vec<&str> = TOOLS.iter().copied().filter(|t| !shown.contains(t)).collect();
    if !missing.is_empty() {
        h.get_by_label("More tools").click();
        h.run_steps(4);
        snap(&mut h, &format!("narrow_{tag}_more"));
        for t in &missing {
            assert!(h.query_by_label(t).is_some(), "{t} unreachable at {w}");
        }
        // a menu row runs its tool
        let last = *missing.last().unwrap();
        h.get_by_label(last).click();
        h.run_steps(6);
        let ran = !h.state().mode.is_model() || h.state().export.is_some() || h.state().dialog.is_some();
        if ran && !h.state().mode.is_model() {
            h.state_mut().mode = easycad_app::app::Mode::Model;
            h.run_steps(4);
        }
        eprintln!("{w}: overflow {missing:?}; clicked {last}");
        // the last tools are FILE's: Drawing opens its screen, Export its dialog
        assert!(ran, "{last} from the menu did nothing");
    }
    // the display modes: shown, or one click away (dropdown or the » menu)
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    if h.query_by_label("Wireframe").is_none() {
        let opener = ["Shaded + edges ▾", "More view options"]
            .into_iter()
            .find(|l| h.query_by_label(l).is_some())
            .unwrap_or_else(|| panic!("display modes unreachable at {w}"));
        h.get_by_label(opener).click();
        h.run_steps(4);
        snap(&mut h, &format!("narrow_{tag}_view"));
        h.get_by_label("Wireframe").click();
        h.run_steps(4);
        assert_eq!(h.state().display, easycad_app::viewport::gpu::DisplayMode::Wireframe);
        eprintln!("{w}: display modes behind {opener:?}");
    }
    let _ = std::fs::remove_dir_all(&cfg);
}

#[test]
fn sketch_at_480() {
    let _serial = serial();
    let cfg = config("sketch");
    let mut h = harness(480.0, &cfg);
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None); // S asks for a plane first
    h.run_steps(10);
    assert!(matches!(h.state().mode, easycad_app::app::Mode::Sketch(_)));
    snap(&mut h, "narrow_480_sketch");
    inside_window(&h, 480.0, &["Finish sketch", "Cancel"]);
    // one pane fits: the palette, unless the user opened the other in a sketch
    assert!(h.query_by_label("Snap to grid and geometry (hold Alt to bypass)").is_some(), "the sketch palette shows");
    assert!(h.query_by_label("DXF LAYERS").is_none() && h.query_by_label("DXF layers").is_none(), "the layers pane is on its rail");
    let _ = std::fs::remove_dir_all(&cfg);
}

/// At 800 only one pane shows at a time: the one opened last.
#[test]
fn one_pane_at_a_time_when_narrow() {
    let _serial = serial();
    let cfg = config("onepane");
    let mut h = harness(800.0, &cfg);
    let r = h.state().viewport_rect;
    assert!((r.left() < 20.0) != (r.right() > 780.0), "exactly one pane shown: {r:?}");
    // asking for the hidden one shows it and folds the other
    let left_shown = r.left() > 20.0;
    let k = if left_shown { egui::Key::CloseBracket } else { egui::Key::OpenBracket };
    h.key_press_modifiers(egui::Modifiers::COMMAND, k);
    h.run_steps(10);
    let r = h.state().viewport_rect;
    assert_eq!(r.left() > 20.0, !left_shown, "{r:?}");
    assert_eq!(r.right() < 780.0, left_shown, "{r:?}");
    snap(&mut h, "narrow_800_swapped");
    let s = &h.state().settings;
    assert!(!s.left_pane_collapsed && !s.right_pane_collapsed, "both still wanted open");
    let _ = std::fs::remove_dir_all(&cfg);
}

#[test]
fn dialog_at_480() {
    let _serial = serial();
    let cfg = config("dialog");
    let mut h = harness(480.0, &cfg);
    h.key_press(egui::Key::E);
    h.run_steps(10);
    assert!(h.state().dialog.is_some());
    snap(&mut h, "narrow_480_dialog");
    // the card is on screen and Parameters folded for it (the saved choice kept)
    let vp = h.state().viewport_rect;
    assert!(vp.right() > 440.0, "Parameters folded for the dialog: {vp:?}");
    assert!(!h.state().settings.right_pane_collapsed);
    // its rail still opens it on purpose
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(10);
    assert!(h.state().viewport_rect.right() < 300.0, "opened on request");
    h.key_press(egui::Key::Escape);
    h.run_steps(10);
    let _ = std::fs::remove_dir_all(&cfg);
}

#[test]
fn toolbars_at_480() {
    let _serial = serial();
    at_width(480.0, false);
    at_width(480.0, true);
}

#[test]
fn toolbars_at_800() {
    let _serial = serial();
    at_width(800.0, true);
    at_width(800.0, false);
}

#[test]
fn toolbars_at_1440() {
    let _serial = serial();
    at_width(1440.0, true);
}
