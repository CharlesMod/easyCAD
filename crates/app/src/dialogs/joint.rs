//! Parting joint (tongue & groove, lip, O-ring gland, snap fit), built on the
//! engine's `FeatureKind::PartingJoint`. The checks come from the preview's
//! `JointReport`; the live wall section is drawn from the dialog's values.

use casework_model::document::DocData;
use casework_model::feature::*;
use casework_model::geom::Plane;
use casework_model::joint::{Half, JointKind as EK, JointReport, Rim, SnapPositions};
use casework_model::params::Expr;
use casework_model::process::{Fit, FitClass};
use casework_model::regen::FeatureStatus;
use egui::{pos2, vec2, Color32, RichText, Stroke, Ui};

use super::{BuildCx, DialogCx, Field};
use crate::theme;
use crate::widgets;
use crate::worker::Output;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Tongue,
    Lip,
    ORing,
    Snap,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Tongue, Kind::Lip, Kind::ORing, Kind::Snap];
    pub fn label(self) -> &'static str {
        match self {
            Kind::Tongue => "Tongue & groove",
            Kind::Lip => "Lip",
            Kind::ORing => "O-ring",
            Kind::Snap => "Snap fit",
        }
    }
}

pub struct JointD {
    pub body: Option<BodyId>,
    pub z: Field,
    pub kind: Kind,
    pub class: FitClass,
    pub male_lower: bool,
    pub rim_on: bool,
    pub rim_w: Field,
    pub rim_h: Field,
    // tongue & groove
    pub t_width: Field,
    pub t_height: Field,
    pub t_clear: Field,
    pub t_extra: Field,
    pub t_lead: Field,
    // lip
    pub l_width: Field,
    pub l_height: Field,
    pub l_clear: Field,
    // o-ring
    pub cord: String,
    pub o_squeeze: Field,
    pub o_depth: Field,
    pub o_width: Field,
    // snap
    pub s_count: u32,
    pub s_len: Field,
    pub s_thick: Field,
    pub s_width: Field,
    pub s_under: Field,
    pub s_ins: Field,
    pub s_ret: Field,
    pub s_clear: Field,
    pub material: String,
    pub explode: bool,
    pub z_min: f64,
    pub z_max: f64,
    /// From the last preview.
    pub report: Option<JointReport>,
    /// The z slider is being dragged (coarse previews).
    pub dragging: bool,
    pub status: Option<FeatureStatus>,
    /// Name of the upper half.
    pub upper_name: String,
    /// The plain Split this joint takes the place of (its name), see `adopt_split`.
    pub replaces: Option<String>,
}

fn f(s: &str) -> Field {
    Field::new(s)
}

fn opt(e: &Option<Expr>) -> Field {
    Field::new(e.as_ref().map(|e| e.0.clone()).unwrap_or_default())
}

impl JointD {
    pub fn new(d: &DocData, model: Option<&Output>) -> Self {
        let has = |n: &str| d.params.get(n).is_some();
        let wall = if has("wall") { "wall" } else { "2" };
        let solid = model.and_then(|m| m.bodies.iter().find(|b| !b.body.reference));
        // the slider spans every solid (a model already split into halves too)
        let (z_min, z_max) = model
            .into_iter()
            .flat_map(|m| m.bodies.iter())
            .filter(|b| !b.body.reference)
            .filter_map(|b| b.body.shape().bbox().ok())
            .fold(None, |acc: Option<(f64, f64)>, b| {
                Some(match acc {
                    None => (b.min.z, b.max.z),
                    Some((lo, hi)) => (lo.min(b.min.z), hi.max(b.max.z)),
                })
            })
            .unwrap_or((0.0, 40.0));
        JointD {
            body: solid.map(|b| b.body.id),
            z: f(if has("lid_split") { "lid_split" } else { "20" }),
            kind: Kind::Tongue,
            class: FitClass::Slide,
            male_lower: true,
            rim_on: true,
            rim_w: f(if has("rim_w") { "rim_w" } else { "4" }),
            rim_h: f("3"),
            t_width: f(&format!("{wall} * 0.5")),
            t_height: f("1.3"),
            // clearances come from the fit class / process profile, not a typed number
            t_clear: f("0"),
            t_extra: f("0.2"),
            t_lead: f("0.3"),
            l_width: f(&format!("{wall} / 2")),
            l_height: f("1.6"),
            l_clear: f("0"),
            cord: "AS568-0xx".into(),
            o_squeeze: f("0.25"),
            o_depth: f(""),
            o_width: f(""),
            s_count: 4,
            s_len: f("8"),
            s_thick: f("1"),
            s_width: f("6"),
            s_under: f("0.5"),
            s_ins: f("30"),
            s_ret: f("90"),
            s_clear: f("0.2"),
            material: "PETG".into(),
            explode: true,
            z_min,
            z_max,
            report: None,
            dragging: false,
            status: None,
            upper_name: "Lid".into(),
            replaces: None,
        }
    }

    /// A new joint on a model that is already cut in two by a plain horizontal
    /// Split takes that Split's place: same feature id (so the upper half keeps its
    /// body id and later features keep their references), same body, plane
    /// expression and upper-half name. Returns the Split's id to edit in place.
    pub fn adopt_split(&mut self, d: &DocData) -> Option<FeatureId> {
        if d
            .features
            .iter()
            .any(|f| !f.suppressed && matches!(f.kind, FeatureKind::PartingJoint { .. }))
        {
            return None;
        }
        let (f, body, plane, upper) = d.features.iter().rev().find_map(|f| match &f.kind {
            FeatureKind::Split {
                body,
                plane,
                new_name,
                ..
            } if !f.suppressed && plane.normal.z.abs() > 0.999 => {
                Some((f, *body, *plane, new_name.clone()))
            }
            _ => None,
        })?;
        self.body = body.or(self.body);
        self.z = Field::new(
            d.meta
                .get(&crate::adapters::bind_key(f.id))
                .cloned()
                .unwrap_or_else(|| super::fmt(plane.origin.z * plane.normal.z.signum())),
        );
        self.upper_name = upper;
        self.replaces = Some(f.name.clone());
        Some(f.id)
    }

    pub fn from_feature(feat: &Feature, d: &DocData) -> Option<Self> {
        let FeatureKind::PartingJoint {
            body,
            offset,
            kind,
            rim,
            class,
            ..
        } = &feat.kind
        else {
            return None;
        };
        let mut j = JointD::new(d, None);
        j.body = *body;
        if let FeatureKind::PartingJoint { upper_name, .. } = &feat.kind {
            j.upper_name = upper_name.clone();
        }
        j.z = Field::from_expr(offset, None);
        j.class = *class;
        j.rim_on = rim.is_some();
        if let Some(r) = rim {
            j.rim_w = Field::from_expr(&r.width, None);
            j.rim_h = Field::from_expr(&r.height, None);
        }
        match kind {
            EK::TongueGroove {
                width,
                height,
                clearance,
                extra_depth,
                tongue_on,
                lead_in,
            } => {
                j.kind = Kind::Tongue;
                j.t_width = Field::from_expr(width, None);
                j.t_height = Field::from_expr(height, None);
                j.t_clear = Field::from_expr(clearance, None);
                j.t_extra = Field::from_expr(extra_depth, None);
                j.t_lead = Field::from_expr(lead_in, None);
                j.male_lower = *tongue_on == Half::Lower;
            }
            EK::Lip {
                width,
                height,
                clearance,
                lip_on,
            } => {
                j.kind = Kind::Lip;
                j.l_width = Field::from_expr(width, None);
                j.l_height = Field::from_expr(height, None);
                j.l_clear = Field::from_expr(clearance, None);
                j.male_lower = *lip_on == Half::Lower;
            }
            EK::ORingGland {
                cord,
                squeeze,
                depth,
                width,
                gland_on,
                ..
            } => {
                j.kind = Kind::ORing;
                j.cord = cord.clone();
                j.o_squeeze = opt(squeeze);
                j.o_depth = opt(depth);
                j.o_width = opt(width);
                j.male_lower = *gland_on == Half::Lower;
            }
            EK::SnapFit {
                positions,
                beam_length,
                thickness,
                width,
                undercut,
                insertion_deg,
                retention_deg,
                material,
                beam_on,
                clearance,
                ..
            } => {
                j.kind = Kind::Snap;
                j.s_count = match positions {
                    SnapPositions::Count(n) => *n,
                    SnapPositions::At(v) => v.len() as u32,
                };
                j.s_len = Field::from_expr(beam_length, None);
                j.s_thick = Field::from_expr(thickness, None);
                j.s_width = Field::from_expr(width, None);
                j.s_under = Field::from_expr(undercut, None);
                j.s_ins = Field::from_expr(insertion_deg, None);
                j.s_ret = Field::from_expr(retention_deg, None);
                j.s_clear = Field::from_expr(clearance, None);
                j.material = material.clone();
                j.male_lower = *beam_on == Half::Lower;
            }
        }
        Some(j)
    }

    fn half(&self) -> Half {
        if self.male_lower {
            Half::Lower
        } else {
            Half::Upper
        }
    }

    fn engine_kind(&self) -> EK {
        let o = |fl: &Field| (!fl.text.trim().is_empty()).then(|| fl.expr(None));
        match self.kind {
            Kind::Tongue => EK::TongueGroove {
                width: self.t_width.expr(None),
                height: self.t_height.expr(None),
                clearance: self.t_clear.expr(None),
                extra_depth: self.t_extra.expr(None),
                tongue_on: self.half(),
                lead_in: self.t_lead.expr(None),
            },
            Kind::Lip => EK::Lip {
                width: self.l_width.expr(None),
                height: self.l_height.expr(None),
                clearance: self.l_clear.expr(None),
                lip_on: self.half(),
            },
            Kind::ORing => EK::ORingGland {
                cord: self.cord.clone(),
                squeeze: o(&self.o_squeeze),
                depth: o(&self.o_depth),
                width: o(&self.o_width),
                gland_on: self.half(),
                compression_rib: None,
            },
            Kind::Snap => EK::SnapFit {
                positions: SnapPositions::Count(self.s_count),
                beam_length: self.s_len.expr(None),
                thickness: self.s_thick.expr(None),
                width: self.s_width.expr(None),
                undercut: self.s_under.expr(None),
                insertion_deg: self.s_ins.expr(None),
                retention_deg: self.s_ret.expr(None),
                material: self.material.clone(),
                beam_on: self.half(),
                clearance: self.s_clear.expr(None),
                margin: Expr::new("2"),
            },
        }
    }

    fn fields(&self) -> Vec<&Field> {
        let mut v = vec![&self.z];
        if self.rim_on {
            v.extend([&self.rim_w, &self.rim_h]);
        }
        match self.kind {
            Kind::Tongue => v.extend([
                &self.t_width,
                &self.t_height,
                &self.t_clear,
                &self.t_extra,
                &self.t_lead,
            ]),
            Kind::Lip => v.extend([&self.l_width, &self.l_height, &self.l_clear]),
            Kind::ORing => v.extend([&self.o_squeeze, &self.o_depth, &self.o_width]),
            Kind::Snap => v.extend([
                &self.s_len,
                &self.s_thick,
                &self.s_width,
                &self.s_under,
                &self.s_ins,
                &self.s_ret,
                &self.s_clear,
            ]),
        }
        v
    }

    pub fn refs(&self) -> std::collections::BTreeSet<String> {
        let mut s = std::collections::BTreeSet::new();
        for fl in self.fields() {
            s.extend(crate::adapters::refs_of(&fl.text));
        }
        s
    }

    pub fn build(
        &self,
        cx: &BuildCx,
        editing: Option<FeatureId>,
        name: &str,
    ) -> Result<Vec<Feature>, String> {
        let vals = crate::adapters::values(&cx.data);
        crate::widgets::eval(&self.z.text, &vals)?;
        Ok(vec![Feature {
            id: cx.id(editing, 0),
            name: if name == "Parting joint" {
                format!("Joint · {}", self.kind.label())
            } else {
                name.to_string()
            },
            suppressed: false,
            kind: self.feature_kind(),
        }])
    }

    /// The engine feature for the current settings.
    pub fn feature_kind(&self) -> FeatureKind {
        FeatureKind::PartingJoint {
            body: self.body,
            plane: Plane::XY,
            offset: self.z.expr(None),
            kind: self.engine_kind(),
            rim: self.rim_on.then(|| Rim {
                width: self.rim_w.expr(None),
                height: self.rim_h.expr(None),
            }),
            upper_name: self.upper_name.clone(),
            class: self.class,
            closing_radius: None,
        }
    }

    pub fn on_preview(&mut self, out: &Output) {
        let fid = out
            .prefix_bodies
            .keys()
            .min()
            .and_then(|i| out.data.features.get(*i))
            .map(|f| f.id);
        if let Some(r) = fid.and_then(|id| out.result.report(id)) {
            self.report = r.joint.clone();
            self.status = Some(r.status.clone());
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, id: egui::Id, cx: &DialogCx) -> bool {
        let env = &cx.env;
        let mut ch = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Body").color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let n = self
                    .body
                    .and_then(|b| cx.model.and_then(|m| m.body(b)))
                    .map(|b| format!("{} (shelled)", b.name))
                    .unwrap_or_else(|| "last solid".into());
                ui.label(n);
            });
        });
        if let Some(r) = &self.replaces {
            ui.label(
                RichText::new(format!(
                    "Replaces “{r}”: same halves, later features keep their references."
                ))
                .size(11.0)
                .color(theme::ACCENT_LINK),
            );
        }
        ch |= self.z.row(ui, id.with("z"), "Split plane z", env);
        let mut zv = crate::widgets::eval(&self.z.text, env.values).unwrap_or(self.z_min);
        ui.spacing_mut().slider_width = ui.available_width();
        let sr = ui.add(
            egui::Slider::new(&mut zv, self.z_min..=self.z_max)
                .step_by(0.5)
                .show_value(false),
        );
        self.dragging = sr.dragged();
        if sr.changed() {
            self.z.text = super::fmt(zv);
            ch = true;
        }
        ui.label(widgets::muted(
            "Drag, or type an expression (e.g. lid_split).",
        ));
        widgets::divider(ui);
        ui.columns(4, |cols| {
            for (i, k) in Kind::ALL.iter().enumerate() {
                let on = self.kind == *k;
                let b = egui::Button::new(RichText::new(k.label()).size(11.5))
                    .fill(if on {
                        theme::ACCENT_BG
                    } else {
                        Color32::TRANSPARENT
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if on {
                            theme::ACCENT
                        } else {
                            theme::BORDER_INPUT
                        },
                    ))
                    .min_size(vec2(cols[i].available_width(), 40.0))
                    .wrap();
                if cols[i].add(b).clicked() {
                    self.kind = *k;
                    ch = true;
                }
            }
        });
        widgets::divider(ui);
        let male = match self.kind {
            Kind::Tongue => "Tongue on",
            Kind::Lip => "Lip on",
            Kind::ORing => "Groove in",
            Kind::Snap => "Beams on",
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(male).size(12.0).color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::pill(ui, "top half", !self.male_lower).clicked() {
                    self.male_lower = false;
                    ch = true;
                }
                if widgets::pill(ui, "bottom half", self.male_lower).clicked() {
                    self.male_lower = true;
                    ch = true;
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Fit class").size(12.0).color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for c in FitClass::ALL.iter().rev() {
                    if widgets::pill(ui, c.name(), self.class == *c)
                        .on_hover_text(widgets::fit_tooltip(1.0, Fit::hole(*c), env.profile))
                        .clicked()
                    {
                        self.class = *c;
                        ch = true;
                    }
                }
            });
        });
        let pin = Some(Fit::pin(self.class));
        let gap = Some(Fit::gap(self.class));
        let hole = Some(Fit::hole(self.class));
        let row = |ui: &mut Ui, fl: &mut Field, key: &str, label: &str, fit: Option<Fit>| {
            if fl.fit.is_none() {
                fl.fit = fit;
            }
            fl.row(ui, id.with(key), label, env)
        };
        match self.kind {
            Kind::Tongue => {
                ch |= row(ui, &mut self.t_width, "tw", "Tongue width", pin);
                ch |= row(ui, &mut self.t_height, "th", "Tongue height", None);
                ch |= row(ui, &mut self.t_extra, "te", "Extra groove depth", gap);
                ch |= row(ui, &mut self.t_lead, "tl", "Lead-in chamfer", None);
            }
            Kind::Lip => {
                ch |= row(ui, &mut self.l_width, "lw", "Lip width", pin);
                ch |= row(ui, &mut self.l_height, "lh", "Lip height", None);
            }
            Kind::ORing => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Cord").size(12.0).color(theme::TEXT_DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt(id.with("cord"))
                            .selected_text(&self.cord)
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                for (c, d) in casework_model::joint::cords() {
                                    if ui
                                        .selectable_label(self.cord == c, format!("{c} · ⌀{d:.2}"))
                                        .clicked()
                                    {
                                        self.cord = c;
                                        ch = true;
                                    }
                                }
                            });
                    });
                });
                ch |= row(ui, &mut self.o_squeeze, "os", "Target squeeze", None);
                ch |= row(ui, &mut self.o_depth, "od", "Gland depth (auto)", None);
                ch |= row(ui, &mut self.o_width, "ow", "Gland width (auto)", hole);
            }
            Kind::Snap => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Count").size(12.0).color(theme::TEXT_DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ch |= ui
                            .add(egui::DragValue::new(&mut self.s_count).range(1..=16))
                            .changed();
                    });
                });
                ch |= row(ui, &mut self.s_len, "sl", "Beam length", None);
                ch |= row(ui, &mut self.s_thick, "st", "Beam thickness", None);
                ch |= row(ui, &mut self.s_width, "sw", "Beam width", None);
                ch |= row(ui, &mut self.s_under, "su", "Undercut", None);
                ch |= row(ui, &mut self.s_ins, "si", "Insertion angle", None);
                ch |= row(ui, &mut self.s_ret, "sr", "Retention angle", None);
                ch |= row(ui, &mut self.s_clear, "sc", "Window clearance", None);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Material").size(12.0).color(theme::TEXT_DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        for m in ["PP", "PA6", "PC", "ABS", "PETG", "PLA"] {
                            if widgets::pill(ui, m, self.material == m).clicked() {
                                self.material = m.into();
                                ch = true;
                            }
                        }
                    });
                });
            }
        }
        ch |= ui
            .checkbox(&mut self.rim_on, "Thicken rim inward at the joint")
            .changed();
        if self.rim_on {
            ch |= row(ui, &mut self.rim_w, "rw", "Rim width (total)", None);
            ch |= row(ui, &mut self.rim_h, "rh", "Rim height / side", None);
        }
        widgets::divider(ui);
        ui.label(RichText::new("CHECKS").strong().size(12.0));
        self.checks_ui(ui, cx.regenerating);
        ui.checkbox(&mut self.explode, "Exploded section");
        ch
    }

    fn checks_ui(&self, ui: &mut Ui, busy: bool) {
        let line = |ui: &mut Ui, ok: bool, t: String| {
            ui.horizontal_top(|ui| {
                widgets::icon(
                    ui,
                    if ok {
                        widgets::Icon::Check
                    } else {
                        widgets::Icon::Warn
                    },
                    15.0,
                    if ok { theme::GREEN } else { theme::ORANGE_SOFT },
                );
                ui.add(egui::Label::new(RichText::new(t).size(12.0)).wrap());
            });
        };
        if let Some(FeatureStatus::Error(e)) = &self.status {
            line(ui, false, e.clone());
            return;
        }
        let Some(r) = &self.report else {
            ui.label(widgets::muted(if busy {
                "computing…"
            } else {
                "no preview yet"
            }));
            return;
        };
        line(
            ui,
            true,
            format!(
                "Wall {:.2} → {:.2} at the joint{}",
                r.wall_thickness,
                r.effective_wall,
                if r.rim_added { " (rim added)" } else { "" }
            ),
        );
        if let Some(c) = r.clearance_per_side {
            line(ui, c > 0.0, format!("Clearance {c:.3} per side after fits"));
        }
        for c in &r.checks {
            // a failing check the engine also explains in a warning shows once,
            // as the (more actionable) warning
            let lim = format!("{:.2}", c.limit);
            if !c.ok && r.warnings.iter().any(|w| w.contains(&lim)) {
                continue;
            }
            line(
                ui,
                c.ok,
                format!("{} {:.2} (limit {:.2})", c.name, c.value, c.limit),
            );
        }
        if let Some(o) = &r.oring {
            line(
                ui,
                (0.15..=0.30).contains(&o.squeeze) || (15.0..=30.0).contains(&o.squeeze),
                format!(
                    "Cord {} ⌀{:.2} · squeeze {:.0} % · fill {:.0} %",
                    o.cord,
                    o.cord_dia,
                    pct(o.squeeze),
                    pct(o.fill)
                ),
            );
        }
        if let Some(s) = &r.snap {
            line(
                ui,
                s.pass,
                format!(
                    "Peak strain {:.2} % (allowable {:.2} %) · mating force {:.1} N",
                    pct(s.strain),
                    pct(s.allowable),
                    s.mating_force_n
                ),
            );
        }
        for p in &r.snaps {
            if let Some(reason) = &p.reason {
                line(
                    ui,
                    p.placed.is_some(),
                    format!("Snap at {:.2}: {reason}", p.requested),
                );
            }
        }
        for w in &r.warnings {
            line(ui, false, w.clone());
        }
    }

    /// The live wall section (bottom-left card): the real cut of both halves at a
    /// plane across a straight stretch of wall (computed by the worker with
    /// `Shape::section`), zoomed on the front wall around the parting plane.
    pub fn section(
        &self,
        ui: &mut Ui,
        cuts: &[crate::worker::SectionCut],
        upper: Option<BodyId>,
        z: f64,
    ) {
        let (rect, _) = ui.allocate_exact_size(vec2(320.0, 200.0), egui::Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 6.0, theme::SKETCH_BG);
        let all: Vec<&[glam::DVec2; 3]> = cuts.iter().flat_map(|c| c.tris.iter()).collect();
        if all.is_empty() {
            p.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "computing section…",
                theme::prop(12.0),
                theme::TEXT_MUTED,
            );
            return;
        }
        // plane coords: u = world Y (front wall at the minimum), v = world Z
        let umin = all
            .iter()
            .flat_map(|t| t.iter())
            .map(|q| q.x)
            .fold(f64::INFINITY, f64::min);
        let gap = if self.explode { 2.0 } else { 0.0 };
        let (wu, wv) = (14.0, 14.0 * rect.height() as f64 / rect.width() as f64);
        let (u0, v0) = (umin - 3.0, z - wv / 2.0);
        let k = rect.width() as f64 / wu;
        let to = |q: glam::DVec2, dv: f64| {
            pos2(
                rect.left() + ((q.x - u0) * k) as f32,
                rect.bottom() - ((q.y + dv - v0) * k) as f32,
            )
        };
        for c in cuts {
            let up = Some(c.body) == upper;
            let col = if up {
                theme::hex(0xc7ccd4)
            } else {
                theme::hex(0x7d848e)
            };
            let dv = if up { gap } else { 0.0 };
            let mut mesh = egui::Mesh::default();
            for t in &c.tris {
                let base = mesh.vertices.len() as u32;
                for q in t {
                    mesh.colored_vertex(to(*q, dv), col);
                }
                mesh.add_triangle(base, base + 1, base + 2);
            }
            p.add(mesh);
        }
        let y = to(glam::DVec2::new(u0, z), 0.0).y;
        p.add(egui::Shape::dashed_line(
            &[pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, theme::ACCENT),
            6.0,
            4.0,
        ));
        let note = match &self.report {
            Some(r) => match r.clearance_per_side {
                Some(c) => format!(
                    "{} · wall {:.2} · clearance {:.3}/side",
                    self.kind.label(),
                    r.effective_wall,
                    c
                ),
                None => format!("{} · wall {:.2}", self.kind.label(), r.effective_wall),
            },
            None => self.kind.label().to_string(),
        };
        ui.label(
            RichText::new(note)
                .font(theme::mono(11.0))
                .color(theme::TEXT_DIM),
        );
        p.text(
            pos2(rect.left() + 8.0, rect.bottom() - 12.0),
            egui::Align2::LEFT_CENTER,
            "outside",
            theme::prop(11.0),
            theme::TEXT_MUTED,
        );
        p.text(
            pos2(rect.left() + 8.0, rect.top() + 12.0),
            egui::Align2::LEFT_CENTER,
            "upper half",
            theme::prop(10.0),
            theme::TEXT_MUTED,
        );
        p.text(
            pos2(rect.right() - 8.0, rect.bottom() - 12.0),
            egui::Align2::RIGHT_CENTER,
            "cavity →",
            theme::prop(11.0),
            theme::TEXT_MUTED,
        );
        p.text(
            pos2(rect.right() - 8.0, y - 8.0),
            egui::Align2::RIGHT_CENTER,
            "parting plane",
            theme::prop(10.0),
            theme::ACCENT_LINK,
        );
    }
}

fn pct(v: f64) -> f64 {
    if v <= 1.0 {
        v * 100.0
    } else {
        v
    }
}
