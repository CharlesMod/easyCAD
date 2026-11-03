//! PROCESS section of the Parameters panel: active profile, fit allowances,
//! calibration, build-plate orientation (per body / document default) with a mini
//! plate preview, and the teardrop toggle for FDM profiles.

use easycad_model::document::{DocError, TeardropOverride};
use easycad_model::feature::BodyId;
use easycad_model::process::{
    calibration_coupon, default_coupon_offsets, BuildOrientation, CalibrationResult, CubeFace,
    FitClass, ProcessKind, ProcessProfile, TeardropStyle,
};
use easycad_model::select::FaceSelector;
use egui::{pos2, vec2, Color32, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui};
use glam::{DQuat, DVec3};

use crate::app::EasyCadApp;
use crate::pickstack::PickKind;
use crate::theme;
use crate::widgets;

pub struct CalibrateUi {
    pub nominal: f64,
    pub exported: Option<String>,
    pub result: CalibrationResult,
    pub hole: String,
    pub pin: String,
    pub first: String,
}

impl CalibrateUi {
    pub fn new(p: &ProcessProfile) -> Self {
        CalibrateUi {
            nominal: 5.0,
            exported: None,
            result: CalibrationResult {
                name: format!("{} (calibrated)", p.name.trim_end_matches(" (calibrated)")),
                kind: p.kind,
                nominal: 5.0,
                press_offset: 0.0,
                snug_offset: 0.15,
                slide_offset: 0.3,
                loose_offset: 0.5,
                hole_measured: None,
                pin_measured: None,
                first_layer_measured: None,
            },
            hole: String::new(),
            pin: String::new(),
            first: String::new(),
        }
    }
}

fn kind_name(k: ProcessKind) -> &'static str {
    match k {
        ProcessKind::Fdm => "FDM",
        ProcessKind::Sla => "SLA",
        ProcessKind::Sls => "SLS",
        ProcessKind::Molded => "molded",
        ProcessKind::Cnc => "CNC",
    }
}

fn commit_profile(app: &mut EasyCadApp, label: &str, p: ProcessProfile) {
    app.doc.update_profile(label, p);
    app.after_edit();
}

pub fn process_section(app: &mut EasyCadApp, ui: &mut Ui) {
    let d = &app.doc.data;
    let active = d.profile();
    let names: Vec<(String, ProcessKind)> = d
        .processes
        .iter()
        .map(|p| (p.name.clone(), p.kind))
        .collect();
    Frame::new().inner_margin(Margin { left: 14, right: 14, top: 12, bottom: 10 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            theme::section_label(ui, "Process");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("Calibrate…").size(12.0)).fill(Color32::TRANSPARENT).min_size(vec2(0.0, 24.0)))
                    .on_hover_text("Export a fit-test coupon STL, print it, then enter what fits")
                    .clicked()
                {
                    app.calibrate = Some(CalibrateUi::new(&active));
                }
            });
        });
        let mut switch = None;
        egui::ComboBox::from_id_salt("active_profile").selected_text(&active.name).width(ui.available_width() - 8.0).show_ui(ui, |ui| {
            for (n, k) in &names {
                if ui.selectable_label(*n == active.name, format!("{n}  ·  {}", kind_name(*k))).clicked() {
                    switch = Some(n.clone());
                }
            }
        });
        if let Some(n) = switch {
            if let Err(e) = app.doc.set_active_profile(&n) {
                app.warn(e.to_string());
            }
            let mut st = crate::settings::Settings::load();
            st.last_profile = Some(n.clone());
            st.save();
            app.after_edit();
        }
        // fit allowance table (per side, mm)
        let mut edited = None;
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 18.0) / 4.0;
            for c in FitClass::ALL {
                ui.allocate_ui_with_layout(vec2(w, 40.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.label(RichText::new(c.name()).size(10.5).color(theme::TEXT_LABEL));
                    let mut v = active.allowance(c);
                    let prefix = if v >= 0.0 { "+" } else { "" };
                    let r = ui.add(egui::DragValue::new(&mut v).speed(0.005).range(-1.0..=2.0).fixed_decimals(2).prefix(prefix));
                    if r.changed() {
                        edited = Some((c, v));
                    }
                    r.on_hover_text(format!("{} allowance per side under {} (drag or type)", c.name(), active.name));
                });
            }
        });
        ui.label(widgets::muted(format!(
            "per side · hole comp {:+.2}{} · xy comp {:+.2}",
            active.hole_comp.base,
            if active.hole_comp.per_mm != 0.0 { format!(" + {:.3}·d", active.hole_comp.per_mm) } else { String::new() },
            active.xy_comp
        )));
        if let Some((c, v)) = edited {
            let mut p = active.clone();
            match c {
                FitClass::Press => p.fits.press = v,
                FitClass::Snug => p.fits.snug = v,
                FitClass::Slide => p.fits.slide = v,
                FitClass::Loose => p.fits.loose = v,
            }
            app.pending_profile = Some((p, app.now + crate::app::DEBOUNCE));
        }
        widgets::divider(ui);
        orientation_picker(app, ui);
        let active = app.doc.data.profile();
        if active.kind == ProcessKind::Fdm {
            let mut td = active.teardrop;
            let mut ch = ui.checkbox(&mut td.enabled, "V-cut horizontal holes (teardrop)").on_hover_text("Support-free horizontal holes: the top of each round hole becomes a 45° point").changed();
            if td.enabled {
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    let pointed = matches!(td.style, TeardropStyle::Pointed45);
                    if widgets::pill(ui, "Pointed 45°", pointed).clicked() && !pointed {
                        td.style = TeardropStyle::Pointed45;
                        ch = true;
                    }
                    if widgets::pill(ui, "Flat top", !pointed).clicked() && pointed {
                        td.style = TeardropStyle::Flat(1.0);
                        ch = true;
                    }
                });
                if let Some(m) = &app.model {
                    let r = &m.result.process;
                    let n = r.teardropped.len();
                    let s = r.skipped.len();
                    let l = ui.label(widgets::muted(format!("{n} hole{} teardropped{}", if n == 1 { "" } else { "s" }, if s > 0 { format!(" · {s} skipped") } else { String::new() })));
                    if s > 0 {
                        // why, grouped: "8 × not horizontal"
                        let mut why: std::collections::BTreeMap<&str, usize> = Default::default();
                        for (_, reason) in &r.skipped {
                            *why.entry(reason.as_str()).or_default() += 1;
                        }
                        let tip = why.iter().map(|(w, k)| format!("{k} × {w}")).collect::<Vec<_>>().join("\n");
                        l.on_hover_text(format!("Skipped holes:\n{tip}"));
                    }
                }
            }
            if ch {
                let label = if td.enabled != active.teardrop.enabled {
                    if td.enabled { "Teardrops on" } else { "Teardrops off" }
                } else {
                    "Teardrop style"
                };
                let mut p = active.clone();
                p.teardrop = td;
                commit_profile(app, label, p);
            }
        }
    });
}

const FACES: [(CubeFace, &str); 6] = [
    (CubeFace::PosZ, "Top"),
    (CubeFace::NegY, "Front"),
    (CubeFace::PosX, "Right"),
    (CubeFace::NegZ, "Bottom"),
    (CubeFace::PosY, "Back"),
    (CubeFace::NegX, "Left"),
];

/// Unfolded-cube picker: click the face that goes down on the plate.
fn orientation_picker(app: &mut EasyCadApp, ui: &mut Ui) {
    let bodies: Vec<(BodyId, String)> = app
        .model
        .as_ref()
        .map(|m| {
            m.bodies
                .iter()
                .filter(|b| !b.body.reference)
                .map(|b| (b.body.id, b.body.name.clone()))
                .collect()
        })
        .unwrap_or_default();
    let target = app
        .orientation_body
        .filter(|b| bodies.iter().any(|x| x.0 == *b));
    let has_override =
        target.is_some_and(|b| app.doc.data.body_orientations.iter().any(|o| o.body == b));
    let o = match target {
        Some(b) => app.doc.data.orientation_of(b),
        None => app.doc.data.build_orientation,
    };
    let face = FACES
        .iter()
        .find(|x| x.0 == o.down)
        .map(|x| x.1)
        .unwrap_or("");
    let open = app.show_build_plate;
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::Label::new(
                RichText::new(format!("{} BUILD PLATE", if open { "▾" } else { "▸" }))
                    .size(10.5)
                    .color(theme::TEXT_LABEL),
            )
            .sense(Sense::click()),
        );
        if r.on_hover_text("Which face goes down on the printer bed")
            .clicked()
        {
            app.show_build_plate = !open;
        }
        if !open {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(widgets::muted(format!(
                    "{face} down · {}°",
                    o.quarter_turns as u32 * 90
                )));
            });
            return;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let name = match target {
                Some(b) => bodies
                    .iter()
                    .find(|x| x.0 == b)
                    .map(|x| x.1.clone())
                    .unwrap_or_default(),
                None => "document default".into(),
            };
            egui::ComboBox::from_id_salt("orient_body")
                .selected_text(name)
                .width(130.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(target.is_none(), "document default")
                        .clicked()
                    {
                        app.orientation_body = None;
                    }
                    for (id, n) in &bodies {
                        if ui.selectable_label(target == Some(*id), n).clicked() {
                            app.orientation_body = Some(*id);
                        }
                    }
                });
        });
    });
    if !open {
        return;
    }
    let mut new_o = None;
    ui.horizontal(|ui| {
        // net: [  Top  ] / [Left Front Right Back] / [ Bottom ]
        let cell = 26.0;
        let (rect, _) =
            ui.allocate_exact_size(vec2(cell * 4.0 + 6.0, cell * 3.0 + 4.0), Sense::hover());
        let at = |c: f32, r: f32| {
            Rect::from_min_size(
                rect.min + vec2(c * (cell + 2.0), r * (cell + 2.0)),
                vec2(cell, cell),
            )
        };
        let layout = [
            (CubeFace::PosZ, 1.0, 0.0),
            (CubeFace::NegX, 0.0, 1.0),
            (CubeFace::NegY, 1.0, 1.0),
            (CubeFace::PosX, 2.0, 1.0),
            (CubeFace::PosY, 3.0, 1.0),
            (CubeFace::NegZ, 1.0, 2.0),
        ];
        for (f, c, r) in layout {
            let rr = at(c, r);
            let resp = ui.interact(rr, ui.scope_id().with(("plate", f as u8)), Sense::click());
            let down = o.down == f;
            let fill = if down {
                theme::ACCENT_BG
            } else if resp.hovered() {
                theme::HOVER
            } else {
                theme::hex(0x2a2e35)
            };
            ui.painter().rect(
                rr,
                3.0,
                fill,
                Stroke::new(
                    1.0,
                    if down {
                        theme::ACCENT
                    } else {
                        theme::BORDER_INPUT
                    },
                ),
                egui::StrokeKind::Inside,
            );
            let name = FACES.iter().find(|x| x.0 == f).map(|x| x.1).unwrap_or("");
            ui.painter().text(
                rr.center(),
                egui::Align2::CENTER_CENTER,
                &name[..1],
                theme::prop(10.0),
                if down {
                    theme::TEXT_STRONG
                } else {
                    theme::TEXT_DIM
                },
            );
            if resp
                .on_hover_text(format!("{name} face down on the plate"))
                .clicked()
            {
                new_o = Some(BuildOrientation {
                    down: f,
                    quarter_turns: o.quarter_turns,
                });
            }
        }
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if ui
                    .small_button("⟲ 90°")
                    .on_hover_text("Turn on the plate −90°")
                    .clicked()
                {
                    new_o = Some(BuildOrientation {
                        quarter_turns: (o.quarter_turns + 3) % 4,
                        ..o
                    });
                }
                if ui
                    .small_button("⟳ 90°")
                    .on_hover_text("Turn on the plate +90°")
                    .clicked()
                {
                    new_o = Some(BuildOrientation {
                        quarter_turns: (o.quarter_turns + 1) % 4,
                        ..o
                    });
                }
            });
            ui.label(widgets::muted(format!(
                "{face} down · {}°",
                o.quarter_turns as u32 * 90
            )));
            if has_override && ui.small_button("Use document default").clicked() {
                let b = target.unwrap();
                let _ = app.doc.edit("Build orientation", |d| {
                    d.body_orientations.retain(|x| x.body != b);
                    Ok::<_, DocError>(())
                });
                app.after_edit();
            }
        });
    });
    if let Some(no) = new_o {
        app.doc.set_build_orientation(target, no);
        app.after_edit();
    }
    plate_preview(app, ui, target, o);
}

/// Mini preview: the body's silhouette (convex hull, iso view) sitting on a plate.
fn plate_preview(app: &EasyCadApp, ui: &mut Ui, body: Option<BodyId>, o: BuildOrientation) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 72.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 6.0, theme::SKETCH_BG);
    let Some(m) = &app.model else { return };
    let mesh = match body {
        Some(b) => m.mesh(b),
        None => m
            .bodies
            .iter()
            .find(|b| !b.body.reference)
            .and_then(|b| b.mesh.as_ref()),
    };
    let Some(mesh) = mesh else { return };
    let q: DQuat = o.quat();
    let step = (mesh.positions.len() / 1500).max(1);
    let pts: Vec<DVec3> = mesh
        .positions
        .iter()
        .step_by(step)
        .map(|v| q * DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64))
        .collect();
    if pts.is_empty() {
        return;
    }
    let minz = pts.iter().map(|v| v.z).fold(f64::INFINITY, f64::min);
    // iso projection
    let iso = |v: DVec3| -> (f64, f64) { ((v.x - v.y) * 0.866, -(v.z - minz) - (v.x + v.y) * 0.5) };
    let proj: Vec<(f64, f64)> = pts.iter().map(|v| iso(*v)).collect();
    let (mut x0, mut x1, mut y0, mut y1) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for (x, y) in &proj {
        x0 = x0.min(*x);
        x1 = x1.max(*x);
        y0 = y0.min(*y);
        y1 = y1.max(*y);
    }
    let s = ((rect.width() as f64 - 40.0) / (x1 - x0).max(1e-6))
        .min((rect.height() as f64 - 26.0) / (y1 - y0).max(1e-6));
    let cx = rect.center().x as f64 - (x0 + x1) / 2.0 * s;
    let cy = rect.bottom() as f64 - 14.0 - y1 * s;
    let to = |(x, y): (f64, f64)| pos2((cx + x * s) as f32, (cy + y * s) as f32);
    // plate (a parallelogram under the body)
    let ex = (x1 - x0) * 0.75;
    let base_y = cy as f32 + (y1 * s) as f32 - ((y1 - y0) * 0.0) as f32;
    let plate = vec![
        pos2(rect.center().x - ex as f32 * s as f32 * 0.9, base_y + 4.0),
        pos2(rect.center().x, base_y - 6.0),
        pos2(rect.center().x + ex as f32 * s as f32 * 0.9, base_y + 4.0),
        pos2(rect.center().x, base_y + 12.0),
    ];
    p.add(egui::Shape::convex_polygon(
        plate,
        theme::hex(0x2c3a4a),
        Stroke::new(1.0, theme::hex(0x3d5a7a)),
    ));
    let hull = convex_hull(proj.iter().map(|v| to(*v)).collect());
    if hull.len() >= 3 {
        p.add(egui::Shape::convex_polygon(
            hull,
            theme::hex(0x8b929d),
            Stroke::new(1.0, theme::EDGE_LIGHT),
        ));
    }
}

fn convex_hull(mut pts: Vec<Pos2>) -> Vec<Pos2> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup_by(|a, b| (a.x - b.x).abs() < 0.25 && (a.y - b.y).abs() < 0.25);
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: Pos2, a: Pos2, b: Pos2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut lower: Vec<Pos2> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<Pos2> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Selection context menu: per-hole teardrop override.
pub fn teardrop_menu(app: &mut EasyCadApp, ui: &mut Ui) {
    if app.doc.data.profile().kind != ProcessKind::Fdm {
        return;
    }
    let Some(m) = app.model.clone() else { return };
    let holes: Vec<(BodyId, FaceSelector)> = app
        .selection
        .iter()
        .filter_map(|k| match k.kind {
            PickKind::Face(f) => {
                let info = m.meshed(k.body)?.faces.get(f as usize)?;
                (info.kind == easycad_model::occt::SurfaceKind::Cylinder)
                    .then(|| (k.body, FaceSelector::from_pick(info)))
            }
            _ => None,
        })
        .collect();
    if holes.is_empty() {
        return;
    }
    ui.separator();
    for (label, val) in [
        ("Teardrop: always", Some(true)),
        ("Teardrop: never", Some(false)),
        ("Teardrop: automatic", None),
    ] {
        if ui.button(label).clicked() {
            let hs = holes.clone();
            let _ = app.doc.edit("Teardrop override", |d| {
                for (b, f) in hs {
                    d.teardrop_overrides
                        .retain(|o| !(o.body == b && o.face == f));
                    if let Some(on) = val {
                        d.teardrop_overrides.push(TeardropOverride {
                            body: b,
                            face: f,
                            enabled: on,
                        });
                    }
                }
                Ok::<_, DocError>(())
            });
            app.after_edit();
            ui.close();
        }
    }
}

/// Calibration: export the coupon, then enter what fit and update the profile.
pub fn calibrate_window(app: &mut EasyCadApp, ui: &mut Ui) {
    let Some(mut c) = app.calibrate.take() else {
        return;
    };
    let mut open = true;
    let mut apply = false;
    let mut export = false;
    egui::Window::new("Calibrate process").open(&mut open).collapsible(false).resizable(false).default_width(380.0).frame(widgets::card_frame().inner_margin(Margin::same(14))).show(ui.ctx(), |ui| {
        ui.label(RichText::new("1 · Print the coupon").strong());
        ui.label(widgets::muted("A plate of holes at nominal + offset (−0.10 … +0.50 in 0.05 steps) and nominal pins, with index notches. Print it with the process you are calibrating."));
        ui.horizontal(|ui| {
            ui.label("Nominal ⌀");
            ui.add(egui::DragValue::new(&mut c.nominal).range(2.0..=12.0).speed(0.1).suffix(" mm"));
            if widgets::button(ui, "Export coupon STL…").clicked() {
                export = true;
            }
        });
        if let Some(e) = &c.exported {
            ui.label(RichText::new(e).size(11.0).color(theme::GREEN));
        }
        ui.add_space(6.0);
        ui.label(RichText::new("2 · Which offset felt like…").strong());
        let offs = default_coupon_offsets();
        let r = &mut c.result;
        for (name, v) in [("press", &mut r.press_offset), ("snug", &mut r.snug_offset), ("slide", &mut r.slide_offset), ("loose", &mut r.loose_offset)] {
            ui.horizontal(|ui| {
                ui.add_sized(vec2(60.0, 20.0), egui::Label::new(name));
                egui::ComboBox::from_id_salt(("cal", name)).selected_text(format!("{:+.2}", *v)).width(90.0).show_ui(ui, |ui| {
                    for (i, o) in offs.iter().enumerate() {
                        if ui.selectable_label((*v - *o).abs() < 1e-9, format!("#{} · {:+.2}", i + 1, o)).clicked() {
                            *v = *o;
                        }
                    }
                });
            });
        }
        ui.add_space(4.0);
        ui.label(RichText::new("3 · Optional measurements (calipers)").strong());
        for (label, s) in [("Nominal hole measured ⌀", &mut c.hole), ("Pin measured ⌀", &mut c.pin), ("First-layer bulge / side", &mut c.first)] {
            ui.horizontal(|ui| {
                ui.add_sized(vec2(170.0, 20.0), egui::Label::new(label));
                ui.add(egui::TextEdit::singleline(s).desired_width(80.0).font(theme::mono(12.0)));
            });
        }
        ui.horizontal(|ui| {
            ui.label("Save as");
            ui.add(egui::TextEdit::singleline(&mut c.result.name).desired_width(220.0));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if widgets::primary(ui, "Update profile").clicked() {
                apply = true;
            }
        });
    });
    if export {
        let nominal = c.nominal;
        let req = crate::files::Request::save("Export fit coupon", "STL", "stl", "fit-coupon.stl");
        app.calibrate = Some(c);
        app.ask_path(req, move |app, path| {
            match calibration_coupon(nominal, &default_coupon_offsets())
                .and_then(|(s, _)| s.write_stl(&path, 0.02, false))
            {
                Ok(()) => {
                    if let Some(c) = app.calibrate.as_mut() {
                        c.exported = Some(format!("Wrote {}", path.display()));
                    }
                }
                Err(e) => app.warn(format!("Coupon export failed: {}", e.0)),
            }
        });
        return;
    }
    if apply {
        let parse = |s: &str| s.trim().parse::<f64>().ok();
        c.result.nominal = c.nominal;
        c.result.hole_measured = parse(&c.hole);
        c.result.pin_measured = parse(&c.pin);
        c.result.first_layer_measured = parse(&c.first);
        let p = ProcessProfile::from_calibration(&c.result);
        let name = p.name.clone();
        app.doc.add_profile(p);
        let _ = app.doc.set_active_profile(&name);
        app.toast(format!("Profile \"{name}\" updated and active"));
        app.after_edit();
        return;
    }
    if open {
        app.calibrate = Some(c);
    }
}
