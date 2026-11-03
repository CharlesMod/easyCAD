//! Headless UI snapshots with egui_kittest on wgpu (lavapipe / llvmpipe in CI
//! containers). Each test drives the real app (worker thread, GPU viewport) on
//! the sample enclosure and writes a PNG to `tests/snapshots/`.
//!
//! The PNGs are written, not compared: geometry changes in the engine move pixels.
//! Run with `cargo test -p casework --test ui_snapshots -- --test-threads=1`.

use std::path::PathBuf;
use std::time::Duration;

use casework_app::camera::CubeRegion;
use casework_app::dialogs::{Body, DialogKind};
use casework_app::pickstack::{PickKey, PickKind};
use casework_app::CaseworkApp;
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

fn harness(doc: casework_model::document::Document) -> Harness<'static, CaseworkApp> {
    let mut h = Harness::builder()
        .with_size(egui::vec2(W, H))
        .with_pixels_per_point(1.0)
        .with_accessibility_check(false)
        .with_step_dt(1.0 / 30.0)
        .wgpu()
        .build_eframe(move |cc| CaseworkApp::new(cc, Some(doc)));
    settle(&mut h);
    h
}

fn settle(h: &mut Harness<'static, CaseworkApp>) {
    for _ in 0..6 {
        h.run_steps(2);
        assert!(
            h.state_mut().settle(Duration::from_secs(240)),
            "regeneration did not finish"
        );
    }
    h.run_steps(3);
}

fn save(h: &mut Harness<'static, CaseworkApp>, name: &str) -> image::RgbaImage {
    let img = h.render().expect("render");
    img.save(out_dir().join(format!("{name}.png"))).unwrap();
    img
}

fn crop(img: &image::RgbaImage, r: egui::Rect, name: &str) {
    let (x, y) = (r.left().max(0.0) as u32, r.top().max(0.0) as u32);
    let w = (r.width() as u32).min(img.width() - x);
    let hh = (r.height() as u32).min(img.height() - y);
    image::imageops::crop_imm(img, x, y, w, hh)
        .to_image()
        .save(out_dir().join(format!("{name}.png")))
        .unwrap();
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

fn screen_of(h: &Harness<'static, CaseworkApp>, p: Vec3) -> egui::Pos2 {
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
    let mut h = harness(casework_app::sample::sensor_node());
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
    let mut h = harness(casework_app::sample::sensor_node());
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
    let mut h = harness(casework_app::sample::sensor_node());
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
                f.kind == casework_model::occt::SurfaceKind::Cylinder
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
    let mut h = harness(casework_app::sample::sensor_node());
    {
        let app = h.state_mut();
        let m = app.model.clone().unwrap();
        let base = m.bodies.iter().find(|b| b.body.name == "Base").unwrap();
        let front = -(casework_app::sample::PCB_H / 2.0 + 2.3);
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
                e.kind == casework_model::occt::CurveKind::Circle
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
            f.sets.push(casework_app::dialogs::fillet::EdgeSet {
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
                casework_app::dialogs::fillet::EdgeResult::Failed { max: Some(_), .. }
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
    let mut h = harness(casework_app::sample::sensor_node());
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
        let mut s = casework_app::sketch_mode::SketchSession::from_dxf(
            imp,
            "sensor-node_outline.dxf",
            d,
            &app.doc.data,
        );
        let first = casework_model::sketch::EntityId(0);
        s.offset_loop(first, "clearance + wall").unwrap();
        // a dimension on the offset's width, bound to an expression
        s.tool = casework_app::sketch_mode::Tool::Offset;
        assert!(s.profile_count() >= 1);
        app.mode = casework_app::app::Mode::Sketch(Box::new(s));
    }
    h.run_steps(4);
    save(&mut h, "sketch_mode");
}

#[test]
fn process_panel_and_fit_chips() {
    let _serial = serial();
    let mut doc = casework_app::sample::sensor_node();
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
}

#[test]
fn parts_joint_drawing_screens() {
    let _serial = serial();
    let mut h = harness(casework_app::sample::sensor_node());
    {
        let app = h.state_mut();
        app.mode = casework_app::app::Mode::Parts(Box::new(casework_app::ui_parts::PartsUi::new(
            &app.doc,
        )));
    }
    h.run_steps(4);
    save(&mut h, "parts");
    {
        let app = h.state_mut();
        app.mode = casework_app::app::Mode::Model;
        // roll back to just before the plain split, then add a real parting joint there
        let split = app
            .doc
            .data
            .features
            .iter()
            .position(|f| matches!(f.kind, casework_model::feature::FeatureKind::Split { .. }))
            .unwrap();
        app.rollback = Some(split);
        app.request_model();
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
        app.mode = casework_app::app::Mode::Drawing(Box::default());
    }
}

#[test]
fn drawing_sheet_with_views_and_dimensions() {
    let _serial = serial();
    use casework_model::drawing::*;
    let mut h = harness(casework_app::sample::sensor_node());
    h.state_mut().mode = casework_app::app::Mode::Drawing(Box::default());
    settle(&mut h);
    {
        let app = h.state();
        let casework_app::app::Mode::Drawing(d) = &app.mode else {
            panic!()
        };
        let r = d.result.as_ref().expect("drawing computed");
        assert_eq!(r.views.len(), 4);
        assert!(
            r.views.iter().all(|v| !v.edges.is_empty()),
            "every view has HLR edges"
        );
        let ok = r
            .annotations
            .iter()
            .filter(|a| a.status == AnnotationStatus::Ok)
            .count();
        assert!(
            ok >= 3,
            "dimensions resolve: {:?}",
            r.annotations
                .iter()
                .map(|a| (&a.text, &a.status))
                .collect::<Vec<_>>()
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
        let casework_app::app::Mode::Drawing(d) = &mut app.mode else {
            panic!()
        };
        let dr = d.draft.as_mut().unwrap();
        let front = dr.views[1].id;
        let gone = casework_model::select::EdgeSelector::Hint {
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
        let casework_app::app::Mode::Drawing(d) = &app.mode else {
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
        std::env::temp_dir().join("casework-test-config"),
    );
    let mut h = harness(casework_app::sample::sensor_node());
    h.state_mut().new_doc_prompt = Some(("bracket".into(), "FDM 0.4 generic".into()));
    h.run_steps(3);
    save(&mut h, "new_document");
    h.state_mut().create_doc("bracket", "FDM 0.4 generic");
    assert_eq!(h.state().doc.data.active_profile, "FDM 0.4 generic");
}
