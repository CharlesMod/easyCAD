//! Headless UI snapshots with egui_kittest on wgpu (lavapipe / llvmpipe in CI
//! containers). Each test drives the real app (worker thread, GPU viewport) on
//! the sample enclosure and writes a PNG to `tests/snapshots/`.
//!
//! The PNGs are written, not compared: geometry changes in the engine move pixels.
//! Run with `cargo test -p easycad --test ui_snapshots -- --test-threads=1`.

use std::path::PathBuf;
use std::time::Duration;

use easycad_app::camera::CubeRegion;
use easycad_app::dialogs::{Body, DialogKind};
use easycad_app::pickstack::{PickKey, PickKind};
use easycad_app::EasyCadApp;
use egui_kittest::Harness;
use glam::Vec3;

/// The tests share one GPU (lavapipe) and the OCCT kernel; run them one at a time.
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
    // the user's crash-recovery copies would open the Recover prompt over the viewport
    std::env::set_var("XDG_DATA_HOME", std::env::temp_dir().join(format!("easycad-test-data-{}", std::process::id())));
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
        assert!(
            h.state_mut().settle(Duration::from_secs(240)),
            "regeneration did not finish"
        );
    }
    h.run_steps(3);
}

fn save(h: &mut Harness<'static, EasyCadApp>, name: &str) -> image::RgbaImage {
    let img = h.render().expect("render");
    write_png(&img, &out_dir().join(format!("{name}.png")));
    img
}

/// Write a snapshot unless the one on disk is the same picture (renders jitter by a
/// few pixels from run to run; rewriting them only churns the working tree).
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

fn crop(img: &image::RgbaImage, r: egui::Rect, name: &str) {
    let (x, y) = (r.left().max(0.0) as u32, r.top().max(0.0) as u32);
    let w = (r.width() as u32).min(img.width() - x);
    let hh = (r.height() as u32).min(img.height() - y);
    let c = image::imageops::crop_imm(img, x, y, w, hh).to_image();
    write_png(&c, &out_dir().join(format!("{name}.png")));
}

/// The viewport must show geometry: plenty of pixels differ from the background.
fn assert_viewport_rendered(img: &image::RgbaImage, r: egui::Rect) {
    let bg = [0x1d, 0x20, 0x25];
    let mut differ = 0usize;
    let mut total = 0usize;
    for y in (r.top() as u32..r.bottom() as u32).step_by(4) {
        for x in (r.left() as u32..r.right() as u32).step_by(4) {
            let p = img.get_pixel(x, y).0;
            total += 1;
            if (0..3).any(|i| (p[i] as i32 - bg[i]).abs() > 12) {
                differ += 1;
            }
        }
    }
    assert!(
        differ * 20 > total,
        "viewport looks empty: {differ} of {total} samples differ"
    );
}

fn screen_of(h: &Harness<'static, EasyCadApp>, p: Vec3) -> egui::Pos2 {
    let app = h.state();
    let r = app.viewport_rect;
    let s = app
        .camera
        .project(p, glam::Vec2::new(r.width(), r.height()))
        .unwrap();
    egui::pos2(r.left() + s.x, r.top() + s.y)
}

#[test]
fn main_screen() {
    let _serial = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    {
        let app = h.state_mut();
        assert!(app.model.is_some());
        assert!(
            app.model.as_ref().unwrap().result.errors().is_empty(),
            "{:?}",
            app.model.as_ref().unwrap().result.errors()
        );
        let m = app.model.as_ref().unwrap();
        let lid = m
            .bodies
            .iter()
            .find(|b| b.body.name == "Lid")
            .unwrap()
            .body
            .id;
        let base = m
            .bodies
            .iter()
            .find(|b| b.body.name == "Base")
            .unwrap()
            .body
            .id;
        // like the mock: lid toggled off with its eye, base selected
        app.hidden.insert(lid);
        app.selection = vec![PickKey {
            body: base,
            kind: PickKind::Body,
        }];
    }
    h.run_steps(3);
    let img = save(&mut h, "main");
    assert_viewport_rendered(&img, h.state().viewport_rect.shrink(40.0));
    let r = h.state().viewport_rect;
    crop(
        &img,
        egui::Rect::from_min_size(
            egui::pos2(r.right() - 120.0, r.top()),
            egui::vec2(120.0, 180.0),
        ),
        "viewcube",
    );
}

#[test]
fn view_cube_corner_highlight() {
    let _serial = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().cube_hover = Some(CubeRegion([1, -1, 1]));
    h.run_steps(2);
    let img = save(&mut h, "viewcube_corner_full");
    let r = h.state().viewport_rect;
    crop(
        &img,
        egui::Rect::from_min_size(
            egui::pos2(r.right() - 116.0, r.top() + 4.0),
            egui::vec2(112.0, 160.0),
        ),
        "viewcube_corner",
    );
    // click the TOP face centre: animates to the top view, kept upright (+Y up)
    {
        let app = h.state_mut();
        app.cube_hover = None;
        let now = app.now;
        app.camera.go_region(CubeRegion::TOP, now, 1.6);
        app.camera.finish();
        let up = app.camera.up();
        assert!(up.y > 0.99, "{up}");
    }
}

#[test]
fn pick_stack_popover() {
    let _serial = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    {
        let app = h.state_mut();
        let lid = app
            .model
            .as_ref()
            .unwrap()
            .bodies
            .iter()
            .find(|b| b.body.name == "Lid")
            .unwrap()
            .body
            .id;
        app.hidden.insert(lid);
    }
    h.run_steps(2);
    // a standoff top seen from the iso view: board (reference) in front, then
    // the standoff top ring / bore / floor behind it
    let p = {
        let app = h.state();
        let m = app.model.as_ref().unwrap();
        let base = m.bodies.iter().find(|b| b.body.name == "Base").unwrap();
        let hole = base
            .faces
            .iter()
            .find(|f| {
                f.kind == easycad_model::occt::SurfaceKind::Cylinder
                    && (f.radius - 1.25).abs() < 0.2
                    && f.centroid.x < 0.0
                    && f.centroid.y > 0.0
            })
            .expect("standoff bore");
        let c = hole.axis.unwrap().0;
        Vec3::new(c.x as f32 + 2.4, c.y as f32, 20.0)
    };
    let s = screen_of(&h, p);
    for _ in 0..14 {
        h.hover_at(s);
        h.step();
    }
    {
        let app = h.state_mut();
        assert!(
            app.stack.len() >= 3,
            "pick stack has {} items",
            app.stack.len()
        );
        app.stack.cycle(1);
        app.stack.cycle(1);
    }
    h.hover_at(s);
    h.run_steps(2);
    let img = save(&mut h, "pick_stack");
    crop(
        &img,
        egui::Rect::from_center_size(s + egui::vec2(120.0, 120.0), egui::vec2(520.0, 420.0)),
        "pick_stack_detail",
    );
}

#[test]
fn fillet_dialog_with_reduced_radius_report() {
    let _serial = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    {
        let app = h.state_mut();
        let m = app.model.clone().unwrap();
        let lid = m
            .bodies
            .iter()
            .find(|b| b.body.name == "Lid")
            .unwrap()
            .body
            .id;
        app.hidden.insert(lid);
        let base = m.bodies.iter().find(|b| b.body.name == "Base").unwrap();
        let front = -(easycad_app::sample::PCB_H / 2.0 + 2.3);
        // the USB-C slot's outer rim (3.5 mm tall slot)
        let rim: Vec<PickKey> = base
            .edges
            .iter()
            .filter(|e| {
                (e.midpoint.y - front).abs() < 0.05
                    && e.midpoint.x.abs() <= 4.6
                    && (20.0..26.0).contains(&e.midpoint.z)
            })
            .map(|e| PickKey {
                body: base.body.id,
                kind: PickKind::Edge(e.index),
            })
            .collect();
        assert!(rim.len() >= 4, "found {} rim edges", rim.len());
        app.selection = rim;
        app.open_dialog(DialogKind::Fillet);
        let tops: Vec<PickKey> = base
            .edges
            .iter()
            .filter(|e| {
                e.kind == easycad_model::occt::CurveKind::Circle
                    && e.circle.is_some_and(|c| c.2 > 2.0 && c.2 < 3.6)
                    && (e.midpoint.z - 20.0).abs() < 0.05
            })
            .map(|e| PickKey {
                body: base.body.id,
                kind: PickKind::Edge(e.index),
            })
            .collect();
        let d = app.dialog.as_mut().unwrap();
        if let Body::Fillet(f) = &mut d.body {
            f.sets[0].radius.text = "clearance + wall".into();
            f.sets[0].name = Some("USB-C cutout".into());
            f.sets.push(easycad_app::dialogs::fillet::EdgeSet {
                name: Some("Standoff tops".into()),
                ..f.sets[0].clone()
            });
            let s = f.sets.last_mut().unwrap();
            s.edges.clear();
            s.radius.text = "0.5".into();
            f.active = 1;
        }
        for k in tops {
            d.on_pick(k, Some(&m));
        }
        app.request_preview();
    }
    settle(&mut h);
    {
        let app = h.state();
        let d = app.dialog.as_ref().unwrap();
        let Body::Fillet(f) = &d.body else { panic!() };
        let failed = f.sets[0].results.iter().flatten().any(|r| {
            matches!(
                r,
                easycad_app::dialogs::fillet::EdgeResult::Failed { max: Some(_), .. }
            )
        });
        assert!(
            failed,
            "expected a reduced-radius suggestion: {:?}",
            f.sets[0].results
        );
    }
    save(&mut h, "fillet_dialog");
}

#[test]
fn sketch_mode_dxf_offset() {
    let _serial = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    {
        let app = h.state_mut();
        let imp = app
            .doc
            .data
            .imports
            .iter()
            .find(|i| i.import.name.ends_with(".dxf"))
            .unwrap()
            .id;
        let d = app.doc.dxf(imp).unwrap();
        let mut s = easycad_app::sketch_mode::SketchSession::from_dxf(
            imp,
            "sensor-node_outline.dxf",
            d,
            &app.doc.data,
        );
        let first = easycad_model::sketch::EntityId(0);
        s.offset_loop(first, "clearance + wall").unwrap();
        // a dimension on the offset's width, bound to an expression
        s.tool = easycad_app::sketch_mode::Tool::Offset;
        assert!(s.profile_count() >= 1);
        app.mode = easycad_app::app::Mode::Sketch(Box::new(s));
    }
    h.run_steps(4);
    save(&mut h, "sketch_mode");
}

#[test]
fn process_panel_and_fit_chips() {
    let _serial = serial();
    let mut doc = easycad_app::sample::sensor_node();
    doc.set_active_profile("FDM 0.4 generic").unwrap();
    let mut h = harness(doc);
    h.state_mut().open_dialog(DialogKind::Standoffs);
    settle(&mut h);
    let img = save(&mut h, "standoffs_fdm");
    let w = img.width() as f32;
    crop(
        &img,
        egui::Rect::from_min_max(egui::pos2(w - 300.0, 40.0), egui::pos2(w, H - 28.0)),
        "params_process",
    );
    // one hole left out, gussets and lid holes (GUI_NEEDS #7, engine-native now)
    {
        let app = h.state_mut();
        let Body::Standoffs(s) = &mut app.dialog.as_mut().unwrap().body else {
            panic!("standoffs dialog")
        };
        assert!(s.holes.len() >= 4, "{} holes", s.holes.len());
        s.holes[0].on = false;
        s.gussets = true;
        s.lid_holes = true;
        app.request_preview();
    }
    settle(&mut h);
    {
        let app = h.state();
        let p = app.preview.as_ref().expect("preview");
        assert!(p.result.errors().is_empty(), "{:?}", p.result.errors());
        let feats = app.dialog.as_ref().unwrap().build(&app.dialog_cx_data()).unwrap();
        match &feats[0].kind {
            easycad_model::feature::FeatureKind::StandoffsFromPcb { exclude, gussets, lid_holes, .. } => {
                assert_eq!(exclude.len(), 1);
                assert!(gussets.is_some() && lid_holes.is_some());
            }
            k => panic!("{k:?}"),
        }
    }
    save(&mut h, "standoffs_options");
}

#[test]
fn parts_joint_drawing_screens() {
    let _serial = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    {
        let app = h.state_mut();
        app.mode = easycad_app::app::Mode::Parts(Box::new(easycad_app::ui_parts::PartsUi::new(
            &app.doc,
        )));
    }
    h.run_steps(4);
    save(&mut h, "parts");
    {
        let app = h.state_mut();
        app.mode = easycad_app::app::Mode::Model;
        // FDM profile so the fit chips show resolved values
        app.doc.set_active_profile("FDM 0.4 generic").unwrap();
        // roll back to just before the plain split, then add a real parting joint there
        let split = app
            .doc
            .data
            .features
            .iter()
            .position(|f| matches!(f.kind, easycad_model::feature::FeatureKind::Split { .. }))
            .unwrap();
        app.set_rollback(Some(split));
    }
    settle(&mut h);
    h.state_mut().open_dialog(DialogKind::Joint);
    settle(&mut h);
    {
        let app = h.state();
        let Body::Joint(j) = &app.dialog.as_ref().unwrap().body else {
            panic!()
        };
        assert!(
            j.report.is_some() || j.status.is_some(),
            "joint preview produced no report"
        );
    }
    save(&mut h, "joint");
    {
        let app = h.state_mut();
        app.close_dialog(false);
        app.mode = easycad_app::app::Mode::Drawing(Box::default());
    }
}

#[test]
fn drawing_sheet_with_views_and_dimensions() {
    let _serial = serial();
    use easycad_model::drawing::*;
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().mode = easycad_app::app::Mode::Drawing(Box::default());
    settle(&mut h);
    {
        let app = h.state();
        let easycad_app::app::Mode::Drawing(d) = &app.mode else {
            panic!()
        };
        let r = d.result.as_ref().expect("drawing computed");
        assert_eq!(r.views.len(), 4);
        assert!(
            r.views.iter().all(|v| !v.edges.is_empty()),
            "every view has HLR edges"
        );
        let bad: Vec<_> = r
            .annotations
            .iter()
            .filter(|a| a.status != AnnotationStatus::Ok)
            .map(|a| (&a.text, &a.status))
            .collect();
        assert!(bad.is_empty(), "every default annotation resolves: {bad:?}");
        assert!(r.annotations.len() >= 6);
        assert!(
            r.title.iter().any(|(l, v)| l == "Rev" && v == "A"),
            "title block revision is the user field: {:?}",
            r.title
        );
    }
    // a dimension whose edge no longer exists stays, shown orange as dangling
    {
        let app = h.state_mut();
        let base = app
            .model
            .as_ref()
            .unwrap()
            .bodies
            .iter()
            .find(|b| b.body.name == "Base")
            .unwrap()
            .body
            .id;
        let now = app.now;
        let easycad_app::app::Mode::Drawing(d) = &mut app.mode else {
            panic!()
        };
        let dr = d.draft.as_mut().unwrap();
        let front = dr.views[1].id;
        let gone = easycad_model::select::EdgeSelector::Hint {
            midpoint: glam::DVec3::new(0.0, -80.0, 50.0),
            tangent: glam::DVec3::X,
            kind: None,
        };
        dr.annotate(AnnotationKind::Linear {
            view: front,
            a: DimRef::Edge {
                body: base,
                edge: gone,
            },
            b: None,
            kind: LinearKind::Horizontal,
            offset: 30.0,
        });
        d.dirty_at = Some(now);
        d.edit_gen += 1;
    }
    settle(&mut h);
    {
        let app = h.state();
        let easycad_app::app::Mode::Drawing(d) = &app.mode else {
            panic!()
        };
        let r = d.result.as_ref().unwrap();
        assert!(r
            .annotations
            .iter()
            .any(|a| matches!(a.status, AnnotationStatus::Dangling(_))));
    }
    save(&mut h, "drawing");
}

#[test]
fn new_document_prompt() {
    let _serial = serial();
    // keep the "last used profile" setting out of the real config dir
    std::env::set_var(
        "XDG_CONFIG_HOME",
        std::env::temp_dir().join("easycad-test-config"),
    );
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().new_doc_prompt = Some(("bracket".into(), "FDM 0.4 generic".into()));
    h.run_steps(3);
    save(&mut h, "new_document");
    h.state_mut().create_doc("bracket", "FDM 0.4 generic");
    assert_eq!(h.state().doc.data.active_profile, "FDM 0.4 generic");
}

#[test]
fn welcome_and_pcb_wizard() {
    let _serial = serial();
    std::env::set_var(
        "XDG_CONFIG_HOME",
        std::env::temp_dir().join("easycad-test-config"),
    );
    let mut h = harness(easycad_model::document::Document::new("Untitled"));
    save(&mut h, "welcome");
    // the wizard on the sample board's DXF
    let dir = std::env::temp_dir().join(format!("easycad-wizard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dxf = dir.join("sensor-node.dxf");
    std::fs::write(&dxf, easycad_app::sample::board_dxf()).unwrap();
    let mut w = easycad_app::ui_welcome::WizardUi::new();
    w.load(&dxf);
    assert!(w.board.is_some(), "{:?}", w.error);
    w.settings.profile = "FDM 0.4 generic".into();
    h.state_mut().wizard = Some(w);
    h.run_steps(3);
    save(&mut h, "pcb_wizard");
    // create: an ordinary, editable model with two halves
    let (board, settings) = {
        let w = h.state().wizard.as_ref().unwrap();
        (w.board.clone().unwrap(), w.settings.clone())
    };
    let doc = easycad_app::wizard::build(&board, &settings).unwrap();
    h.state_mut().wizard = None;
    h.state_mut().load(doc, None);
    settle(&mut h);
    let m = h.state().model.clone().expect("model");
    assert!(m.result.errors().is_empty(), "{:?}", m.result.errors());
    let img = save(&mut h, "pcb_wizard_result");
    assert_viewport_rendered(&img, h.state().viewport_rect);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The wizard's assembly from the sample STEP board.
fn wizard_assembly() -> easycad_model::document::Document {
    let dir = std::env::temp_dir().join(format!("easycad-asm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let step = dir.join("sensor-node.step");
    std::fs::write(&step, easycad_app::sample::pcb_step()).unwrap();
    let board = easycad_app::wizard::Board::read(&step).unwrap();
    let s = easycad_app::wizard::Settings::new("FDM 0.4 generic");
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
        .unwrap_or_else(|| panic!("no occurrence {name}"))
        .0
}

/// A straight edge along X on an occurrence's front face (y min), at its top or
/// bottom: the hinge line of the wizard's box.
fn front_edge(app: &EasyCadApp, name: &str, top: bool) -> PickKey {
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
fn assembly_tree_active_mates_explode() {
    let _serial = serial();
    let mut h = harness(wizard_assembly());
    {
        let app = h.state();
        let m = app.model.as_ref().expect("model");
        let a = m.asm.as_ref().expect("assembly output");
        assert!(a.result.errors().is_empty(), "{:?}", a.result.errors());
        assert!(m.ctx.len() >= 3, "Base, Lid and the board are placed: {}", m.ctx.len());
    }
    // the tree: select the Lid occurrence
    let lid = occ_path(h.state(), "Lid:1");
    h.state_mut().select_occurrence(Some(lid.clone()), false);
    h.run_steps(3);
    let img = save(&mut h, "assembly_tree");
    assert_viewport_rendered(&img, h.state().viewport_rect.shrink(60.0));

    // edit the Base in place: the rest dims, history and params scope to it
    let base = occ_path(h.state(), "Base:1");
    h.state_mut().activate(base.clone());
    settle(&mut h);
    {
        let app = h.state();
        assert_eq!(app.scope(), app.doc.data.component_at(&base).unwrap());
        assert!(!app.view().features.is_empty(), "the Base's in-context feature");
        assert!(app.model.as_ref().unwrap().bodies.iter().any(|b| !b.body.reference));
    }
    save(&mut h, "assembly_active_component");
    h.state_mut().activate(Vec::new());
    settle(&mut h);

    // the move gizmo on the (unmated) Lid; a committed 10 mm lift and its undo
    h.state_mut().select_occurrence(Some(lid.clone()), false);
    h.state_mut().toggle_gizmo();
    assert!(h.state().asm.gizmo, "the Lid is free to move");
    h.run_steps(3);
    save(&mut h, "assembly_gizmo");
    let before = h.state().doc.data.occurrence(lid[0]).unwrap().transform;
    easycad_app::ui_gizmo::commit(h.state_mut(), &lid, glam::DAffine3::from_translation(glam::DVec3::Z * 10.0));
    let after = h.state().doc.data.occurrence(lid[0]).unwrap().transform;
    assert!((after.translation.z - before.translation.z - 10.0).abs() < 1e-6, "{before:?} -> {after:?}");
    assert_eq!(h.state().doc.undo_label(), Some("Move Lid:1"));
    h.state_mut().undo();
    h.state_mut().asm.gizmo = false;
    settle(&mut h);
    // grounded parts refuse the gizmo with a reason
    let base_path = occ_path(h.state(), "Base:1");
    assert!(easycad_app::ui_gizmo::blocked(h.state(), &base_path).is_some());

    // a revolute hinge along the front seam: Base stays, Lid swings open
    h.state_mut().open_mate(None);
    let a = front_edge(h.state(), "Base:1", true);
    let b = front_edge(h.state(), "Lid:1", false);
    assert!(h.state_mut().mate_pick(a));
    assert!(h.state_mut().mate_pick(b));
    {
        let mu = h.state_mut().asm.mate.as_mut().unwrap();
        assert_eq!(mu.kind, easycad_model::assembly::MateKind::Revolute);
        mu.angle = 100.0;
    }
    h.state_mut().request_model();
    settle(&mut h);
    {
        let m = h.state().model.clone().unwrap();
        let st = m.asm.as_ref().unwrap().solve.clone().expect("mates solved");
        assert!(st.ok(), "{st:?}");
    }
    save(&mut h, "mates_hinge");
    h.state_mut().commit_mate();
    settle(&mut h);
    assert_eq!(h.state().doc.data.mates.len(), 1);

    // explode + interference (swing the lid back into the base first)
    let id = h.state().doc.data.mates[0].id;
    h.state_mut().open_mate(Some(id));
    h.state_mut().asm.mate.as_mut().unwrap().angle = -12.0;
    h.state_mut().commit_mate();
    settle(&mut h);
    h.state_mut().explode = 0.5;
    h.state_mut().asm.explode_open = true;
    h.state_mut().request_model();
    settle(&mut h);
    h.state_mut().check_interference();
    for _ in 0..600 {
        h.run_steps(1);
        if h.state().asm.interference.as_ref().is_some_and(|i| i.job.is_none()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    h.run_steps(3);
    let r = h.state().asm.interference.as_ref().and_then(|i| i.report.clone()).expect("report");
    save(&mut h, "explode_interference");
    let lid_t = h.state().doc.data.occurrence(lid[0]).unwrap().transform;
    assert!(!r.pairs.is_empty(), "a lid turned -12° into the base interferes: {r:?} {lid_t:?} {:?}", h.state().doc.data.mates);
    // BOM: Base, Lid made; the board purchased; the master left out
    h.state_mut().open_bom();
    h.run_steps(4);
    save(&mut h, "assembly_bom");
}

#[test]
fn section_view_and_sketch_slice() {
    let _serial = serial();
    let mut h = harness(easycad_app::sample::sensor_node());
    h.state_mut().toggle_section();
    settle(&mut h);
    {
        let app = h.state();
        let m = app.model.as_ref().unwrap();
        assert!(!m.caps.is_empty(), "cut faces");
        assert!(!m.cut_away.is_empty(), "the cut-away half is kept, to draw as a wireframe");
        assert!(m.cut_away.keys().all(|id| m.bodies.iter().any(|b| b.body.id == *id)));
        assert!(app.section_cut().is_some());
    }
    let img = save(&mut h, "section_view");
    assert_viewport_rendered(&img, h.state().viewport_rect.shrink(60.0));
    // shaded without edges: only the cut-away half has lines
    h.state_mut().display = easycad_app::viewport::gpu::DisplayMode::Shaded;
    h.run_steps(4);
    save(&mut h, "section_view_shaded");
    h.state_mut().display = easycad_app::viewport::gpu::DisplayMode::ShadedEdges;
    h.state_mut().toggle_section();
    settle(&mut h);
    assert!(h.state().model.as_ref().unwrap().caps.is_empty());
    // a sketch half way up the walls, sliced
    {
        let app = h.state_mut();
        let plane = easycad_model::geom::Plane::XY.offset(12.0);
        let mut s = easycad_app::sketch_mode::SketchSession::new_on(plane, &app.doc.data);
        s.slice = true;
        app.mode = easycad_app::app::Mode::Sketch(Box::new(s));
    }
    h.run_steps(4);
    if let easycad_app::app::Mode::Sketch(s) = &h.state().mode {
        assert!(!s.slice_caps.is_empty() && !s.slice_outline.is_empty(), "the walls cut at z 12");
    }
    save(&mut h, "sketch_slice");
}

/// The wizard assembly's first sheet: assembly-scoped views with a BOM table,
/// then an item balloon, a section view, a datum and a feature control frame.
#[test]
fn drawing_of_an_assembly_with_bom_section_and_gdt() {
    let _serial = serial();
    use easycad_model::assembly::Scope;
    use easycad_model::drawing::*;
    let mut h = harness(wizard_assembly());
    h.state_mut().mode = easycad_app::app::Mode::Drawing(Box::default());
    settle(&mut h);
    let (front, lid_edge) = {
        let app = h.state();
        let easycad_app::app::Mode::Drawing(d) = &app.mode else { panic!() };
        let dr = d.draft.as_ref().expect("a default sheet");
        assert!(dr.views.iter().all(|v| v.scope == Scope::Assembly), "the root has no solids: the views show the assembly");
        let r = d.result.as_ref().expect("drawing computed");
        assert!(r.views.iter().all(|v| !v.edges.is_empty() && v.error.is_none()), "{:?}", r.views.iter().map(|v| &v.error).collect::<Vec<_>>());
        let table = dr.annotations.iter().find(|a| matches!(a.kind, AnnotationKind::BomTable { .. })).expect("BOM table on the sheet");
        let tr = r.annotation(table.id).unwrap();
        assert_eq!(tr.status, AnnotationStatus::Ok, "{:?}", tr.status);
        assert!(tr.value.unwrap() >= 2.0, "Base and Lid at least: {}", tr.text);
        // a visible straight edge of the Lid in the front view
        let lid = occ_path(app, "Lid:1");
        let front = dr.views[1].id;
        let fv = r.view(front).unwrap();
        let e = fv.edges.iter().filter(|e| e.visible && e.path == lid && e.points.len() == 2 && e.reference.is_some()).max_by(|a, b| a.points[0].distance(a.points[1]).total_cmp(&b.points[0].distance(b.points[1]))).expect("a Lid edge");
        (front, (e.reference.clone().unwrap(), lid, (e.points[0] + e.points[1]) / 2.0))
    };
    {
        let app = h.state_mut();
        let now = app.now;
        let easycad_app::app::Mode::Drawing(d) = &mut app.mode else { panic!() };
        let r = d.result.clone().unwrap();
        let fv = r.view(front).unwrap();
        let dr = d.draft.as_mut().unwrap();
        let (rf, lid, at) = lid_edge;
        dr.annotate(AnnotationKind::ItemBalloon { view: front, path: lid, body: None, position: at + glam::DVec2::new(-20.0, 25.0) });
        dr.annotate(AnnotationKind::Datum { view: front, target: rf.clone(), label: "A".into(), position: at + glam::DVec2::new(10.0, 18.0) });
        dr.annotate(AnnotationKind::FeatureControl {
            view: front,
            target: Some(rf),
            position: at + glam::DVec2::new(25.0, 30.0),
            symbol: GdtSymbol::Flatness,
            tolerance: "0.2".into(),
            diameter: false,
            modifier: None,
            datums: Vec::new(),
        });
        // a vertical cut through the middle of the front view, placed on the free
        // paper to the right (top right of the sheet)
        let c = (fv.min + fv.max) / 2.0;
        let sec = dr
            .add_section_view(front, &fv.frame, glam::DVec2::new(c.x, fv.min.y - 5.0), glam::DVec2::new(c.x, fv.max.y + 5.0), glam::DVec2::new(340.0, 255.0))
            .unwrap();
        d.selected = Some(sec);
        d.dirty_at = Some(now);
        d.edit_gen += 1;
    }
    settle(&mut h);
    {
        let app = h.state();
        let easycad_app::app::Mode::Drawing(d) = &app.mode else { panic!() };
        let r = d.result.as_ref().unwrap();
        let bad: Vec<_> = r.annotations.iter().filter(|a| a.status != AnnotationStatus::Ok).map(|a| (&a.text, &a.status)).collect();
        assert!(bad.is_empty(), "{bad:?}");
        let sec = r.views.last().unwrap();
        assert!(sec.error.is_none(), "{:?}", sec.error);
        assert!(!sec.hatch.is_empty(), "cut walls hatched");
        assert!(sec.marks.iter().any(|m| matches!(m, Prim::Text { text, .. } if text == "SECTION A-A")));
        // the exports carry the BOM table, the hatch and the frames
        let dr = d.draft.as_ref().unwrap();
        let svg = String::from_utf8(easycad_app::ui_drawing::export_bytes(r, dr, "svg")).unwrap();
        assert!(svg.contains(">Lid<") && svg.contains("SECTION A-A"));
        let dxf = String::from_utf8(easycad_app::ui_drawing::export_bytes(r, dr, "dxf")).unwrap();
        assert!(dxf.contains("\nHATCH\n"));
        assert!(easycad_app::ui_drawing::export_bytes(r, dr, "pdf").starts_with(b"%PDF-"));
    }
    save(&mut h, "drawing_assembly");
}

/// Import an STL through the app's import path, combine it with a solid post in the
/// Combine dialog (cut with clearance at two positions), OK, and export the
/// watertight result.
#[test]
fn mesh_import_and_combine_dialog() {
    use easycad_model::feature::{BodyId, BoolOp, FeatureKind, ProfileSel};
    use easycad_model::meshbody;
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!("easycad-ui-mesh-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stl = dir.join("pile.stl");
    std::fs::write(&stl, meshbody::write_stl_bytes(&meshbody::sample_blob(glam::DVec3::new(0.0, 0.0, 10.0), 10.0, 4))).unwrap();
    let mut doc = easycad_model::document::Document::new("Pile");
    let mut s = easycad_model::sketch::Sketch::new("Post", easycad_model::geom::Plane::XY);
    s.rect(glam::DVec2::new(9.0, -20.0), glam::DVec2::new(20.0, 20.0));
    let s = doc.add_sketch(s);
    let post = BodyId::of(doc.add_feature(
        "Post",
        FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: 30.0.into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
    ));
    let mut h = harness(doc);
    let msg = h.state_mut().import_path(&stl).unwrap();
    assert!(msg.contains("mesh body"), "{msg}");
    settle(&mut h);
    let pile = {
        let app = h.state_mut();
        let m = app.model.clone().unwrap();
        let pile = m.bodies.iter().find(|b| b.body.is_mesh()).expect("mesh body").body.id;
        app.selection = vec![PickKey { body: pile, kind: PickKind::Face(0) }, PickKey { body: post, kind: PickKind::Face(0) }];
        app.open_dialog(DialogKind::Combine);
        let d = app.dialog.as_mut().expect("Combine dialog");
        let Body::Combine(c) = &mut d.body else { panic!("not the Combine dialog") };
        assert_eq!(c.target, Some(pile), "the picked mesh body is the target");
        assert_eq!(c.tools, vec![post]);
        c.clearance.text = "0.5".into();
        c.positions = "0, 0, 0; 0, 0, 14".into();
        app.request_preview();
        pile
    };
    settle(&mut h);
    save(&mut h, "mesh_combine_dialog");
    {
        let app = h.state_mut();
        app.close_dialog(true);
    }
    settle(&mut h);
    let app = h.state_mut();
    let f = app.doc.data.features.last().unwrap();
    assert!(matches!(f.kind, FeatureKind::MeshBoolean { op: BoolOp::Cut, .. }), "{:?}", f.kind);
    let m = app.model.clone().unwrap();
    let b = m.body(pile).unwrap();
    assert!(b.is_mesh() && b.bbox().unwrap().max.x <= 8.5 + 1e-6);
    assert!(m.result.errors().is_empty(), "{:?}", m.result.errors());
}

/// The Positions panel next to the display modes: three positions, the cart snapped
/// to "very full" (after its tween), the lid selected with its move shown.
#[test]
fn positions_panel() {
    use easycad_model::feature::{BodyId, BoolOp, FeatureKind, ProfileSel};
    use easycad_model::positions::{PoseMove, PoseTarget};
    let _serial = serial();
    let mut doc = easycad_model::document::Document::new("Cart");
    let mut ids = Vec::new();
    for (name, lo, hi) in [
        ("Tub floor", [-20.0, -22.0, 0.0], [20.0, 22.0, 2.0]),
        ("Lid", [-20.0, -22.0, 56.5], [20.0, 22.0, 58.5]),
        ("Pile", [-10.0, -10.0, 20.0], [10.0, 10.0, 50.0]),
    ] {
        let mut s = easycad_model::sketch::Sketch::new(name, easycad_model::geom::Plane::XY.offset(lo[2]));
        s.rect(glam::DVec2::new(lo[0], lo[1]), glam::DVec2::new(hi[0], hi[1]));
        let s = doc.add_sketch(s);
        ids.push(BodyId::of(doc.add_feature(
            name,
            FeatureKind::Extrude { sketch: s, profiles: ProfileSel::All, distance: (hi[2] - lo[2]).into(), symmetric: false, reverse: false, draft_deg: 0.0.into(), op: BoolOp::NewBody, target: None },
        )));
    }
    let (lid, pile) = (ids[1], ids[2]);
    let full = doc
        .edit("positions", |d| {
            for (name, dz, deg) in [("closed", "0", "0"), ("peeking full", "7", "-25"), ("very full", "14", "-100")] {
                let id = d.add_position(name);
                d.positions.last_mut().unwrap().moves = vec![
                    PoseMove { translate: ["0".into(), "0".into(), dz.into()], ..PoseMove::new(PoseTarget::Body(pile)) },
                    PoseMove { angle_deg: deg.into(), axis: glam::DVec3::X, pivot: glam::DVec3::new(0.0, 22.0, 56.5), ..PoseMove::new(PoseTarget::Body(lid)) },
                ];
                if name == "very full" {
                    return Ok::<_, ()>(id);
                }
            }
            unreachable!()
        })
        .unwrap();
    let mut h = harness(doc);
    {
        let app = h.state_mut();
        app.positions.open = true;
        app.go_to_position(Some(full));
    }
    settle(&mut h);
    h.run_steps(20); // the tween (0.35 s at 30 fps)
    {
        let app = h.state_mut();
        assert_eq!(app.model.as_ref().unwrap().pose, Some(full));
        assert!(app.positions.anim.is_none(), "the tween ended");
        let lid_top = app.model.as_ref().unwrap().body(lid).unwrap().bbox().unwrap().max.z;
        assert!(lid_top > 90.0, "{lid_top}");
        app.selection = vec![PickKey { body: lid, kind: PickKind::Face(0) }];
    }
    h.run_steps(3);
    save(&mut h, "positions_panel");
    // a dialog shows the modeled pose; closing it goes back
    h.state_mut().open_dialog(DialogKind::Split);
    settle(&mut h);
    assert_eq!(h.state().model.as_ref().unwrap().pose, None);
    h.state_mut().close_dialog(false);
    settle(&mut h);
    assert_eq!(h.state().model.as_ref().unwrap().pose, Some(full));
}
