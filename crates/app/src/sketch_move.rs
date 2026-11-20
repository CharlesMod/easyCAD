//! Sketch Move (M): select entities, press M, and a gizmo appears at the selection's
//! bounding-box centre: a red X arrow, a green Y arrow and a square for free
//! placement. Dragging moves the selected entities' points (points shared with
//! unselected geometry move too, dragging the connected lines along). The reference
//! point snaps to points / curves (the moved geometry is ignored); axis drags with
//! no geometry snap step by 1 mm, Shift 10 mm, Ctrl 0.1 mm (as the 3D drag arrow);
//! Alt bypasses all of it. A dX / dY box takes exact offsets: Tab switches, Enter
//! applies, Esc restores. One undo step per move.
//!
//! Fixed points (projected / imported geometry) carry their `Fix` target along;
//! other constraints are left to the solver on Enter (if it cannot converge the
//! moved positions stay as dropped).

use egui::{vec2, Color32, Pos2, Rect, Stroke, Ui};
use easycad_model::sketch::{Constraint, EntityId, PointId};
use glam::DVec2;

use crate::drag_arrow::{quantise, step_for, value_text};
use crate::sketch_mode::{geom_points, Sel, SketchSession};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    X,
    Y,
    Free,
}

#[derive(Clone, Debug)]
pub struct MoveState {
    orig_pts: Vec<(PointId, DVec2)>,
    /// (constraint index, original Fix target) of every moved fixed point.
    orig_fix: Vec<(usize, DVec2)>,
    skip: Vec<EntityId>,
    /// Gizmo position before the offset.
    pub origin: DVec2,
    pub d: DVec2,
    /// (what is grabbed, cursor at the start, offset at the start)
    pub grab: Option<(Grab, DVec2, DVec2)>,
    pub tx: String,
    pub ty: String,
    /// The dX box was just opened: focus it and select its text.
    pub fresh: bool,
    /// The user typed an offset: the boxes stop following the drag.
    pub typed: bool,
}

const ARROW_PX: f32 = 64.0;
const SQUARE_PX: f32 = 7.0;

impl SketchSession {
    /// Start a move of the selected entities (or selected points). False when nothing
    /// is selected.
    pub fn begin_move(&mut self) -> bool {
        if self.mv.is_some() {
            return false;
        }
        let skip = self.selected_entities();
        let mut ids: Vec<PointId> = Vec::new();
        for e in &skip {
            ids.extend(geom_points(&self.sketch.entities[e.0 as usize].geom));
        }
        for s in &self.selection {
            if let Sel::Point(p) = s {
                ids.push(*p);
            }
        }
        ids.sort_by_key(|p| p.0);
        ids.dedup();
        if ids.is_empty() {
            return false;
        }
        let orig_pts: Vec<(PointId, DVec2)> = ids.iter().map(|p| (*p, self.sketch.point(*p))).collect();
        let (lo, hi) = orig_pts.iter().fold((DVec2::MAX, DVec2::MIN), |(l, h), (_, q)| (l.min(*q), h.max(*q)));
        let orig_fix = self
            .sketch
            .constraints
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match c {
                Constraint::Fix(p, v) if ids.contains(p) => Some((i, *v)),
                _ => None,
            })
            .collect();
        self.push_undo();
        self.mv = Some(MoveState {
            orig_pts,
            orig_fix,
            skip,
            origin: (lo + hi) / 2.0,
            d: DVec2::ZERO,
            grab: None,
            tx: "0".into(),
            ty: "0".into(),
            fresh: true,
            typed: false,
        });
        true
    }

    /// Put the moving geometry at offset `d` from where it started.
    pub fn set_move_offset(&mut self, d: DVec2) {
        let Some(mv) = self.mv.as_mut() else { return };
        mv.d = d;
        for (p, q) in &mv.orig_pts {
            self.sketch.points[p.0 as usize] = *q + d;
        }
        for (ci, v) in &mv.orig_fix {
            if let Constraint::Fix(_, t) = &mut self.sketch.constraints[*ci] {
                *t = *v + d;
            }
        }
        if !mv.typed {
            mv.tx = value_text(d.x);
            mv.ty = value_text(d.y);
        }
    }

    /// Keep the move: one undo step (none when nothing moved).
    pub fn commit_move(&mut self) {
        let Some(mv) = self.mv.take() else { return };
        if mv.d.length() < 1e-12 {
            self.pop_undo();
        }
        self.resolve();
    }

    /// Put everything back as it was.
    pub fn cancel_move(&mut self) {
        if self.mv.is_none() {
            return;
        }
        self.set_move_offset(DVec2::ZERO);
        self.mv = None;
        self.pop_undo();
    }

    /// The offset for a drag: `grab` started with the cursor at `from` and offset
    /// `d0`; the cursor is now at `now` (sketch mm).
    pub fn move_drag_offset(&self, grab: Grab, from: DVec2, d0: DVec2, now: DVec2, shift: bool, ctrl: bool) -> DVec2 {
        let Some(mv) = &self.mv else { return d0 };
        let delta = now - from;
        let bypass = self.alt_held;
        let (step, tol) = (step_for(shift, ctrl), 9.0 / self.scale as f64);
        let want = d0 + delta;
        let at = |d: DVec2| mv.origin + d;
        match grab {
            Grab::Free => {
                if bypass {
                    return want;
                }
                let (p, _, lab) = self.snap_at(at(want), &mv.skip);
                if lab.is_some() {
                    p - mv.origin
                } else {
                    DVec2::new(quantise(want.x, step, f64::MIN), quantise(want.y, step, f64::MIN))
                }
            }
            Grab::X | Grab::Y => {
                let k = if grab == Grab::X { 0 } else { 1 };
                let comp = |v: DVec2| if k == 0 { v.x } else { v.y };
                let mk = |t: f64| if k == 0 { DVec2::new(t, d0.y) } else { DVec2::new(d0.x, t) };
                let t = comp(want);
                if bypass {
                    return mk(t);
                }
                if self.snap {
                    let (p, _, lab) = self.snap_at(at(mk(t)), &mv.skip);
                    if lab.is_some() && (p - at(mk(t))).length() < tol {
                        return mk(comp(p - mv.origin));
                    }
                }
                mk(quantise(t, step, f64::MIN))
            }
        }
    }

    fn move_skip(&self) -> Vec<EntityId> {
        self.mv.as_ref().map(|m| m.skip.clone()).unwrap_or_default()
    }
}

fn hit(s: &SketchSession, rect: Rect, hp: Pos2) -> Option<Grab> {
    let mv = s.mv.as_ref()?;
    let o = s.to_screen(rect, mv.origin + mv.d);
    let near = |a: Pos2, b: Pos2| {
        let (lo, hi) = (a.min(b) - vec2(7.0, 7.0), a.max(b) + vec2(7.0, 7.0));
        Rect::from_min_max(lo, hi).contains(hp)
    };
    if Rect::from_center_size(o, vec2(2.0 * SQUARE_PX, 2.0 * SQUARE_PX)).contains(hp) {
        Some(Grab::Free)
    } else if near(o, o + vec2(ARROW_PX, 0.0)) {
        Some(Grab::X)
    } else if near(o, o - vec2(0.0, ARROW_PX)) {
        Some(Grab::Y)
    } else {
        None
    }
}

/// Pointer handling while a move is open. Returns true when it used the pointer.
pub fn interact(s: &mut SketchSession, ui: &Ui, rect: Rect, resp: &egui::Response) -> bool {
    if s.mv.is_none() {
        return false;
    }
    let prim = egui::PointerButton::Primary;
    if resp.drag_started_by(prim) {
        if let Some(hp) = resp.interact_pointer_pos() {
            if let Some(g) = hit(s, rect, hp) {
                let q = s.to_sketch(rect, hp);
                let mv = s.mv.as_mut().unwrap();
                mv.grab = Some((g, q, mv.d));
            }
        }
    }
    let grab = s.mv.as_ref().and_then(|m| m.grab);
    if let (Some((g, from, d0)), true) = (grab, resp.dragged_by(prim)) {
        if let Some(hp) = resp.interact_pointer_pos() {
            let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
            s.alt_held = ui.input(|i| i.modifiers.alt);
            let now = s.to_sketch(rect, hp);
            let d = s.move_drag_offset(g, from, d0, now, shift, ctrl);
            if let Some(m) = s.mv.as_mut() {
                m.typed = false;
            }
            s.set_move_offset(d);
        }
    }
    if resp.drag_stopped() {
        if let Some(m) = s.mv.as_mut() {
            m.grab = None;
        }
    }
    // a click away from the gizmo applies the move
    if resp.clicked_by(prim) {
        if let Some(hp) = resp.interact_pointer_pos() {
            if hit(s, rect, hp).is_none() {
                s.commit_move();
            }
        }
    }
    true
}

pub fn paint(s: &SketchSession, p: &egui::Painter, rect: Rect) {
    let Some(mv) = &s.mv else { return };
    let o = s.to_screen(rect, mv.origin + mv.d);
    let arrow = |dir: egui::Vec2, col: Color32, on: bool| {
        let col = if on { theme::ORANGE } else { col };
        let tip = o + dir * ARROW_PX;
        p.line_segment([o, tip], Stroke::new(2.4, col));
        let n = vec2(-dir.y, dir.x);
        p.add(egui::Shape::convex_polygon(
            vec![tip + dir * 10.0, tip - dir * 2.0 + n * 6.0, tip - dir * 2.0 - n * 6.0],
            col,
            Stroke::NONE,
        ));
    };
    let g = mv.grab.map(|x| x.0);
    arrow(vec2(1.0, 0.0), theme::RED, g == Some(Grab::X));
    arrow(vec2(0.0, -1.0), theme::GREEN, g == Some(Grab::Y));
    let sq = Rect::from_center_size(o, vec2(2.0 * SQUARE_PX, 2.0 * SQUARE_PX));
    let c = if g == Some(Grab::Free) { theme::ORANGE } else { theme::ACCENT };
    p.rect_filled(sq, 1.0, c.gamma_multiply(0.35));
    p.rect_stroke(sq, 1.0, Stroke::new(1.6, c), egui::StrokeKind::Inside);
    // what the reference point would snap to
    if let Some((_, _, Some(l))) = (!s.alt_held && mv.grab.is_some()).then(|| s.snap_at(mv.origin + mv.d, &s.move_skip())) {
        p.text(o + vec2(12.0, -ARROW_PX - 8.0), egui::Align2::LEFT_BOTTOM, l, theme::mono(10.0), theme::ORANGE);
    }
}

/// The dX / dY box. Enter applies, Esc restores, Tab moves between the two.
pub fn value_box(s: &mut SketchSession, ui: &mut Ui, rect: Rect) {
    let Some(mv) = &s.mv else { return };
    let anchor = s.to_screen(rect, mv.origin + mv.d) + vec2(18.0, 18.0);
    let (mut tx, mut ty, fresh) = (mv.tx.clone(), mv.ty.clone(), mv.fresh);
    let (idx, idy) = (egui::Id::NULL.with("sk_move_dx"), egui::Id::NULL.with("sk_move_dy"));
    let (mut changed, mut enter, mut esc) = (false, false, false);
    egui::Area::new(egui::Id::NULL.with("sk_move_box"))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (label, id, text) in [("dX", idx, &mut tx), ("dY", idy, &mut ty)] {
                        ui.label(label);
                        let r = ui.add(egui::TextEdit::singleline(text).id(id).desired_width(54.0));
                        changed |= r.changed();
                        enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        esc |= r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
                    }
                });
            });
        });
    if fresh {
        let mut st = egui::TextEdit::load_state(ui.ctx(), idx).unwrap_or_default();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(tx.chars().count()),
        )));
        st.store(ui.ctx(), idx);
        ui.ctx().memory_mut(|m| m.request_focus(idx));
        if let Some(m) = s.mv.as_mut() {
            m.fresh = false;
        }
    }
    if changed {
        s.apply_move_text(&tx, &ty);
    }
    if esc {
        s.cancel_move();
    } else if enter {
        s.apply_move_text(&tx, &ty);
        s.commit_move();
    }
}

impl SketchSession {
    /// Typed dX / dY (numbers or expressions); an unreadable box counts as 0.
    pub fn apply_move_text(&mut self, tx: &str, ty: &str) {
        let ev = |t: &str| crate::widgets::eval(t, &self.values).unwrap_or(0.0);
        let d = DVec2::new(ev(tx), ev(ty));
        if let Some(m) = self.mv.as_mut() {
            m.tx = tx.into();
            m.ty = ty.into();
            m.typed = true;
        }
        self.set_move_offset(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sketch_mode::Tool;
    use easycad_model::geom::Plane;
    use easycad_model::sketch::Geom;

    fn session() -> SketchSession {
        SketchSession::new_on(Plane::XY, &easycad_model::document::DocData::new("t", "test"))
    }

    /// A line (0,0)-(10,0) selected, an unrelated point at (30,5) far away.
    fn with_line() -> (SketchSession, EntityId) {
        let mut s = session();
        s.scale = 10.0;
        let (a, b) = (s.sketch.add_point(DVec2::new(0.0, 0.0)), s.sketch.add_point(DVec2::new(10.0, 0.0)));
        let e = s.sketch.add_entity(Geom::Line { a, b }, false);
        s.selection = vec![Sel::Entity(e)];
        (s, e)
    }

    fn pts(s: &SketchSession, e: EntityId) -> (DVec2, DVec2) {
        let Geom::Line { a, b } = s.sketch.entities[e.0 as usize].geom else { panic!() };
        (s.sketch.point(a), s.sketch.point(b))
    }

    #[test]
    fn x_arrow_moves_in_whole_mm_shift_ten_and_undoes() {
        let (mut s, e) = with_line();
        assert!(s.begin_move());
        let o = s.mv.as_ref().unwrap().origin;
        assert_eq!(o, DVec2::new(5.0, 0.0));
        // cursor wanders 7.4 right and 3 up: only X moves, by whole mm
        let d = s.move_drag_offset(Grab::X, o, DVec2::ZERO, o + DVec2::new(7.4, 3.0), false, false);
        assert_eq!(d, DVec2::new(7.0, 0.0));
        let d = s.move_drag_offset(Grab::X, o, DVec2::ZERO, o + DVec2::new(14.0, 3.0), true, false);
        assert_eq!(d, DVec2::new(10.0, 0.0));
        let d = s.move_drag_offset(Grab::X, o, DVec2::ZERO, o + DVec2::new(1.26, 3.0), false, true);
        assert!((d.x - 1.3).abs() < 1e-9 && d.y == 0.0);
        s.set_move_offset(DVec2::new(7.0, 0.0));
        assert_eq!(pts(&s, e), (DVec2::new(7.0, 0.0), DVec2::new(17.0, 0.0)));
        s.commit_move();
        assert_eq!(pts(&s, e).0, DVec2::new(7.0, 0.0));
        assert!(s.undo());
        assert_eq!(pts(&s, e), (DVec2::new(0.0, 0.0), DVec2::new(10.0, 0.0)));
    }

    #[test]
    fn free_square_snaps_onto_an_existing_point() {
        let (mut s, _e) = with_line();
        let t = s.sketch.add_point(DVec2::new(30.37, 5.0));
        let other = s.sketch.add_point(DVec2::new(40.0, 40.0));
        s.sketch.add_entity(Geom::Line { a: t, b: other }, false);
        assert!(s.begin_move());
        let o = s.mv.as_ref().unwrap().origin;
        // drag the reference (line centre) near (30.4, 5.1): lands on the point
        let now = o + DVec2::new(25.4, 5.1);
        let d = s.move_drag_offset(Grab::Free, o, DVec2::ZERO, now, false, false);
        assert!(((o + d) - DVec2::new(30.37, 5.0)).length() < 1e-9, "{d:?}");
        // Alt bypasses
        s.alt_held = true;
        let d = s.move_drag_offset(Grab::Free, o, DVec2::ZERO, now, false, false);
        assert!((d - DVec2::new(25.4, 5.1)).length() < 1e-9);
    }

    #[test]
    fn typed_offsets_and_esc_restore() {
        let (mut s, e) = with_line();
        s.selection = vec![Sel::Entity(e)];
        s.tool = Tool::Select;
        assert!(s.begin_move());
        s.apply_move_text("3", "-2.5");
        assert_eq!(pts(&s, e).0, DVec2::new(3.0, -2.5));
        s.cancel_move();
        assert_eq!(pts(&s, e), (DVec2::new(0.0, 0.0), DVec2::new(10.0, 0.0)));
        assert!(s.mv.is_none() && s.undo.is_empty());
        assert!(s.begin_move());
        s.apply_move_text("3", "4");
        s.commit_move();
        assert_eq!(pts(&s, e).0, DVec2::new(3.0, 4.0));
        assert!(s.undo());
        assert_eq!(pts(&s, e).0, DVec2::new(0.0, 0.0));
    }

    #[test]
    fn fixed_points_carry_their_fix_along() {
        let (mut s, e) = with_line();
        let Geom::Line { a, .. } = s.sketch.entities[e.0 as usize].geom else { panic!() };
        s.sketch.constrain(Constraint::Fix(a, DVec2::ZERO));
        assert!(s.begin_move());
        s.apply_move_text("5", "0");
        s.commit_move();
        assert_eq!(pts(&s, e).0, DVec2::new(5.0, 0.0), "the solver keeps it where it was dropped");
    }
}
