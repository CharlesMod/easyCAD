//! Standoffs from an imported PCB (STEP board or DXF holes layer). The dialog
//! finds the board's mounting holes, lets the user untick some, picks a fastener
//! (setting hole/OD expressions) and sizes the height to the board underside.
//!
//! The engine's `StandoffsFromPcb` takes every hole in a diameter range; when some
//! holes are unticked the dialog falls back to a `ScrewBoss` at the ticked centres
//! (not linked to the board file; see GUI_NEEDS).

use casework_model::document::{DocData, ImportKind};
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::params::Expr;
use casework_model::occt::{CurveKind, Shape, SurfaceKind};
use casework_model::process::{Fit, FitClass};
use egui::{RichText, Ui};
use glam::{DVec2, DVec3};

use super::{BuildCx, DialogCx, Field};
use crate::theme;
use crate::widgets;
use crate::worker::Output;

#[derive(Clone, Debug)]
pub struct Hole {
    pub center: DVec2,
    pub dia: f64,
    pub on: bool,
}

pub struct Board {
    pub name: String,
    pub import: ImportId,
    pub kind: ImportKind,
    pub size: DVec3,
    pub bottom_z: f64,
    pub bbox_min: DVec3,
    pub bbox_max: DVec3,
}

pub struct StandoffsD {
    pub board: Option<Board>,
    pub boards: Vec<(ImportId, String)>,
    pub holes: Vec<Hole>,
    pub fastener: usize,
    pub plane_z: Field,
    pub od: Field,
    pub hole: Field,
    pub height: Field,
    pub base_fillet: Field,
    pub draft: Field,
    pub explode: bool,
    /// Wall clearance computed when the board was loaded.
    pub clearance_mm: Option<f64>,
    pub gussets: bool,
    pub lid_holes: bool,
    pub linked: bool,
}

pub const FASTENERS: [(&str, &str, &str, &str); 3] = [
    ("M3 self-tap", "boss_hole", "2.5", "self-tap"),
    ("M3 heat-set insert", "4", "4", "insert, 5.7 deep"),
    ("M2.5 self-tap", "2", "2", "self-tap"),
];

/// Holes of a STEP board: circular edges on its largest planar face facing ±Z.
pub fn board_holes(shape: &Shape) -> (Vec<Hole>, f64) {
    let Ok(faces) = shape.faces() else {
        return (Vec::new(), 0.0);
    };
    let Some(big) = faces
        .iter()
        .filter(|f| f.kind == SurfaceKind::Plane && f.normal.z.abs() > 0.99)
        .max_by(|a, b| a.area.total_cmp(&b.area))
        .cloned()
    else {
        return (Vec::new(), 0.0);
    };
    let mut holes: Vec<Hole> = Vec::new();
    for e in shape.face_edges(big.index).unwrap_or_default() {
        if let Ok(edges) = shape.edges() {
            if let Some(info) = edges.get(e as usize) {
                if info.kind == CurveKind::Circle && info.closed {
                    if let Some((c, _, r)) = info.circle {
                        let p = DVec2::new(c.x, c.y);
                        if !holes.iter().any(|h| h.center.distance(p) < 1e-3) {
                            holes.push(Hole {
                                center: p,
                                dia: 2.0 * r,
                                on: true,
                            });
                        }
                    }
                }
            }
        }
    }
    holes.sort_by(|a, b| {
        (a.center.y.round(), a.center.x)
            .partial_cmp(&(b.center.y.round(), b.center.x))
            .unwrap()
    });
    (holes, big.centroid.z)
}

fn ref_body_shape<'a>(d: &DocData, model: Option<&'a Output>, imp: ImportId) -> Option<&'a Shape> {
    let f = d
        .features
        .iter()
        .find(|f| matches!(&f.kind, FeatureKind::ImportStep { import, .. } if *import == imp))?;
    model?
        .bodies
        .iter()
        .find(|b| b.body.id.feature == f.id)
        .map(|b| b.body.shape())
}

impl StandoffsD {
    pub fn new(d: &DocData, model: Option<&Output>) -> Result<Self, String> {
        let boards: Vec<(ImportId, String)> = d
            .imports
            .iter()
            .map(|i| (i.id, i.import.name.clone()))
            .collect();
        if boards.is_empty() {
            return Err("Import the PCB first (STEP or DXF), then add standoffs.".into());
        }
        let mut s = StandoffsD {
            board: None,
            boards,
            holes: Vec::new(),
            fastener: 0,
            plane_z: Field::new(if d.params.get("wall").is_some() {
                "wall"
            } else {
                "0"
            }),
            od: Field::new(if d.params.get("boss_od").is_some() {
                "boss_od"
            } else {
                "7"
            }),
            hole: Field::with_fit(
                if d.params.get("boss_hole").is_some() {
                    "boss_hole"
                } else {
                    "2.5"
                },
                Fit::hole(FitClass::Snug),
            ),
            height: Field::new("10"),
            base_fillet: Field::new("0.8"),
            draft: Field::new(if d.params.get("draft").is_some() {
                "draft"
            } else {
                "0"
            }),
            explode: true,
            clearance_mm: None,
            gussets: false,
            lid_holes: false,
            linked: true,
        };
        // prefer a STEP board
        let first = d
            .imports
            .iter()
            .rev()
            .find(|i| i.import.kind == ImportKind::Step)
            .or(d.imports.last())
            .map(|i| i.id);
        if let Some(i) = first {
            s.load_board(d, model, i);
        }
        Ok(s)
    }

    pub fn from_feature(f: &Feature, d: &DocData) -> Option<Self> {
        let FeatureKind::StandoffsFromPcb {
            source,
            od,
            hole_dia,
            height,
            base_fillet,
            draft_deg,
            plane,
            offset,
            exclude,
            gussets,
            lid_holes,
            ..
        } = &f.kind
        else {
            return None;
        };
        let import = match source {
            PcbSource::Step { import, .. } | PcbSource::Dxf { import, .. } => *import,
        };
        let mut s = StandoffsD::new(d, None).ok()?;
        s.board = None;
        s.load_board(d, None, import);
        s.od = Field::from_expr(od, None);
        s.hole = Field::from_expr(hole_dia, crate::dialogs::default_fit(&crate::dialogs::boss_template(), "hole_dia"));
        s.height = Field::from_expr(height, None);
        s.base_fillet = Field::from_expr(base_fillet, None);
        s.draft = Field::from_expr(draft_deg, None);
        s.plane_z = Field::new(super::plane_text(plane, offset));
        for h in &mut s.holes {
            h.on = !exclude.iter().any(|e| e.distance(h.center) < 0.5);
        }
        s.gussets = gussets.is_some();
        s.lid_holes = lid_holes.is_some();
        Some(s)
    }

    pub fn load_board(&mut self, d: &DocData, model: Option<&Output>, imp: ImportId) {
        let Some(entry) = d.imports.iter().find(|i| i.id == imp) else {
            return;
        };
        let name = entry.import.name.clone();
        match entry.import.kind {
            ImportKind::Step => {
                let shape = ref_body_shape(d, model, imp);
                let (holes, _) = shape.map(board_holes).unwrap_or_default();
                let bb = shape.and_then(|s| s.bbox().ok());
                let (size, minp, maxp) = bb.map(|b| (b.size(), b.min, b.max)).unwrap_or((
                    DVec3::ZERO,
                    DVec3::ZERO,
                    DVec3::ZERO,
                ));
                self.holes = holes;
                self.board = Some(Board {
                    name: format!("{name}.step"),
                    import: imp,
                    kind: ImportKind::Step,
                    size,
                    bottom_z: minp.z,
                    bbox_min: minp,
                    bbox_max: maxp,
                });
            }
            ImportKind::Dxf => {
                self.holes.clear();
                self.board = Some(Board {
                    name,
                    import: imp,
                    kind: ImportKind::Dxf,
                    size: DVec3::ZERO,
                    bottom_z: 0.0,
                    bbox_min: DVec3::ZERO,
                    bbox_max: DVec3::ZERO,
                });
            }
        }
        self.height_to_board(d);
        self.clearance_mm = self.clearance(d, model);
    }

    fn height_to_board(&mut self, d: &DocData) {
        let vals = crate::adapters::values(d);
        if let (Some(b), Ok(z)) = (&self.board, crate::widgets::eval(&self.plane_z.text, &vals)) {
            if b.kind == ImportKind::Step && b.bottom_z > z {
                self.height = Field::new(super::fmt(b.bottom_z - z));
            }
        }
    }

    fn range(&self) -> (f64, f64) {
        let ds: Vec<f64> = self.holes.iter().map(|h| h.dia).collect();
        let lo = ds.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = ds.iter().cloned().fold(0.0, f64::max);
        if lo.is_finite() {
            (lo - 0.05, hi + 0.05)
        } else {
            (2.5, 4.0)
        }
    }

    pub fn build(
        &self,
        cx: &BuildCx,
        editing: Option<FeatureId>,
        name: &str,
    ) -> Result<Vec<Feature>, String> {
        let board = self.board.as_ref().ok_or("pick the board")?;
        let vals = crate::adapters::values(&cx.data);
        let z = crate::widgets::eval(&self.plane_z.text, &vals)?;
        let _ = z;
        let plane = Plane::XY;
        let offset = Expr::new(crate::units::normalize_input(self.plane_z.text.trim()));
        let id = cx.id(editing, 0);
        let all_on = self.holes.iter().all(|h| h.on);
        let fit = crate::dialogs::default_fit(&crate::dialogs::boss_template(), "hole_dia");
        let _ = all_on;
        if !self.holes.is_empty() && self.holes.iter().all(|h| !h.on) {
            return Err("tick at least one mounting hole".into());
        }
        let (lo, hi) = self.range();
        let source = match board.kind {
            ImportKind::Step => PcbSource::Step {
                import: board.import,
                min_dia: lo,
                max_dia: hi,
            },
            ImportKind::Dxf => PcbSource::Dxf {
                import: board.import,
                layers: Vec::new(),
                min_dia: lo,
                max_dia: hi,
                offset: DVec2::ZERO,
            },
        };
        let has = |n: &str| vals.contains_key(n);
        // unticked holes stay linked to the board, just without a standoff
        let kind = FeatureKind::StandoffsFromPcb {
            body: None,
            source,
            plane,
            od: self.od.expr(None),
            hole_dia: self.hole.expr(fit),
            height: self.height.expr(None),
            base_fillet: self.base_fillet.expr(None),
            draft_deg: self.draft.expr(None),
            offset,
            exclude: self.holes.iter().filter(|h| !h.on).map(|h| h.center).collect(),
            gussets: self.gussets.then(|| Gussets {
                thickness: Expr::new(if has("wall") { "wall * 0.75" } else { "1.5" }),
                height: None,
                max_length: Expr::new("25"),
            }),
            lid_holes: self.lid_holes.then(|| LidHoles {
                body: None,
                hole_dia: Expr::new("3.4"),
                cbore_dia: Expr::new("6.5"),
                cbore_depth: Expr::new("3"),
            }),
        };
        Ok(vec![Feature {
            id,
            name: name.to_string(),
            suppressed: false,
            kind,
        }])
    }

    /// Approximate wall clearance (bounding boxes) for the banner.
    pub fn clearance(&self, d: &DocData, model: Option<&Output>) -> Option<f64> {
        let b = self.board.as_ref()?;
        let vals = crate::adapters::values(d);
        let wall = vals.get("wall").copied().unwrap_or(0.0);
        let solid = model?.bodies.iter().find(|x| !x.body.reference)?;
        let bb = solid.body.shape().bbox().ok()?;
        let g = [
            bb.max.x - wall - b.bbox_max.x,
            b.bbox_min.x - (bb.min.x + wall),
            bb.max.y - wall - b.bbox_max.y,
            b.bbox_min.y - (bb.min.y + wall),
        ];
        g.into_iter().reduce(f64::min)
    }

    pub fn ui(&mut self, ui: &mut Ui, id: egui::Id, cx: &DialogCx) -> bool {
        let env = &cx.env;
        let mut ch = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Board").color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, "Re-read")
                    .on_hover_text("Find the holes again from the imported board")
                    .clicked()
                {
                    if let Some(i) = self.board.as_ref().map(|b| b.import) {
                        self.load_board(cx.data, cx.model, i);
                        ch = true;
                    }
                }
                let cur = self
                    .board
                    .as_ref()
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| "pick a board".into());
                let mut pick = None;
                egui::ComboBox::from_id_salt(id.with("board"))
                    .selected_text(cur)
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for (i, n) in &self.boards {
                            if ui
                                .selectable_label(
                                    self.board.as_ref().is_some_and(|b| b.import == *i),
                                    n,
                                )
                                .clicked()
                            {
                                pick = Some(*i);
                            }
                        }
                    });
                if let Some(i) = pick {
                    self.load_board(cx.data, cx.model, i);
                    ch = true;
                }
            });
        });
        if let Some(b) = &self.board {
            ui.label(widgets::mono_text(&b.name));
            if b.kind == ImportKind::Step {
                ui.label(widgets::muted(format!(
                    "{:.2} × {:.2} × {:.2} · bottom face at z = {:.2}",
                    b.size.x,
                    b.size.y,
                    b.size.z.min(9.99),
                    b.bottom_z
                )));
            }
        }
        widgets::divider(ui);
        let used = self.holes.iter().filter(|h| h.on).count();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Mounting holes").color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(widgets::muted(format!(
                    "{used} of {} used",
                    self.holes.len()
                )));
            });
        });
        if self.holes.is_empty() {
            ui.label(widgets::muted(
                "No round holes found on the board's largest face.",
            ));
        }
        for (i, h) in self.holes.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ch |= ui.checkbox(&mut h.on, "").changed();
                ui.label(widgets::mono_text(format!("H{}", i + 1)));
                ui.label(
                    RichText::new(format!("({:6.2}, {:6.2})", h.center.x, h.center.y))
                        .font(theme::mono(12.0))
                        .color(theme::TEXT_DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(widgets::muted(format!("⌀{:.2} PTH", h.dia)));
                });
            });
        }
        if used < self.holes.len() && used > 0 {
            ui.label(
                RichText::new(
                    "Unticked holes get no standoff; the rest stay linked to the board file.",
                )
                .size(11.0)
                .color(theme::ORANGE_SOFT),
            );
        }
        widgets::divider(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Fastener").color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt(id.with("fast"))
                    .selected_text(FASTENERS[self.fastener].0)
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for (i, f) in FASTENERS.iter().enumerate() {
                            if ui.selectable_label(self.fastener == i, f.0).clicked() {
                                self.fastener = i;
                                let hole = if cx.data.params.get(f.1).is_some() {
                                    f.1
                                } else {
                                    f.2
                                };
                                self.hole.text = hole.into();
                                self.od.text = match i {
                                    1 => "boss_od + 1.5",
                                    2 => "boss_od - 1",
                                    _ => "boss_od",
                                }
                                .into();
                                if cx.data.params.get("boss_od").is_none() {
                                    self.od.text = ["7", "8.5", "6"][i].into();
                                }
                                ch = true;
                            }
                        }
                    });
            });
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Hole").color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let v = crate::widgets::eval(&self.hole.text, env.values).unwrap_or(0.0);
                ui.label(widgets::muted(format!("· {}", FASTENERS[self.fastener].3)));
                ui.label(widgets::mono_text(format!("⌀{v:.2}")));
            });
        });
        ch |= self.hole.row(ui, id.with("h"), "Hole expression", env);
        ch |= self.od.row(ui, id.with("od"), "Boss OD", env);
        ch |= self.plane_z.row(ui, id.with("z"), "Floor z", env);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Height").color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("To board")
                    .on_hover_text("Size to the board underside")
                    .clicked()
                {
                    self.height_to_board(cx.data);
                    ch = true;
                }
                let o = crate::widgets::expr_field(
                    ui,
                    id.with("ht"),
                    &mut self.height.text,
                    80.0,
                    env,
                    None,
                );
                ch |= o.changed;
                ui.label(widgets::muted("to board underside"));
            });
        });
        ch |= self.base_fillet.row(ui, id.with("bf"), "Base fillet", env);
        ch |= self.draft.row(ui, id.with("dr"), "Draft", env);
        widgets::divider(ui);
        ch |= ui
            .checkbox(&mut self.gussets, "Gusset ribs to nearest wall")
            .on_hover_text("Ribs from each standoff to the closest wall (up to 25 mm away)")
            .changed();
        ch |= ui
            .checkbox(&mut self.lid_holes, "Matching counterbored holes in lid")
            .on_hover_text("M3 clearance holes, counterbored for the head, through the lid above each standoff")
            .changed();
        let _ = used;
        let mut l = true;
        ui.add_enabled(false, egui::Checkbox::new(&mut l, "Stay linked to the PCB file"))
            .on_disabled_hover_text("Always linked: unticked holes are excluded, not unlinked");
        ch
    }
}
