//! Planar regions: the faces of the arrangement of a sketch's curves.
//!
//! Every curve is split at all mutual intersections (and where another curve's
//! endpoint touches it), the pieces form a planar graph, and the bounded faces of
//! that graph are the regions. Dangling pieces bound nothing and are dropped; a
//! component lying inside a face becomes a hole of it.

use crate::geom::{Loop2, Segment2};
use crate::sketch::{arc_sweep, loop_area, point_in_polygon, polygonize, reverse_segment, seg_ends, signed_area, CHAIN_TOL};
use glam::DVec2;
use std::f64::consts::{PI, TAU};

/// One bounded face of a sketch: CCW outer loop, CW holes.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub outer: Loop2,
    pub holes: Vec<Loop2>,
    /// A point well inside the region (not in a hole).
    pub sample: DVec2,
    pub area: f64,
    /// Part of what `ProfileSel::All` extrudes (the even/odd fill of the sketch's closed
    /// loops, as before regions existed). Other regions are only used when picked.
    pub filled: bool,
}

impl Region {
    pub fn profile(&self) -> crate::geom::Profile {
        crate::geom::Profile { outer: self.outer.clone(), holes: self.holes.clone() }
    }

    pub fn contains(&self, p: DVec2) -> bool {
        point_in_polygon(p, &fine(&self.outer)) && !self.holes.iter().any(|h| point_in_polygon(p, &fine(h)))
    }
}

/// Fine polyline of a loop, for containment tests.
fn fine(l: &Loop2) -> Vec<DVec2> {
    let mut out = Vec::new();
    for s in l {
        let n = match s {
            Segment2::Line { .. } => 1,
            Segment2::Arc { center, start, end, ccw } => ((arc_sweep(*center, *start, *end, *ccw) / 0.01).ceil() as usize).max(2),
            Segment2::Circle { .. } => 600,
        };
        for i in 0..n {
            out.push(crate::sketch::segment_point(s, i as f64 / n as f64));
        }
    }
    out
}

/// The region containing `p`, if any.
pub fn region_at(regions: &[Region], p: DVec2) -> Option<usize> {
    regions.iter().position(|r| r.contains(p))
}

/// Indices of the regions a `ProfileSel` picks (`All` = the filled ones).
pub fn selected_regions(sel: &crate::feature::ProfileSel, sk: &crate::sketch::Sketch, regions: &[Region]) -> Vec<usize> {
    use crate::feature::ProfileSel;
    let mut v: Vec<usize> = match sel {
        ProfileSel::All => (0..regions.len()).filter(|&i| regions[i].filled).collect(),
        ProfileSel::Containing(pts) => pts.iter().filter_map(|p| region_at(regions, *p)).collect(),
        ProfileSel::Indices(ix) => {
            let profs = sk.profiles().profiles;
            (0..regions.len())
                .filter(|&i| {
                    regions[i].filled
                        && ix.iter().any(|&k| {
                            profs.get(k).is_some_and(|p| {
                                point_in_polygon(regions[i].sample, &fine(&p.outer)) && !p.holes.iter().any(|h| point_in_polygon(regions[i].sample, &fine(h)))
                            })
                        })
                })
                .collect()
        }
    };
    v.sort_unstable();
    v.dedup();
    v
}

/// The selection that picks exactly these regions (by interior point).
pub fn selection_of(regions: &[Region], idx: &[usize]) -> crate::feature::ProfileSel {
    crate::feature::ProfileSel::Containing(idx.iter().filter_map(|&i| regions.get(i)).map(|r| r.sample).collect())
}

// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Curve {
    Line { a: DVec2, b: DVec2 },
    /// CCW arc from angle `a0` sweeping `sw` (a full circle has `sw == TAU`).
    Arc { c: DVec2, r: f64, a0: f64, sw: f64 },
}

fn norm_ang(a: f64) -> f64 {
    let mut a = a % TAU;
    if a < 0.0 {
        a += TAU;
    }
    a
}

fn curve_of(s: &Segment2, tol: f64) -> Option<Curve> {
    match *s {
        Segment2::Line { a, b } => (a.distance(b) > tol).then_some(Curve::Line { a, b }),
        Segment2::Circle { center, radius } => (radius > tol).then_some(Curve::Arc { c: center, r: radius, a0: 0.0, sw: TAU }),
        Segment2::Arc { center, start, end, ccw } => {
            let r = (start - center).length();
            if r <= tol {
                return None;
            }
            let (s0, e0) = if ccw { (start, end) } else { (end, start) };
            Some(Curve::Arc { c: center, r, a0: (s0 - center).to_angle(), sw: arc_sweep(center, s0, e0, true) })
        }
    }
}

impl Curve {
    fn is_full(&self) -> bool {
        matches!(self, Curve::Arc { sw, .. } if *sw >= TAU - 1e-12)
    }
    fn ends(&self) -> Vec<DVec2> {
        match *self {
            Curve::Line { a, b } => vec![a, b],
            Curve::Arc { c, r, a0, sw } => {
                if self.is_full() {
                    vec![c + DVec2::from_angle(a0) * r]
                } else {
                    vec![c + DVec2::from_angle(a0) * r, c + DVec2::from_angle(a0 + sw) * r]
                }
            }
        }
    }
    /// Parameter of `p` along the curve when it lies on it (within `tol`).
    /// Lines: arc length from `a`. Arcs: angle from the start.
    fn param(&self, p: DVec2, tol: f64) -> Option<f64> {
        match *self {
            Curve::Line { a, b } => {
                let d = b - a;
                let len = d.length();
                let u = d / len;
                let t = (p - a).dot(u);
                let off = (p - a).perp_dot(u).abs();
                (off <= tol && t >= -tol && t <= len + tol).then_some(t.clamp(0.0, len))
            }
            Curve::Arc { c, r, a0, sw } => {
                if ((p - c).length() - r).abs() > tol {
                    return None;
                }
                let da = norm_ang((p - c).to_angle() - a0);
                if self.is_full() {
                    return Some(da);
                }
                let at = tol / r;
                if da <= sw + at {
                    Some(da.min(sw))
                } else if da >= TAU - at {
                    Some(0.0)
                } else {
                    None
                }
            }
        }
    }
    fn at(&self, t: f64) -> DVec2 {
        match *self {
            Curve::Line { a, b } => a + (b - a).normalize() * t,
            Curve::Arc { c, r, a0, .. } => c + DVec2::from_angle(a0 + t) * r,
        }
    }
}

fn intersections(a: &Curve, b: &Curve, tol: f64, out: &mut Vec<DVec2>) {
    use Curve::*;
    match (*a, *b) {
        (Line { a: p, b: q }, Line { a: r, b: s }) => {
            let d1 = q - p;
            let d2 = s - r;
            let den = d1.perp_dot(d2);
            if den.abs() < 1e-12 * d1.length() * d2.length() {
                return;
            }
            let t = (r - p).perp_dot(d2) / den;
            out.push(p + d1 * t);
        }
        (Line { a: p, b: q }, Arc { c, r, .. }) | (Arc { c, r, .. }, Line { a: p, b: q }) => {
            let d = (q - p).normalize();
            let foot_t = (c - p).dot(d);
            let foot = p + d * foot_t;
            let h = foot.distance(c);
            if h > r + tol {
                return;
            }
            if (h - r).abs() <= tol || h >= r {
                out.push(foot);
                return;
            }
            let w = (r * r - h * h).sqrt();
            out.push(foot + d * w);
            out.push(foot - d * w);
        }
        (Arc { c: c1, r: r1, .. }, Arc { c: c2, r: r2, .. }) => {
            let d = c1.distance(c2);
            if d < 1e-12 {
                return;
            }
            if d > r1 + r2 + tol || d < (r1 - r2).abs() - tol {
                return;
            }
            let u = (c2 - c1) / d;
            let x = ((d * d + r1 * r1 - r2 * r2) / (2.0 * d)).clamp(-r1, r1);
            let h2 = r1 * r1 - x * x;
            let base = c1 + u * x;
            // near-tangent: one point
            if h2.sqrt() <= tol {
                out.push(base);
                return;
            }
            let h = h2.sqrt();
            out.push(base + u.perp() * h);
            out.push(base - u.perp() * h);
        }
    }
}

struct Edge {
    u: usize,
    v: usize,
    /// Forward geometry from `u` to `v`.
    seg: Segment2,
    /// Is the full circle through a single vertex (u == v)?
    full: bool,
}

/// Half-edge `2*e` is forward, `2*e + 1` is reversed.
struct Graph {
    verts: Vec<DVec2>,
    edges: Vec<Edge>,
}

fn build_graph(curves: &[Curve], tol: f64) -> Graph {
    // candidate split points
    let mut cand: Vec<DVec2> = Vec::new();
    for c in curves {
        cand.extend(c.ends());
    }
    for i in 0..curves.len() {
        for j in i + 1..curves.len() {
            intersections(&curves[i], &curves[j], tol, &mut cand);
        }
    }
    // vertices: cluster candidates
    let mut verts: Vec<DVec2> = Vec::new();
    let vid = |p: DVec2, verts: &mut Vec<DVec2>| -> usize {
        if let Some(i) = verts.iter().position(|q| q.distance(p) <= tol * 2.0) {
            return i;
        }
        verts.push(p);
        verts.len() - 1
    };
    let mut edges: Vec<Edge> = Vec::new();
    // per curve: (param, vertex)
    let mut per: Vec<Vec<(f64, usize)>> = Vec::new();
    for c in curves {
        let mut v: Vec<(f64, usize)> = Vec::new();
        for p in &cand {
            if let Some(t) = c.param(*p, tol * 2.0) {
                v.push((t, usize::MAX));
                let last = v.len() - 1;
                // vertex position: snap to an existing vertex or the candidate itself
                v[last].1 = vid(*p, &mut verts);
            }
        }
        if c.is_full() && v.is_empty() {
            let p = c.at(0.0);
            v.push((0.0, vid(p, &mut verts)));
        }
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        per.push(v);
    }
    for (c, v) in curves.iter().zip(&per) {
        // dedupe consecutive same vertices
        let mut seq: Vec<(f64, usize)> = Vec::new();
        for &(t, id) in v {
            if seq.last().is_none_or(|l| l.1 != id) {
                seq.push((t, id));
            }
        }
        if c.is_full() {
            if seq.len() > 1 && seq.first().unwrap().1 == seq.last().unwrap().1 {
                seq.pop();
            }
            let n = seq.len();
            for k in 0..n {
                let (t0, u) = seq[k];
                let (t1, v2) = if k + 1 < n { seq[k + 1] } else { (seq[0].0 + TAU, seq[0].1) };
                push_edge(&mut edges, &verts, c, t0, t1, u, v2, n == 1);
            }
        } else {
            for k in 0..seq.len().saturating_sub(1) {
                push_edge(&mut edges, &verts, c, seq[k].0, seq[k + 1].0, seq[k].1, seq[k + 1].1, false);
            }
        }
    }
    // dedupe coincident edges
    let mut keep: Vec<Edge> = Vec::new();
    for e in edges {
        let mid = mid_of(&e);
        let dup = keep.iter().any(|k| {
            if k.full != e.full {
                return false;
            }
            let same = k.u == e.u && k.v == e.v;
            let rev = k.u == e.v && k.v == e.u;
            (same || rev) && mid_of(k).distance(mid) <= tol * 4.0
        });
        if !dup {
            keep.push(e);
        }
    }
    Graph { verts, edges: keep }
}

fn mid_of(e: &Edge) -> DVec2 {
    crate::sketch::segment_point(&e.seg, 0.5)
}

#[allow(clippy::too_many_arguments)]
fn push_edge(edges: &mut Vec<Edge>, verts: &[DVec2], c: &Curve, t0: f64, t1: f64, u: usize, v: usize, full: bool) {
    if !full && u == v {
        return;
    }
    let seg = match *c {
        Curve::Line { .. } => Segment2::Line { a: verts[u], b: verts[v] },
        Curve::Arc { c: ctr, .. } => Segment2::Arc { center: ctr, start: verts[u], end: verts[v], ccw: true },
    };
    let _ = (t0, t1);
    edges.push(Edge { u, v, seg, full });
}

/// Direction and signed curvature of half-edge `h` at its start.
fn leave(g: &Graph, h: usize) -> (f64, f64) {
    let e = &g.edges[h / 2];
    let fwd = h % 2 == 0;
    match e.seg {
        Segment2::Line { a, b } => {
            let d = if fwd { b - a } else { a - b };
            (d.to_angle(), 0.0)
        }
        Segment2::Arc { center, start, end, .. } => {
            let (p, k) = if fwd { (start, 1.0) } else { (end, -1.0) };
            let rad = p - center;
            let t = rad.perp() * k;
            let _ = end;
            (t.to_angle(), k / rad.length())
        }
        Segment2::Circle { .. } => unreachable!(),
    }
}

fn start_of(g: &Graph, h: usize) -> usize {
    let e = &g.edges[h / 2];
    if h % 2 == 0 {
        e.u
    } else {
        e.v
    }
}

fn half_seg(g: &Graph, h: usize) -> Segment2 {
    let e = &g.edges[h / 2];
    if h % 2 == 0 {
        e.seg
    } else {
        reverse_segment(&e.seg)
    }
}

fn wrap(a: f64) -> f64 {
    if a <= -PI + 1e-9 {
        a + TAU
    } else {
        a
    }
}

/// All regions of a set of segments.
pub fn regions_from_segments(segs: &[Segment2], tol: f64) -> Vec<Region> {
    let curves: Vec<Curve> = segs.iter().filter_map(|s| curve_of(s, tol)).collect();
    if curves.is_empty() {
        return Vec::new();
    }
    let g = build_graph(&curves, tol);
    let ne = g.edges.len();
    let mut alive = vec![true; ne];
    let nv = g.verts.len();

    // outgoing half-edges per vertex, sorted ccw
    let sorted = |alive: &[bool]| -> Vec<Vec<usize>> {
        let mut out: Vec<Vec<usize>> = vec![Vec::new(); nv];
        for e in 0..ne {
            if alive[e] {
                out[g.edges[e].u].push(2 * e);
                out[g.edges[e].v].push(2 * e + 1);
            }
        }
        for (v, list) in out.iter_mut().enumerate() {
            let _ = v;
            list.sort_by(|&x, &y| {
                let (ax, kx) = leave(&g, x);
                let (ay, ky) = leave(&g, y);
                let (ax, ay) = (wrap(ax), wrap(ay));
                if (ax - ay).abs() > 1e-9 {
                    ax.total_cmp(&ay)
                } else {
                    kx.total_cmp(&ky)
                }
            });
        }
        out
    };

    // faces as lists of half-edges
    let trace = |alive: &[bool], out: &[Vec<usize>]| -> Vec<Vec<usize>> {
        let mut seen = vec![false; 2 * ne];
        let mut faces = Vec::new();
        for e in 0..ne {
            for h0 in [2 * e, 2 * e + 1] {
                if !alive[e] || seen[h0] {
                    continue;
                }
                let mut face = Vec::new();
                let mut h = h0;
                loop {
                    seen[h] = true;
                    face.push(h);
                    // at the end vertex, the next half-edge is just clockwise of the twin
                    let tw = h ^ 1;
                    let v = start_of(&g, tw);
                    let list = &out[v];
                    let i = list.iter().position(|&x| x == tw).unwrap();
                    let nxt = list[(i + list.len() - 1) % list.len()];
                    h = nxt;
                    if h == h0 || face.len() > 4 * ne + 4 {
                        break;
                    }
                }
                faces.push(face);
            }
        }
        faces
    };

    // prune dangling pieces and bridges until stable
    loop {
        let mut changed = false;
        let mut deg = vec![0usize; nv];
        for e in 0..ne {
            if alive[e] {
                deg[g.edges[e].u] += 1;
                deg[g.edges[e].v] += 1;
            }
        }
        for e in 0..ne {
            if alive[e] && !g.edges[e].full && (deg[g.edges[e].u] == 1 || deg[g.edges[e].v] == 1) {
                alive[e] = false;
                changed = true;
                break;
            }
        }
        if changed {
            continue;
        }
        let out = sorted(&alive);
        let mut face_of = vec![usize::MAX; 2 * ne];
        let faces = trace(&alive, &out);
        for (fi, f) in faces.iter().enumerate() {
            for &h in f {
                face_of[h] = fi;
            }
        }
        for e in 0..ne {
            if alive[e] && face_of[2 * e] == face_of[2 * e + 1] {
                alive[e] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let out = sorted(&alive);
    let faces = trace(&alive, &out);

    // connected components of the surviving graph
    let mut comp = vec![usize::MAX; nv];
    let mut ncomp = 0;
    for s in 0..nv {
        if comp[s] != usize::MAX || out[s].is_empty() {
            continue;
        }
        let mut stack = vec![s];
        comp[s] = ncomp;
        while let Some(v) = stack.pop() {
            for &h in &out[v] {
                let e = &g.edges[h / 2];
                for w in [e.u, e.v] {
                    if comp[w] == usize::MAX {
                        comp[w] = ncomp;
                        stack.push(w);
                    }
                }
            }
        }
        ncomp += 1;
    }

    struct Face {
        comp: usize,
        outer: Loop2,
        holes: Vec<Loop2>,
    }
    let mut bounded: Vec<Face> = Vec::new();
    // outer boundary cycles (clockwise walks) of each component
    let mut comp_bounds: Vec<Vec<Loop2>> = vec![Vec::new(); ncomp];
    for f in &faces {
        let c = comp[start_of(&g, f[0])];
        let cycles = split_cycles(&g, f);
        let area: f64 = cycles.iter().map(|c| c.1).sum();
        if area > tol * tol {
            let mut outer = None;
            let mut holes = Vec::new();
            for (cy, a) in cycles {
                if a > 0.0 && outer.is_none() {
                    outer = Some(cy);
                } else {
                    holes.push(cy);
                }
            }
            if let Some(outer) = outer {
                bounded.push(Face { comp: c, outer, holes });
            }
        } else {
            for (cy, a) in cycles {
                if a < 0.0 {
                    comp_bounds[c].push(cy);
                }
            }
        }
    }
    // islands: each component's outside boundary becomes a hole of the smallest
    // bounded face of another component that contains it
    let polys: Vec<Vec<DVec2>> = bounded.iter().map(|f| polygonize(&f.outer)).collect();
    let areas: Vec<f64> = bounded.iter().map(|f| loop_area(&f.outer)).collect();
    let mut extra: Vec<(usize, Loop2)> = Vec::new();
    for (c, cycles) in comp_bounds.iter().enumerate() {
        for cy in cycles {
            let p = seg_ends(&cy[0]).map(|e| e.0).unwrap_or(DVec2::ZERO);
            let p = match cy[0] {
                Segment2::Circle { center, radius } => center + DVec2::new(radius, 0.0),
                _ => p,
            };
            let best = (0..bounded.len())
                .filter(|&i| bounded[i].comp != c && point_in_polygon(p, &polys[i]))
                .min_by(|&a, &b| areas[a].total_cmp(&areas[b]));
            if let Some(i) = best {
                extra.push((i, cy.clone()));
            }
        }
    }
    for (i, cy) in extra {
        bounded[i].holes.push(cy);
    }
    let mut regions: Vec<Region> = bounded
        .into_iter()
        .map(|f| {
            let area = loop_area(&f.outer).abs() - f.holes.iter().map(|h| loop_area(h).abs()).sum::<f64>();
            let sample = sample_point(&f.outer, &f.holes);
            Region { outer: f.outer, holes: f.holes, sample, area, filled: false }
        })
        .collect();
    regions.retain(|r| r.area > tol * tol);
    let old = crate::sketch::profiles_from_segments(segs, tol).profiles;
    for r in &mut regions {
        r.filled = old.iter().any(|p| point_in_polygon(r.sample, &fine(&p.outer)) && !p.holes.iter().any(|h| point_in_polygon(r.sample, &fine(h))));
    }
    // deterministic order: by sample point (y then x)
    regions.sort_by(|a, b| a.sample.y.total_cmp(&b.sample.y).then(a.sample.x.total_cmp(&b.sample.x)));
    regions
}

/// Split a face walk at repeated vertices into simple cycles; a lone closed circle
/// becomes `Segment2::Circle`.
fn split_cycles(g: &Graph, walk: &[usize]) -> Vec<(Loop2, f64)> {
    let mut cycles: Vec<(Loop2, f64)> = Vec::new();
    let mut stack: Vec<(usize, Segment2)> = Vec::new(); // (start vertex, segment)
    for &h in walk {
        let u = start_of(g, h);
        let e = &g.edges[h / 2];
        let v = if h % 2 == 0 { e.v } else { e.u };
        if e.full {
            if let Segment2::Arc { center, start, .. } = e.seg {
                let r = (start - center).length();
                let sg = Segment2::Circle { center, radius: r };
                let a = PI * r * r;
                cycles.push((vec![sg], if h % 2 == 0 { a } else { -a }));
            }
            continue;
        }
        stack.push((u, half_seg(g, h)));
        if let Some(pos) = stack.iter().position(|s| s.0 == v) {
            let cyc: Vec<Segment2> = stack.drain(pos..).map(|s| s.1).collect();
            let a = loop_area(&cyc);
            cycles.push((cyc, a));
        }
    }
    cycles
}

/// Interior point: widest scanline interval over a few horizontal scans.
fn sample_point(outer: &Loop2, holes: &[Loop2]) -> DVec2 {
    let polys: Vec<Vec<DVec2>> = std::iter::once(outer).chain(holes.iter()).map(fine).collect();
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for p in &polys[0] {
        lo = lo.min(p.y);
        hi = hi.max(p.y);
    }
    let mut best: Option<(f64, DVec2)> = None;
    let n = 19;
    for k in 0..n {
        let y = lo + (hi - lo) * (k as f64 + 0.5) / n as f64;
        let mut xs: Vec<f64> = Vec::new();
        for poly in &polys {
            let m = poly.len();
            for i in 0..m {
                let (a, b) = (poly[i], poly[(i + 1) % m]);
                if (a.y > y) != (b.y > y) {
                    xs.push(a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x));
                }
            }
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        for pair in xs.chunks_exact(2) {
            let w = pair[1] - pair[0];
            if best.is_none_or(|b| w > b.0) {
                best = Some((w, DVec2::new((pair[0] + pair[1]) / 2.0, y)));
            }
        }
    }
    match best {
        Some((_, p)) => p,
        None => {
            let a = signed_area(&polys[0]);
            let _ = a;
            polys[0].iter().copied().sum::<DVec2>() / polys[0].len() as f64
        }
    }
}

/// Default tolerance.
pub const TOL: f64 = CHAIN_TOL;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Plane;
    use crate::sketch::Sketch;

    fn v(x: f64, y: f64) -> DVec2 {
        DVec2::new(x, y)
    }
    fn areas(s: &Sketch) -> Vec<f64> {
        let mut a: Vec<f64> = s.regions().iter().map(|r| r.area).collect();
        a.sort_by(|x, y| x.total_cmp(y));
        a
    }
    fn ring() -> Sketch {
        let mut s = Sketch::new("r", Plane::XY);
        s.circle(v(0.0, 0.0), 9.8);
        s.circle(v(0.0, 0.0), 11.4);
        s
    }

    #[test]
    fn concentric_circles_give_ring_and_disc() {
        let s = ring();
        let a = areas(&s);
        assert_eq!(a.len(), 2);
        assert!((a[1] - PI * 9.8 * 9.8).abs() < 1e-6);
        assert!((a[0] - PI * (11.4f64.powi(2) - 9.8f64.powi(2))).abs() < 1e-6, "{a:?}");
        let ring = s.regions().into_iter().find(|r| !r.holes.is_empty()).unwrap();
        assert!(ring.contains(ring.sample));
        let rr = ring.sample.length();
        assert!(rr > 9.8 && rr < 11.4);
    }

    #[test]
    fn ring_with_radial_lines_gives_sectors() {
        let mut s = ring();
        // two radial lines through the ring, ending beyond it (ignored outside)
        for ang in [0.0f64, 120.0] {
            let d = v(ang.to_radians().cos(), ang.to_radians().sin());
            s.line(d * 9.0, d * 13.0);
        }
        let a = areas(&s);
        // disc is not split (lines start inside it: dangling there), ring cut in two
        assert_eq!(a.len(), 3, "{a:?}");
        let ring_a = PI * (11.4f64.powi(2) - 9.8f64.powi(2));
        assert!((a[0] + a[1] - ring_a).abs() < 1e-6, "{a:?}");
        assert!((a[2] - PI * 9.8 * 9.8).abs() < 1e-6);
    }

    #[test]
    fn ring_with_three_radials_at_120() {
        let mut s = ring();
        for ang in [0.0f64, 120.0, 240.0] {
            let d = v(ang.to_radians().cos(), ang.to_radians().sin());
            s.line(d * 9.0, d * 12.0);
        }
        let a = areas(&s);
        assert_eq!(a.len(), 4, "{a:?}");
        let ring_a = PI * (11.4f64.powi(2) - 9.8f64.powi(2));
        for x in &a[..3] {
            assert!((x - ring_a / 3.0).abs() < 1e-6, "{a:?}");
        }
    }

    #[test]
    fn overlapping_circles_give_three_regions() {
        let mut s = Sketch::new("o", Plane::XY);
        s.circle(v(0.0, 0.0), 5.0);
        s.circle(v(6.0, 0.0), 5.0);
        let a = areas(&s);
        assert_eq!(a.len(), 3, "{a:?}");
        let total = PI * 25.0 * 2.0 - a[0];
        assert!((a.iter().sum::<f64>() - total).abs() < 1e-6);
    }

    #[test]
    fn t_junction_and_dangling() {
        let mut s = Sketch::new("t", Plane::XY);
        s.rect(v(0.0, 0.0), v(10.0, 10.0));
        // T-junction: a line whose endpoint lies on the right edge, splitting the square
        s.line(v(0.0, 5.0), v(10.0, 5.0));
        // dangling line sticking out and one stopping inside
        s.line(v(10.0, 2.0), v(14.0, 2.0));
        s.line(v(2.0, 0.0), v(2.0, 3.0));
        let a = areas(&s);
        assert_eq!(a.len(), 2, "{a:?}");
        assert!((a[0] - 50.0).abs() < 1e-6 && (a[1] - 50.0).abs() < 1e-6);
    }

    #[test]
    fn tangent_circles_and_coincident_curves() {
        let mut s = Sketch::new("tan", Plane::XY);
        s.circle(v(0.0, 0.0), 5.0);
        s.circle(v(10.0, 0.0), 5.0); // externally tangent
        s.circle(v(0.0, 0.0), 5.0); // drawn twice
        s.circle(v(0.0, 0.0), 2.0); // inside
        let a = areas(&s);
        assert_eq!(a.len(), 3, "{a:?}");
        // inner tangent
        let mut s = Sketch::new("tan2", Plane::XY);
        s.circle(v(0.0, 0.0), 5.0);
        s.circle(v(2.0, 0.0), 3.0);
        let a = areas(&s);
        assert_eq!(a.len(), 2, "{a:?}");
        assert!((a[0] - PI * 9.0).abs() < 1e-6);
        assert!((a[1] - PI * 16.0).abs() < 1e-6, "{a:?}");
        // line tangent to a circle bounds nothing
        let mut s = Sketch::new("lt", Plane::XY);
        s.circle(v(0.0, 0.0), 5.0);
        s.line(v(-8.0, 5.0), v(8.0, 5.0));
        assert_eq!(areas(&s).len(), 1);
    }

    #[test]
    fn island_becomes_a_hole() {
        let mut s = Sketch::new("i", Plane::XY);
        s.rect(v(0.0, 0.0), v(20.0, 20.0));
        s.rect(v(5.0, 5.0), v(8.0, 8.0));
        // a bridge joining island and frame
        s.line(v(8.0, 6.0), v(20.0, 6.0));
        let a = areas(&s);
        assert_eq!(a.len(), 2, "{a:?}");
        assert!((a[1] - (400.0 - 9.0)).abs() < 1e-6, "{a:?}");
    }

    #[test]
    fn regions_union_matches_profiles() {
        let mut s = ring();
        s.circle(v(30.0, 0.0), 4.0);
        s.rect(v(-30.0, -30.0), v(-20.0, -20.0));
        let prof: f64 = s.profiles().profiles.iter().map(|p| crate::sketch::loop_area(&p.outer).abs() - p.holes.iter().map(|h| crate::sketch::loop_area(h).abs()).sum::<f64>()).sum();
        let reg: f64 = s.regions().iter().filter(|r| r.filled).map(|r| r.area).sum();
        assert!((prof - reg).abs() < 1e-6, "{prof} {reg} {:?}", areas(&s));
    }
}
