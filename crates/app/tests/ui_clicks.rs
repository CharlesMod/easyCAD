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

/// The wizard's enclosure from the sample STEP board: an assembly (`components`)
/// or a flat part.
fn wizard_doc(components: bool) -> easycad_model::document::Document {
    let dir = std::env::temp_dir().join(format!("easycad-clicks-{}-{components}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let step = dir.join("sensor-node.step");
    std::fs::write(&step, easycad_app::sample::pcb_step()).unwrap();
    let board = easycad_app::wizard::Board::read(&step).unwrap();
    let mut s = easycad_app::wizard::Settings::new("FDM 0.4 generic");
    s.components = components;
    let doc = easycad_app::wizard::build(&board, &s).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    doc
}

fn occ_path(app: &EasyCadApp, name: &str) -> Vec<easycad_model::assembly::OccurrenceId> {
    app.doc
        .data
        .all_occurrences()
        .into_iter()
        .find(|(_, o, _)| o.name == name)
        .unwrap_or_else(|| panic!("no occurrence {name}: {:?}", app.doc.data.all_occurrences().iter().map(|o| o.1.name.clone()).collect::<Vec<_>>()))
        .0
}

fn occ_names(app: &EasyCadApp) -> Vec<String> {
    app.doc.data.all_occurrences().into_iter().map(|(_, o, _)| o.name.clone()).collect()
}

fn click_ok(h: &mut Harness<'static, EasyCadApp>) {
    h.get_by_label("   OK   ").click();
    h.run_steps(2);
    settle(h);
}

#[test]
fn joint_output_as_components() {
    let _serial = serial();
    // the sample part: a new parting joint takes the Split's place
    let mut h = harness(easycad_app::sample::sensor_node());
    assert!(h.state().doc.data.occurrences.is_empty());
    h.state_mut().open_dialog(easycad_app::dialogs::DialogKind::Joint);
    settle(&mut h);
    assert_eq!(h.state().dialog.as_ref().unwrap().as_components, Some(false));
    h.get_by_label("Output as components").click();
    h.run_steps(2);
    assert_eq!(h.state().dialog.as_ref().unwrap().as_components, Some(true));
    click_ok(&mut h);
    let app = h.state();
    assert!(app.dialog.is_none(), "{:?}", app.toasts.iter().map(|t| t.text.clone()).collect::<Vec<_>>());
    let names = occ_names(app);
    assert_eq!(names, ["sensor-node:1", "Base:1", "Lid:1"], "master + halves");
    let d = &app.doc.data;
    let master = d.occurrence(occ_path(app, "sensor-node:1")[0]).unwrap();
    assert!(!master.visible && !master.in_bom, "the master is hidden and out of the BOM");
    assert!(d.features.is_empty(), "the root's history moved into the master");
    for n in ["Base:1", "Lid:1"] {
        let c = d.component_at(&occ_path(app, n)).unwrap();
        assert!(d.component(c).unwrap().context_frame.is_some(), "{n} is a movable in-context component");
    }
    let m = app.model.as_ref().unwrap();
    let a = m.asm.as_ref().expect("an assembly now");
    assert!(a.result.errors().is_empty(), "{:?}", a.result.errors());
    assert!(m.ctx.iter().filter(|c| !c.mb.body.reference).count() >= 2, "both halves are placed");
    assert!(app.active.is_empty());
    save(&mut h, "joint_as_components");
}

#[test]
fn split_output_as_components_in_an_active_component() {
    let _serial = serial();
    let mut h = harness(wizard_doc(true));
    let base = occ_path(h.state(), "Base:1");
    h.state_mut().activate(base.clone());
    settle(&mut h);
    h.state_mut().open_dialog(easycad_app::dialogs::DialogKind::Split);
    if let Some(easycad_app::dialogs::Body::Split(s)) = h.state_mut().dialog.as_mut().map(|d| &mut d.body) {
        s.z = easycad_app::dialogs::Field::new("4");
        s.new_name = "Upper".into();
    }
    h.state_mut().request_preview();
    settle(&mut h);
    h.get_by_label("Output as components").click();
    h.run_steps(2);
    click_ok(&mut h);
    let app = h.state();
    let names: Vec<String> = app.doc.data.occurrences.iter().map(|o| o.name.clone()).collect();
    assert_eq!(names, ["Enclosure:1", "Base:1", "Lid:1", "sensor-node:1", "Base (lower):1", "Upper:1"], "two halves of the Base");
    let src = app.doc.data.occurrence(base[0]).unwrap();
    assert!(!src.visible && !src.in_bom, "the Base became a design master");
    assert!(app.active.is_empty(), "back at the root");
    let m = app.model.as_ref().unwrap();
    assert!(m.asm.as_ref().unwrap().result.errors().is_empty());
    // editing that split again offers no second output
    let base_c = app.doc.data.component_at(&base).unwrap();
    let split = app.doc.data.component(base_c).unwrap().features.iter().rev().find(|f| f.name.starts_with("Split")).unwrap().id;
    h.state_mut().activate(base.clone());
    settle(&mut h);
    h.state_mut().edit_feature(split);
    assert_eq!(h.state().dialog.as_ref().map(|d| d.as_components), Some(None));
}
