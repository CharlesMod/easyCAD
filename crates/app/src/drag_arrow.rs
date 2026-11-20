//! Drag arrow for the Extrude distance and the offset-plane distance.
//!
//! While an Extrude dialog or an offset-plane construct is open, an arrow sits at
//! the preview's position pointing the way the distance grows. Dragging it moves
//! along that axis (closest point between the mouse ray and the axis); the value
//! is rounded to 1 mm, Shift 10 mm, Ctrl 0.1 mm. The value field follows live and
//! the preview is re-requested; Esc during the drag puts the old text back.
//!
//! Steps are always millimetres, whatever unit the document displays.

use egui::{pos2, vec2, Color32, Pos2, Rect, Stroke, Ui};
use glam::{DVec3, Vec2, Vec3};

use crate::app::EasyCadApp;
use crate::dialogs::{Body, Field};
use crate::theme;

/// What one open dialog lets the arrow move.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    /// Axis point at value 0.
    pub base: DVec3,
    /// Unit direction in which the value grows.
    pub dir: DVec3,
    /// World mm the arrow moves per unit of value (0.5 for a symmetric extrude).
    pub per: f64,
    pub value: f64,
    /// Lowest value the drag may reach.
    pub min: f64,
}

impl Spec {
    pub fn at(&self, v: f64) -> DVec3 {
        self.base + self.dir * self.per * v
    }
}

#[derive(Clone, Debug)]
pub struct ArrowDrag {
    pub spec: Spec,
    pub start_value: f64,
    pub start_text: String,
    /// Axis parameter (world mm) under the pointer when the drag began.
    pub t0: f64,
    pub value: f64,
}

/// Step for the held modifiers: Shift 10 mm, Ctrl 0.1 mm, else 1 mm.
pub fn step_for(shift: bool, ctrl: bool) -> f64 {
    if shift {
        10.0
    } else if ctrl {
        0.1
    } else {
        1.0
    }
}

/// `raw` rounded to a multiple of `step`, not below `min`.
pub fn quantise(raw: f64, step: f64, min: f64) -> f64 {
    let v = ((raw / step).round() * step).max(min);
    // clean binary noise (0.30000000000000004)
    (v * 1e6).round() / 1e6
}

/// Parameter `t` of the point on the line `p0 + t*dir` (unit `dir`) closest to the
/// ray `o + s*r`. `None` when the ray runs along the axis.
pub fn axis_param(o: DVec3, r: DVec3, p0: DVec3, dir: DVec3) -> Option<f64> {
    let w = p0 - o;
    let b = dir.dot(r);
    let (d, e) = (dir.dot(w), r.dot(w));
    let c = r.dot(r);
    let den = c - b * b;
    if den.abs() < 1e-6 * c.max(1e-12) {
        return None;
    }
    Some((b * e - c * d) / den)
}

/// The value after dragging: start + travel along the axis, rounded.
pub fn drag_value(spec: &Spec, start_value: f64, t0: f64, t: f64, step: f64) -> f64 {
    quantise(start_value + (t - t0) / spec.per, step, spec.min)
}

/// Text for a value in a field: "12" or "12.3".
pub fn value_text(v: f64) -> String {
    let v = (v * 10.0).round() / 10.0;
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

fn profile_center(sk: &easycad_model::sketch::Sketch) -> Option<glam::DVec2> {
    use easycad_model::geom::Segment2::*;
    let mut lo = glam::DVec2::splat(f64::MAX);
    let mut hi = glam::DVec2::splat(f64::MIN);
    let mut any = false;
    for pr in &sk.profiles().profiles {
        for s in &pr.outer {
            let pts: Vec<glam::DVec2> = match *s {
                Line { a, b } => vec![a, b],
                Arc { start, end, .. } => vec![start, end],
                Circle { center, radius } => {
                    vec![center - glam::DVec2::splat(radius), center + glam::DVec2::splat(radius)]
                }
            };
            for p in pts {
                lo = lo.min(p);
                hi = hi.max(p);
                any = true;
            }
        }
    }
    any.then(|| (lo + hi) * 0.5)
}

/// What the open dialog (if any) lets the arrow move.
pub fn spec(app: &EasyCadApp) -> Option<Spec> {
    let d = app.dialog.as_ref()?;
    let view = app.view();
    let vals = crate::adapters::values(view);
    match &d.body {
        Body::Extrude(e) => {
            let sk = view.sketch(e.sketch?)?;
            let value = crate::widgets::eval(&e.distance.text, &vals).ok()?;
            let c = profile_center(sk)?;
            let n = sk.plane.normal.normalize();
            // A symmetric extrude runs value/2 each way: the arrow marks the far (+) face.
            Some(Spec {
                base: sk.plane.to_world(c),
                dir: if e.reverse { -n } else { n },
                per: if e.symmetric { 0.5 } else { 1.0 },
                value,
                min: 0.1,
            })
        }
        Body::Datum(dd) if dd.kind == crate::datum_ui::DatumKind::PlaneOffset => {
            let value = crate::widgets::eval(&dd.fields.first()?.text, &vals).ok()?;
            let fid = d.editing.unwrap_or(easycad_model::feature::FeatureId(app.doc.data.next_id));
            let out = app.preview.as_deref()?;
            let pl = out.result.datums.iter().find_map(|(f, g)| match (f == &fid, g) {
                (true, easycad_model::datum::DatumGeom::Plane(p)) => Some(*p),
                _ => None,
            })?;
            // the preview normal is flipped by "Flip"; the offset always grows along the base normal
            let dir = if dd.flip { -pl.normal } else { pl.normal }.normalize();
            // arrow at the picked point carried onto the plane, else the plane's origin
            let on = dd
                .auto
                .as_ref()
                .and_then(|a| a.picks.first())
                .map(|p| p.p - pl.normal * (p.p - pl.origin).dot(pl.normal))
                .unwrap_or(pl.origin);
            Some(Spec { base: on - dir * value, dir, per: 1.0, value, min: f64::MIN })
        }
        _ => None,
    }
}

fn field_mut(app: &mut EasyCadApp) -> Option<&mut Field> {
    match &mut app.dialog.as_mut()?.body {
        Body::Extrude(e) => Some(&mut e.distance),
        Body::Datum(dd) => dd.fields.first_mut(),
        _ => None,
    }
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t).distance(p)
}

const LEN: f32 = 54.0;

/// Draw the arrow and run its drag. True while it owns the pointer.
pub fn arrow(app: &mut EasyCadApp, ui: &Ui, rect: Rect, resp: &egui::Response) -> bool {
    let live = app.arrow_drag.clone();
    let Some(spec) = live.as_ref().map(|d| d.spec.clone()).or_else(|| spec(app)) else {
        app.arrow_drag = None;
        return false;
    };
    let size = Vec2::new(rect.width(), rect.height());
    let cam = app.camera.clone();
    let proj = |q: DVec3| cam.project(q.as_vec3(), size).map(|s| pos2(rect.left() + s.x, rect.top() + s.y));
    let value = live.as_ref().map(|d| d.value).unwrap_or(spec.value);
    let tail_w = spec.at(value);
    let Some(tail) = proj(tail_w) else { return false };
    // screen direction of the axis (fixed length, so it stays grabbable edge-on)
    let probe = cam.world_per_point(size.y).max(1e-4) as f64 * 40.0;
    let head = proj(tail_w + spec.dir * probe).unwrap_or(tail);
    let mut sd = head - tail;
    if sd.length() < 4.0 {
        sd = vec2(0.0, -1.0);
    }
    let tip = tail + sd.normalized() * LEN;
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let near = pointer.is_some_and(|p| rect.contains(p) && (seg_dist(p, tail, tip) < 8.0 || p.distance(tip) < 10.0));
    let hot = live.is_some() || (near && resp.hovered());
    let painter = ui.painter_at(rect);

    if resp.drag_started_by(egui::PointerButton::Primary) && live.is_none() && near {
        if let Some(p) = resp.interact_pointer_pos() {
            let (o, r) = cam.ray(Vec2::new(p.x - rect.left(), p.y - rect.top()), size);
            if let Some(t0) = axis_param(o.as_dvec3(), r.as_dvec3(), spec.base, spec.dir) {
                let text = field_mut(app).map(|f| f.text.clone()).unwrap_or_default();
                app.arrow_drag = Some(ArrowDrag { spec: spec.clone(), start_value: spec.value, start_text: text, t0, value: spec.value });
            }
        }
    }
    let mut owns = hot;
    if let Some(mut g) = app.arrow_drag.clone() {
        owns = true;
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.ctrl || i.modifiers.command));
        let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let down = ui.input(|i| i.pointer.primary_down());
        if esc {
            if let Some(f) = field_mut(app) {
                f.text = g.start_text.clone();
                f.select = true;
            }
            app.arrow_drag = None;
            app.dragging = false;
            app.request_preview();
            return true;
        }
        if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
            let (o, r) = cam.ray(Vec2::new(p.x - rect.left(), p.y - rect.top()), size);
            if let Some(t) = axis_param(o.as_dvec3(), r.as_dvec3(), g.spec.base, g.spec.dir) {
                let v = drag_value(&g.spec, g.start_value, g.t0, t, step_for(shift, ctrl));
                if (v - g.value).abs() > 1e-9 {
                    g.value = v;
                    if let Some(f) = field_mut(app) {
                        f.text = value_text(v);
                    }
                    app.dragging = true;
                    app.request_preview();
                }
                app.arrow_drag = Some(g.clone());
            }
            painter.text(
                p + vec2(16.0, -10.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{} mm", value_text(g.value)),
                theme::mono(12.0),
                theme::TEXT_STRONG,
            );
        }
        if resp.drag_stopped() || !down {
            app.arrow_drag = None;
            app.dragging = false;
            if let Some(f) = field_mut(app) {
                f.select = true;
            }
            app.request_preview();
        }
    } else if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        if let Some(p) = pointer {
            painter.text(
                p + vec2(14.0, 18.0),
                egui::Align2::LEFT_TOP,
                "Drag: 1 mm, Shift: 10 mm, Ctrl: 0.1 mm",
                theme::prop(11.0),
                theme::TEXT_DIM,
            );
        }
    }
    let col: Color32 = if hot { theme::ACCENT } else { theme::ACCENT.gamma_multiply(0.75) };
    painter.arrow(tail, tip - tail, Stroke::new(if hot { 4.0 } else { 2.6 }, col));
    painter.circle(tail, 4.5, theme::CARD, Stroke::new(1.5, col));
    let _ = (Vec3::ZERO, Rect::NOTHING);
    owns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(per: f64) -> Spec {
        Spec { base: DVec3::ZERO, dir: DVec3::Z, per, value: 5.0, min: 0.1 }
    }

    #[test]
    fn steps_follow_modifiers() {
        assert_eq!(step_for(false, false), 1.0);
        assert_eq!(step_for(true, false), 10.0);
        assert_eq!(step_for(false, true), 0.1);
        assert_eq!(step_for(true, true), 10.0);
    }

    #[test]
    fn quantises_to_steps() {
        assert_eq!(quantise(7.4, 1.0, 0.0), 7.0);
        assert_eq!(quantise(7.6, 1.0, 0.0), 8.0);
        assert_eq!(quantise(14.0, 10.0, 0.0), 10.0);
        assert_eq!(quantise(16.0, 10.0, 0.0), 20.0);
        assert_eq!(quantise(0.34, 0.1, 0.0), 0.3);
        assert_eq!(quantise(-3.0, 1.0, 0.1), 0.1);
    }

    #[test]
    fn closest_point_on_axis() {
        // ray along X at height z=7, passing the Z axis at (0,0,7)
        let t = axis_param(DVec3::new(-10.0, 0.0, 7.0), DVec3::X, DVec3::ZERO, DVec3::Z).unwrap();
        assert!((t - 7.0).abs() < 1e-9);
        // a ray parallel to the axis has no answer
        assert!(axis_param(DVec3::new(1.0, 0.0, 0.0), DVec3::Z, DVec3::ZERO, DVec3::Z).is_none());
    }

    #[test]
    fn drag_value_and_modifier_switch() {
        let s = spec(1.0);
        // pulled 3.4 mm up from a 5 mm start
        assert_eq!(drag_value(&s, 5.0, 0.0, 3.4, 1.0), 8.0);
        // same pointer, Shift now held: nearest 10
        assert_eq!(drag_value(&s, 5.0, 0.0, 3.4, 10.0), 10.0);
        // Ctrl: 0.1 steps
        assert_eq!(drag_value(&s, 5.0, 0.0, 3.46, 0.1), 8.5);
        // symmetric extrude: the arrow travels half as far as the value
        assert_eq!(drag_value(&spec(0.5), 5.0, 0.0, 3.0, 1.0), 11.0);
        // never below the minimum
        assert_eq!(drag_value(&s, 5.0, 0.0, -20.0, 1.0), 0.1);
    }

    #[test]
    fn text_is_short() {
        assert_eq!(value_text(8.0), "8");
        assert_eq!(value_text(8.5), "8.5");
        assert_eq!(value_text(0.30000000000000004), "0.3");
    }
}
