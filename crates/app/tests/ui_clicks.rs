//! Click tests on the real app (worker thread, GPU viewport) with egui_kittest:
//! the overhang heat map, placing a part with modeled threads, linked-file Update / Break, drag-to-reparent in the
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
    // never read the user's real crash-recovery copies or settings: a leftover recovery
    // offer holds the keyboard and fails unrelated tests
    static ISOLATE: std::sync::Once = std::sync::Once::new();
    ISOLATE.call_once(|| {
        let root = std::env::temp_dir().join(format!("easycad-ui-clicks-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        if std::env::var_os("XDG_CONFIG_HOME").is_none() {
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        }
    });
    let mut h = Harness::builder()
        .with_size(egui::vec2(W, H))
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .with_step_dt(1.0 / 30.0)
        .allow_missing_glyphs()
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

fn click_at(h: &mut Harness<'static, EasyCadApp>, p: egui::Pos2, button: egui::PointerButton) {
    h.hover_at(p);
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    h.run_steps(2);
}

fn row(h: &Harness<'static, EasyCadApp>, name: &str) -> egui::Rect {
    let p = occ_path(h.state(), name);
    h.state().asm.row_rects.iter().find(|(q, _)| *q == p).unwrap_or_else(|| panic!("no tree row for {name}")).1
}

#[test]
fn linked_file_update_and_break_by_clicks() {
    use easycad_model::assembly::{ComponentId, LinkStatus, Transform};
    use easycad_model::document::Document;
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!("easycad-link-clicks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let part = dir.join("bracket.ecad");
    let mut b = Document::new("bracket");
    b.set_param("t", "2").unwrap();
    b.save_to_dir(&part).unwrap();
    let mut host = Document::new("host");
    let c = host.link_component(&part, &dir).unwrap();
    host.add_occurrence(ComponentId::ROOT, c, "bracket:1", Transform::IDENTITY).unwrap();
    let mut h = harness(host);
    // as if saved into that folder: links are relative to the host file, and the
    // cached status follows the move at once
    h.run_steps(2);
    h.state_mut().path = Some(dir.join("host.ecad"));
    h.run_steps(3);
    let badge = |h: &Harness<'static, EasyCadApp>| h.state().asm.link_rects.iter().find(|(k, _)| *k == c).map(|x| x.1);
    assert!(badge(&h).is_some(), "the row shows a link badge");
    assert_eq!(h.state_mut().link_status(c), LinkStatus::UpToDate);
    // the part changes on disk: the badge turns into "update"; a click updates
    b.set_param("t", "3").unwrap();
    b.save_to_dir(&part).unwrap();
    h.state_mut().asm.links.clear();
    h.run_steps(3);
    assert_eq!(h.state_mut().link_status(c), LinkStatus::OutOfDate);
    save(&mut h, "link_out_of_date");
    let r = badge(&h).expect("badge");
    click_at(&mut h, r.center(), egui::PointerButton::Primary);
    settle(&mut h);
    assert_eq!(h.state_mut().link_status(c), LinkStatus::UpToDate);
    assert_eq!(h.state().doc.data.component(c).unwrap().params.get("t").map(|p| p.expr.0.as_str()), Some("3"), "the new content came in");
    assert!(h.state().toasts.iter().any(|t| t.text.starts_with("Updated")));
    assert!(h.state().doc.data.component(c).unwrap().locked);
    // right-click the row, Break link
    let r = row(&h, "bracket:1");
    click_at(&mut h, r.left_center() + egui::vec2(40.0, 0.0), egui::PointerButton::Secondary);
    h.get_by_label("Break link").click();
    h.run_steps(3);
    let comp = h.state().doc.data.component(c).unwrap().clone();
    assert!(!comp.locked, "an editable component now");
    assert_eq!(h.state_mut().link_status(c), LinkStatus::NotLinked);
    assert!(badge(&h).is_none(), "no badge once the link is broken");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drag_a_tree_row_onto_another_to_reparent() {
    let _serial = serial();
    let mut h = harness(easycad_model::document::Document::new("asm"));
    h.state_mut().new_component(easycad_model::assembly::ComponentId::ROOT);
    h.state_mut().new_component(easycad_model::assembly::ComponentId::ROOT);
    {
        let app = h.state_mut();
        let p2 = occ_path(app, "Component2:1");
        app.occ_edit(p2[0], "Move", |o| o.transform = easycad_model::assembly::Transform::translation(glam::DVec3::new(10.0, 0.0, 0.0)));
    }
    settle(&mut h);
    let (from, to) = (row(&h, "Component2:1"), row(&h, "Component1:1"));
    let start = from.left_center() + egui::vec2(40.0, 0.0);
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    // move in steps past the drag threshold, onto the other row
    let end = to.left_center() + egui::vec2(40.0, 0.0);
    for i in 1..=8 {
        h.hover_at(start + (end - start) * (i as f32 / 8.0));
        h.step();
    }
    assert!(h.state().asm.tree_drag.is_some(), "dragging a row");
    save(&mut h, "tree_drag_reparent");
    h.drop_at(end);
    h.run_steps(3);
    settle(&mut h);
    let app = h.state();
    assert!(app.asm.tree_drag.is_none());
    let p2 = occ_path(app, "Component2:1");
    assert_eq!(p2.len(), 2, "Component2 is inside Component1 now");
    assert_eq!(p2[0], occ_path(app, "Component1:1")[0]);
    let w = app.doc.data.world_transform(&p2).unwrap();
    assert!((w.translation - glam::DVec3::new(10.0, 0.0, 0.0)).length() < 1e-9, "it stays where it was");
    assert_eq!(app.doc.undo_label(), Some("Move Component2:1 into Component1:1"));
    // and back out onto the root row
    let root = h.state().asm.row_rects.iter().find(|(p, _)| p.is_empty()).map(|x| x.1).expect("the root row");
    let start = row(&h, "Component2:1").left_center() + egui::vec2(50.0, 0.0);
    let end = root.center().y;
    h.hover_at(start);
    h.step();
    h.drag_at(start);
    h.step();
    for i in 1..=8 {
        let t = i as f32 / 8.0;
        h.hover_at(egui::pos2(start.x, start.y + (end - start.y) * t));
        h.step();
    }
    h.drop_at(egui::pos2(start.x, end));
    h.run_steps(3);
    assert_eq!(occ_path(h.state(), "Component2:1").len(), 1, "back at the top level");
}

/// Drag in the viewport with a button (and modifiers held), in small steps.
fn drag_view(h: &mut Harness<'static, EasyCadApp>, button: egui::PointerButton, mods: egui::Modifiers, by: egui::Vec2) {
    let a = h.state().viewport_rect.center() + egui::vec2(-150.0, 60.0);
    h.event(egui::Event::ModifiersChanged(mods));
    h.hover_at(a);
    h.step();
    h.event(egui::Event::PointerButton { pos: a, button, pressed: true, modifiers: mods });
    h.step();
    for i in 1..=10 {
        h.event(egui::Event::PointerMoved(a + by * (i as f32 / 10.0)));
        h.step();
    }
    h.event(egui::Event::PointerButton { pos: a + by, button, pressed: false, modifiers: mods });
    h.step();
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run_steps(2);
}

fn pick_preset(h: &mut Harness<'static, EasyCadApp>, from: &str, to: &str) {
    h.state_mut().show_settings = true;
    h.run_steps(2);
    h.get_by_value(from).click();
    h.run_steps(2);
    h.get_by_label(to).click();
    h.run_steps(2);
    h.state_mut().show_settings = false;
    h.run_steps(2);
}

#[test]
fn alternate_navigation_presets_by_mouse() {
    use easycad_app::settings::NavPreset;
    let _serial = serial();
    // the Settings window saves on change: keep it away from the user's settings
    let cfg = std::env::temp_dir().join(format!("easycad-nav-{}", std::process::id()));
    std::env::set_var("XDG_CONFIG_HOME", &cfg);
    let mut h = harness(easycad_app::sample::sensor_node());
    assert_eq!(h.state().settings.nav, NavPreset::easyCAD);
    let pose = |h: &Harness<'static, EasyCadApp>| (h.state().camera.rot, h.state().camera.target);
    let none = egui::Modifiers::NONE;
    // Fusion: plain middle pans
    let (r0, t0) = pose(&h);
    drag_view(&mut h, egui::PointerButton::Middle, none, egui::vec2(120.0, 40.0));
    let (r1, t1) = pose(&h);
    assert!(r1.angle_between(r0) < 1e-4 && t1.distance(t0) > 1.0, "Fusion: MMB pans");

    // SolidWorks, picked in Settings: middle orbits, Ctrl+middle pans
    pick_preset(&mut h, "Fusion 360 / easyCAD", "SolidWorks");
    assert_eq!(h.state().settings.nav, NavPreset::SolidWorks);
    assert_eq!(easycad_app::settings::Settings::load().nav, NavPreset::SolidWorks, "saved");
    h.get_by_label_contains("Orbit MMB");
    let (r0, t0) = pose(&h);
    drag_view(&mut h, egui::PointerButton::Middle, none, egui::vec2(120.0, 40.0));
    let (r1, t1) = pose(&h);
    assert!(r1.angle_between(r0) > 0.1 && t1.distance(t0) < 1e-3, "SolidWorks: MMB orbits");
    drag_view(&mut h, egui::PointerButton::Middle, egui::Modifiers::CTRL, egui::vec2(-80.0, 30.0));
    let (r2, t2) = pose(&h);
    assert!(r2.angle_between(r1) < 1e-4 && t2.distance(t1) > 1.0, "SolidWorks: Ctrl+MMB pans");

    // Onshape: right orbits, and a right drag opens no context menu
    pick_preset(&mut h, "SolidWorks", "Onshape");
    let (r0, _) = pose(&h);
    drag_view(&mut h, egui::PointerButton::Secondary, none, egui::vec2(100.0, 0.0));
    let (r1, _) = pose(&h);
    assert!(r1.angle_between(r0) > 0.1, "Onshape: RMB orbits");
    assert!(h.query_by_label("Fit all").is_none(), "no context menu after a right drag");
    save(&mut h, "nav_onshape_orbit");
    pick_preset(&mut h, "Onshape", "Fusion 360 / easyCAD");
    assert_eq!(easycad_app::settings::Settings::load().nav, NavPreset::easyCAD);
    let _ = std::fs::remove_dir_all(&cfg);
}

/// A straight edge along X on an occurrence's front face (y min), at its top or
/// bottom: the hinge line of the wizard's box.
fn front_edge(app: &EasyCadApp, name: &str, top: bool) -> easycad_app::pickstack::PickKey {
    use easycad_app::pickstack::{PickKey, PickKind};
    let path = occ_path(app, name);
    let m = app.model.as_ref().unwrap();
    let mut best: Option<(f64, PickKey)> = None;
    for c in m.ctx.iter().filter(|c| c.path == path && !c.mb.body.reference) {
        for e in &c.mb.edges {
            if e.kind != easycad_model::occt::CurveKind::Line || e.tangent.x.abs() < 0.99 || e.length < 20.0 {
                continue;
            }
            let p = c.xf.transform_point3(e.midpoint);
            let score = -p.y * 1000.0 + if top { p.z } else { -p.z };
            if best.as_ref().is_none_or(|b| score > b.0) {
                best = Some((score, PickKey { body: c.key, kind: PickKind::Edge(e.index) }));
            }
        }
    }
    best.expect("front edge").1
}

#[test]
fn mate_free_motion_and_interference_overlap() {
    use easycad_app::ui_mates::FreeGlyph;
    let _serial = serial();
    let mut h = harness(wizard_doc(true));
    // a hinge on the front seam, swung 12° into the base
    h.state_mut().open_mate(None);
    let a = front_edge(h.state(), "Base:1", true);
    let b = front_edge(h.state(), "Lid:1", false);
    assert!(h.state_mut().mate_pick(a));
    assert!(h.state_mut().mate_pick(b));
    h.state_mut().asm.mate.as_mut().unwrap().angle = -12.0;
    h.state_mut().request_model();
    settle(&mut h);
    // while the dialog is open the draft's free motion shows
    let g = easycad_app::ui_mates::free_glyphs(h.state());
    assert_eq!(g.len(), 1, "{g:?}");
    h.state_mut().commit_mate();
    settle(&mut h);
    assert!(easycad_app::ui_mates::free_glyphs(h.state()).is_empty(), "nothing selected, nothing drawn");

    // click the mate's row in the Mates folder: its free rotation is drawn
    let id = h.state().doc.data.mates[0].id;
    let r = h.state().asm.mate_rects.iter().find(|(m, _)| *m == id).map(|x| x.1).expect("a Mates row");
    click_at(&mut h, r.center(), egui::PointerButton::Primary);
    assert_eq!(h.state().asm.sel_mate, Some(id));
    let g = easycad_app::ui_mates::free_glyphs(h.state());
    match g.as_slice() {
        [FreeGlyph::Turn { at, axis }] => {
            assert!(axis.x.abs() > 0.99, "the hinge turns about X: {axis}");
            let base = easycad_app::ui_gizmo::occ_bounds(h.state(), &occ_path(h.state(), "Base:1")).unwrap();
            assert!((at.y - base.0.y).abs() < 3.0, "on the front seam: {at} vs {base:?}");
        }
        g => panic!("one rotation expected: {g:?}"),
    }
    save(&mut h, "mate_free_motion");
    // a second click (not a double-click) clears it
    h.run_steps(15);
    click_at(&mut h, r.center(), egui::PointerButton::Primary);
    assert_eq!(h.state().asm.sel_mate, None);

    // interference: the lid turned into the base overlaps it; the overlap solid is drawn
    h.state_mut().check_interference();
    for _ in 0..600 {
        h.run_steps(1);
        if h.state().asm.interference.as_ref().is_some_and(|i| i.job.is_none()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    h.run_steps(3);
    let i = h.state().asm.interference.as_ref().unwrap();
    let rep = i.report.clone().expect("report");
    assert!(!rep.pairs.is_empty(), "{rep:?}");
    let p = &rep.pairs[0];
    assert!(p.volume > 0.0, "{p:?}");
    assert!(p.common.is_some(), "the overlap solid (GUI_NEEDS #19)");
    let shown = i.shown_overlaps();
    assert!(!shown.is_empty() && !shown[0].indices.is_empty(), "meshed for display");
    let vp = h.state().viewport_rect.shrink(40.0);
    let img = save(&mut h, "interference_overlap");
    assert!(reddish(&img, vp) > 20, "the overlap and the pair show red");
}

#[test]
fn place_a_part_with_modeled_threads() {
    use easycad_model::feature::{FeatureKind, Threads};
    let _serial = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    {
        let app = h.state_mut();
        app.mode = easycad_app::app::Mode::Parts(Box::new(easycad_app::ui_parts::PartsUi::new(&app.doc)));
    }
    h.run_steps(4);
    h.get_by_label("Modeled threads").click();
    h.run_steps(2);
    h.get_by_label("Place at origin").click();
    h.run_steps(2);
    settle(&mut h);
    let app = h.state();
    let last = app.doc.data.features.last().expect("a feature was placed");
    let FeatureKind::InsertPart { threads, .. } = &last.kind else { panic!("placed {:?}", last.kind) };
    assert_eq!(*threads, Threads::Modeled);
    save(&mut h, "parts_modeled_threads");
}

fn has(app: &EasyCadApp, sid: easycad_model::feature::SketchId) -> bool {
    easycad_app::sketch_view::draw_list(app).iter().any(|(i, _)| *i == sid)
}

#[test]
fn finished_sketch_is_drawn_in_3d_until_an_extrude_consumes_it() {
    use easycad_model::feature::{BoolOp, FeatureKind, ProfileSel};
    use easycad_model::geom::Plane;
    use easycad_model::sketch::Sketch;
    let _serial = serial();
    let mut doc = easycad_app::sample::sensor_node();
    let mut s = Sketch::new("S", Plane::XY);
    s.rect(glam::DVec2::ZERO, glam::DVec2::new(40.0, 20.0));
    let sid = doc.add_sketch(s);
    let mut h = harness(doc);
    let list = easycad_app::sketch_view::draw_list(h.state());
    let mine = h.state().view().sketches.iter().position(|e| e.id == sid).unwrap();
    let list: Vec<_> = list.into_iter().filter(|(i, _)| *i == sid).collect();
    let _ = mine;
    assert_eq!(list.len(), 1, "the finished sketch is in the draw list");
    assert_eq!(list[0].1.edges.len(), 4, "four lines");
    assert!(!list[0].1.indices.is_empty(), "closed profile filled");
    assert!(list[0].1.edges.iter().flat_map(|e| &e.points).all(|p| p[2] == 0.0), "on the XY plane");
    // cached: frames do not rebuild it
    let builds = easycad_app::sketch_view::builds();
    h.run_steps(5);
    let again: Vec<_> = easycad_app::sketch_view::draw_list(h.state()).into_iter().filter(|(i, _)| *i == sid).collect();
    assert!(std::sync::Arc::ptr_eq(&list[0].1, &again[0].1));
    assert_eq!(easycad_app::sketch_view::builds(), builds);
    save(&mut h, "sketch_visible_in_3d");
    // an extrude consumes it: hidden by default
    h.state_mut().doc.add_feature(
        "Block",
        FeatureKind::Extrude { sketch: sid, profiles: ProfileSel::All, distance: 5.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    );
    h.run_steps(3);
    assert!(!has(h.state(), sid), "consumed sketch hidden");
    // the toggle brings it back and sticks
    easycad_app::sketch_view::set_visible(h.state_mut(), sid, true);
    assert!(has(h.state(), sid));
    settle(&mut h);
    assert!(has(h.state(), sid), "regeneration keeps the choice");
}

#[test]
fn lone_sketch_shows_on_its_plane_in_an_empty_model() {
    use easycad_model::geom::Plane;
    use easycad_model::sketch::Sketch;
    let _serial = serial();
    let mut doc = easycad_model::document::Document::new("blank");
    let mut s = Sketch::new("S", Plane::XY);
    s.rect(glam::DVec2::new(-20.0, -10.0), glam::DVec2::new(20.0, 10.0));
    doc.add_sketch(s);
    let mut h = harness(doc);
    let img = save(&mut h, "sketch_visible_alone");
    let vp = h.state().viewport_rect.shrink(20.0);
    let green = img
        .enumerate_pixels()
        .filter(|(x, y, p)| vp.contains(egui::pos2(*x as f32, *y as f32)) && p.0[1] as i32 > p.0[0] as i32 + 60 && p.0[1] > 150)
        .count();
    assert!(green > 100, "the sketch outline is drawn in the sketch colour ({green} px)");
}

// ---- sketch input: undo / redo keys -------------------------------------------------

fn chord(h: &mut Harness<'static, EasyCadApp>, m: egui::Modifiers, k: egui::Key) {
    h.key_press_modifiers(m, k);
    h.run_steps(3);
}

#[test]
fn undo_redo_keys_in_model_mode() {
    let _g = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear(); // crash leftovers on this machine would hold the keyboard
    h.state_mut().set_param("zz", "5").unwrap();
    assert_eq!(h.state().doc.undo_label(), Some("Set zz"));
    chord(&mut h, egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(h.state().doc.redo_label(), Some("Set zz"), "Ctrl+Z undoes the document edit");
    chord(&mut h, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
    assert_eq!(h.state().doc.undo_label(), Some("Set zz"), "Ctrl+Shift+Z redoes");
    assert_eq!(h.state().doc.redo_label(), None);
    chord(&mut h, egui::Modifiers::COMMAND, egui::Key::Z);
    chord(&mut h, egui::Modifiers::COMMAND, egui::Key::Y);
    assert_eq!(h.state().doc.undo_label(), Some("Set zz"), "Ctrl+Y redoes");
}

#[test]
fn undo_redo_keys_in_sketch_mode() {
    let _g = serial();
    use easycad_app::app::Mode;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear(); // crash leftovers on this machine would hold the keyboard
    h.state_mut().recovery_offer.clear();
    h.state_mut().set_param("zz", "5").unwrap();
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None);
    h.run_steps(3);
    let n = |h: &Harness<'static, EasyCadApp>| match &h.state().mode {
        Mode::Sketch(s) => (s.sketch.points.len(), s.undo.len(), s.redo.len()),
        _ => panic!("not in sketch mode"),
    };
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.push_undo();
        s.sketch.add_point(glam::DVec2::new(1.0, 2.0));
    }
    assert_eq!(n(&h).0, 1);
    chord(&mut h, egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(n(&h).0, 0, "Ctrl+Z undoes the sketch edit");
    assert_eq!(h.state().doc.undo_label(), Some("Set zz"), "the document edit is untouched while the sketch has history");
    chord(&mut h, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
    assert_eq!(n(&h).0, 1, "Ctrl+Shift+Z redoes the sketch edit");
    chord(&mut h, egui::Modifiers::COMMAND, egui::Key::Z);
    chord(&mut h, egui::Modifiers::COMMAND, egui::Key::Y);
    assert_eq!(n(&h).0, 1, "Ctrl+Y redoes the sketch edit");
    // with the sketch history spent, Ctrl+Z does nothing: the document is not touched
    chord(&mut h, egui::Modifiers::COMMAND, egui::Key::Z);
    chord(&mut h, egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(h.state().doc.undo_label(), Some("Set zz"), "the document stack stays put in sketch mode");
    assert_eq!(h.state().doc.redo_label(), None);
}

#[test]
fn sketch_value_box_opens_on_first_click_with_text_selected() {
    let _g = serial();
    use easycad_app::app::Mode;
    use easycad_app::sketch_mode::Tool;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear();
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None);
    h.run_steps(3);
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.tool = Tool::Circle;
    }
    click_at(&mut h, egui::pos2(700.0, 450.0), egui::PointerButton::Primary);
    h.run_steps(3);
    let input = |h: &Harness<'static, EasyCadApp>| match &h.state().mode {
        Mode::Sketch(s) => s.input.clone(),
        _ => panic!(),
    };
    let text = input(&h).expect("the value box opens on the first click");
    let id = egui::Id::NULL.with("sk_input_text");
    let range = egui::TextEdit::load_state(&h.ctx, id).and_then(|st| st.cursor.char_range()).expect("a selection");
    assert_eq!((range.primary.index.0.min(range.secondary.index.0), range.primary.index.0.max(range.secondary.index.0)), (0, text.chars().count()), "all text selected");
    // typing replaces the reading
    h.event(egui::Event::Text("40".into()));
    h.run_steps(3);
    assert_eq!(input(&h).as_deref(), Some("40"));
    // Enter commits a circle of that diameter and closes the box
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(input(&h), None);
    if let Mode::Sketch(s) = &h.state().mode {
        assert!(s.clicks.is_empty());
        let r: Vec<f64> = s.sketch.entities.iter().filter_map(|e| match e.geom { easycad_model::sketch::Geom::Circle { radius, .. } => Some(radius), _ => None }).collect();
        assert_eq!(r.len(), 1);
        assert!((r[0] - 20.0).abs() < 1e-6, "{r:?}");
    }
}

#[test]
fn new_sketch_button_does_what_the_s_key_does() {
    let _g = serial();
    let doc = || easycad_model::document::Document::new("empty");
    let mut by_key = harness(doc());
    assert!(by_key.state().plane_pick.is_none());
    by_key.key_press(egui::Key::S);
    by_key.run_steps(3);
    assert!(by_key.state().plane_pick.is_some(), "S asks for a plane first");
    assert!(easycad_app::app::Mode::is_model(&by_key.state().mode), "and does not drop into a sketch");
    drop(by_key);
    let mut by_button = harness(doc());
    by_button.get_by_label("Start a new sketch (or press s)").click();
    by_button.run_steps(3);
    assert!(by_button.state().plane_pick.is_some());
}

/// Screen position of a world point in the viewport.
fn screen_of(h: &Harness<'static, EasyCadApp>, w: glam::Vec3) -> egui::Pos2 {
    let app = h.state();
    let r = app.viewport_rect;
    let s = app.camera.project(w, glam::Vec2::new(r.width(), r.height())).expect("on screen");
    egui::pos2(r.left() + s.x, r.top() + s.y)
}

fn hover_click(h: &mut Harness<'static, EasyCadApp>, p: egui::Pos2) {
    h.hover_at(p);
    h.run_steps(3);
    click_at(h, p, egui::PointerButton::Primary);
    h.run_steps(3);
}

fn top_view(h: &mut Harness<'static, EasyCadApp>) {
    h.state_mut().camera.rot = easycad_app::camera::look_rot(glam::Vec3::Z, glam::Vec3::Y);
    h.run_steps(3);
}


#[test]
fn plane_pick_xy_region_opens_a_sketch_on_xy() {
    use easycad_app::app::Mode;
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    top_view(&mut h);
    h.key_press(egui::Key::S);
    h.run_steps(3);
    assert!(h.state().plane_pick.is_some());
    assert_eq!(easycad_app::ui_main::status_text(h.state()), easycad_app::plane_pick::HINT);
    let p = screen_of(&h, glam::Vec3::new(15.0, 12.0, 0.0));
    h.hover_at(p);
    h.step();
    h.step();
    assert_eq!(h.state().plane_pick.as_ref().unwrap().hover, Some(easycad_app::plane_pick::Target::Origin(0)), "hover highlights the XY plane");
    save(&mut h, "plane_pick_hover_xy");
    hover_click(&mut h, p);
    match &h.state().mode {
        Mode::Sketch(s) => assert_eq!(s.sketch.plane, easycad_model::geom::Plane::XY),
        _ => panic!("expected sketch mode"),
    }
    assert!(h.state().plane_pick.is_none());
}

#[test]
fn plane_pick_on_a_box_top_face_sketches_on_that_face() {
    use easycad_app::app::Mode;
    use easycad_model::feature::{BoolOp, FeatureKind, ProfileSel};
    use easycad_model::sketch::Sketch;
    let _g = serial();
    let mut doc = easycad_model::document::Document::new("box");
    let mut s = Sketch::new("S", easycad_model::geom::Plane::XY);
    s.rect(glam::DVec2::new(-30.0, -20.0), glam::DVec2::new(30.0, 20.0));
    let sid = doc.add_sketch(s);
    doc.add_feature(
        "Block",
        FeatureKind::Extrude { sketch: sid, profiles: ProfileSel::All, distance: 20.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    );
    let mut h = harness(doc);
    top_view(&mut h);
    h.key_press(egui::Key::S);
    h.run_steps(3);
    let p = screen_of(&h, glam::Vec3::new(5.0, 3.0, 20.0));
    hover_click(&mut h, p);
    match &h.state().mode {
        Mode::Sketch(s) => {
            assert!((s.sketch.plane.origin.z - 20.0).abs() < 1e-6, "{:?}", s.sketch.plane);
            assert!(s.sketch.plane.normal.z > 0.99);
        }
        _ => panic!("expected sketch mode on the face"),
    }
    assert!(h.state().sketch_body.is_some());
}

#[test]
fn plane_pick_cancels_on_escape_and_right_click() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    h.key_press(egui::Key::S);
    h.run_steps(3);
    assert!(h.state().plane_pick.is_some());
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().plane_pick.is_none(), "Esc cancels");
    assert!(easycad_app::app::Mode::is_model(&h.state().mode));
    h.key_press(egui::Key::S);
    h.run_steps(3);
    let c = h.state().viewport_rect.center();
    click_at(&mut h, c, egui::PointerButton::Secondary);
    assert!(h.state().plane_pick.is_none(), "right-click cancels");
    assert!(easycad_app::app::Mode::is_model(&h.state().mode));
}

#[test]
fn x_key_toggles_construction_in_sketch_mode() {
    let _g = serial();
    use easycad_app::app::Mode;
    use easycad_app::sketch_mode::Sel;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear();
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None);
    h.run_steps(3);
    let s = |h: &Harness<'static, EasyCadApp>| match &h.state().mode {
        Mode::Sketch(s) => (s.construction_mode, s.sketch.entities.iter().filter(|e| e.construction).count(), s.undo.len()),
        _ => panic!("not in sketch mode"),
    };
    // nothing selected: X turns construction mode on, X again off
    chord(&mut h, egui::Modifiers::NONE, egui::Key::X);
    assert!(s(&h).0);
    chord(&mut h, egui::Modifiers::NONE, egui::Key::X);
    assert!(!s(&h).0);
    // a selected line: X makes it construction (one undo step)
    if let Mode::Sketch(sk) = &mut h.state_mut().mode {
        let e = sk.add_line((glam::DVec2::ZERO, None), (glam::DVec2::new(5.0, 0.0), None));
        sk.selection = vec![Sel::Entity(e)];
    }
    h.run_steps(2);
    let u0 = s(&h).2;
    chord(&mut h, egui::Modifiers::NONE, egui::Key::X);
    assert_eq!(s(&h), (false, 1, u0 + 1));
}

// ---------------------------------------------------------------- the Construct tool

fn construct_state(h: &Harness<'static, EasyCadApp>) -> (usize, Vec<easycad_app::datum_ui::DatumKind>, usize) {
    match h.state().dialog.as_ref().map(|d| &d.body) {
        Some(easycad_app::dialogs::Body::Datum(dd)) => {
            let a = dd.auto.as_ref().expect("auto");
            (a.picks.len(), a.alts.iter().map(|c| c.kind).collect(), a.alt)
        }
        _ => panic!("the Construct tool is not open"),
    }
}

fn datum_kinds(h: &Harness<'static, EasyCadApp>) -> Vec<String> {
    h.state()
        .doc
        .data
        .features
        .iter()
        .filter(|f| f.kind.is_datum())
        .map(|f| format!("{:?}", f.kind))
        .collect()
}

fn box_doc() -> easycad_model::document::Document {
    use easycad_model::feature::{BoolOp, FeatureKind, ProfileSel};
    use easycad_model::sketch::Sketch;
    let mut doc = easycad_model::document::Document::new("box");
    let mut s = Sketch::new("S", easycad_model::geom::Plane::XY);
    s.rect(glam::DVec2::new(-30.0, -20.0), glam::DVec2::new(30.0, 20.0));
    let sid = doc.add_sketch(s);
    doc.add_feature(
        "Block",
        FeatureKind::Extrude { sketch: sid, profiles: ProfileSel::All, distance: 20.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    );
    doc
}

fn click_w(h: &mut Harness<'static, EasyCadApp>, w: glam::Vec3) {
    let p = screen_of(h, w);
    hover_click(h, p);
}

fn press_enter(h: &mut Harness<'static, EasyCadApp>) {
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    settle(h);
}

#[test]
fn construct_one_point_makes_a_point_and_stays_active() {
    use easycad_app::datum_ui::DatumKind;
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    top_view(&mut h);
    h.key_press(egui::Key::Q);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::ZERO);
    let (n, kinds, _) = construct_state(&h);
    assert_eq!((n, kinds), (1, vec![DatumKind::PointAt]));
    press_enter(&mut h);
    let made = datum_kinds(&h);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].contains("DatumPoint"), "{made:?}");
    let (n, _, _) = construct_state(&h);
    assert_eq!(n, 0, "tool stays active with a cleared selection");
    // undo takes the whole construct back in one step
    h.state_mut().undo();
    assert!(datum_kinds(&h).is_empty());
}

#[test]
fn construct_two_points_axis_three_points_plane() {
    use easycad_app::datum_ui::DatumKind;
    let _g = serial();
    let mut h = harness(box_doc());
    top_view(&mut h);
    h.key_press(egui::Key::Q);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::new(-30.0, -20.0, 20.0));
    click_w(&mut h, glam::Vec3::new(30.0, -20.0, 20.0));
    let (n, kinds, _) = construct_state(&h);
    assert_eq!((n, kinds), (2, vec![DatumKind::AxisTwoPoints]));
    press_enter(&mut h);
    assert!(datum_kinds(&h)[0].contains("DatumAxis"), "{:?}", datum_kinds(&h));
    click_w(&mut h, glam::Vec3::new(-30.0, -20.0, 20.0));
    click_w(&mut h, glam::Vec3::new(30.0, -20.0, 20.0));
    click_w(&mut h, glam::Vec3::new(30.0, 20.0, 20.0));
    let (n, kinds, _) = construct_state(&h);
    assert_eq!((n, kinds), (3, vec![DatumKind::PlaneThreePoints]));
    press_enter(&mut h);
    let made = datum_kinds(&h);
    assert_eq!(made.len(), 2, "{made:?}");
    assert!(made[1].contains("DatumPlane"), "{made:?}");
    // a click on a picked point removes it again
    click_w(&mut h, glam::Vec3::new(30.0, 20.0, 20.0));
    assert_eq!(construct_state(&h).0, 1);
    click_w(&mut h, glam::Vec3::new(30.0, 20.0, 20.0));
    assert_eq!(construct_state(&h).0, 0);
}

#[test]
fn construct_plane_types_the_offset_and_enter_commits() {
    use easycad_app::datum_ui::DatumKind;
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    top_view(&mut h);
    h.key_press(egui::Key::Q);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::new(15.0, 12.0, 0.0));
    let (n, kinds, _) = construct_state(&h);
    assert_eq!((n, kinds), (1, vec![DatumKind::PlaneOffset]));
    // the value is already focused with its text selected: just type
    h.run_steps(2);
    h.event(egui::Event::Text("10".into()));
    h.run_steps(3);
    press_enter(&mut h);
    let made = datum_kinds(&h);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].contains("Offset") && made[0].contains("10"), "{made:?}");
}

#[test]
fn construct_line_and_plane_make_an_angle_plane_and_tab_cycles() {
    use easycad_app::datum_ui::DatumKind;
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    top_view(&mut h);
    h.key_press(egui::Key::Q);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::new(20.0, 0.0, 0.0));
    assert_eq!(construct_state(&h).1, vec![DatumKind::AxisLine, DatumKind::PlaneNormal]);
    click_w(&mut h, glam::Vec3::new(15.0, 12.0, 0.0));
    let (n, kinds, alt) = construct_state(&h);
    assert_eq!(n, 2);
    assert_eq!(kinds, vec![DatumKind::PlaneAngle, DatumKind::PointEdgePlane]);
    assert_eq!(alt, 0);
    h.key_press(egui::Key::Tab);
    h.run_steps(3);
    assert_eq!(construct_state(&h).2, 1, "Tab picks the next alternative");
    h.key_press(egui::Key::Tab);
    h.run_steps(3);
    assert_eq!(construct_state(&h).2, 0, "and wraps around");
    press_enter(&mut h);
    let made = datum_kinds(&h);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].contains("Angle"), "{made:?}");
}

#[test]
fn construct_esc_clears_then_exits() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    top_view(&mut h);
    h.key_press(egui::Key::Q);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::ZERO);
    assert_eq!(construct_state(&h).0, 1);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert_eq!(construct_state(&h).0, 0);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().dialog.is_none());
}

#[test]
fn editing_a_datum_reopens_the_tool_with_its_refs() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    top_view(&mut h);
    h.key_press(egui::Key::Q);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::new(15.0, 12.0, 0.0));
    h.run_steps(2);
    h.event(egui::Event::Text("10".into()));
    h.run_steps(3);
    press_enter(&mut h);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    let fid = h.state().doc.data.features.iter().find(|f| f.kind.is_datum()).unwrap().id;
    h.state_mut().edit_feature(fid);
    h.run_steps(3);
    settle(&mut h);
    let (n, kinds, _) = construct_state(&h);
    assert_eq!((n, kinds), (1, vec![easycad_app::datum_ui::DatumKind::PlaneOffset]));
    h.event(egui::Event::Text("25".into()));
    h.run_steps(3);
    press_enter(&mut h);
    let made = datum_kinds(&h);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].contains("25"), "{made:?}");
}

#[test]
fn sketch_on_a_construction_plane_follows_it() {
    use easycad_app::app::Mode;
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    top_view(&mut h);
    h.key_press(egui::Key::Q);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::new(15.0, 12.0, 0.0));
    h.run_steps(2);
    h.event(egui::Event::Text("10".into()));
    h.run_steps(3);
    press_enter(&mut h);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    let fid = h.state().doc.data.features.iter().find(|f| f.kind.is_datum()).unwrap().id;
    h.key_press(egui::Key::S);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::new(15.0, 12.0, 10.0));
    match &h.state().mode {
        Mode::Sketch(s) => {
            assert_eq!(s.sketch.plane_src, Some(fid));
            assert!((s.sketch.plane.origin.z - 10.0).abs() < 1e-6, "{:?}", s.sketch.plane);
        }
        _ => panic!("expected sketch mode on the construction plane"),
    }
}

#[test]
fn hiding_a_construction_plane_removes_it_from_the_scene() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    top_view(&mut h);
    h.key_press(egui::Key::Q);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::new(15.0, 12.0, 0.0));
    press_enter(&mut h);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    let fid = h.state().doc.data.features.iter().find(|f| f.kind.is_datum()).unwrap().id;
    let shown = |h: &Harness<'static, EasyCadApp>| easycad_app::datum_ui::datum_items(h.state(), None).len();
    assert_eq!(shown(&h), 1);
    easycad_app::datum_ui::set_visible(h.state_mut(), fid, false);
    h.run_steps(2);
    assert_eq!(shown(&h), 0);
    easycad_app::datum_ui::set_visible(h.state_mut(), fid, true);
    assert_eq!(shown(&h), 1);
}

// ---------------------------------------------------------------- the drag arrow

fn arrow_text(h: &Harness<'static, EasyCadApp>) -> String {
    match h.state().dialog.as_ref().map(|d| &d.body) {
        Some(easycad_app::dialogs::Body::Extrude(e)) => e.distance.text.clone(),
        Some(easycad_app::dialogs::Body::Datum(dd)) => dd.fields[0].text.clone(),
        _ => panic!("no dialog with a distance"),
    }
}

/// Press on the arrow shaft and pull `px` pixels along it (negative: against it).
fn arrow_drag(h: &mut Harness<'static, EasyCadApp>, mods: egui::Modifiers, px: f32) {
    let spec = easycad_app::drag_arrow::spec(h.state()).expect("arrow spec");
    let tail = screen_of(h, spec.at(spec.value).as_vec3());
    let head = screen_of(h, (spec.at(spec.value) + spec.dir * 40.0).as_vec3());
    let sd = (head - tail).normalized();
    let a = tail + sd * 20.0;
    h.event(egui::Event::ModifiersChanged(mods));
    h.hover_at(a);
    h.run_steps(2);
    h.event(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: mods });
    h.step();
    for i in 1..=12 {
        h.event(egui::Event::PointerMoved(a + sd * px * (i as f32 / 12.0)));
        h.step();
    }
    h.event(egui::Event::PointerButton { pos: a + sd * px, button: egui::PointerButton::Primary, pressed: false, modifiers: mods });
    h.step();
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run_steps(3);
}

fn open_extrude(h: &mut Harness<'static, EasyCadApp>) {
    h.state_mut().open_dialog(easycad_app::dialogs::DialogKind::Extrude);
    h.run_steps(3);
    settle(h);
}

fn mult_of(v: f64, step: f64) -> bool {
    let q = v / step;
    (q - q.round()).abs() < 1e-6
}

#[test]
fn extrude_arrow_plain_drag_changes_by_whole_mm() {
    let _g = serial();
    let mut h = harness(box_doc());
    open_extrude(&mut h);
    let v0: f64 = arrow_text(&h).parse().unwrap();
    arrow_drag(&mut h, egui::Modifiers::NONE, 43.0);
    let v: f64 = arrow_text(&h).parse().unwrap();
    assert!(v != v0 && mult_of(v, 1.0), "{v0} -> {v}");
    // the dialog stays open and the preview was asked for
    assert!(h.state().dialog.is_some() && h.state().arrow_drag.is_none());
}

#[test]
fn extrude_arrow_shift_and_ctrl_steps() {
    let _g = serial();
    let mut h = harness(box_doc());
    open_extrude(&mut h);
    arrow_drag(&mut h, egui::Modifiers::SHIFT, 300.0);
    let v: f64 = arrow_text(&h).parse().unwrap();
    assert!(mult_of(v, 10.0) && v > 20.0, "shift: {v}");
    arrow_drag(&mut h, egui::Modifiers::CTRL, -37.0);
    let w = arrow_text(&h).parse::<f64>().unwrap();
    assert!(mult_of(w, 0.1) && w < v, "ctrl: {w}");
    // with a solid the op is auto: the drag goes negative (Cut) but never inside the dead zone
    for _ in 0..8 {
        if arrow_text(&h).parse::<f64>().unwrap() < 0.0 {
            break;
        }
        arrow_drag(&mut h, egui::Modifiers::SHIFT, -250.0);
    }
    let n = arrow_text(&h).parse::<f64>().unwrap();
    assert!(n < 0.0 && n.abs() >= easycad_app::dialogs::EXTRUDE_DEAD, "{n}");
}

#[test]
fn extrude_arrow_without_auto_never_goes_below_zero() {
    let _g = serial();
    let mut h = harness(box_doc());
    open_extrude(&mut h);
    if let Some(easycad_app::dialogs::Body::Extrude(e)) = h.state_mut().dialog.as_mut().map(|d| &mut d.body) {
        e.auto = false;
    }
    for _ in 0..4 {
        arrow_drag(&mut h, egui::Modifiers::SHIFT, -250.0);
    }
    assert!(arrow_text(&h).parse::<f64>().unwrap() > 0.0);
}

fn cylinder_doc() -> easycad_model::document::Document {
    use easycad_model::feature::{BoolOp, FeatureKind, ProfileSel};
    use easycad_model::sketch::Sketch;
    let mut doc = easycad_model::document::Document::new("cyl");
    let mut s = Sketch::new("S", easycad_model::geom::Plane::XY);
    s.circle(glam::DVec2::ZERO, 15.0);
    let sid = doc.add_sketch(s);
    doc.add_feature(
        "Cyl",
        FeatureKind::Extrude { sketch: sid, profiles: ProfileSel::All, distance: 20.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    );
    doc
}

fn extrude_body<'a>(h: &'a Harness<'static, EasyCadApp>) -> &'a easycad_app::dialogs::ExtrudeD {
    match h.state().dialog.as_ref().map(|d| &d.body) {
        Some(easycad_app::dialogs::Body::Extrude(e)) => e,
        _ => panic!("no extrude dialog"),
    }
}

/// Select the cylinder's top face and press E.
fn face_extrude_open() -> Harness<'static, EasyCadApp> {
    let mut h = harness(cylinder_doc());
    top_view(&mut h);
    click_w(&mut h, glam::Vec3::new(4.0, 3.0, 20.0));
    h.key_press(egui::Key::E);
    h.run_steps(3);
    settle(&mut h);
    // an oblique view, so the arrow along the face normal is not end-on
    h.state_mut().camera.rot = easycad_app::camera::look_rot(glam::Vec3::new(1.0, 1.0, 1.0).normalize(), glam::Vec3::Z);
    h.run_steps(3);
    h
}

#[test]
fn e_on_a_selected_top_face_extrudes_that_face() {
    let _g = serial();
    let h = face_extrude_open();
    let e = extrude_body(&h);
    let f = e.face.as_ref().expect("face source");
    assert!(f.normal.z > 0.99 && (f.centroid.z - 20.0).abs() < 1e-6, "{f:?}");
    assert!(e.sketch.is_none() && e.auto && e.target.is_some());
    let s = easycad_app::drag_arrow::spec(h.state()).unwrap();
    assert!(s.dir.z > 0.99 && (s.base.z - 20.0).abs() < 1e-6);
}

#[test]
fn face_extrude_arrow_negative_cuts_positive_joins() {
    use easycad_model::feature::{BoolOp, FeatureKind};
    let _g = serial();
    let mut h = face_extrude_open();
    let build_op = |h: &Harness<'static, EasyCadApp>| {
        let cx = h.state().dialog_cx_data();
        let d = h.state().dialog.as_ref().unwrap();
        match d.build(&cx).unwrap().remove(0).kind {
            FeatureKind::ExtrudeFace { op, distance, .. } => (op, distance.0),
            k => panic!("{k:?}"),
        }
    };
    // down (against the outward normal) = into the body
    arrow_drag(&mut h, egui::Modifiers::NONE, -320.0);
    let v: f64 = arrow_text(&h).parse().unwrap();
    assert!(v < 0.0, "{v}");
    assert_eq!(build_op(&h).0, BoolOp::Cut);
    assert_eq!(extrude_body(&h).live_op(Some(v)), easycad_app::dialogs::Op::Cut);
    // and back out past zero = Join
    arrow_drag(&mut h, egui::Modifiers::NONE, 500.0);
    let w: f64 = arrow_text(&h).parse().unwrap();
    assert!(w > 0.0, "{w}");
    assert_eq!(build_op(&h).0, BoolOp::Join);
    // picking an op in the dialog turns auto off
    if let Some(easycad_app::dialogs::Body::Extrude(e)) = h.state_mut().dialog.as_mut().map(|d| &mut d.body) {
        e.auto = false;
        e.op = easycad_app::dialogs::Op::Cut;
    }
    let (op, dist) = build_op(&h);
    assert_eq!(op, BoolOp::Cut);
    assert!(dist.trim_start().starts_with("-("), "explicit Cut goes into the body: {dist}");
}

#[test]
fn editing_a_face_extrude_restores_face_and_op() {
    use easycad_app::dialogs::{Body, Dialog, Op};
    let _g = serial();
    let mut h = face_extrude_open();
    if let Some(Body::Extrude(e)) = h.state_mut().dialog.as_mut().map(|d| &mut d.body) {
        e.distance.text = "-3".into();
    }
    let cx = h.state().dialog_cx_data();
    let feats = h.state().dialog.as_ref().unwrap().build(&cx).unwrap();
    let d = Dialog::edit(&feats[0], &cx).expect("edit dialog");
    let Body::Extrude(e) = &d.body else { panic!("not extrude") };
    assert!(e.face.is_some() && e.sketch.is_none() && e.target.is_some());
    assert_eq!((e.distance.text.as_str(), e.op, e.auto), ("-3", Op::Cut, true));
}

#[test]
fn extrude_arrow_esc_cancels_the_drag() {
    let _g = serial();
    let mut h = harness(box_doc());
    open_extrude(&mut h);
    let t0 = arrow_text(&h);
    let spec = easycad_app::drag_arrow::spec(h.state()).unwrap();
    let tail = screen_of(&h, spec.at(spec.value).as_vec3());
    let head = screen_of(&h, (spec.at(spec.value) + spec.dir * 40.0).as_vec3());
    let sd = (head - tail).normalized();
    let a = tail + sd * 20.0;
    h.hover_at(a);
    h.run_steps(2);
    h.event(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(a + sd * 8.0 * i as f32));
        h.step();
    }
    assert_ne!(arrow_text(&h), t0, "the drag changed the value");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(arrow_text(&h), t0);
    assert!(h.state().arrow_drag.is_none());
    h.event(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(2);
}

#[test]
fn offset_plane_arrow_steps_in_the_construct_tool() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    top_view(&mut h);
    h.key_press(egui::Key::Q);
    h.run_steps(3);
    click_w(&mut h, glam::Vec3::new(15.0, 12.0, 0.0));
    settle(&mut h);
    // the arrow is along Z; look from the side so a pull moves it on screen
    h.state_mut().camera.rot = easycad_app::camera::look_rot(glam::Vec3::Y, glam::Vec3::Z);
    h.run_steps(3);
    arrow_drag(&mut h, egui::Modifiers::NONE, -35.0);
    let v: f64 = arrow_text(&h).parse().unwrap();
    assert!(v != 10.0 && mult_of(v, 1.0), "plain: {v}");
    arrow_drag(&mut h, egui::Modifiers::SHIFT, -50.0);
    let v: f64 = arrow_text(&h).parse().unwrap();
    assert!(mult_of(v, 10.0), "shift: {v}");
    arrow_drag(&mut h, egui::Modifiers::CTRL, 21.0);
    let v: f64 = arrow_text(&h).parse().unwrap();
    assert!(mult_of(v, 0.1), "ctrl: {v}");
    // Enter still commits what the arrow set
    press_enter(&mut h);
    assert_eq!(datum_kinds(&h).len(), 1);
}

// ------------------------------------------------------------ history: select / delete

mod hist {
    use super::*;
    use easycad_app::hist_sel::HistItem;
    use easycad_model::feature::{BoolOp, FeatureId, FeatureKind, ProfileSel, SketchId};
    use easycad_model::sketch::Sketch;

    fn two_blocks() -> (easycad_model::document::Document, SketchId, FeatureId, FeatureId) {
        let mut doc = easycad_model::document::Document::new("hist");
        let mut s = Sketch::new("S1", easycad_model::geom::Plane::XY);
        s.rect(glam::DVec2::new(-30.0, -20.0), glam::DVec2::new(30.0, 20.0));
        let sid = doc.add_sketch(s);
        let k = |sk, op, target| FeatureKind::Extrude { sketch: sk, profiles: ProfileSel::All, distance: 20.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op, target };
        let a = doc.add_feature("Block", k(sid, BoolOp::NewBody, None));
        let mut s2 = Sketch::new("S2", easycad_model::geom::Plane::XY);
        s2.rect(glam::DVec2::new(100.0, 100.0), glam::DVec2::new(120.0, 120.0));
        let sid2 = doc.add_sketch(s2);
        let b = doc.add_feature("Block2", k(sid2, BoolOp::NewBody, None));
        (doc, sid, a, b)
    }

    fn chip(h: &Harness<'static, EasyCadApp>, it: HistItem) -> egui::Rect {
        h.state().hist_rects.iter().find(|(i, _)| *i == it).map(|(_, r)| *r).expect("chip drawn")
    }

    fn click_chip(h: &mut Harness<'static, EasyCadApp>, it: HistItem) {
        let p = chip(h, it).center();
        click_at(h, p, egui::PointerButton::Primary);
    }

    fn nfeat(h: &Harness<'static, EasyCadApp>) -> usize {
        h.state().doc.data.features.len()
    }

    #[test]
    fn click_then_delete_and_backspace_remove_a_feature_and_undo_restores() {
        let _g = serial();
        let (doc, _s, a, b) = two_blocks();
        let mut h = harness(doc);
        click_chip(&mut h, HistItem::Feature(b));
        assert_eq!(h.state().hist_sel, vec![HistItem::Feature(b)]);
        assert_eq!(h.state().rollback(), None, "a click selects; it no longer rolls back");
        h.key_press(egui::Key::Delete);
        h.run_steps(3);
        assert_eq!(nfeat(&h), 1);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(3);
        assert_eq!(nfeat(&h), 2, "Ctrl+Z restores");
        click_chip(&mut h, HistItem::Feature(a));
        h.key_press(egui::Key::Backspace);
        h.run_steps(3);
        assert!(h.state().doc.data.features.iter().all(|f| f.id != a), "Backspace deletes too");
    }

    #[test]
    fn dragging_the_golden_head_rolls_back_in_one_undo_step() {
        let _g = serial();
        let (doc, _s, a, b) = two_blocks();
        let mut h = harness(doc);
        let from = chip(&h, HistItem::Feature(b)).center();
        let to = egui::pos2(chip(&h, HistItem::Feature(a)).right() + 3.0, from.y);
        h.hover_at(from);
        h.step();
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        for i in 1..=10 {
            h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 10.0)));
            h.step();
        }
        h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
        assert_eq!(h.state().rollback(), Some(1), "the head moved to the gap after the first feature");
        assert!(h.state().hist_sel.is_empty(), "a drag is not a click");
        assert_eq!(h.state().doc.undo_label(), Some("Roll back to 1"));
        h.state_mut().undo();
        assert_eq!(h.state().rollback(), None, "one undo step per drag");
    }

    #[test]
    fn alt_arrows_nudge_the_head() {
        let _g = serial();
        let (doc, _s, _a, _b) = two_blocks();
        let mut h = harness(doc);
        h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::ALT, egui::Key::ArrowLeft);
        h.run_steps(3);
        assert_eq!(h.state().rollback(), Some(1));
        h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::ALT, egui::Key::ArrowRight);
        h.run_steps(3);
        assert_eq!(h.state().rollback(), None);
    }

    #[test]
    fn hovering_undo_previews_the_diff_in_the_history() {
        let _g = serial();
        let (doc, _s, _a, _b) = two_blocks();
        let mut h = harness(doc);
        assert!(h.state().hist_diff.is_none());
        // an edit made through the app, so there is something to undo
        let x = h.state_mut().doc.add_feature("Extra", easycad_model::feature::FeatureKind::ImportStep { import: easycad_model::feature::ImportId(1), reference: false, translate: glam::DVec3::ZERO });
        h.run_steps(2);
        h.state_mut().hist_peek = Some(easycad_app::history_ui::HistPeek::Undo);
        h.step();
        let d = h.state().hist_diff.clone().expect("a diff is previewed");
        assert_eq!(d.removed_features, vec![x], "undo would remove the last feature");
        h.run_steps(2);
        assert!(h.state().hist_diff.is_none(), "it stops with the hover");
    }

    #[test]
    fn branch_here_then_switch_back_restores_the_features() {
        use easycad_app::history_ui::{apply_branch, BranchAct};
        let _g = serial();
        let (doc, _s, _a, _b) = two_blocks();
        let mut h = harness(doc);
        apply_branch(h.state_mut(), BranchAct::Here(1));
        settle(&mut h);
        assert_eq!(nfeat(&h), 1);
        assert_eq!(h.state().doc.data.branch, "Try 1");
        let p = row_label(&h, "main").center();
        click_at(&mut h, p, egui::PointerButton::Primary);
        settle(&mut h);
        assert_eq!(nfeat(&h), 2, "clicking the tab switches back");
        assert_eq!(h.state().doc.data.branch, "main");
        apply_branch(h.state_mut(), BranchAct::Switch("Try 1".into()));
        apply_branch(h.state_mut(), BranchAct::BackToSplit);
        settle(&mut h);
        assert_eq!(h.state().rollback(), Some(1), "back to the split rolls the head to it");
    }

    fn row_label(h: &Harness<'static, EasyCadApp>, name: &str) -> egui::Rect {
        h.state().hist_row_rects.iter().find(|(n, _)| n == name).map(|(_, r)| *r).unwrap_or_else(|| panic!("no row {name}"))
    }

    fn drag(h: &mut Harness<'static, EasyCadApp>, from: egui::Pos2, to: egui::Pos2) {
        h.hover_at(from);
        h.step();
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        for i in 1..=10 {
            h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 10.0)));
            h.step();
        }
        h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
    }

    #[test]
    fn rewind_to_gap_zero_is_an_empty_model() {
        let _g = serial();
        let (doc, _s, a, b) = two_blocks();
        let mut h = harness(doc);
        let from = chip(&h, HistItem::Feature(b)).center();
        let to = egui::pos2(chip(&h, HistItem::Feature(a)).left() - 3.0, from.y);
        drag(&mut h, from, to);
        settle(&mut h);
        assert_eq!(h.state().rollback(), Some(0), "the head went before the first feature");
        assert_eq!(h.state().head_feature(), None);
        let m = h.state().model.as_ref().unwrap();
        assert!(m.result.report(a).is_none() && m.result.report(b).is_none(), "nothing is computed");
        assert_eq!(m.bodies.iter().filter(|b| !b.body.reference).count(), 0, "an empty model");
        assert!(easycad_app::ui_main::status_text(h.state()).contains("start"), "status says so");
        h.state_mut().undo();
        assert_eq!(h.state().rollback(), None, "one undo step");
        // Ctrl+Alt+arrows reach gap 0 too
        for _ in 0..2 {
            h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::ALT, egui::Key::ArrowLeft);
            h.run_steps(3);
        }
        assert_eq!(h.state().rollback(), Some(0));
    }

    #[test]
    fn rewind_to_just_after_a_sketch_keeps_the_sketch_live() {
        let _g = serial();
        let (doc, _s, a, b) = two_blocks();
        let s2 = doc.data.sketches[1].id;
        let mut h = harness(doc);
        assert!(!h.state().view().sketch_visible(s2), "consumed: hidden at first");
        let r = chip(&h, HistItem::Sketch(s2));
        click_at(&mut h, r.center(), egui::PointerButton::Secondary);
        h.get_by_label("Roll back to here").click();
        settle(&mut h);
        assert_eq!(h.state().rollback(), Some(1), "its extrude and everything later is rolled back");
        let m = h.state().model.as_ref().unwrap();
        assert!(m.result.report(a).is_some(), "the first extrude is live");
        assert!(m.result.report(b).is_none(), "the second is rolled back");
        assert!(h.state().view().sketch(s2).is_some());
        assert!(h.state().view().sketch_visible(s2), "the sketch stays visible");
        // the head cursor sits right of the sketch chip, and the chip is still drawn live
        assert!(chip(&h, HistItem::Sketch(s2)).right() < chip(&h, HistItem::Feature(b)).left());
        // double-click edits it
        let p = chip(&h, HistItem::Sketch(s2)).center();
        click_at(&mut h, p, egui::PointerButton::Primary);
        click_at(&mut h, p, egui::PointerButton::Primary);
        assert!(matches!(h.state().mode, easycad_app::app::Mode::Sketch(_)));
    }

    #[test]
    fn a_branch_grows_the_panel_and_discarding_it_shrinks_it_back() {
        use easycad_app::history_ui::{apply_branch, BranchAct};
        let _g = serial();
        let (doc, _s, _a, _b) = two_blocks();
        let mut h = harness(doc);
        let h1 = h.state().hist_view.height;
        assert_eq!(h1, easycad_app::history_tree::BASE_H, "one trunk: the base height");
        assert!(h.state().hist_row_rects.is_empty() || h.state().hist_row_rects.len() == 1);
        apply_branch(h.state_mut(), BranchAct::Here(1));
        settle(&mut h);
        assert!(h.state().hist_view.height > h1, "taller with two rows");
        assert_eq!(h.state().hist_row_rects.len(), 2, "two rows render");
        let (live, stored) = (row_label(&h, "Try 1"), row_label(&h, "main"));
        assert!(stored.top() > live.top(), "the stored branch is a row below the trunk");
        save(&mut h, "history_tree");
        apply_branch(h.state_mut(), BranchAct::Discard("main".into()));
        settle(&mut h);
        assert_eq!(h.state().hist_view.height, h1, "shrinks back to the single trunk");
    }

    #[test]
    fn dragging_empty_space_pans_and_leaves_the_head_alone() {
        let _g = serial();
        let (doc, _s, _a, b) = two_blocks();
        let mut h = harness(doc);
        let c = chip(&h, HistItem::Feature(b));
        let before = h.state().hist_view.pan;
        let from = egui::pos2(c.right() + 200.0, c.center().y);
        drag(&mut h, from, from + egui::vec2(-60.0, 7.0));
        let after = h.state().hist_view.pan;
        assert!((after.x - before.x + 60.0).abs() < 1.5 && (after.y - before.y - 7.0).abs() < 1.5, "{before:?} -> {after:?}");
        assert_eq!(h.state().rollback(), None, "the head did not move");
        assert!((chip(&h, HistItem::Feature(b)).left() - (c.left() - 60.0)).abs() < 1.5, "the chips moved with the pan");
    }

    #[test]
    fn ctrl_wheel_zooms_clamped_and_fit_recentres() {
        let _g = serial();
        let (doc, _s, _a, b) = two_blocks();
        let mut h = harness(doc);
        let c = chip(&h, HistItem::Feature(b));
        let p = egui::pos2(c.right() + 100.0, c.center().y);
        h.hover_at(p);
        h.step();
        for _ in 0..30 {
            h.event_modifiers(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, 80.0), modifiers: egui::Modifiers::COMMAND, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown }, egui::Modifiers::COMMAND);
            h.step();
        }
        let z = h.state().hist_view.zoom;
        assert!(z > 1.5 && z <= easycad_app::history_tree::Z_MAX + 1e-4, "zoomed in and clamped: {z}");
        // plain wheel scrolls sideways
        let px = h.state().hist_view.pan.x;
        h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -40.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown });
        h.run_steps(3);
        assert!(h.state().hist_view.pan.x < px, "wheel scrolls horizontally");
        drag(&mut h, p, p + egui::vec2(0.0, 5.0));
        h.get_by_label("Fit").click();
        h.run_steps(3);
        let v = h.state().hist_view.clone();
        assert_eq!(v.zoom, 1.0);
        assert_eq!(v.pan.y, 0.0);
        let hc = chip(&h, HistItem::Feature(b));
        assert!((hc.right() + 3.0 - 700.0 - 12.0).abs() < 140.0, "head is about centred: {hc:?}");
    }

    #[test]
    fn multi_select_deletes_as_one_undo_step() {
        let _g = serial();
        let (doc, _s, a, b) = two_blocks();
        let mut h = harness(doc);
        click_chip(&mut h, HistItem::Feature(a));
        let p = chip(&h, HistItem::Feature(b)).center();
        h.hover_at(p);
        h.step();
        for pressed in [true, false] {
            h.event_modifiers(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::COMMAND }, egui::Modifiers::COMMAND);
            h.step();
        }
        h.run_steps(2);
        assert_eq!(h.state().hist_sel.len(), 2);
        h.key_press(egui::Key::Delete);
        h.run_steps(3);
        assert_eq!(nfeat(&h), 0);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(3);
        assert_eq!(nfeat(&h), 2, "one Ctrl+Z restores both");
    }

    #[test]
    fn deleting_a_sketch_with_a_consumer_asks_first() {
        let _g = serial();
        let (doc, s, a, _b) = two_blocks();
        let mut h = harness(doc);
        click_chip(&mut h, HistItem::Sketch(s));
        assert_eq!(h.state().hist_sel, vec![HistItem::Sketch(s)]);
        h.key_press(egui::Key::Delete);
        h.run_steps(3);
        assert!(h.state().delete_ask.is_some(), "popup");
        assert_eq!(nfeat(&h), 2);
        h.get_by_label("Delete all").click();
        h.run_steps(3);
        assert!(h.state().doc.data.features.iter().all(|f| f.id != a));
        assert!(h.state().doc.data.sketch(s).is_none());
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(3);
        assert_eq!(nfeat(&h), 2);
        // only this: the extrude stays and reports a problem
        h.state_mut().hist_sel = vec![HistItem::Sketch(s)];
        h.key_press(egui::Key::Delete);
        h.run_steps(3);
        h.get_by_label("Delete only this").click();
        h.run_steps(3);
        settle(&mut h);
        assert_eq!(nfeat(&h), 2);
        assert!(h.state().doc.data.sketch(s).is_none());
        assert!(!h.state().model.as_ref().unwrap().result.errors().is_empty(), "the extrude reports a problem");
    }

    #[test]
    fn text_focus_does_not_delete() {
        let _g = serial();
        let (doc, _s, _a, b) = two_blocks();
        let mut h = harness(doc);
        click_chip(&mut h, HistItem::Feature(b));
        h.state_mut().params.adding = Some(("zz".into(), "1".into()));
        h.run_steps(3);
        h.get_all_by_role(egui::accesskit::Role::TextInput).next().expect("a text field").click();
        h.run_steps(3);
        assert!(h.ctx.text_edit_focused());
        h.key_press(egui::Key::Backspace);
        h.run_steps(3);
        assert_eq!(nfeat(&h), 2, "Backspace in a text field edits text, not history");
    }

    #[test]
    fn head_chip_and_rollback_marker() {
        let _g = serial();
        let (doc, _s, a, b) = two_blocks();
        let mut h = harness(doc);
        assert_eq!(h.state().head_feature(), Some(b), "head is the last feature when not rolled back");
        h.state_mut().set_rollback(Some(1));
        h.run_steps(3);
        assert_eq!(h.state().head_feature(), Some(a));
        // chip menu: roll back to here on the last chip rolls forward again
        h.state_mut().set_rollback(None);
        h.run_steps(3);
        let r = chip(&h, HistItem::Feature(a));
        click_at(&mut h, r.center(), egui::PointerButton::Secondary);
        h.get_by_label("Roll back to here").click();
        h.run_steps(3);
        assert_eq!(h.state().rollback(), Some(1));
        assert_eq!(h.state().head_feature(), Some(a));
    }

    #[test]
    fn sketch_chip_double_click_opens_the_sketch() {
        let _g = serial();
        let (doc, s, _a, _b) = two_blocks();
        let mut h = harness(doc);
        let p = chip(&h, HistItem::Sketch(s)).center();
        click_at(&mut h, p, egui::PointerButton::Primary);
        click_at(&mut h, p, egui::PointerButton::Primary);
        h.run_steps(3);
        assert!(matches!(h.state().mode, easycad_app::app::Mode::Sketch(_)));
    }

    #[test]
    fn sketch_lines_in_3d_select_and_double_click_opens() {
        let _g = serial();
        let mut doc = easycad_model::document::Document::new("lines");
        let mut s = Sketch::new("Loose", easycad_model::geom::Plane::XY);
        s.rect(glam::DVec2::new(-30.0, -20.0), glam::DVec2::new(30.0, 20.0));
        let sid = doc.add_sketch(s);
        let mut h = harness(doc);
        top_view(&mut h);
        let p = screen_of(&h, glam::Vec3::new(0.0, 20.0, 0.0));
        click_at(&mut h, p, egui::PointerButton::Primary);
        assert_eq!(h.state().hist_sel, vec![HistItem::Sketch(sid)]);
        click_at(&mut h, p, egui::PointerButton::Primary);
        h.run_steps(3);
        assert!(matches!(h.state().mode, easycad_app::app::Mode::Sketch(_)));
    }
}

#[test]
fn dimension_tool_centre_to_centre_orientation_follows_placement() {
    let _g = serial();
    use easycad_app::app::Mode;
    use easycad_app::sketch_mode::{DimOrient, Tool};
    use easycad_model::sketch::Constraint;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear();
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None);
    h.run_steps(3);
    let (a, b) = (egui::pos2(600.0, 450.0), egui::pos2(800.0, 400.0));
    for c in [a, b] {
        if let Mode::Sketch(s) = &mut h.state_mut().mode {
            s.tool = Tool::Circle;
        }
        click_at(&mut h, c, egui::PointerButton::Primary);
        h.run_steps(3);
        h.event(egui::Event::Text("20".into()));
        h.run_steps(3);
        press_enter(&mut h);
    }
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.tool = Tool::Dimension;
    }
    h.run_steps(2);
    // pick both centres (no edge there: the circles' centres are anchors)
    hover_click(&mut h, a);
    hover_click(&mut h, b);
    let sk = |h: &Harness<'static, EasyCadApp>| match &h.state().mode {
        Mode::Sketch(s) => (s.dim_pending, s.input.clone(), s.dim_orient, s.sketch.constraints.clone()),
        _ => panic!(),
    };
    let (pending, input, _, _) = sk(&h);
    assert!(pending && input.is_none(), "two anchors wait for a placement click");
    // above the pair, between them: horizontal
    hover_click(&mut h, egui::pos2(700.0, 250.0));
    let (pending, input, orient, _) = sk(&h);
    assert!(!pending && input.is_some());
    assert_eq!(orient, DimOrient::Horizontal);
    h.event(egui::Event::Text("100".into()));
    h.run_steps(3);
    press_enter(&mut h);
    let (_, input, _, cs) = sk(&h);
    assert!(input.is_none());
    assert!(cs.iter().any(|c| matches!(c, Constraint::DistanceX(..))), "{cs:?}");
}

#[test]
fn dimension_tool_circle_centre_to_sketch_origin_makes_a_distance() {
    let _g = serial();
    use easycad_app::app::Mode;
    use easycad_app::sketch_mode::Tool;
    use easycad_model::sketch::Constraint;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear();
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None);
    h.run_steps(3);
    let a = egui::pos2(560.0, 380.0);
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.tool = Tool::Circle;
        s.snap = false;
    }
    click_at(&mut h, a, egui::PointerButton::Primary);
    h.run_steps(3);
    h.event(egui::Event::Text("20".into()));
    h.run_steps(3);
    press_enter(&mut h);
    // where the origin is on screen: the centre was placed unsnapped under `a`
    let origin = match &h.state().mode {
        Mode::Sketch(s) => {
            let c = match s.sketch.entities[0].geom {
                easycad_model::sketch::Geom::Circle { center, .. } => s.sketch.point(center),
                _ => panic!(),
            };
            assert!(c.length() > 1.0, "centre is away from the origin");
            egui::pos2(a.x - (c.x as f32) * s.scale, a.y + (c.y as f32) * s.scale)
        }
        _ => panic!(),
    };
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.tool = Tool::Dimension;
    }
    h.run_steps(2);
    hover_click(&mut h, a);
    hover_click(&mut h, origin);
    let (pending, input) = match &h.state().mode {
        Mode::Sketch(s) => (s.dim_pending, s.input.clone()),
        _ => panic!(),
    };
    assert!(pending && input.is_none(), "centre + origin wait for a placement click");
    // beside the pair: vertical or aligned, any placement works; pick aligned (below-left diagonal)
    hover_click(&mut h, egui::pos2((a.x + origin.x) / 2.0 + 60.0, (a.y + origin.y) / 2.0 - 60.0));
    let input = match &h.state().mode {
        Mode::Sketch(s) => s.input.clone(),
        _ => panic!(),
    };
    assert!(input.is_some(), "the value box opens");
    h.event(egui::Event::Text("55".into()));
    h.run_steps(3);
    press_enter(&mut h);
    match &h.state().mode {
        Mode::Sketch(s) => {
            let cs = &s.sketch.constraints;
            assert!(
                cs.iter().any(|c| matches!(
                    c,
                    Constraint::Distance(..) | Constraint::DistanceX(..) | Constraint::DistanceY(..)
                )),
                "{cs:?}"
            );
            assert!(cs.iter().any(|c| matches!(c, Constraint::Fix(_, v) if v.length() == 0.0)), "origin point fixed");
            assert!(s.input.is_none());
        }
        _ => panic!(),
    }
}

#[test]
fn m_key_opens_move_gizmo_esc_restores_and_ctrl_z_undoes() {
    let _g = serial();
    use easycad_app::app::Mode;
    use easycad_app::sketch_mode::Sel;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear();
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None);
    h.run_steps(3);
    let e = if let Mode::Sketch(sk) = &mut h.state_mut().mode {
        let e = sk.add_line((glam::DVec2::ZERO, None), (glam::DVec2::new(5.0, 0.0), None));
        sk.selection = vec![Sel::Entity(e)];
        e
    } else {
        panic!()
    };
    h.run_steps(2);
    let start = |h: &Harness<'static, EasyCadApp>| match &h.state().mode {
        Mode::Sketch(s) => {
            let easycad_model::sketch::Geom::Line { a, .. } = s.sketch.entities[e.0 as usize].geom else { panic!() };
            (s.mv.is_some(), s.sketch.point(a))
        }
        _ => panic!(),
    };
    chord(&mut h, egui::Modifiers::NONE, egui::Key::M);
    assert!(start(&h).0, "M opens the gizmo");
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.apply_move_text("4", "2");
    }
    assert_eq!(start(&h).1, glam::DVec2::new(4.0, 2.0));
    chord(&mut h, egui::Modifiers::NONE, egui::Key::Escape);
    assert_eq!(start(&h), (false, glam::DVec2::ZERO), "Esc restores");
    chord(&mut h, egui::Modifiers::NONE, egui::Key::M);
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.apply_move_text("4", "2");
        s.commit_move();
    }
    assert_eq!(start(&h).1, glam::DVec2::new(4.0, 2.0));
    chord(&mut h, egui::Modifiers::COMMAND, egui::Key::Z);
    assert_eq!(start(&h).1, glam::DVec2::ZERO, "Ctrl+Z undoes the move");
}

#[test]
fn line_after_an_angled_one_locks_exactly_horizontal() {
    let _g = serial();
    use easycad_app::app::Mode;
    use easycad_app::sketch_mode::Tool;
    use easycad_model::sketch::Geom;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear();
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None);
    h.run_steps(3);
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.tool = Tool::Line;
    }
    let p = egui::PointerButton::Primary;
    click_at(&mut h, egui::pos2(700.0, 450.0), p);
    h.run_steps(3);
    // an angled first line, ending off the grid
    click_at(&mut h, egui::pos2(823.0, 391.0), p);
    h.run_steps(3);
    // the second ends a few px off the horizontal through that end
    click_at(&mut h, egui::pos2(950.0, 394.0), p);
    h.run_steps(3);
    let Mode::Sketch(s) = &h.state().mode else { panic!() };
    let ys: Vec<(f64, f64)> = s
        .sketch
        .entities
        .iter()
        .filter_map(|e| match e.geom {
            Geom::Line { a, b } => Some((s.sketch.point(a).y, s.sketch.point(b).y)),
            _ => None,
        })
        .collect();
    assert_eq!(ys.len(), 2, "{ys:?}");
    assert_eq!(ys[1].0, ys[1].1, "second line exactly horizontal: {ys:?}");
    assert_ne!(ys[0].0, ys[0].1, "first stays angled");
}

// ---- inline dimension editing -------------------------------------------------

fn double_click_at(h: &mut Harness<'static, EasyCadApp>, p: egui::Pos2) {
    h.hover_at(p);
    h.run_steps(2);
    for pressed in [true, false, true, false] {
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(3);
}

/// Two 20 mm circles; the Dimension tool picks both centres and a placement click
/// above them: the inline field is open on a new horizontal distance.
fn two_circles_pending_dimension() -> (Harness<'static, EasyCadApp>, egui::Pos2, egui::Pos2) {
    use easycad_app::app::Mode;
    use easycad_app::sketch_mode::Tool;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear();
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None);
    h.run_steps(3);
    let (a, b) = (egui::pos2(600.0, 450.0), egui::pos2(800.0, 400.0));
    for c in [a, b] {
        if let Mode::Sketch(s) = &mut h.state_mut().mode {
            s.tool = Tool::Circle;
        }
        click_at(&mut h, c, egui::PointerButton::Primary);
        h.run_steps(3);
        h.event(egui::Event::Text("20".into()));
        h.run_steps(3);
        press_enter(&mut h);
    }
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.tool = Tool::Dimension;
    }
    h.run_steps(2);
    hover_click(&mut h, a);
    hover_click(&mut h, b);
    hover_click(&mut h, egui::pos2(700.0, 250.0));
    (h, a, b)
}

fn sk_state(h: &Harness<'static, EasyCadApp>) -> (Option<String>, Option<usize>, Vec<easycad_model::sketch::Constraint>) {
    match &h.state().mode {
        easycad_app::app::Mode::Sketch(s) => (s.input.clone(), s.edit_dim, s.sketch.constraints.clone()),
        _ => panic!(),
    }
}

#[test]
fn dimension_value_is_an_inline_field_not_a_window() {
    let _g = serial();
    use easycad_model::sketch::Constraint;
    let (mut h, _, _) = two_circles_pending_dimension();
    let (input, _, _) = sk_state(&h);
    assert!(input.is_some(), "the inline field is open after placement");
    h.run_steps(2);
    // focused, and no window with a title: only the foreground area
    let ctx = h.ctx.clone();
    let id = egui::Id::NULL.with("sk_input_text");
    assert!(ctx.memory(|m| m.has_focus(id)), "the field has focus");
    assert!(h.query_by_label("value").is_none(), "no 'value' window caption");
    h.event(egui::Event::Text("100".into()));
    h.run_steps(3);
    press_enter(&mut h);
    let (input, _, cs) = sk_state(&h);
    assert!(input.is_none());
    assert!(cs.iter().any(|c| matches!(c, Constraint::DistanceX(..))), "{cs:?}");
}

#[test]
fn escape_on_a_new_dimension_leaves_no_constraint() {
    let _g = serial();
    let (mut h, _, _) = two_circles_pending_dimension();
    let before = sk_state(&h).2.len();
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    let (input, _, cs) = sk_state(&h);
    assert!(input.is_none());
    assert_eq!(cs.len(), before);
}

#[test]
fn invalid_dimension_text_keeps_the_field_open() {
    let _g = serial();
    let (mut h, _, _) = two_circles_pending_dimension();
    let before = sk_state(&h).2.len();
    h.event(egui::Event::Text("nonsense_param".into()));
    h.run_steps(3);
    press_enter(&mut h);
    let (input, _, cs) = sk_state(&h);
    assert!(input.is_some(), "an invalid value keeps the field open");
    assert_eq!(cs.len(), before);
    match &h.state().mode {
        easycad_app::app::Mode::Sketch(s) => assert!(s.input_err.is_some()),
        _ => panic!(),
    }
}

#[test]
fn double_click_on_the_dimension_line_edits_inline() {
    let _g = serial();
    use easycad_model::sketch::Constraint;
    let (mut h, _, _) = two_circles_pending_dimension();
    h.event(egui::Event::Text("100".into()));
    h.run_steps(3);
    press_enter(&mut h);
    h.run_steps(3);
    // a point on the dimension line, well away from its label
    let p = match &h.state().mode {
        easycad_app::app::Mode::Sketch(s) => {
            let segs = s.dim_segs.borrow();
            let seg = segs.first().expect("dimension line recorded").0;
            let label = s.dim_rects.borrow()[0].0;
            let mid = seg[0] + (seg[1] - seg[0]) * 0.5;
            let dir = (seg[1] - seg[0]).normalized();
            let q = mid + dir * (label.width() / 2.0 + 25.0);
            assert!(!label.contains(q));
            q
        }
        _ => panic!(),
    };
    double_click_at(&mut h, p);
    let (input, edit, _) = sk_state(&h);
    assert!(edit.is_some() && input.is_some(), "double-click on the line opens the inline edit");
    assert_eq!(input.as_deref(), Some("100"));
    h.event(egui::Event::Text("77".into()));
    h.run_steps(3);
    press_enter(&mut h);
    let (input, edit, cs) = sk_state(&h);
    assert!(input.is_none() && edit.is_none());
    let ok = cs.iter().any(|c| matches!(c, Constraint::DistanceX(_, _, e) if e.0.trim() == "77"));
    assert!(ok, "{cs:?}");
}

// ---- angle between two lines / lone length (Fusion style) --------------------

/// A chain of two lines A-B-C drawn with the Line tool (they share B), then the
/// Dimension tool.
fn two_lines_dimension_tool() -> Harness<'static, EasyCadApp> {
    use easycad_app::app::Mode;
    use easycad_app::sketch_mode::Tool;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().recovery_offer.clear();
    h.state_mut().start_sketch_on(easycad_model::geom::Plane::XY, None);
    h.run_steps(3);
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.tool = Tool::Line;
    }
    h.run_steps(2);
    for p in [egui::pos2(700.0, 500.0), egui::pos2(700.0, 300.0), egui::pos2(850.0, 400.0)] {
        hover_click(&mut h, p);
    }
    if let Mode::Sketch(s) = &mut h.state_mut().mode {
        s.cancel_input();
        s.clicks.clear();
        s.tool = Tool::Dimension;
    }
    h.run_steps(3);
    h
}

#[test]
fn click_line_then_line_then_placement_makes_an_angle_with_inline_field() {
    let _g = serial();
    use easycad_model::sketch::Constraint;
    let mut h = two_lines_dimension_tool();
    hover_click(&mut h, egui::pos2(700.0, 330.0)); // first line (clear of its midpoint and the origin)
    let (input, _, cs) = sk_state(&h);
    assert!(input.is_none() && !cs.iter().any(|c| c.dim_expr().is_some()), "a lone pick waits");
    hover_click(&mut h, egui::pos2(812.0, 375.0)); // second line
    match &h.state().mode {
        easycad_app::app::Mode::Sketch(s) => assert!(s.dim_pending, "two lines wait for the angle placement click"),
        _ => panic!(),
    }
    hover_click(&mut h, egui::pos2(740.0, 360.0)); // inside the angle
    let (input, _, _) = sk_state(&h);
    assert!(input.is_some(), "the inline field opens");
    h.event(egui::Event::Text("45".into()));
    h.run_steps(3);
    press_enter(&mut h);
    let (input, _, cs) = sk_state(&h);
    assert!(input.is_none());
    assert!(cs.iter().any(|c| matches!(c, Constraint::AngleRays(..))), "{cs:?}");
}

#[test]
fn click_line_then_empty_space_makes_a_length() {
    let _g = serial();
    use easycad_model::sketch::Constraint;
    let mut h = two_lines_dimension_tool();
    hover_click(&mut h, egui::pos2(700.0, 330.0));
    let (input, _, _) = sk_state(&h);
    assert!(input.is_none());
    hover_click(&mut h, egui::pos2(500.0, 600.0)); // empty space
    let (input, _, _) = sk_state(&h);
    assert!(input.is_some(), "the inline length field opens");
    h.event(egui::Event::Text("40".into()));
    h.run_steps(3);
    press_enter(&mut h);
    let (_, _, cs) = sk_state(&h);
    assert!(cs.iter().any(|c| matches!(c, Constraint::Length(..))), "{cs:?}");
}

// ------------------------------------------------------------ region picking

mod regions {
    use super::*;
    use easycad_model::feature::ProfileSel;
    use easycad_model::sketch::Sketch;
    use glam::DVec2;

    fn ring_doc() -> easycad_model::document::Document {
        let mut doc = easycad_model::document::Document::new("ring");
        let mut s = Sketch::new("Ring", easycad_model::geom::Plane::XY);
        s.circle(DVec2::ZERO, 9.8);
        s.circle(DVec2::ZERO, 11.4);
        for a in [0.0f64, 120.0, 240.0] {
            let d = DVec2::new(a.to_radians().cos(), a.to_radians().sin());
            s.line(d * 9.0, d * 13.0);
        }
        doc.add_sketch(s);
        doc
    }

    fn sel(h: &Harness<'static, EasyCadApp>) -> ProfileSel {
        h.state().dialog.as_ref().and_then(|d| d.region_target()).map(|(_, s)| s.clone()).expect("extrude dialog open")
    }

    #[test]
    fn click_regions_of_a_ring_toggles_them_in_the_extrude_dialog() {
        let _g = serial();
        let mut h = harness(ring_doc());
        top_view(&mut h);
        open_extrude(&mut h);
        assert_eq!(sel(&h), ProfileSel::All, "default is All");
        let sk = h.state().doc.data.sketches[0].sketch.clone();
        let regions = sk.regions();
        assert_eq!(regions.len(), 4, "disc + three ring sectors");
        let sectors: Vec<_> = regions.iter().filter(|r| r.filled).collect();
        assert_eq!(sectors.len(), 3);
        fn at(h: &Harness<'static, EasyCadApp>, p: DVec2) -> egui::Pos2 {
            screen_of(h, glam::Vec3::new(p.x as f32, p.y as f32, 0.0))
        }
        // click one sector: All -> the other two
        let p0 = at(&h, sectors[0].sample);
        hover_click(&mut h, p0);
        match sel(&h) {
            ProfileSel::Containing(v) => assert_eq!(v.len(), 2),
            o => panic!("{o:?}"),
        }
        // click it again: back to three
        hover_click(&mut h, p0);
        match sel(&h) {
            ProfileSel::Containing(v) => assert_eq!(v.len(), 3),
            o => panic!("{o:?}"),
        }
        // the disc is not part of All but can be picked
        let disc = regions.iter().find(|r| !r.filled).unwrap();
        let pd = at(&h, disc.sample + DVec2::new(4.0, 3.0)); // not the centre: the drag arrow sits there
        hover_click(&mut h, pd);
        match sel(&h) {
            ProfileSel::Containing(v) => assert_eq!(v.len(), 4),
            o => panic!("{o:?}"),
        }
        // OK writes the explicit regions
        click_ok(&mut h);
        let f = h.state().doc.data.features.last().unwrap().clone();
        match f.kind {
            easycad_model::feature::FeatureKind::Extrude { profiles: ProfileSel::Containing(v), .. } => assert_eq!(v.len(), 4),
            o => panic!("{o:?}"),
        }
    }
}

#[test]
fn dialog_sketch_is_drawn_while_extrude_dialog_is_open() {
    use easycad_model::feature::{BoolOp, FeatureKind, ProfileSel};
    use easycad_model::geom::Plane;
    use easycad_model::sketch::Sketch;
    let _serial = serial();
    let mut doc = easycad_app::sample::sensor_node();
    let mut s = Sketch::new("S", Plane::XY);
    s.rect(glam::DVec2::ZERO, glam::DVec2::new(40.0, 20.0));
    let sid = doc.add_sketch(s);
    let fid = doc.add_feature(
        "Block",
        FeatureKind::Extrude {
            sketch: sid,
            profiles: ProfileSel::All,
            distance: 5.0.into(),
            symmetric: false,
            reverse: false,
            draft_deg: 0.0.into(),
            op: BoolOp::NewBody,
            target: None,
        },
    );
    let mut h = harness(doc);
    h.run_steps(3);
    assert!(!has(h.state(), sid), "consumed sketch hidden");
    // edit the extrude from history: its sketch shows, whatever its stored visibility
    h.state_mut().edit_feature(fid);
    h.run_steps(3);
    assert!(
        has(h.state(), sid),
        "sketch drawn while its extrude is edited"
    );
    assert!(easycad_app::sketch_view::dialog_sketches(h.state()).contains(&sid));
    h.state_mut().close_dialog(false);
    h.run_steps(3);
    assert!(!has(h.state(), sid), "hidden again after Cancel");
    // an explicitly hidden sketch still shows for a new extrude, and the flag is untouched
    easycad_app::sketch_view::set_visible(h.state_mut(), sid, false);
    h.state_mut()
        .open_dialog(easycad_app::dialogs::DialogKind::Extrude);
    h.run_steps(3);
    assert!(has(h.state(), sid), "new extrude shows its sketch");
    h.state_mut().close_dialog(false);
    h.run_steps(3);
    assert!(!has(h.state(), sid));
    assert_eq!(
        h.state()
            .view()
            .sketches
            .iter()
            .find(|e| e.id == sid)
            .unwrap()
            .visible,
        Some(false)
    );
}

/// The face of the first body whose outward normal best matches `n`.
fn face_key(app: &EasyCadApp, n: glam::DVec3) -> easycad_app::pickstack::PickKey {
    use easycad_app::pickstack::{PickKey, PickKind};
    let m = app.model.as_ref().unwrap();
    let b = m.bodies.iter().find(|b| !b.body.reference).expect("body");
    let f = b.faces.iter().max_by(|a, c| a.normal.dot(n).total_cmp(&c.normal.dot(n))).unwrap();
    PickKey { body: b.body.id, kind: PickKind::Face(f.index) }
}

/// Fillet dialog open (faces and edges both pickable): a cursor a couple of pixels
/// from a top edge has that edge as the current candidate, not the face it borders;
/// mid-face the face is current.
#[test]
fn fillet_dialog_hover_near_an_edge_prefers_the_edge() {
    use easycad_app::pickstack::PickKind;
    let _serial = serial();
    let mut h = harness(box_doc());
    top_view(&mut h);
    h.state_mut().open_dialog(easycad_app::dialogs::DialogKind::Fillet);
    h.run_steps(2);
    let f = h.state().filter;
    assert!(f.faces && f.edges);
    // top edge at x = 30 (the block is -30..30 x -20..20 x 0..20): hover 2 px inside it
    let on = screen_of(&h, glam::Vec3::new(30.0, 0.0, 20.0));
    let p = egui::pos2(on.x - 2.0, on.y);
    h.hover_at(p);
    h.run_steps(3);
    let cur = h.state().stack.current().cloned().expect("a candidate under the cursor");
    assert!(matches!(cur.key.kind, PickKind::Edge(_)), "{:?} of {:?}", cur, h.state().stack.items);
    // mid-face: the face
    let mid = screen_of(&h, glam::Vec3::new(0.0, 0.0, 20.0));
    h.hover_at(mid);
    h.run_steps(3);
    let cur = h.state().stack.current().cloned().expect("a candidate under the cursor");
    assert!(matches!(cur.key.kind, PickKind::Face(_)), "{:?}", cur);
}

#[test]
fn fillet_dialog_takes_face_picks() {
    use easycad_model::feature::FeatureKind;
    use easycad_model::select::EdgeSelector;
    let _serial = serial();
    let mut h = harness(box_doc());
    let top = face_key(h.state(), glam::DVec3::Z);
    let bottom = face_key(h.state(), glam::DVec3::NEG_Z);
    // a face selected outside the dialog pre-fills it
    h.state_mut().selection = vec![top];
    h.state_mut().open_dialog(easycad_app::dialogs::DialogKind::Fillet);
    h.run_steps(2);
    assert_eq!(h.state().dialog.as_ref().unwrap().picked(), vec![top]);
    // the filter lets faces through; a click on the second face adds it, again removes it
    let f = h.state().filter;
    assert!(f.faces && f.edges);
    let model = h.state().model.clone();
    let d = h.state_mut().dialog.as_mut().unwrap();
    assert!(d.on_pick(bottom, model.as_deref()));
    assert_eq!(d.picked(), vec![top, bottom]);
    assert!(d.on_pick(bottom, model.as_deref()));
    assert!(d.on_pick(bottom, model.as_deref()));
    assert_eq!(d.picked(), vec![top, bottom]);
    click_ok(&mut h);
    let fillet = h.state().doc.data.features.iter().find_map(|f| match &f.kind {
        FeatureKind::Fillet { edges, .. } => Some(edges.clone()),
        _ => None,
    });
    let edges = fillet.expect("a fillet feature");
    assert_eq!(edges.len(), 2);
    assert!(edges.iter().all(|e| matches!(e, EdgeSelector::OfFace(_))));
}

/// `box_doc` with its (+x,+y) vertical corner edge filleted.
fn filleted_box_doc() -> easycad_model::document::Document {
    use easycad_model::feature::FeatureKind;
    use easycad_model::select::{EdgeKind, EdgeSelector};
    let mut doc = box_doc();
    let edge = EdgeSelector::Hint { midpoint: glam::DVec3::new(30.0, 20.0, 10.0), tangent: glam::DVec3::Z, kind: Some(EdgeKind::Line) };
    doc.add_feature("Fillet", FeatureKind::Fillet { body: None, edges: vec![edge], radius: 3.0.into(), auto_reduce: false, sets: vec![], skipped: vec![] });
    doc
}

#[test]
fn del_on_a_selected_face_adds_a_delete_face_feature() {
    use easycad_app::pickstack::{PickKey, PickKind};
    use easycad_model::feature::FeatureKind;
    use easycad_model::occt::SurfaceKind;
    let _serial = serial();
    let mut h = harness(filleted_box_doc());
    let m = h.state().model.clone().unwrap();
    let b = m.bodies.iter().find(|b| !b.body.reference).unwrap();
    let cyl = b.faces.iter().find(|f| f.kind == SurfaceKind::Cylinder).expect("fillet face");
    h.state_mut().selection = vec![PickKey { body: b.body.id, kind: PickKind::Face(cyl.index) }];
    let n = h.state().doc.data.features.len();
    h.key_press(egui::Key::Delete);
    settle(&mut h);
    let feats = &h.state().doc.data.features;
    assert_eq!(feats.len(), n + 1);
    assert!(matches!(feats.last().unwrap().kind, FeatureKind::DeleteFace { .. }));
    assert!(h.state().selection.is_empty());
    let m = h.state().model.clone().unwrap();
    let b = m.bodies.iter().find(|b| !b.body.reference).unwrap();
    assert_eq!(b.faces.len(), 6, "the sharp corner is back");
    // one undo step takes it away
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(h.state().doc.data.features.len(), n);
}

#[test]
fn a_failing_delete_face_leaves_nothing_behind_and_says_why() {
    use easycad_model::feature::FeatureKind;
    let _serial = serial();
    let mut h = harness(box_doc());
    let top = face_key(h.state(), glam::DVec3::Z);
    let n = h.state().doc.data.features.len();
    h.state_mut().selection = vec![top];
    h.key_press(egui::Key::Delete);
    settle(&mut h);
    let feats = &h.state().doc.data.features;
    assert_eq!(feats.len(), n);
    assert!(!feats.iter().any(|f| matches!(f.kind, FeatureKind::DeleteFace { .. })));
    assert!(h.state().toasts.iter().any(|t| t.warn && t.text.starts_with("Delete face failed")), "{:?}", h.state().toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
    assert!(h.state().doc.data.features.iter().all(|f| h.state().feature_status(f.id).map_or(true, |s| !matches!(s, easycad_model::regen::FeatureStatus::Error(_)))));
}

#[test]
fn right_click_a_face_then_delete_face_from_the_menu() {
    use easycad_model::feature::FeatureKind;
    use easycad_model::occt::SurfaceKind;
    let _serial = serial();
    let mut h = harness(filleted_box_doc());
    let m = h.state().model.clone().unwrap();
    let b = m.bodies.iter().find(|b| !b.body.reference).unwrap();
    let cyl = b.faces.iter().find(|f| f.kind == SurfaceKind::Cylinder).expect("fillet face");
    // look straight at the fillet strip and zoom in, so the cursor sits on the face, clear of its edges
    let n = cyl.normal.as_vec3().normalize();
    let up = if n.z.abs() > 0.9 { glam::Vec3::Y } else { glam::Vec3::Z };
    h.state_mut().camera.rot = easycad_app::camera::look_rot(n, up);
    h.state_mut().camera.target = cyl.centroid.as_vec3();
    h.state_mut().camera.distance *= 0.15;
    h.run_steps(3);
    // a few px off the strip's centre: the exact middle can fall on a seam between two triangles
    let p = screen_of(&h, cyl.centroid.as_vec3()) + egui::vec2(4.0, 3.0);
    assert!(h.state().selection.is_empty(), "nothing selected before the right-click");
    h.hover_at(p);
    h.run_steps(3);
    let n = h.state().doc.data.features.len();
    click_at(&mut h, p, egui::PointerButton::Secondary);
    h.run_steps(3);
    assert!(
        h.state().selection.iter().any(|k| matches!(k.kind, easycad_app::pickstack::PickKind::Face(f) if f == cyl.index)),
        "right-click selects the face under the cursor: {:?}",
        h.state().selection
    );
    h.get_by_label("Delete face   Del").click();
    settle(&mut h);
    let feats = &h.state().doc.data.features;
    assert_eq!(feats.len(), n + 1);
    assert!(matches!(feats.last().unwrap().kind, FeatureKind::DeleteFace { .. }));
}

/// Play loops through the positions until the view moves: a middle-drag orbit stops
/// it, and the model stays at the position it reached (no snap back).
#[test]
fn play_positions_until_the_view_moves() {
    use easycad_model::feature::{BodyId, BoolOp, FeatureKind, ProfileSel};
    use easycad_model::positions::{PoseMove, PoseTarget};
    let _serial = serial();
    let mut doc = easycad_model::document::Document::new("Cart");
    let mut s = easycad_model::sketch::Sketch::new("Pile", easycad_model::geom::Plane::XY);
    s.rect(glam::DVec2::new(-10.0, -10.0), glam::DVec2::new(10.0, 10.0));
    let s = doc.add_sketch(s);
    let pile = BodyId::of(doc.add_feature(
        "Pile",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 20.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ));
    let ids = doc
        .edit("positions", |d| {
            let mut ids = Vec::new();
            for (name, dz) in [("closed", "0"), ("peeking", "7"), ("very full", "14")] {
                ids.push(d.add_position(name));
                d.positions.last_mut().unwrap().moves = vec![PoseMove { translate: ["0".into(), "0".into(), dz.into()], ..PoseMove::new(PoseTarget::Body(pile)) }];
            }
            Ok::<_, ()>(ids)
        })
        .unwrap();
    let mut h = harness(doc);
    {
        let app = h.state_mut();
        app.positions.open = true;
        app.positions.dwell_s = 0.1;
        app.positions.travel_s = 0.1;
    }
    h.run_steps(2);
    // ▶ sits inline with the state numbers
    h.get_by_label("▶").click();
    h.run_steps(1);
    assert!(h.state().positions.play.is_some());
    // it visits every position, in order, and loops
    let mut seen = Vec::new();
    for _ in 0..400 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
        let shown = h.state().model.as_ref().and_then(|m| m.pose);
        if let Some(p) = shown {
            if seen.last() != Some(&p) {
                seen.push(p);
            }
        }
        if seen.len() >= 5 {
            break;
        }
    }
    assert!(seen.len() >= 5, "played {seen:?}");
    assert_eq!(seen[..4], [ids[0], ids[1], ids[2], ids[0]], "in list order, looping: {seen:?}");
    // orbit the view: playback stops where it is
    drag_view(&mut h, egui::PointerButton::Middle, egui::Modifiers::NONE, egui::vec2(120.0, 40.0));
    assert!(h.state().positions.play.is_none(), "a camera move stops Play");
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(h.state_mut().settle(Duration::from_secs(60)));
    let at = h.state().pose;
    assert!(at.is_some_and(|p| ids.contains(&p)), "it stays at a saved position, not as modeled: {at:?}");
    h.run_steps(20);
    assert_eq!(h.state().pose, at, "and stays there");
    assert_eq!(h.state().model.as_ref().unwrap().pose, at);
}

#[test]
fn geometry_arriving_ends_the_empty_scene_plane_pick() {
    let _g = serial();
    let mut h = harness(easycad_model::document::Document::new("empty"));
    h.key_press(egui::Key::S);
    h.run_steps(3);
    assert!(h.state().plane_pick.is_some(), "S asks for a plane");
    assert_eq!(easycad_app::plane_pick::draw_list(h.state()).len(), 3, "the origin planes are drawn");
    let dir = std::env::temp_dir().join(format!("easycad-clicks-planepick-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let step = dir.join("board.step");
    std::fs::write(&step, easycad_app::sample::pcb_step()).unwrap();
    h.state_mut().import_path(&step).expect("import the STEP");
    let _ = std::fs::remove_file(&step);
    let _ = std::fs::remove_dir(&dir);
    h.run_steps(3);
    assert!(h.state().plane_pick.is_none(), "the pick ends once the scene has geometry");
    assert!(easycad_app::plane_pick::draw_list(h.state()).is_empty(), "no origin plane items are drawn");
    // a sketch begun on purpose after geometry exists still asks for its plane
    h.key_press(egui::Key::S);
    h.run_steps(3);
    assert!(h.state().plane_pick.is_some());
}
