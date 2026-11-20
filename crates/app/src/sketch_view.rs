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
pub(crate) fn seg_points(s: &Segment2) -> Vec<DVec2> {
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

fn loop_points(l: &easycad_model::geom::Loop2) -> Vec<DVec2> {
    let mut poly = Vec::new();
    for s in l {
        let mut p = seg_points(s);
        p.pop();
        poly.extend(p);
    }
    poly
}

fn proper_cross(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> bool {
    let o = |p: DVec2, q: DVec2, r: DVec2| (q - p).perp_dot(r - p);
    let (d1, d2, d3, d4) = (o(a, b, c), o(a, b, d), o(c, d, a), o(c, d, b));
    ((d1 > 1e-12 && d2 < -1e-12) || (d1 < -1e-12 && d2 > 1e-12)) && ((d3 > 1e-12 && d4 < -1e-12) || (d3 < -1e-12 && d4 > 1e-12))
}

/// Triangles of a region (outer minus holes): holes are bridged into the outer polygon,
/// then ear-clipped. Returns the vertex list and index triples into it.
pub fn triangulate_region(r: &easycad_model::regions::Region) -> (Vec<DVec2>, Vec<[usize; 3]>) {
    let mut poly = loop_points(&r.outer);
    if area(&poly) < 0.0 {
        poly.reverse();
    }
    let mut holes: Vec<Vec<DVec2>> = r
        .holes
        .iter()
        .map(|h| {
            let mut p = loop_points(h);
            if area(&p) > 0.0 {
                p.reverse();
            }
            p
        })
        .filter(|h| h.len() >= 3)
        .collect();
    holes.sort_by(|a, b| {
        let mx = |h: &Vec<DVec2>| h.iter().map(|p| p.x).fold(f64::MIN, f64::max);
        mx(b).total_cmp(&mx(a))
    });
    for hi in 0..holes.len() {
        let hole = holes[hi].clone();
        let mi = (0..hole.len()).max_by(|&a, &b| hole[a].x.total_cmp(&hole[b].x)).unwrap();
        let m = hole[mi];
        // candidate bridge vertices, nearest first; must see M without crossing any edge
        let mut order: Vec<usize> = (0..poly.len()).collect();
        order.sort_by(|&a, &b| poly[a].distance_squared(m).total_cmp(&poly[b].distance_squared(m)));
        let blocked = |v: DVec2, poly: &[DVec2]| {
            let n = poly.len();
            (0..n).any(|i| proper_cross(m, v, poly[i], poly[(i + 1) % n]))
                || holes[hi..].iter().any(|h| (0..h.len()).any(|i| proper_cross(m, v, h[i], h[(i + 1) % h.len()])))
        };
        let Some(&vi) = order.iter().find(|&&k| !blocked(poly[k], &poly)) else { continue };
        let mut merged: Vec<DVec2> = poly[..=vi].to_vec();
        for k in 0..=hole.len() {
            merged.push(hole[(mi + k) % hole.len()]);
        }
        merged.extend_from_slice(&poly[vi..]);
        poly = merged;
    }
    let tris = clip_ears(&poly);
    (poly, tris)
}

/// Ear clipping of a CCW polygon that may have coincident (bridge) vertices.
fn clip_ears(poly: &[DVec2]) -> Vec<[usize; 3]> {
    let mut idx: Vec<usize> = (0..poly.len()).collect();
    let mut out = Vec::new();
    let mut guard = 0;
    while idx.len() > 3 && guard < 100_000 {
        guard += 1;
        let n = idx.len();
        let mut done = false;
        for i in 0..n {
            let (a, b, c) = (idx[(i + n - 1) % n], idx[i], idx[(i + 1) % n]);
            let (pa, pb, pc) = (poly[a], poly[b], poly[c]);
            if (pb - pa).perp_dot(pc - pb) <= 1e-12 {
                continue;
            }
            let same = |p: DVec2, q: DVec2| p.distance_squared(q) < 1e-18;
            let inside = idx.iter().any(|&k| {
                let p = poly[k];
                if k == a || k == b || k == c || same(p, pa) || same(p, pb) || same(p, pc) {
                    return false;
                }
                (pb - pa).perp_dot(p - pa) >= 0.0 && (pc - pb).perp_dot(p - pb) >= 0.0 && (pa - pc).perp_dot(p - pc) >= 0.0
            });
            if !inside {
                out.push([a, b, c]);
                idx.remove(i);
                done = true;
                break;
            }
        }
        if !done {
            // drop a degenerate (zero-area) vertex, else give up
            let n = idx.len();
            match (0..n).find(|&i| {
                let (a, b, c) = (idx[(i + n - 1) % n], idx[i], idx[(i + 1) % n]);
                (poly[b] - poly[a]).perp_dot(poly[c] - poly[b]).abs() <= 1e-12
            }) {
                Some(i) => {
                    idx.remove(i);
                }
                None => break,
            }
        }
    }
    if idx.len() == 3 {
        out.push([idx[0], idx[1], idx[2]]);
    }
    out
}

/// Dash and gap length (model units) of construction lines in the 3D view.
const CONSTRUCTION_DASH: f64 = 2.0;

/// The "on" pieces of a polyline cut into dashes of `len` with gaps of `len`.
fn dashes(pts: &[DVec2], len: f64) -> Vec<Vec<DVec2>> {
    let mut out = Vec::new();
    let mut cur: Vec<DVec2> = Vec::new();
    let mut on = true;
    let mut left = len;
    if let Some(p0) = pts.first() {
        cur.push(*p0);
    }
    for w in pts.windows(2) {
        let (mut a, b) = (w[0], w[1]);
        loop {
            let d = a.distance(b);
            if d <= left {
                left -= d;
                if on {
                    cur.push(b);
                }
                break;
            }
            let q = a + (b - a) * (left / d);
            if on {
                cur.push(q);
                out.push(std::mem::take(&mut cur));
            } else {
                cur = vec![q];
            }
            on = !on;
            a = q;
            left = len;
        }
    }
    if on && cur.len() > 1 {
        out.push(cur);
    }
    out
}

/// The sketch as a mesh in world coordinates: `edges` = its entities (construction
/// ones dashed), triangles = the closed profiles without holes.
pub fn sketch_mesh(sk: &Sketch) -> Mesh {
    let pl = &sk.plane;
    let w = |p: DVec2| pl.to_world(p).as_vec3().to_array();
    let mut m = Mesh::default();
    for (i, e) in sk.entities.iter().enumerate() {
        let pts: Vec<DVec2> = match e.geom {
            Geom::Line { a, b } => vec![sk.point(a), sk.point(b)],
            Geom::Circle { center, radius } => arc_points(sk.point(center), 0.0, std::f64::consts::TAU, radius),
            Geom::Arc { center, start, end } => {
                let c = sk.point(center);
                let (s, t) = (sk.point(start), sk.point(end));
                arc_points(c, (s - c).to_angle(), ccw_sweep((s - c).to_angle(), (t - c).to_angle()), (s - c).length())
            }
        };
        if e.construction {
            // the renderer has no dash style: cut the polyline into short pieces
            for d in dashes(&pts, CONSTRUCTION_DASH) {
                m.edges.push(EdgePolyline { edge: i as u32, points: d.into_iter().map(w).collect() });
            }
        } else {
            m.edges.push(EdgePolyline { edge: i as u32, points: pts.into_iter().map(w).collect() });
        }
    }
    let n = pl.normal.as_vec3().to_array();
    for r in sk.regions().iter().filter(|r| r.filled) {
        let (poly, tris) = triangulate_region(r);
        let base = m.positions.len() as u32;
        for p in &poly {
            m.positions.push(w(*p));
            m.normals.push(n);
        }
        for t in tris {
            m.indices.push([base + t[0] as u32, base + t[1] as u32, base + t[2] as u32]);
            m.tri_face.push(0);
        }
    }
    m
}

type Cache = HashMap<(String, SketchId), (Sketch, Arc<Mesh>)>;

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(HashMap::new());
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

/// The visible sketch whose lines pass within `tol` points of `p` (viewport-local points).
pub fn pick(app: &EasyCadApp, p: glam::Vec2, size: glam::Vec2, tol: f32) -> Option<SketchId> {
    let mut best: Option<(f32, SketchId)> = None;
    for (id, mesh) in draw_list(app) {
        for e in &mesh.edges {
            let pts: Vec<Option<glam::Vec2>> =
                e.points.iter().map(|q| app.camera.project(glam::Vec3::from(*q), size)).collect();
            for w in pts.windows(2) {
                let (Some(a), Some(b)) = (w[0], w[1]) else { continue };
                let ab = b - a;
                let t = if ab.length_squared() < 1e-9 { 0.0 } else { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
                let d = (a + ab * t).distance(p);
                if d <= tol && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, id));
                }
            }
        }
    }
    best.map(|b| b.1)
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

#[cfg(test)]
mod construction_tests {
    use super::*;

    #[test]
    fn construction_is_dashed_in_the_mesh_and_never_filled() {
        let mut sk = Sketch::new("s", easycad_model::geom::Plane::XY);
        sk.rect(DVec2::ZERO, DVec2::new(10.0, 10.0));
        let solid = sketch_mesh(&sk);
        assert_eq!(solid.edges.len(), 4);
        assert!(!solid.indices.is_empty());
        for i in 0..4 {
            sk.set_construction(easycad_model::sketch::EntityId(i), true);
        }
        let m = sketch_mesh(&sk);
        assert!(m.edges.len() > 4, "dashed into pieces");
        assert!(m.indices.is_empty(), "construction forms no profile");
    }
}
