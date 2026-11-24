//! "Pick a plane" state that precedes a new sketch: the three origin planes show
//! as translucent labelled squares, hovering one (or any planar face) highlights
//! it, a left click designates it and opens sketch mode there, Esc or a right
//! click cancels.

use std::sync::Arc;

use easycad_model::feature::BodyId;
use easycad_model::geom::Plane;
use easycad_model::occt::{EdgePolyline, Mesh, SurfaceKind};
use egui::{pos2, Color32, Rect, Ui};
use glam::{DVec2, Vec2, Vec3};

use crate::app::EasyCadApp;
use crate::worker::Output;

/// What the pointer designates.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// One of the origin planes: 0 = XY, 1 = XZ, 2 = YZ.
    Origin(usize),
    /// A planar face of a body.
    Face(BodyId, u32),
    /// A visible construction plane (its feature).
    Datum(easycad_model::feature::FeatureId),
}

/// The state while waiting for the click.
#[derive(Clone, Debug, Default)]
pub struct PlanePick {
    pub hover: Option<Target>,
    /// The document had no geometry when the pick began: geometry arriving later
    /// (an import, a wizard, Claude) ends the pick, the planes having served.
    pub scene_was_empty: bool,
}

pub const HINT: &str = "Click a plane or flat face to sketch on (Esc to cancel)";
const NAMES: [&str; 3] = ["XY", "XZ", "YZ"];

pub fn origin_plane(i: usize) -> Plane {
    [Plane::XY, Plane::XZ, Plane::YZ][i]
}

/// Half-size of the origin squares: scaled to the model, never tiny.
pub fn half_size(app: &EasyCadApp) -> f64 {
    let bb = app.model.as_deref().and_then(|m| m.bbox);
    bb.map(|b| (b.size().max_element() * 0.6).max(30.0)).unwrap_or(40.0)
}

/// The plane of a planar face (`None` for any other surface).
pub fn face_plane(m: &Output, body: BodyId, face: u32) -> Option<Plane> {
    let info = m.meshed(body)?.faces.get(face as usize)?;
    if info.kind != SurfaceKind::Plane {
        return None;
    }
    let n = info.normal;
    let x = if n.z.abs() > 0.9 { glam::DVec3::X } else { glam::DVec3::Z.cross(n) };
    Some(Plane::new(n * info.point.dot(n), x, n))
}

fn square_mesh(pl: &Plane, h: f64) -> Mesh {
    let c = [DVec2::new(-h, -h), DVec2::new(h, -h), DVec2::new(h, h), DVec2::new(-h, h)];
    let pts: Vec<[f32; 3]> = c.iter().map(|p| pl.to_world(*p).as_vec3().to_array()).collect();
    let n = pl.normal.as_vec3().to_array();
    let mut ring = pts.clone();
    ring.push(pts[0]);
    Mesh {
        positions: pts,
        normals: vec![n; 4],
        indices: vec![[0, 1, 2], [0, 2, 3]],
        tri_face: vec![0, 0],
        edges: vec![EdgePolyline { edge: 0, points: ring }],
    }
}

thread_local! {
    static SQUARES: std::cell::RefCell<Option<(u64, Vec<Arc<Mesh>>)>> = const { std::cell::RefCell::new(None) };
}

fn squares(h: f64) -> Vec<Arc<Mesh>> {
    SQUARES.with(|c| {
        let mut c = c.borrow_mut();
        let key = h.to_bits();
        if let Some((k, v)) = c.as_ref() {
            if *k == key {
                return v.clone();
            }
        }
        let v: Vec<Arc<Mesh>> = (0..3).map(|i| Arc::new(square_mesh(&origin_plane(i), h))).collect();
        *c = Some((key, v.clone()));
        v
    })
}

/// The squares to draw (mesh, highlighted) while picking.
pub fn draw_list(app: &EasyCadApp) -> Vec<(Arc<Mesh>, bool)> {
    let Some(pp) = &app.plane_pick else { return Vec::new() };
    squares(half_size(app))
        .into_iter()
        .enumerate()
        .map(|(i, m)| (m, pp.hover == Some(Target::Origin(i))))
        .collect()
}

/// The hovered face to highlight (body, face), if any.
pub fn hovered_face(app: &EasyCadApp) -> Option<(BodyId, u32)> {
    match app.plane_pick.as_ref()?.hover {
        Some(Target::Face(b, f)) => Some((b, f)),
        _ => None,
    }
}

/// What the ray designates: the nearest of the origin squares and planar faces.
pub fn target_at(app: &EasyCadApp, o: Vec3, d: Vec3, tol: easycad_model::pick::EdgeTolerance) -> Option<Target> {
    let mut best: Option<(f32, Target)> = None;
    let h = half_size(app);
    for i in 0..3 {
        let pl = origin_plane(i);
        let (n, p0) = (pl.normal.as_vec3(), pl.origin.as_vec3());
        let den = n.dot(d);
        if den.abs() < 1e-6 {
            continue;
        }
        let t = n.dot(p0 - o) / den;
        if t <= 0.0 {
            continue;
        }
        let q = o + d * t - p0;
        let (x, y) = (q.dot(pl.x_dir.as_vec3()), q.dot(pl.y_dir().as_vec3()));
        if x.abs() <= h as f32 && y.abs() <= h as f32 && best.as_ref().is_none_or(|b| t < b.0) {
            best = Some((t, Target::Origin(i)));
        }
    }
    for it in crate::datum_ui::datum_items(app, None) {
        let (crate::datum_ui::G::Plane(pl), Some(fid)) = (&it.g, it.fid) else { continue };
        let (n, p0) = (pl.normal.as_vec3(), pl.origin.as_vec3());
        let den = n.dot(d);
        if den.abs() < 1e-6 {
            continue;
        }
        let t = n.dot(p0 - o) / den;
        if t <= 0.0 {
            continue;
        }
        let q = o + d * t - p0;
        let (x, y) = (q.dot(pl.x_dir.as_vec3()), q.dot(pl.y_dir().as_vec3()));
        if x.abs() <= h as f32 && y.abs() <= h as f32 && best.as_ref().is_none_or(|b| t < b.0) {
            best = Some((t, Target::Datum(fid)));
        }
    }
    if let Some(m) = app.model.as_deref() {
        for hit in m.pick_all(o, d, tol, &|_| false) {
            if let easycad_model::pick::HitKind::Face(f) = hit.kind {
                if !app.body_visible(hit.body) || face_plane(m, hit.body, f).is_none() {
                    continue;
                }
                if best.as_ref().is_none_or(|b| hit.t < b.0) {
                    best = Some((hit.t, Target::Face(hit.body, f)));
                }
                break; // nearest planar face only
            }
        }
    }
    best.map(|b| b.1)
}

/// Frame-by-frame input while picking. Returns true when it consumed the click.
pub fn interact(app: &mut EasyCadApp, ui: &Ui, resp: &egui::Response, rect: Rect) {
    if app.plane_pick.is_none() {
        return;
    }
    let size = Vec2::new(rect.width(), rect.height());
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) || resp.secondary_clicked() {
        app.plane_pick = None;
        return;
    }
    let hover = resp.hover_pos().map(|p| {
        let l = p - rect.min;
        let (o, d) = app.camera.ray(Vec2::new(l.x, l.y), size);
        let tol = if app.camera.ortho {
            easycad_model::pick::EdgeTolerance::ortho(app.camera.world_per_point(size.y) * 5.0)
        } else {
            easycad_model::pick::EdgeTolerance::perspective(5.0, app.camera.fov_y, size.y)
        };
        target_at(app, o, d, tol)
    });
    let target = hover.flatten();
    if let Some(pp) = &mut app.plane_pick {
        pp.hover = target.clone();
    }
    if resp.clicked_by(egui::PointerButton::Primary) {
        if let Some(t) = target {
            app.start_sketch_on_target(&t);
        }
    }
    ui.ctx().request_repaint();
}

/// Origin-plane labels, painted over the 3D image.
pub fn paint_labels(app: &EasyCadApp, p: &egui::Painter, rect: Rect) {
    if app.plane_pick.is_none() {
        return;
    }
    let size = Vec2::new(rect.width(), rect.height());
    let h = half_size(app);
    for (i, name) in NAMES.iter().enumerate() {
        let pl = origin_plane(i);
        let w = pl.to_world(DVec2::splat(h * 0.82)).as_vec3();
        if let Some(s) = app.camera.project(w, size) {
            let hot = app.plane_pick.as_ref().is_some_and(|pp| pp.hover == Some(Target::Origin(i)));
            p.text(
                pos2(rect.left() + s.x, rect.top() + s.y),
                egui::Align2::CENTER_CENTER,
                *name,
                crate::theme::prop(13.0),
                if hot { Color32::WHITE } else { Color32::from_rgb(150, 215, 190) },
            );
        }
    }
}

/// The plane and body a target stands for.
pub fn resolve(app: &EasyCadApp, t: &Target) -> Option<(Plane, Option<BodyId>)> {
    match t {
        Target::Origin(i) => Some((origin_plane(*i), None)),
        Target::Datum(f) => crate::datum_ui::plane_of(app, *f).map(|p| (p, None)),
        Target::Face(b, f) => {
            let m = app.model.as_deref()?;
            face_plane(m, *b, *f).map(|p| (p, Some(*b)))
        }
    }
}
