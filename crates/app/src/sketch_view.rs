//! Finished sketches drawn in the 3D view, on their own plane: crisp polylines
//! plus a faint fill on closed profiles. Geometry is cached per sketch and only
//! rebuilt when that sketch changes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use easycad_model::feature::SketchId;
use easycad_model::geom::Segment2;
use easycad_model::sketch::{Geom, Sketch};
use glam::DVec2;
use easycad_model::occt::{EdgePolyline, Mesh};

use crate::app::{EasyCadApp, Mode};

/// Sketch line colour (sRGB, 0..1) and the translucent fill's.
pub const LINE: [f32; 4] = [0.18, 0.82, 0.62, 1.0];
pub const FILL_ALPHA: f32 = 0.14;

const ARC_STEP: f64 = std::f64::consts::PI / 36.0;

fn arc_points(c: DVec2, a0: f64, sweep: f64, r: f64) -> Vec<DVec2> {
    let n = ((sweep.abs() / ARC_STEP).ceil() as usize).max(2);
    (0..=n).map(|i| c + DVec2::from_angle(a0 + sweep * i as f64 / n as f64) * r).collect()
}

fn ccw_sweep(a0: f64, a1: f64) -> f64 {
    let mut s = a1 - a0;
    while s <= 0.0 {
        s += std::f64::consts::TAU;
    }
    s
}

/// Tessellated outline of one segment (2D).
fn seg_points(s: &Segment2) -> Vec<DVec2> {
    match *s {
        Segment2::Line { a, b } => vec![a, b],
        Segment2::Arc { center, start, end, ccw } => {
            let r = (start - center).length();
            let a0 = (start - center).to_angle();
            let a1 = (end - center).to_angle();
            let sw = ccw_sweep(a0, a1);
            let sweep = if ccw { sw } else { sw - std::f64::consts::TAU };
            arc_points(center, a0, sweep, r)
        }
        Segment2::Circle { center, radius } => arc_points(center, 0.0, std::f64::consts::TAU, radius),
    }
}

fn area(p: &[DVec2]) -> f64 {
    p.iter().enumerate().map(|(i, a)| a.perp_dot(p[(i + 1) % p.len()])).sum::<f64>() / 2.0
}

/// Ear clipping of a simple polygon (any winding).
fn triangulate(poly: &[DVec2]) -> Vec<[usize; 3]> {
    let mut idx: Vec<usize> = (0..poly.len()).collect();
    if area(poly) < 0.0 {
        idx.reverse();
    }
    let mut out = Vec::new();
    let mut guard = 0;
    while idx.len() > 3 && guard < 10_000 {
        guard += 1;
        let n = idx.len();
        let mut clipped = false;
        for i in 0..n {
            let (a, b, c) = (idx[(i + n - 1) % n], idx[i], idx[(i + 1) % n]);
            let (pa, pb, pc) = (poly[a], poly[b], poly[c]);
            if (pb - pa).perp_dot(pc - pb) <= 1e-12 {
                continue;
            }
            let inside = idx.iter().any(|&k| {
                if k == a || k == b || k == c {
                    return false;
                }
                let p = poly[k];
                (pb - pa).perp_dot(p - pa) >= 0.0 && (pc - pb).perp_dot(p - pb) >= 0.0 && (pa - pc).perp_dot(p - pc) >= 0.0
            });
            if !inside {
                out.push([a, b, c]);
                idx.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            break;
        }
    }
    if idx.len() == 3 {
        out.push([idx[0], idx[1], idx[2]]);
    }
    out
}

/// The sketch as a mesh in world coordinates: `edges` = its non-construction
/// entities, triangles = the closed profiles without holes.
pub fn sketch_mesh(sk: &Sketch) -> Mesh {
    let pl = &sk.plane;
    let w = |p: DVec2| pl.to_world(p).as_vec3().to_array();
    let mut m = Mesh::default();
    for (i, e) in sk.entities.iter().enumerate() {
        if e.construction {
            continue;
        }
        let pts: Vec<DVec2> = match e.geom {
            Geom::Line { a, b } => vec![sk.point(a), sk.point(b)],
            Geom::Circle { center, radius } => arc_points(sk.point(center), 0.0, std::f64::consts::TAU, radius),
            Geom::Arc { center, start, end } => {
                let c = sk.point(center);
                let (s, t) = (sk.point(start), sk.point(end));
                arc_points(c, (s - c).to_angle(), ccw_sweep((s - c).to_angle(), (t - c).to_angle()), (s - c).length())
            }
        };
        m.edges.push(EdgePolyline { edge: i as u32, points: pts.into_iter().map(w).collect() });
    }
    let n = pl.normal.as_vec3().to_array();
    for prof in sk.profiles().profiles.iter().filter(|p| p.holes.is_empty()) {
        let mut poly: Vec<DVec2> = Vec::new();
        for s in &prof.outer {
            let mut p = seg_points(s);
            p.pop();
            poly.extend(p);
        }
        if poly.len() < 3 {
            continue;
        }
        let base = m.positions.len() as u32;
        for p in &poly {
            m.positions.push(w(*p));
            m.normals.push(n);
        }
        for t in triangulate(&poly) {
            m.indices.push([base + t[0] as u32, base + t[1] as u32, base + t[2] as u32]);
            m.tri_face.push(0);
        }
    }
    m
}

thread_local! {
    static CACHE: RefCell<HashMap<(String, SketchId), (Sketch, Arc<Mesh>)>> = RefCell::new(HashMap::new());
    static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many sketch meshes this thread has built (for cache tests).
pub fn builds() -> usize {
    BUILDS.with(|b| b.get())
}

fn cached(doc_id: &str, id: SketchId, sk: &Sketch) -> Arc<Mesh> {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let k = (doc_id.to_string(), id);
        if let Some((old, m)) = c.get(&k) {
            if old == sk {
                return m.clone();
            }
        }
        BUILDS.with(|b| b.set(b.get() + 1));
        let m = Arc::new(sketch_mesh(sk));
        c.insert(k, (sk.clone(), m.clone()));
        m
    })
}

/// The sketches the 3D view draws now: finished, visible, and not the one being edited.
pub fn draw_list(app: &EasyCadApp) -> Vec<(SketchId, Arc<Mesh>)> {
    if matches!(app.mode, Mode::Sketch(_)) {
        return Vec::new();
    }
    let view = app.view();
    view.sketches
        .iter()
        .filter(|e| view.sketch_visible(e.id))
        .filter(|e| !e.sketch.entities.is_empty())
        .map(|e| (e.id, cached(&app.doc.data.doc_id, e.id, &e.sketch)))
        .collect()
}

/// Show or hide a sketch in the 3D view (an undoable edit; the choice sticks).
pub fn set_visible(app: &mut EasyCadApp, id: SketchId, visible: bool) {
    let scope = app.scope();
    let label = if visible { "Show sketch" } else { "Hide sketch" };
    let _ = crate::scope::edit(&mut app.doc, scope, label, |d| {
        d.set_sketch_visible(id, Some(visible));
        Ok(())
    });
}
