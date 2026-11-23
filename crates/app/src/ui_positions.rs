//! Positions (animation states): the floating panel next to the display modes,
//! snapping between named poses with a short tween, capturing and editing them.
//! A position only changes how the model is shown; the history never sees it.

use easycad_model::feature::BodyId;
use easycad_model::params::Expr;
use easycad_model::positions::{Pose, PoseMove, PoseTarget, Position, PositionId};
use egui::{pos2, RichText, Ui};
use glam::{DAffine3, DVec3};

use crate::app::EasyCadApp;
use crate::theme;
use crate::widgets;

/// Seconds a snap between positions takes.
pub const TWEEN_S: f64 = 0.35;

/// Panel state (display only, not saved).
pub struct PositionsUi {
    pub open: bool,
    /// The position being renamed and its text.
    pub renaming: Option<(PositionId, String)>,
    /// Text buffers of the move editor: (position, target) → fields.
    pub edit: Option<MoveEdit>,
    /// Snap animation from the pose shown to the new one.
    pub anim: Option<Anim>,
    /// Button rect of the display bar (the panel opens above it).
    pub button: Option<egui::Rect>,
    /// The position the last model request asked for.
    pub sent: Option<PositionId>,
    /// Playing through the positions in list order (loops).
    pub play: Option<Play>,
    /// Seconds a played position is held.
    pub dwell_s: f32,
    /// Seconds a played move between positions takes.
    pub travel_s: f32,
}

impl Default for PositionsUi {
    fn default() -> Self {
        PositionsUi { open: false, renaming: None, edit: None, anim: None, button: None, sent: None, play: None, dwell_s: 1.0, travel_s: 0.8 }
    }
}

/// Playback: any camera change (orbit, pan, zoom, view cube) stops it.
pub struct Play {
    /// The camera when last checked.
    pub cam: crate::camera::Pose,
    /// When to move on (set once the current position is reached).
    pub next_at: Option<f64>,
}

pub struct Anim {
    pub from: Pose,
    pub to: Pose,
    pub to_id: Option<PositionId>,
    /// Set when the worker's model in the new position arrives.
    pub start: Option<f64>,
    /// Seconds the move takes.
    pub dur: f64,
}

/// Editable text of one move.
#[derive(Clone, PartialEq)]
pub struct MoveEdit {
    pub position: PositionId,
    pub target: PoseTarget,
    pub t: [String; 3],
    pub angle: String,
    pub axis: usize,
    pub pivot: [String; 3],
}

const AXES: [(&str, DVec3); 3] = [("X", DVec3::X), ("Y", DVec3::Y), ("Z", DVec3::Z)];

impl MoveEdit {
    fn of(position: PositionId, m: &PoseMove) -> Self {
        let axis = AXES.iter().position(|a| a.1 == m.axis.normalize_or_zero() || a.1 == -m.axis.normalize_or_zero()).unwrap_or(2);
        let f = |x: f64| crate::units::short(x);
        MoveEdit {
            position,
            target: m.target.clone(),
            t: [m.translate[0].0.clone(), m.translate[1].0.clone(), m.translate[2].0.clone()],
            angle: if m.axis.dot(AXES[axis].1) < 0.0 { format!("-({})", m.angle_deg.0) } else { m.angle_deg.0.clone() },
            axis,
            pivot: [f(m.pivot.x), f(m.pivot.y), f(m.pivot.z)],
        }
    }
    fn to_move(&self) -> Result<PoseMove, String> {
        let e = |s: &str| Expr::new(crate::units::normalize_input(if s.trim().is_empty() { "0" } else { s }));
        let num = |s: &str| s.trim().parse::<f64>().or_else(|_| if s.trim().is_empty() { Ok(0.0) } else { Err(format!("pivot `{s}` is not a number")) });
        Ok(PoseMove {
            target: self.target.clone(),
            translate: [e(&self.t[0]), e(&self.t[1]), e(&self.t[2])],
            pivot: DVec3::new(num(&self.pivot[0])?, num(&self.pivot[1])?, num(&self.pivot[2])?),
            axis: AXES[self.axis].1,
            angle_deg: e(&self.angle),
        })
    }
}

impl EasyCadApp {
    /// The position the model is shown in: none while a dialog, sketch or wizard
    /// is open (features are edited on the modeled pose).
    pub fn shown_pose(&self) -> Option<PositionId> {
        let editing = self.dialog.is_some() || !matches!(self.mode, crate::app::Mode::Model) || self.wizard.is_some();
        self.pose.filter(|_| !editing).filter(|p| self.doc.data.position(*p).is_some())
    }

    /// Snap to a position (`None` = as modeled), with a short tween.
    pub fn go_to_position(&mut self, id: Option<PositionId>) {
        self.go_to_position_in(id, TWEEN_S);
    }

    fn go_to_position_in(&mut self, id: Option<PositionId>, dur: f64) {
        let eval = |app: &EasyCadApp, p: Option<PositionId>| p.and_then(|p| app.doc.data.pose(p).ok()).unwrap_or_default();
        let from = match &self.positions.anim {
            // mid-tween: start from where it is drawn now
            Some(a) => Pose::lerp(&a.from, &a.to, self.tween_t().unwrap_or(0.0)),
            None => eval(self, self.model.as_ref().and_then(|m| m.pose)),
        };
        let to = eval(self, id);
        self.pose = id;
        self.positions.anim = Some(Anim { from, to, to_id: id, start: None, dur: dur.max(0.01) });
        self.request_model();
    }

    /// Tween progress 0..1 while a snap animates (eased).
    pub fn tween_t(&self) -> Option<f64> {
        let a = self.positions.anim.as_ref()?;
        let s = a.start?;
        let t = ((self.now - s) / a.dur).clamp(0.0, 1.0);
        Some(t * t * (3.0 - 2.0 * t))
    }

    /// Extra transform for a root body while a snap animates (identity otherwise):
    /// the model already shows the target pose, this draws it part of the way.
    pub fn tween_body(&self, id: BodyId) -> DAffine3 {
        let (Some(a), Some(t)) = (&self.positions.anim, self.tween_t()) else {
            return DAffine3::IDENTITY;
        };
        let now = Pose::lerp(&a.from, &a.to, t).body(id).affine();
        now * a.to.body(id).affine().inverse()
    }

    /// Same for an occurrence's bodies (world frame).
    pub fn tween_occurrence(&self, path: &[easycad_model::assembly::OccurrenceId]) -> DAffine3 {
        let id = |p: &Pose| p.occurrence(path).map(|r| r.affine()).unwrap_or(DAffine3::IDENTITY);
        let (Some(a), Some(t)) = (&self.positions.anim, self.tween_t()) else {
            return DAffine3::IDENTITY;
        };
        id(&Pose::lerp(&a.from, &a.to, t)) * id(&a.to).inverse()
    }

    /// Start or stop playing through the positions.
    pub fn toggle_play(&mut self) {
        if self.positions.play.take().is_none() && !self.doc.data.positions.is_empty() {
            self.positions.play = Some(Play { cam: self.camera.pose(), next_at: None });
            if self.pose.is_none() {
                // start at the first position
                let first = self.doc.data.positions[0].id;
                self.go_to_position_in(Some(first), self.positions.travel_s as f64);
            }
        }
    }

    /// Playback step: stop on any camera change, else hold, then move on.
    fn play_tick(&mut self, ctx: &egui::Context) {
        let Some(play) = &self.positions.play else { return };
        let editing = self.shown_pose() != self.pose;
        if play.cam != self.camera.pose() || editing || self.doc.data.positions.is_empty() {
            // stop where it is: a move under way finishes at its position
            self.positions.play = None;
            return;
        }
        if self.positions.anim.is_none() {
            let now = self.now;
            let dwell = self.positions.dwell_s as f64;
            let play = self.positions.play.as_mut().unwrap();
            match play.next_at {
                None => play.next_at = Some(now + dwell),
                Some(t) if now >= t => {
                    play.next_at = None;
                    let list = &self.doc.data.positions;
                    let i = self.pose.and_then(|p| list.iter().position(|x| x.id == p)).map_or(0, |i| (i + 1) % list.len());
                    let next = list[i].id;
                    self.go_to_position_in(Some(next), self.positions.travel_s as f64);
                }
                Some(_) => {}
            }
        }
        ctx.request_repaint();
    }

    /// Frame bookkeeping: start the tween when its model arrives, end it.
    pub fn positions_tick(&mut self, ctx: &egui::Context) {
        self.play_tick(ctx);
        if let Some(p) = self.positions.play.as_mut() {
            p.cam = self.camera.pose();
        }
        // a dialog opened or closed, or the position was edited: show the right pose
        if self.shown_pose() != self.positions.sent {
            self.request_model();
        }
        let shown = self.model.as_ref().map(|m| m.pose);
        let wanted = self.shown_pose();
        let now = self.now;
        let Some(a) = &mut self.positions.anim else { return };
        if a.start.is_none() {
            if shown == Some(a.to_id) {
                a.start = Some(now);
            } else if wanted != a.to_id {
                // a dialog opened meanwhile: nothing to animate
                self.positions.anim = None;
                return;
            }
        }
        if self.tween_t().is_some_and(|t| t >= 1.0) {
            self.positions.anim = None;
        }
        ctx.request_repaint();
    }

    /// The pose target for a picked body: a root body, or the top-level
    /// occurrence a placed body belongs to.
    pub fn pose_target(&self, key: BodyId) -> Option<(PoseTarget, String)> {
        let m = self.model.as_ref()?;
        if let Some(c) = m.ctx_body(key) {
            let top = c.path.first().copied()?;
            let name = self.doc.data.occurrence(top).map(|o| o.name.clone()).unwrap_or_else(|| "component".into());
            return Some((PoseTarget::Occurrence(vec![top]), name));
        }
        if !self.active.is_empty() {
            return None;
        }
        let b = m.body(key)?;
        Some((PoseTarget::Body(b.id), b.name.clone()))
    }

    fn target_label(&self, t: &PoseTarget) -> String {
        match t {
            PoseTarget::Body(id) => self.model.as_ref().and_then(|m| m.body(*id)).map(|b| b.name.clone()).unwrap_or_else(|| format!("body {}", id.feature.0)),
            PoseTarget::Occurrence(p) => p.last().and_then(|o| self.doc.data.occurrence(*o)).map(|o| o.name.clone()).unwrap_or_else(|| "component".into()),
        }
    }

    fn edit_positions<T>(&mut self, label: &str, f: impl FnOnce(&mut easycad_model::document::DocData) -> T) -> T {
        let out = self.doc.edit(label, |d| Ok::<_, ()>(f(d))).unwrap();
        self.after_edit();
        out
    }
}

/// The Positions toggle in the display bar.
pub fn bar_button(app: &mut EasyCadApp, ui: &mut Ui) {
    let n = app.doc.data.positions.len();
    let label = match app.pose.and_then(|p| app.doc.data.position(p)) {
        Some(p) if app.positions.play.is_some() => format!("▶ {}", p.name),
        Some(p) => format!("Pos: {}", p.name),
        None => "Positions".to_string(),
    };
    let r = widgets::seg(ui, &label, app.positions.open).on_hover_text(format!("Positions: named poses to snap between ({n} saved)"));
    app.positions.button = Some(r.rect);
    if r.clicked() {
        app.positions.open = !app.positions.open;
    }
}

/// The floating panel above the display bar.
pub fn panel(app: &mut EasyCadApp, ui: &mut Ui, viewport: egui::Rect) {
    if !app.positions.open {
        return;
    }
    let anchor = app.positions.button.map(|r| pos2(r.center().x, r.top() - 10.0)).unwrap_or(pos2(viewport.center().x, viewport.bottom() - 64.0));
    let x = anchor.x.clamp(viewport.left() + 150.0, viewport.right() - 150.0);
    egui::Area::new(egui::Id::unique("positions_panel"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(pos2(x, anchor.y))
        .show(ui.ctx(), |ui| {
            crate::widgets::card_frame().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.set_width(260.0);
                panel_body(app, ui);
            });
        });
}

fn panel_body(app: &mut EasyCadApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("POSITIONS").size(10.5).color(theme::TEXT_LABEL));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("✕").on_hover_text("Close").clicked() {
                app.positions.open = false;
            }
        });
    });
    if app.shown_pose() != app.pose {
        ui.label(widgets::muted("Shown as modeled while a dialog or sketch is open."));
    }
    if let Some(e) = app.model.as_ref().and_then(|m| m.pose_error.clone()) {
        ui.label(RichText::new(e).size(11.5).color(theme::ORANGE_SOFT));
    }
    // state numbers with Play inline
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if widgets::pill(ui, "0", app.pose.is_none()).on_hover_text("As modeled").clicked() && app.pose.is_some() {
            app.positions.play = None;
            app.go_to_position(None);
        }
        let ids: Vec<(PositionId, String)> = app.doc.data.positions.iter().map(|p| (p.id, p.name.clone())).collect();
        for (k, (id, name)) in ids.iter().enumerate() {
            if widgets::pill(ui, &(k + 1).to_string(), app.pose == Some(*id)).on_hover_text(name).clicked() && app.pose != Some(*id) {
                app.positions.play = None;
                app.go_to_position(Some(*id));
            }
        }
        let playing = app.positions.play.is_some();
        let r = widgets::pill(ui, if playing { "■" } else { "▶" }, playing)
            .on_hover_text(if playing { "Stop" } else { "Play: move through the positions in order, looping, until you move the view" });
        if r.clicked() && (playing || !ids.is_empty()) {
            app.toggle_play();
        }
    });
    if app.positions.play.is_some() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Hold").size(11.5).color(theme::TEXT_DIM));
            ui.add(egui::DragValue::new(&mut app.positions.dwell_s).range(0.0..=10.0).speed(0.05).suffix(" s"));
            ui.label(RichText::new("Move").size(11.5).color(theme::TEXT_DIM));
            ui.add(egui::DragValue::new(&mut app.positions.travel_s).range(0.05..=10.0).speed(0.05).suffix(" s"));
        });
    }
    // the list
    if ui.selectable_label(app.pose.is_none(), "As modeled").on_hover_text("The history's own pose").clicked() && app.pose.is_some() {
        app.go_to_position(None);
    }
    let list: Vec<Position> = app.doc.data.positions.clone();
    let mut go = None;
    let mut delete = None;
    let mut rename_done = None;
    for p in &list {
        ui.horizontal(|ui| {
            match &mut app.positions.renaming {
                Some((id, text)) if *id == p.id => {
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(170.0));
                    r.request_focus();
                    if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        rename_done = Some((p.id, text.clone()));
                    }
                }
                _ => {
                    let moves = p.moves.len();
                    let r = ui
                        .selectable_label(app.pose == Some(p.id), &p.name)
                        .on_hover_text(format!("Snap to `{}` ({moves} move{}). Double-click to rename.", p.name, if moves == 1 { "" } else { "s" }));
                    if r.double_clicked() {
                        app.positions.renaming = Some((p.id, p.name.clone()));
                    } else if r.clicked() {
                        go = Some(p.id);
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Delete this position").clicked() {
                    delete = Some(p.id);
                }
            });
        });
    }
    if let Some((id, name)) = rename_done {
        app.positions.renaming = None;
        let name = name.trim().to_string();
        if !name.is_empty() {
            app.edit_positions(&format!("Rename position to {name}"), |d| {
                if let Some(p) = d.positions.iter_mut().find(|p| p.id == id) {
                    p.name = name;
                }
            });
        }
    }
    if let Some(id) = go {
        app.positions.play = None;
        if app.pose != Some(id) {
            app.go_to_position(Some(id));
        }
    }
    if let Some(id) = delete {
        let name = app.doc.data.position(id).map(|p| p.name.clone()).unwrap_or_default();
        app.edit_positions(&format!("Delete position {name}"), |d| d.positions.retain(|p| p.id != id));
        if app.pose == Some(id) {
            app.go_to_position(None);
        }
    }
    ui.add_space(4.0);
    if ui.button("+ Capture current pose").on_hover_text("Save the pose shown now as a new position").clicked() {
        let moves = app.pose.and_then(|p| app.doc.data.position(p)).map(|p| p.moves.clone()).unwrap_or_default();
        let name = format!("Position {}", app.doc.data.positions.len() + 1);
        let id = app.edit_positions(&format!("Add position {name}"), |d| {
            let id = d.add_position(&name);
            d.positions.last_mut().unwrap().moves = moves;
            id
        });
        app.pose = Some(id);
        app.request_model();
    }
    ui.separator();
    move_editor(app, ui);
}

/// Transform of the selected body in the active position.
fn move_editor(app: &mut EasyCadApp, ui: &mut Ui) {
    let Some(pid) = app.pose else {
        ui.label(widgets::muted("Pick a position, then select a body to move it there."));
        return;
    };
    let Some((target, name)) = app.selection.first().and_then(|k| app.pose_target(k.body)) else {
        let moved: Vec<String> = app.doc.data.position(pid).map(|p| p.moves.iter().map(|m| app.target_label(&m.target)).collect()).unwrap_or_default();
        ui.label(widgets::muted(if moved.is_empty() {
            "Select a body to move it in this position.".to_string()
        } else {
            format!("Moves: {}. Select a body to edit its move.", moved.join(", "))
        }));
        return;
    };
    let current = app.doc.data.position(pid).and_then(|p| p.moves.iter().find(|m| m.target == target).cloned()).unwrap_or_else(|| PoseMove::new(target.clone()));
    let fresh = MoveEdit::of(pid, &current);
    let mut e = match app.positions.edit.take() {
        Some(e) if e.position == pid && e.target == target => e,
        _ => fresh.clone(),
    };
    ui.label(RichText::new(format!("Move of {name}")).size(12.0).color(theme::TEXT_DIM));
    let mut commit = false;
    let mut field = |ui: &mut Ui, label: &str, s: &mut String| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).size(12.0).color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.add(egui::TextEdit::singleline(s).font(theme::mono(12.0)).desired_width(120.0));
                commit |= r.lost_focus();
            });
        });
    };
    for (i, a) in ["X", "Y", "Z"].iter().enumerate() {
        field(ui, &format!("Move {a} (mm)"), &mut e.t[i]);
    }
    field(ui, "Rotate (deg)", &mut e.angle);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Axis").size(12.0).color(theme::TEXT_DIM));
        for (i, (n, _)) in AXES.iter().enumerate() {
            if widgets::pill(ui, n, e.axis == i).clicked() {
                e.axis = i;
                commit = true;
            }
        }
    });
    ui.label(RichText::new("Axis through (x, y, z)").size(12.0).color(theme::TEXT_DIM));
    ui.horizontal(|ui| {
        for i in 0..3 {
            let r = ui.add(egui::TextEdit::singleline(&mut e.pivot[i]).font(theme::mono(12.0)).desired_width(70.0));
            commit |= r.lost_focus();
        }
    });
    let reset = ui.small_button("Reset this body").on_hover_text("Leave it where the history puts it").clicked();
    if reset {
        let t = target.clone();
        app.edit_positions(&format!("Reset {name} in position"), |d| {
            if let Some(p) = d.positions.iter_mut().find(|p| p.id == pid) {
                p.moves.retain(|m| m.target != t);
            }
        });
        app.positions.edit = None;
        return;
    }
    if commit && e != fresh {
        match e.to_move() {
            Ok(mv) => {
                let pos_name = app.doc.data.position(pid).map(|p| p.name.clone()).unwrap_or_default();
                app.edit_positions(&format!("Move {name} in {pos_name}"), |d| {
                    if let Some(p) = d.positions.iter_mut().find(|p| p.id == pid) {
                        match p.moves.iter_mut().find(|m| m.target == mv.target) {
                            Some(m) => *m = mv,
                            None => p.moves.push(mv),
                        }
                    }
                });
                app.request_model();
            }
            Err(err) => app.warn(err),
        }
    }
    app.positions.edit = Some(e);
}
