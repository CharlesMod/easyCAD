//! Overhang heat map (view toolbar): every shown body coloured by how far its
//! surface leans out over the build direction (`easycad_model::overhang`), with a
//! legend and a build-direction picker. The critical angle comes from the active
//! process profile. Colours are computed once per (mesh, direction in the mesh's
//! frame, critical angle) and kept as the same `Arc`, so the viewport's image
//! cache and GPU uploads see an unchanged body as unchanged. Off, it costs nothing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use easycad_model::occt::Mesh;
use easycad_model::overhang::{analyze, WALL_TOL_DEG};
use easycad_model::process::CubeFace;
use egui::{pos2, vec2, Color32, RichText, Sense, Stroke, Ui};
use glam::DVec3;

use crate::app::EasyCadApp;
use crate::theme;
use crate::widgets;

pub type VertexColors = Arc<Vec<[f32; 3]>>;

/// Heat map colours (sRGB).
pub const OK: [f32; 3] = [0.60, 0.64, 0.69];
pub const PLATE: [f32; 3] = [0.30, 0.52, 0.86];
const MILD: [f32; 3] = [0.96, 0.84, 0.30];
const STEEP: [f32; 3] = [0.96, 0.52, 0.16];
pub const SUPPORT: [f32; 3] = [0.88, 0.20, 0.16];

pub const DIRS: [(CubeFace, &str); 6] = [
    (CubeFace::PosX, "+X"),
    (CubeFace::NegX, "−X"),
    (CubeFace::PosY, "+Y"),
    (CubeFace::NegY, "−Y"),
    (CubeFace::PosZ, "+Z"),
    (CubeFace::NegZ, "−Z"),
];

struct Entry {
    /// Held so the pointer in the key stays unique.
    mesh: Arc<Mesh>,
    colors: VertexColors,
    /// (overhang, needs support) areas, mm².
    areas: (f64, f64),
}

type Key = (usize, [i64; 3], u64);

pub struct OverhangUi {
    pub on: bool,
    /// The build (up) direction in the view frame.
    pub up: CubeFace,
    cache: Mutex<HashMap<Key, Entry>>,
    /// Areas of the bodies drawn in the last frame, keyed like the cache.
    shown: Mutex<HashMap<Key, (f64, f64)>>,
}

impl Default for OverhangUi {
    fn default() -> Self {
        OverhangUi { on: false, up: CubeFace::PosZ, cache: Mutex::default(), shown: Mutex::default() }
    }
}

impl OverhangUi {
    pub fn toggle(&mut self) {
        self.on = !self.on;
        if !self.on {
            self.cache.lock().unwrap().clear();
            self.shown.lock().unwrap().clear();
        }
    }

    /// Start a frame's set of shown bodies (for the legend's totals).
    pub fn begin_frame(&self) {
        self.shown.lock().unwrap().clear();
    }

    /// Heat map colours of a mesh whose frame sees the build direction as `up`.
    pub fn colors(&self, mesh: &Arc<Mesh>, up: DVec3, critical: f64) -> VertexColors {
        let up = up.normalize_or_zero();
        let q = |v: f64| (v * 1e4).round() as i64;
        let k: Key = (Arc::as_ptr(mesh) as usize, [q(up.x), q(up.y), q(up.z)], critical.to_bits());
        let mut c = self.cache.lock().unwrap();
        // drop the colours of meshes nothing else holds any more
        c.retain(|_, e| Arc::strong_count(&e.mesh) > 1);
        let e = c.entry(k).or_insert_with(|| {
            let r = analyze(mesh, up, critical, None);
            Entry { mesh: mesh.clone(), colors: Arc::new(r.vertex_angles.iter().map(|a| heat(*a, critical as f32)).collect()), areas: (r.overhang_area(), r.support_area()) }
        });
        self.shown.lock().unwrap().insert(k, e.areas);
        e.colors.clone()
    }

    /// (overhang, needs support) areas over the bodies drawn last frame.
    pub fn totals(&self) -> (f64, f64) {
        self.shown.lock().unwrap().values().fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1))
    }
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Colour of an overhang angle (degrees from vertical; negative = on the plate).
pub fn heat(angle: f32, critical: f32) -> [f32; 3] {
    if angle < 0.0 {
        PLATE
    } else if angle <= WALL_TOL_DEG as f32 {
        OK
    } else if angle <= critical {
        lerp(MILD, STEEP, angle / critical.max(1.0))
    } else {
        SUPPORT
    }
}

fn c32(c: [f32; 3]) -> Color32 {
    Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)
}

impl EasyCadApp {
    /// The critical overhang angle of the active process profile.
    pub fn critical_overhang(&self) -> f64 {
        self.doc.data.profile().critical_overhang_deg()
    }
}

/// Legend and build-direction picker, above the view toolbar on the left.
pub fn legend(app: &mut EasyCadApp, ui: &mut Ui, rect: egui::Rect) {
    if !app.overhang.on {
        return;
    }
    let crit = app.critical_overhang();
    let (over, support) = app.overhang.totals();
    // bottom left, clear of the view toolbar's row
    let pos = pos2(rect.left() + 12.0, rect.bottom() - 66.0);
    egui::Area::new(egui::Id::unique("overhang_legend")).order(egui::Order::Middle).pivot(egui::Align2::LEFT_BOTTOM).fixed_pos(pos).show(ui.ctx(), |ui| {
        crate::ui_viewport::overlay_frame().show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("OVERHANGS · BUILD UP").size(10.5).color(theme::TEXT_LABEL));
                for (f, label) in DIRS {
                    if widgets::seg(ui, label, app.overhang.up == f).on_hover_text(format!("Build direction {label}")).clicked() {
                        app.overhang.up = f;
                    }
                }
            });
            let row = |ui: &mut Ui, c: Color32, text: String| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(22.0, 10.0), Sense::hover());
                    ui.painter().rect(r, 2.0, c, Stroke::new(1.0, theme::BORDER_CARD), egui::StrokeKind::Inside);
                    ui.label(RichText::new(text).size(11.5));
                });
            };
            row(ui, c32(OK), "Walls and up-facing".into());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(22.0, 10.0), Sense::hover());
                let p = ui.painter();
                for i in 0..11 {
                    let t = i as f32 / 10.0;
                    let x = r.left() + t * r.width();
                    p.vline(x, r.y_range(), Stroke::new(2.4, c32(lerp(MILD, STEEP, t))));
                }
                ui.label(RichText::new(format!("Overhang up to {crit:.0}° · {}", area(over))).size(11.5));
            });
            row(ui, c32(SUPPORT), format!("Needs support (over {crit:.0}°) · {}", area(support)));
            row(ui, c32(PLATE), "On the build plate".into());
            ui.label(widgets::muted(format!("Angles from vertical · {}", app.doc.data.profile().name)));
        });
    });
}

fn area(a: f64) -> String {
    if crate::units::inch() {
        format!("{:.2} in²", a / 645.16)
    } else {
        format!("{a:.0} mm²")
    }
}
