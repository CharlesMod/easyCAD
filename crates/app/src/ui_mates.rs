//! Mates dialog: pick a type (Rigid, Revolute, Slider, Planar, Cylindrical,
//! Ball), then snap geometry on two occurrences (face centre / normal, circle or
//! cylinder axis, straight edge axis, component origin) with a glyph under the
//! cursor, see it solve live, and jog the free motion (swing a hinged lid). The
//! draft mate rides along with every model request, so the worker's solve is the
//! preview; OK adds it (and the solved placements) in one undo step.

use casework_model::assembly::{Mate, MateConnector, MateId, MateKind, MateStatus};
use casework_model::document::DocError;
use casework_model::occt::{CurveKind, SurfaceKind};
use egui::{pos2, vec2, Color32, Frame, Margin, Rect, RichText, Stroke, Ui};
use glam::{DVec3, Vec2, Vec3};

use crate::app::CaseworkApp;
use crate::pickstack::{PickKey, PickKind};
use crate::theme;
use crate::widgets;
use crate::worker::Output;

pub const KINDS: [MateKind; 6] = [
    MateKind::Rigid,
    MateKind::Revolute,
    MateKind::Slider,
    MateKind::Planar,
    MateKind::Cylindrical,
    MateKind::Ball,
];

pub fn kind_label(k: MateKind) -> &'static str {
    match k {
        MateKind::Rigid => "Rigid",
        MateKind::Revolute => "Revolute",
        MateKind::Slider => "Slider",
        MateKind::Planar => "Planar",
        MateKind::Cylindrical => "Cylindrical",
        MateKind::Ball => "Ball",
    }
}

fn kind_tip(k: MateKind) -> &'static str {
    match k {
        MateKind::Rigid => "No relative motion (0 DOF)",
        MateKind::Revolute => "Hinge: rotation about the common axis (1 DOF)",
        MateKind::Slider => "Slide along the common axis (1 DOF)",
        MateKind::Planar => "Faces stay together, free in the plane (3 DOF)",
        MateKind::Cylindrical => "Rotate about and slide along the axis (2 DOF)",
        MateKind::Ball => "Origins together, rotation free (3 DOF)",
    }
}

/// What a pick snaps to.
#[derive(Clone, Debug)]
pub struct Snap {
    pub conn: MateConnector,
    pub label: String,
    /// Where to draw the glyph (view frame) and its axis.
    pub at: Vec3,
    pub axis: Vec3,
}

pub struct MateUi {
    pub editing: Option<MateId>,
    pub name: String,
    pub kind: MateKind,
    pub a: Option<Snap>,
    pub b: Option<Snap>,
    pub flip: bool,
    /// Revolute drive (deg).
    pub angle: f64,
    /// Slider drive / gap (mm).
    pub distance: f64,
    pub error: Option<String>,
    /// Jog slider rect (E2E state dump).
    pub jog_rect: Option<Rect>,
    pub ok_rect: Option<Rect>,
}

/// Id the draft mate is solved under (replaced by a real id on OK).
pub const DRAFT: MateId = MateId(u32::MAX);

impl MateUi {
    pub fn picking(&self) -> Option<&'static str> {
        match (&self.a, &self.b) {
            (None, _) => Some("Pick geometry on the part that stays"),
            (Some(_), None) => Some("Pick geometry on the part that moves"),
            _ => None,
        }
    }

    /// The draft as a mate (when both sides are picked).
    pub fn draft(&self) -> Option<Mate> {
        let (a, b) = (self.a.as_ref()?, self.b.as_ref()?);
        Some(Mate {
            id: self.editing.unwrap_or(DRAFT),
            name: self.name.clone(),
            kind: self.kind,
            a: a.conn.clone(),
            b: b.conn.clone(),
            flip: self.flip,
            angle: casework_model::params::Expr::new(crate::dialogs::fmt((self.angle * 10.0).round() / 10.0)),
            distance: casework_model::params::Expr::new(crate::dialogs::fmt((self.distance * 100.0).round() / 100.0)),
            suppressed: false,
        })
    }

    pub fn jog_range(&self) -> Option<(&'static str, f64, f64, &'static str)> {
        match self.kind {
            MateKind::Revolute => Some(("Angle", -180.0, 180.0, "°")),
            MateKind::Slider => Some(("Distance", -100.0, 100.0, " mm")),
            MateKind::Rigid | MateKind::Planar => Some(("Gap", -20.0, 20.0, " mm")),
            MateKind::Cylindrical | MateKind::Ball => None,
        }
    }
}

/// Snap target of a pick on a placed body (`None`: not on another occurrence).
pub fn snap_of(m: &Output, k: &PickKey) -> Option<Snap> {
    let c = m.ctx_body(k.body)?;
    let path = c.path.clone();
    let to_view = |p: DVec3| c.xf.transform_point3(p).as_vec3();
    let dir_view = |v: DVec3| c.xf.transform_vector3(v).as_vec3().normalize_or_zero();
    let mk = |conn: MateConnector, label: String| {
        let (o, z) = (conn.origin, conn.z);
        Snap { at: to_view(o), axis: dir_view(z), conn, label }
    };
    match k.kind {
        PickKind::Face(f) => {
            let info = c.mb.faces.get(f as usize)?;
            match (info.kind, info.axis) {
                (SurfaceKind::Cylinder | SurfaceKind::Cone, Some((p, dir))) => {
                    let dir = dir.normalize();
                    let o = p + dir * (info.centroid - p).dot(dir);
                    Some(mk(MateConnector::new(path, o, dir), "cylinder axis".into()))
                }
                _ => Some(mk(MateConnector::from_face(path, info), "face centre".into())),
            }
        }
        PickKind::Edge(e) => {
            let info = c.mb.edges.get(e as usize)?;
            match info.kind {
                CurveKind::Circle => MateConnector::from_circular_edge(path, info)
                    .map(|conn| mk(conn, "circle centre".into())),
                CurveKind::Line => {
                    let dir = (info.end - info.start).normalize_or(info.tangent);
                    Some(mk(MateConnector::new(path, info.midpoint, dir), "edge axis".into()))
                }
                _ => Some(mk(MateConnector::point(path, info.midpoint), "edge midpoint".into())),
            }
        }
        PickKind::Body => Some(mk(
            MateConnector::point(path, DVec3::ZERO),
            "component origin".into(),
        )),
    }
}

impl CaseworkApp {
    /// Open the mates dialog (new, or editing an existing mate). Mates are made at
    /// the top level, so a component being edited is left first.
    pub fn open_mate(&mut self, id: Option<MateId>) {
        if !crate::ui_assembly::is_assembly(&self.doc.data) || self.doc.data.occurrences.len() < 2 && id.is_none() {
            self.warn("Mates join two components: make or insert components first");
            return;
        }
        self.activate(Vec::new());
        if self.dialog.is_some() {
            self.close_dialog(false);
        }
        let d = &self.doc.data;
        let ui = match id.and_then(|id| d.mates.iter().find(|m| m.id == id)) {
            Some(m) => {
                let snap = |c: &MateConnector| {
                    let name = crate::ui_assembly::occ_name(d, &c.occurrence);
                    Snap { conn: c.clone(), label: name, at: Vec3::ZERO, axis: Vec3::Z }
                };
                let vals = crate::adapters::values(d);
                MateUi {
                    editing: Some(m.id),
                    name: m.name.clone(),
                    kind: m.kind,
                    a: Some(snap(&m.a)),
                    b: Some(snap(&m.b)),
                    flip: m.flip,
                    angle: m.angle.eval(&vals).unwrap_or(0.0),
                    distance: m.distance.eval(&vals).unwrap_or(0.0),
                    error: None,
                    jog_rect: None,
                    ok_rect: None,
                }
            }
            None => {
                let n = d.mates.len() + 1;
                MateUi {
                    editing: None,
                    name: format!("Mate {n}"),
                    kind: MateKind::Revolute,
                    a: None,
                    b: None,
                    flip: false,
                    angle: 0.0,
                    distance: 0.0,
                    error: None,
                    jog_rect: None,
                    ok_rect: None,
                }
            }
        };
        self.asm.mate = Some(ui);
        self.selection.clear();
        self.filter = crate::pickstack::Filter { bodies: true, faces: true, edges: true, sketches: false };
        self.request_model();
    }

    /// A viewport pick while the mates dialog is open. Returns true when used.
    pub fn mate_pick(&mut self, k: PickKey) -> bool {
        let Some(m) = self.model.clone() else { return false };
        let Some(mu) = self.asm.mate.as_mut() else { return false };
        let Some(s) = snap_of(&m, &k) else {
            mu.error = Some("Pick on a component (not the document's own bodies)".into());
            return true;
        };
        let d = &self.doc.data;
        let label = format!("{} · {}", crate::ui_assembly::occ_name(d, &s.conn.occurrence), s.label);
        let s = Snap { label, ..s };
        mu.error = None;
        if mu.a.is_none() {
            mu.a = Some(s);
        } else if mu.b.is_none() || mu.editing.is_some() {
            if mu.a.as_ref().is_some_and(|a| a.conn.occurrence == s.conn.occurrence) {
                mu.error = Some("Pick the second side on another component".into());
                return true;
            }
            mu.b = Some(s);
        } else {
            // both picked: start over with this as the part that stays
            mu.a = Some(s);
            mu.b = None;
        }
        self.request_model();
        true
    }

    /// OK: add (or replace) the mate and write the solved placements, one undo step.
    pub fn commit_mate(&mut self) {
        let Some(mu) = self.asm.mate.take() else { return };
        let Some(mut m) = mu.draft() else {
            self.warn("Pick both sides first");
            self.asm.mate = Some(mu);
            return;
        };
        let label = format!("{} {}", kind_label(m.kind), m.name);
        let editing = mu.editing;
        let r = self.doc.edit(&label, |d| {
            match editing {
                Some(id) => {
                    let slot = d.mates.iter_mut().find(|x| x.id == id).ok_or_else(|| DocError::Other("mate vanished".into()))?;
                    *slot = m.clone();
                }
                None => {
                    m.id = MateId(d.next_id);
                    d.next_id += 1;
                    d.mates.push(m.clone());
                }
            }
            let (moves, solve) = casework_model::assembly::solve_mates(d);
            for (id, t) in moves {
                if let Some(o) = d.occurrence_mut(id) {
                    o.transform = t;
                }
            }
            Ok::<_, DocError>(solve)
        });
        match r {
            Ok(solve) => {
                let bad: Vec<String> = solve
                    .mates
                    .iter()
                    .filter_map(|r| match &r.status {
                        MateStatus::OverConstrained(w) | MateStatus::Unsolved(w) => Some(format!("{}: {w}", r.name)),
                        _ => None,
                    })
                    .collect();
                if bad.is_empty() {
                    self.toast(format!("{label} · solved"));
                } else {
                    self.warn(bad.join("; "));
                }
            }
            Err(e) => self.warn(e.to_string()),
        }
        self.filter = Default::default();
        self.request_model();
    }

    pub fn cancel_mate(&mut self) {
        self.asm.mate = None;
        self.filter = Default::default();
        self.request_model();
    }
}

/// The floating Mate card (right side, like the feature dialogs).
pub fn mate_card(app: &mut CaseworkApp, ui: &mut Ui, rect: Rect) {
    let Some(mut mu) = app.asm.mate.take() else { return };
    let solve = app.model.as_ref().and_then(|m| m.asm.as_ref()).and_then(|a| a.solve.clone());
    let id = mu.editing.unwrap_or(DRAFT);
    let status = solve.as_ref().and_then(|s| s.mates.iter().find(|r| r.mate == id)).map(|r| r.status.clone());
    let mut changed = false;
    let mut ok = false;
    let mut cancel = false;
    let pos = pos2(rect.right() - 16.0 - 340.0, rect.top() + 12.0);
    egui::Area::new(egui::Id::unique("mate_card"))
        .order(egui::Order::Middle)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            Frame::new()
                .fill(theme::CARD)
                .stroke(Stroke::new(1.0, theme::BORDER_CARD))
                .corner_radius(10.0)
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(312.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if mu.editing.is_some() { "EDIT MATE" } else { "NEW MATE" }).size(11.0).color(theme::TEXT_LABEL));
                        ui.add(egui::TextEdit::singleline(&mut mu.name).desired_width(150.0));
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for k in KINDS {
                            if widgets::pill(ui, kind_label(k), mu.kind == k).on_hover_text(kind_tip(k)).clicked() && mu.kind != k {
                                mu.kind = k;
                                changed = true;
                            }
                        }
                    });
                    for (i, (label, side)) in [("Stays", &mu.a), ("Moves", &mu.b)].into_iter().enumerate() {
                        let active = (i == 0 && mu.a.is_none()) || (i == 1 && mu.a.is_some() && mu.b.is_none());
                        Frame::new()
                            .fill(if active { theme::ORANGE_BG } else { theme::GROUND })
                            .stroke(Stroke::new(1.0, if active { theme::ORANGE } else { theme::BORDER_INPUT }))
                            .corner_radius(6.0)
                            .inner_margin(Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.set_width(290.0);
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("{} {label}", i + 1)).size(12.0).color(theme::TEXT_DIM));
                                    match side {
                                        Some(s) => ui.label(RichText::new(&s.label).size(12.0).color(theme::TEXT_STRONG)),
                                        None => ui.label(RichText::new(if active { "pick on the model…" } else { "—" }).size(12.0).color(theme::ORANGE_SOFT)),
                                    };
                                });
                            });
                    }
                    if mu.a.is_some() {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut mu.flip, "Flip").on_hover_text("Turn the moving side over (faces that touch face each other)").changed() {
                                changed = true;
                            }
                            if widgets::button(ui, "Re-pick").on_hover_text("Pick both sides again").clicked() {
                                mu.a = None;
                                mu.b = None;
                                changed = true;
                            }
                        });
                    }
                    if let Some((what, lo, hi, unit)) = mu.jog_range() {
                        ui.label(RichText::new(format!("JOG · {what}")).size(11.0).color(theme::TEXT_LABEL));
                        let v = if mu.kind == MateKind::Revolute { &mut mu.angle } else { &mut mu.distance };
                        ui.spacing_mut().slider_width = 200.0;
                        let r = ui.add(egui::Slider::new(v, lo..=hi).suffix(unit).step_by(if unit == "°" { 1.0 } else { 0.5 }));
                        // the rail (E2E drags along it)
                        mu.jog_rect = Some(Rect::from_min_max(r.rect.min, pos2(r.rect.left() + 200.0, r.rect.bottom())));
                        if r.changed() {
                            changed = true;
                        }
                        let dragging = r.dragged();
                        app.dragging = dragging;
                    } else {
                        mu.jog_rect = None;
                        ui.label(widgets::muted("Free motion stays where it is (move it with the gizmo)."));
                    }
                    match &status {
                        Some(MateStatus::Solved) => {
                            ui.label(RichText::new("✓ Solved").size(12.0).color(theme::GREEN));
                        }
                        Some(MateStatus::Redundant) => {
                            ui.label(RichText::new("Holds already (redundant)").size(12.0).color(theme::TEXT_DIM));
                        }
                        Some(MateStatus::OverConstrained(w)) => {
                            ui.add(egui::Label::new(RichText::new(format!("Over-constrained: {w}")).size(12.0).color(theme::ORANGE)).wrap());
                        }
                        Some(MateStatus::Unsolved(w)) => {
                            ui.add(egui::Label::new(RichText::new(format!("Unsolved: {w}")).size(12.0).color(theme::ORANGE)).wrap());
                        }
                        _ => {
                            if let Some(p) = mu.picking() {
                                ui.label(widgets::muted(p));
                            }
                        }
                    }
                    if let Some(e) = &mu.error {
                        ui.label(RichText::new(e).size(12.0).color(theme::ORANGE_SOFT));
                    }
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let r = ui.add_enabled(mu.draft().is_some(), egui::Button::new(RichText::new("  OK  ").color(theme::TEXT_STRONG)).fill(theme::PRIMARY));
                            mu.ok_rect = Some(r.rect);
                            if r.clicked() {
                                ok = true;
                            }
                            if widgets::button(ui, "Cancel").clicked() {
                                cancel = true;
                            }
                        });
                    });
                });
        });
    app.asm.mate = Some(mu);
    if changed {
        app.request_model();
    }
    if ok {
        app.commit_mate();
    } else if cancel {
        app.cancel_mate();
    }
}

/// Snap glyphs: the picked sides and what the cursor would snap to.
pub fn glyphs(app: &CaseworkApp, ui: &Ui, rect: Rect) {
    let Some(mu) = &app.asm.mate else { return };
    let Some(m) = &app.model else { return };
    let size = Vec2::new(rect.width(), rect.height());
    let p = ui.painter_at(rect);
    let proj = |q: Vec3| app.camera.project(q, size).map(|s| pos2(rect.left() + s.x, rect.top() + s.y));
    let len = app.camera.world_per_point(size.y).max(1e-4) * 40.0;
    let draw = |s: &Snap, col: Color32, label: Option<&str>, below: bool| {
        // the stored connector is in its occurrence's frame; re-place it
        let (at, axis) = match m.ctx.iter().find(|c| c.path == s.conn.occurrence) {
            Some(c) => (
                c.xf.transform_point3(s.conn.origin).as_vec3(),
                c.xf.transform_vector3(s.conn.z).as_vec3().normalize_or_zero(),
            ),
            None => (s.at, s.axis),
        };
        let (Some(a), Some(b)) = (proj(at), proj(at + axis * len)) else { return };
        p.circle(a, 6.0, col.gamma_multiply(0.35), Stroke::new(1.6, col));
        p.circle_filled(a, 2.0, col);
        p.arrow(a, b - a, Stroke::new(1.8, col));
        if let Some(t) = label {
            let (off, al) = if below { (vec2(10.0, 10.0), egui::Align2::LEFT_TOP) } else { (vec2(10.0, -10.0), egui::Align2::LEFT_BOTTOM) };
            p.text(a + off, al, t, theme::prop(11.0), col);
        }
    };
    if let Some(a) = &mu.a {
        draw(a, theme::GREEN, Some("stays"), false);
    }
    if let Some(b) = &mu.b {
        draw(b, theme::ACCENT, Some("moves"), true);
    }
    if mu.picking().is_some() {
        if let Some(c) = app.stack.current() {
            if let Some(s) = snap_of(m, &c.key) {
                draw(&s, theme::ORANGE, Some(&s.label), false);
            }
        }
    }
}
