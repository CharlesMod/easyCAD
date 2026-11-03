//! Move gizmo for an unconstrained occurrence: three translate arrows and three
//! rotation rings in the view frame, snapping to 1 mm and 15° (Shift: free). The
//! drag is shown live through a model-matrix override (no regeneration); release
//! writes the occurrence's new placement as one undo step.

use casework_model::assembly::{OccPath, Transform};
use egui::{pos2, vec2, Color32, Pos2, Rect, Stroke, Ui};
use glam::{DAffine3, DQuat, DVec3, Vec2, Vec3};

use crate::app::CaseworkApp;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    Move(usize),
    Turn(usize),
}

#[derive(Clone, Debug)]
pub struct GizmoDrag {
    pub path: OccPath,
    pub handle: Handle,
    pub start: Pos2,
    pub origin: Vec3,
    /// Snapped amount (mm or degrees).
    pub amount: f64,
}

const AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];

fn axis_color(i: usize) -> Color32 {
    [theme::hex(0xe5534b), theme::hex(0x57ab5a), theme::hex(0x539bf5)][i]
}

/// View-frame bounds of an occurrence's placed bodies (drags included).
pub fn occ_bounds(app: &CaseworkApp, path: &[casework_model::assembly::OccurrenceId]) -> Option<(Vec3, Vec3)> {
    let m = app.model.as_ref()?;
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    let mut any = false;
    for c in m.ctx.iter().filter(|c| c.path.starts_with(path)) {
        let Some(mesh) = &c.mb.mesh else { continue };
        let xf = crate::ui_assembly::display_xf(app, c);
        for p in mesh.positions.iter().step_by((mesh.positions.len() / 2000).max(1)) {
            let q = xf.transform_point3(Vec3::from_array(*p).as_dvec3()).as_vec3();
            lo = lo.min(q);
            hi = hi.max(q);
            any = true;
        }
    }
    any.then_some((lo, hi))
}

/// Centre of an occurrence's placed bodies in the view frame.
pub fn occ_center(app: &CaseworkApp, path: &[casework_model::assembly::OccurrenceId]) -> Option<Vec3> {
    let m = app.model.as_ref()?;
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    let mut any = false;
    for c in m.ctx.iter().filter(|c| c.path.starts_with(path)) {
        let Some(mesh) = &c.mb.mesh else { continue };
        // a sparse sample of the vertices is plenty for a handle position
        let step = (mesh.positions.len() / 400).max(1);
        for p in mesh.positions.iter().step_by(step) {
            let q = c.xf.transform_point3(Vec3::from_array(*p).as_dvec3()).as_vec3();
            lo = lo.min(q);
            hi = hi.max(q);
            any = true;
        }
    }
    any.then(|| (lo + hi) * 0.5)
}

/// Why the selected occurrence cannot be moved freely, if it cannot.
pub fn blocked(app: &CaseworkApp, path: &[casework_model::assembly::OccurrenceId]) -> Option<String> {
    let d = &app.doc.data;
    let o = path.last().and_then(|o| d.occurrence(*o))?;
    if o.grounded {
        return Some(format!("{} is grounded (unground it to move it)", o.name));
    }
    if crate::ui_assembly::is_mated(d, path) {
        return Some(format!("{} is placed by a mate (edit the mate to move it)", o.name));
    }
    None
}

fn delta(h: Handle, origin: Vec3, amount: f64) -> DAffine3 {
    match h {
        Handle::Move(i) => DAffine3::from_translation(AXES[i].as_dvec3() * amount),
        Handle::Turn(i) => {
            let o = origin.as_dvec3();
            DAffine3::from_translation(o)
                * DAffine3::from_quat(DQuat::from_axis_angle(AXES[i].as_dvec3(), amount.to_radians()))
                * DAffine3::from_translation(-o)
        }
    }
}

/// Draw the gizmo and run its drags. Returns true while it owns the pointer.
pub fn gizmo(app: &mut CaseworkApp, ui: &Ui, rect: Rect, resp: &egui::Response) -> bool {
    app.asm.gizmo_screen = None;
    if !app.asm.gizmo || app.asm.mate.is_some() || app.dialog.is_some() {
        app.asm.gdrag = None;
        return false;
    }
    let Some(path) = app.asm.sel.clone() else { return false };
    if blocked(app, &path).is_some() {
        return false;
    }
    let Some(origin) = app.asm.gdrag.as_ref().map(|g| g.origin).or_else(|| occ_center(app, &path)) else {
        return false;
    };
    let size = Vec2::new(rect.width(), rect.height());
    let cam = app.camera.clone();
    let proj = |q: Vec3| cam.project(q, size).map(|s| pos2(rect.left() + s.x, rect.top() + s.y));
    let len = cam.world_per_point(size.y).max(1e-4) * 80.0;
    let Some(o2) = proj(origin) else { return false };
    let painter = ui.painter_at(rect);
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let shift = ui.input(|i| i.modifiers.shift);
    // handle geometry on screen
    let mut hot: Option<Handle> = None;
    let mut best = 7.0f32;
    let seg_dist = |p: Pos2, a: Pos2, b: Pos2| {
        let ab = b - a;
        let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
        (a + ab * t).distance(p)
    };
    let mut rings: Vec<(usize, Vec<Pos2>)> = Vec::new();
    for i in 0..3 {
        let (u, v) = (AXES[(i + 1) % 3], AXES[(i + 2) % 3]);
        let pts: Vec<Pos2> = (0..=48)
            .filter_map(|k| {
                let a = k as f32 / 48.0 * std::f32::consts::TAU;
                proj(origin + (u * a.cos() + v * a.sin()) * len * 0.7)
            })
            .collect();
        if let Some(p) = pointer {
            for w in pts.windows(2) {
                let d = seg_dist(p, w[0], w[1]);
                if d < best {
                    best = d;
                    hot = Some(Handle::Turn(i));
                }
            }
        }
        rings.push((i, pts));
    }
    let mut tips = [o2; 3];
    for (i, a) in AXES.iter().enumerate() {
        if let Some(t) = proj(origin + *a * len) {
            tips[i] = t;
            if let Some(p) = pointer {
                let d = seg_dist(p, o2, t);
                if d < best + 2.0 {
                    best = d;
                    hot = Some(Handle::Move(i));
                }
            }
        }
    }
    let active = app.asm.gdrag.as_ref().map(|g| g.handle);
    app.asm.gizmo_screen = Some((o2, tips));
    // drag
    if resp.drag_started_by(egui::PointerButton::Primary) {
        if let (Some(h), Some(p)) = (hot, resp.interact_pointer_pos()) {
            app.asm.gdrag = Some(GizmoDrag { path: path.clone(), handle: h, start: p, origin, amount: 0.0 });
        }
    }
    let mut owns = app.asm.gdrag.is_some() || (hot.is_some() && pointer.is_some_and(|p| rect.contains(p)));
    if let Some(mut g) = app.asm.gdrag.clone() {
        if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
            let raw = match g.handle {
                Handle::Move(i) => {
                    let dir = tips[i] - o2;
                    let l2 = dir.length_sq().max(1e-6);
                    ((p - g.start).dot(dir) / l2 * len) as f64
                }
                Handle::Turn(i) => {
                    let a0 = (g.start - o2).angle();
                    let a1 = (p - o2).angle();
                    let mut d = (a1 - a0).to_degrees() as f64;
                    while d > 180.0 {
                        d -= 360.0;
                    }
                    while d < -180.0 {
                        d += 360.0;
                    }
                    // screen y points down; turn with the axis pointing at the viewer
                    let facing = AXES[i].dot(-cam.forward()) >= 0.0;
                    if facing { -d } else { d }
                }
            };
            let step = match g.handle {
                Handle::Move(_) => 1.0,
                Handle::Turn(_) => 15.0,
            };
            let amount = if shift { (raw * 10.0).round() / 10.0 } else { (raw / step).round() * step };
            g.amount = amount;
            if let Some(gd) = app.asm.gdrag.as_mut() {
                gd.amount = amount;
            }
            app.asm.drag = Some((g.path.clone(), delta(g.handle, g.origin, amount)));
            let unit = match g.handle {
                Handle::Move(i) => format!("{} {}", ["X", "Y", "Z"][i], crate::units::len(amount)),
                Handle::Turn(i) => format!("about {} {amount:.0}°", ["X", "Y", "Z"][i]),
            };
            painter.text(p + vec2(16.0, -10.0), egui::Align2::LEFT_BOTTOM, unit, theme::mono(12.0), theme::TEXT_STRONG);
        }
        if resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            app.asm.gdrag = None;
            app.asm.drag = None;
            if g.amount.abs() > 1e-9 {
                commit(app, &g.path, delta(g.handle, g.origin, g.amount));
            }
        }
        owns = true;
    }
    // paint
    for (i, pts) in &rings {
        let lit = hot == Some(Handle::Turn(*i)) || active == Some(Handle::Turn(*i));
        let c = axis_color(*i);
        painter.add(egui::Shape::line(pts.clone(), Stroke::new(if lit { 3.0 } else { 1.6 }, if lit { c } else { c.gamma_multiply(0.7) })));
    }
    for (i, tip) in tips.iter().enumerate() {
        let lit = hot == Some(Handle::Move(i)) || active == Some(Handle::Move(i));
        let c = axis_color(i);
        painter.arrow(o2, *tip - o2, Stroke::new(if lit { 3.5 } else { 2.2 }, c));
    }
    painter.circle(o2, 5.0, theme::CARD, Stroke::new(1.5, theme::TEXT_STRONG));
    if active.is_none() {
        if let Some(h) = hot {
            let tip = match h {
                Handle::Move(i) => format!("Drag to move along {} (1 mm steps, Shift: free)", ["X", "Y", "Z"][i]),
                Handle::Turn(i) => format!("Drag to turn about {} (15° steps, Shift: free)", ["X", "Y", "Z"][i]),
            };
            if let Some(p) = pointer {
                painter.text(p + vec2(14.0, 18.0), egui::Align2::LEFT_TOP, tip, theme::prop(11.0), theme::TEXT_DIM);
            }
        }
    }
    owns
}

/// Write `delta` (view frame) into the occurrence's local placement.
pub fn commit(app: &mut CaseworkApp, path: &OccPath, delta: DAffine3) {
    let d = &app.doc.data;
    let Some(id) = path.last().copied() else { return };
    let Some(o) = d.occurrence(id) else { return };
    let (Ok(active), Ok(parent)) = (d.world_transform(&app.active), d.world_transform(&path[..path.len() - 1])) else {
        return;
    };
    // view frame -> parent frame
    let vp = active.inverse().mul(&parent).affine();
    let local = vp.inverse() * delta * vp * o.transform.affine();
    let t = Transform::from_affine(&local);
    let name = o.name.clone();
    let rev = app.doc.data.revision;
    app.occ_edit(id, &format!("Move {name}"), |o| o.transform = t);
    if app.doc.data.revision != rev {
        app.asm.hold = Some((path.clone(), delta, app.doc.data.revision));
    }
    let _ = DVec3::ZERO;
}
