//! View cube: a shaded cube oriented live with the camera, drawn with egui's
//! painter. Each face is split into a 3×3 grid of hit zones (narrow edge and
//! corner bands), giving the 26 regions of [`CubeRegion`]. Hover highlights the
//! region, click animates to it, drag orbits, right-click opens the home/projection
//! menu, and arrow buttons rotate in 15° steps.

use egui::{pos2, vec2, Color32, Pos2, Rect, Sense, Shape, Stroke, Ui};
use glam::{Quat, Vec3};

use crate::camera::{CubeRegion, StepAxis, StepFrame};
use crate::theme;

/// Fraction of a face taken by each edge band.
pub const BAND: f32 = 0.2;

#[derive(Clone, Debug, PartialEq)]
pub enum CubeAction {
    Region(CubeRegion),
    Orbit(egui::Vec2),
    Step(StepAxis, f32),
    Home,
    SetHome,
    ResetHome,
    SetOrtho(bool),
    SetFitOnChange(bool),
    SetStepFrame(StepFrame),
}

/// One hit zone of one face, as a quad in cube coordinates (cube spans ±1).
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub region: CubeRegion,
    pub normal: Vec3,
    pub corners: [Vec3; 4],
}

fn axes(n: Vec3) -> (Vec3, Vec3) {
    if n.x.abs() > 0.5 {
        (Vec3::Y, Vec3::Z)
    } else if n.y.abs() > 0.5 {
        (Vec3::X, Vec3::Z)
    } else {
        (Vec3::X, Vec3::Y)
    }
}

/// The 54 cells (6 faces × 9) of the cube.
pub fn cells() -> Vec<Cell> {
    let b = 1.0 - 2.0 * BAND;
    let bounds = [(-1.0, -b), (-b, b), (b, 1.0)];
    let mut out = Vec::with_capacity(54);
    for n in [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ] {
        let (u, v) = axes(n);
        for (i, (u0, u1)) in bounds.iter().enumerate() {
            for (j, (v0, v1)) in bounds.iter().enumerate() {
                let d = n + u * (i as f32 - 1.0) + v * (j as f32 - 1.0);
                let region = CubeRegion([d.x.round() as i8, d.y.round() as i8, d.z.round() as i8]);
                let p = |a: f32, c: f32| n + u * a + v * c;
                out.push(Cell {
                    region,
                    normal: n,
                    corners: [p(*u0, *v0), p(*u1, *v0), p(*u1, *v1), p(*u0, *v1)],
                });
            }
        }
    }
    out
}

/// Projection of the cube for a camera orientation (orthographic, cube-centred).
pub struct CubeProjection {
    pub rot: Quat,
    pub center: Pos2,
    pub scale: f32,
}

impl CubeProjection {
    pub fn project(&self, p: Vec3) -> Pos2 {
        let c = self.rot.inverse() * p;
        pos2(
            self.center.x + c.x * self.scale,
            self.center.y - c.y * self.scale,
        )
    }
    pub fn facing(&self, n: Vec3) -> f32 {
        (self.rot.inverse() * n).z
    }
    /// The region under a screen point (front-facing cells only).
    pub fn hit(&self, p: Pos2) -> Option<CubeRegion> {
        cells()
            .into_iter()
            .filter(|c| self.facing(c.normal) > 0.05)
            .find(|c| inside(p, &c.corners.map(|q| self.project(q))))
            .map(|c| c.region)
    }
}

fn inside(p: Pos2, poly: &[Pos2; 4]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let a = poly[i];
        let b = poly[(i + 1) % 4];
        let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

fn face_label(n: Vec3) -> &'static str {
    match (n.x.round() as i32, n.y.round() as i32, n.z.round() as i32) {
        (1, 0, 0) => "RIGHT",
        (-1, 0, 0) => "LEFT",
        (0, 1, 0) => "BACK",
        (0, -1, 0) => "FRONT",
        (0, 0, 1) => "TOP",
        _ => "BOTTOM",
    }
}

pub struct CubeState<'a> {
    pub rot: Quat,
    pub ortho: bool,
    pub fit_on_change: bool,
    pub step_frame: StepFrame,
    /// Region to show highlighted even without hover (tests / keyboard focus).
    pub force_hover: Option<CubeRegion>,
    pub id: &'a str,
}

/// Draw the cube in `rect` (its top-left area holds the home button; the step
/// grid sits under it). Returns the action, if any.
pub fn view_cube(ui: &mut Ui, rect: Rect, st: &CubeState) -> Option<CubeAction> {
    let mut action = None;
    let cube_rect = Rect::from_min_size(rect.min, vec2(rect.width(), rect.width()));
    let resp = ui.interact(
        cube_rect,
        ui.scope_id().with(st.id),
        Sense::click_and_drag(),
    );
    let proj = CubeProjection {
        rot: st.rot,
        center: cube_rect.center(),
        scale: cube_rect.width() * 0.29,
    };
    let hover = st
        .force_hover
        .or_else(|| resp.hover_pos().and_then(|p| proj.hit(p)));
    let painter = ui.painter_at(rect.expand(4.0));

    // faces back to front
    let mut cs = cells();
    cs.retain(|c| proj.facing(c.normal) > 0.0);
    cs.sort_by(|a, b| proj.facing(a.normal).total_cmp(&proj.facing(b.normal)));
    let light = Vec3::new(-0.3, 0.5, 1.0).normalize();
    for c in &cs {
        let pts: Vec<Pos2> = c.corners.iter().map(|q| proj.project(*q)).collect();
        let cam_n = st.rot.inverse() * c.normal;
        let shade = 0.55 + 0.45 * cam_n.dot(light).max(0.0);
        let base = theme::hex(0x3a3f47);
        let mut fill = Color32::from_rgb(
            (base.r() as f32 * shade + 12.0) as u8,
            (base.g() as f32 * shade + 12.0) as u8,
            (base.b() as f32 * shade + 14.0) as u8,
        );
        if hover == Some(c.region) {
            fill = theme::ACCENT_BG;
        }
        painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
    }
    // outlines and labels per face
    for n in [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ] {
        let f = proj.facing(n);
        if f <= 0.0 {
            continue;
        }
        let (u, v) = axes(n);
        let quad = [n - u - v, n + u - v, n + u + v, n - u + v].map(|q| proj.project(q));
        let hi = hover.map(|h| {
            let d = Vec3::new(h.0[0] as f32, h.0[1] as f32, h.0[2] as f32);
            h.is_face() && d == n
        });
        let stroke = Stroke::new(
            1.0,
            if hi == Some(true) {
                theme::ACCENT
            } else {
                theme::hex(0x5b616b)
            },
        );
        painter.add(Shape::closed_line(quad.to_vec(), stroke));
        if f > 0.25 {
            let c = proj.project(n);
            let size = (8.5 * f.sqrt()).max(6.0);
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                face_label(n),
                theme::prop(size),
                theme::TEXT_DIM.gamma_multiply(f.sqrt()),
            );
        }
    }
    if let Some(h) = hover {
        resp.clone().on_hover_text(h.label());
    }
    if resp.dragged() && resp.drag_delta().length() > 0.0 {
        action = Some(CubeAction::Orbit(resp.drag_delta() * 1.5));
    } else if resp.clicked() {
        if let Some(r) = resp.interact_pointer_pos().and_then(|p| proj.hit(p)) {
            action = Some(CubeAction::Region(r));
        }
    }
    resp.context_menu(|ui| {
        if ui.button("Home").clicked() {
            action = Some(CubeAction::Home);
        }
        if ui.button("Set current view as Home").clicked() {
            action = Some(CubeAction::SetHome);
        }
        if ui.button("Reset home").clicked() {
            action = Some(CubeAction::ResetHome);
        }
        ui.separator();
        if ui.radio(!st.ortho, "Perspective").clicked() {
            action = Some(CubeAction::SetOrtho(false));
        }
        if ui.radio(st.ortho, "Orthographic").clicked() {
            action = Some(CubeAction::SetOrtho(true));
        }
        ui.separator();
        let mut fit = st.fit_on_change;
        if ui.checkbox(&mut fit, "Fit to view on change").changed() {
            action = Some(CubeAction::SetFitOnChange(fit));
        }
        ui.separator();
        ui.label(
            egui::RichText::new("15° steps rotate about")
                .small()
                .color(theme::TEXT_MUTED),
        );
        if ui
            .radio(st.step_frame == StepFrame::World, "World X / Y / Z")
            .clicked()
        {
            action = Some(CubeAction::SetStepFrame(StepFrame::World));
        }
        if ui
            .radio(st.step_frame == StepFrame::Screen, "Screen axes")
            .clicked()
        {
            action = Some(CubeAction::SetStepFrame(StepFrame::Screen));
        }
    });

    // home button (top-left of the cube box)
    let home = Rect::from_min_size(rect.min + vec2(-2.0, -2.0), vec2(20.0, 20.0));
    let hr = ui.interact(home, ui.scope_id().with((st.id, "home")), Sense::click());
    let hc = if hr.hovered() {
        theme::TEXT_STRONG
    } else {
        theme::TEXT_MUTED
    };
    let c = home.center();
    let roof = vec![
        pos2(c.x - 6.0, c.y),
        pos2(c.x, c.y - 6.0),
        pos2(c.x + 6.0, c.y),
    ];
    painter.add(Shape::line(roof, Stroke::new(1.4, hc)));
    painter.add(Shape::line(
        vec![
            pos2(c.x - 4.0, c.y - 1.0),
            pos2(c.x - 4.0, c.y + 5.0),
            pos2(c.x + 4.0, c.y + 5.0),
            pos2(c.x + 4.0, c.y - 1.0),
        ],
        Stroke::new(1.4, hc),
    ));
    if hr.on_hover_text("Home view (Home)").clicked() {
        action = Some(CubeAction::Home);
    }

    // 15° step grid under the cube: rows X / Y / Z, columns − / +
    let grid_top = cube_rect.bottom() + 2.0;
    let frame_word = if st.step_frame == StepFrame::World {
        "world"
    } else {
        "screen"
    };
    for (row, (axis, name, keys)) in [
        (StepAxis::X, "X", ("Alt+↑", "Alt+↓")),
        (StepAxis::Y, "Y", ("Alt+PgUp", "Alt+PgDn")),
        (StepAxis::Z, "Z", ("Alt+←", "Alt+→")),
    ]
    .iter()
    .enumerate()
    {
        let w = rect.width() / 3.0;
        let y = grid_top + row as f32 * 20.0;
        for (col, sign) in [-1.0f32, 1.0].iter().enumerate() {
            let x = rect.left() + if col == 0 { 0.0 } else { 2.0 * w };
            let r = Rect::from_min_size(pos2(x, y), vec2(w - 2.0, 18.0));
            let resp = ui.interact(r, ui.scope_id().with((st.id, row, col)), Sense::click());
            let bg = if resp.hovered() {
                theme::HOVER
            } else {
                theme::CARD
            };
            painter.rect_filled(r, 4.0, bg);
            let arrow = if *sign < 0.0 { "⟲" } else { "⟳" };
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                arrow,
                theme::prop(12.0),
                theme::TEXT_DIM,
            );
            let key = if *sign < 0.0 { keys.0 } else { keys.1 };
            let tip = format!(
                "Rotate {}15° about {frame_word} {name} ({key})",
                if *sign < 0.0 { "−" } else { "+" }
            );
            if resp.on_hover_text(tip).clicked() {
                action = Some(CubeAction::Step(*axis, *sign));
            }
        }
        let mid = Rect::from_min_size(pos2(rect.left() + w, y), vec2(w - 2.0, 18.0));
        let col = match axis {
            StepAxis::X => theme::RED,
            StepAxis::Y => theme::GREEN,
            StepAxis::Z => theme::ACCENT,
        };
        painter.text(
            mid.center(),
            egui::Align2::CENTER_CENTER,
            *name,
            theme::mono(11.0),
            col,
        );
    }
    action
}

/// Height the cube widget needs for a given width.
pub fn widget_height(width: f32) -> f32 {
    width + 2.0 + 3.0 * 20.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{iso_rot, look_rot};

    #[test]
    fn cells_cover_all_26_regions() {
        let cs = cells();
        assert_eq!(cs.len(), 54);
        let mut regions: Vec<CubeRegion> = cs.iter().map(|c| c.region).collect();
        regions.sort_by_key(|r| r.0);
        regions.dedup();
        assert_eq!(regions.len(), 26);
        // corners appear on 3 faces, edges on 2, faces once
        for r in CubeRegion::all() {
            let n = cs.iter().filter(|c| c.region == r).count();
            assert_eq!(n, r.nonzero(), "{r:?}");
        }
    }

    #[test]
    fn hit_test_finds_face_edge_corner_bands() {
        let proj = CubeProjection {
            rot: look_rot(Vec3::NEG_Y, Vec3::Z),
            center: pos2(50.0, 50.0),
            scale: 30.0,
        };
        // straight at the front: centre is FRONT, near the top border the top edge band
        assert_eq!(proj.hit(pos2(50.0, 50.0)), Some(CubeRegion::FRONT));
        assert_eq!(proj.hit(pos2(50.0, 21.5)), Some(CubeRegion([0, -1, 1])));
        assert_eq!(proj.hit(pos2(78.5, 21.5)), Some(CubeRegion([1, -1, 1])));
        assert_eq!(proj.hit(pos2(5.0, 5.0)), None);
        // iso: the projected top-front-right corner point is the corner region
        let proj = CubeProjection {
            rot: iso_rot(),
            center: pos2(50.0, 50.0),
            scale: 30.0,
        };
        let p = proj.project(Vec3::new(0.97, -0.97, 0.97));
        assert_eq!(proj.hit(p), Some(CubeRegion([1, -1, 1])));
        let top = proj.project(Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(proj.hit(top), Some(CubeRegion::TOP));
    }
}
