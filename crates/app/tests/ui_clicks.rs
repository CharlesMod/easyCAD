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
    h.state_mut().open_sketch(None);
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
    h.state_mut().open_sketch(None);
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
