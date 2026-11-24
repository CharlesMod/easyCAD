//! Ray picking that returns **every** hit under the cursor, sorted front to back,
//! for a pick stack UI (Tab / Alt+wheel cycles through faces and edges).
//!
//! Faces are tested against a CPU BVH over the display triangles. Edges are tested
//! against their polylines with a tolerance that the caller defines in world units
//! as a function of depth (a pixel radius in perspective grows linearly with
//! distance: `tol(t) = constant + per_unit_depth * t`).

use std::collections::HashMap;

use glam::Vec3;
use occt::Mesh;

use crate::feature::BodyId;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitKind {
    Face(u32),
    Edge(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub body: BodyId,
    pub kind: HitKind,
    /// Distance along the (normalised) ray.
    pub t: f32,
    pub point: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeTolerance {
    pub constant: f32,
    pub per_unit_depth: f32,
}

impl EdgeTolerance {
    /// Orthographic: fixed world-space tolerance.
    pub fn ortho(tol: f32) -> Self {
        EdgeTolerance { constant: tol, per_unit_depth: 0.0 }
    }
    /// Perspective: `pixels` at a vertical FOV and viewport height.
    pub fn perspective(pixels: f32, fov_y_rad: f32, viewport_h: f32) -> Self {
        EdgeTolerance { constant: 0.0, per_unit_depth: pixels * 2.0 * (fov_y_rad / 2.0).tan() / viewport_h }
    }
    pub fn at(&self, t: f32) -> f32 {
        self.constant + self.per_unit_depth * t.max(0.0)
    }
}

#[derive(Clone, Copy, Debug)]
struct Aabb {
    min: Vec3,
    max: Vec3,
}

impl Aabb {
    fn empty() -> Self {
        Aabb { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) }
    }
    fn grow(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }
    fn merge(&mut self, o: &Aabb) {
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }
    /// Slab test; returns entry distance.
    fn ray(&self, o: Vec3, inv: Vec3, pad: f32) -> Option<f32> {
        let t1 = (self.min - Vec3::splat(pad) - o) * inv;
        let t2 = (self.max + Vec3::splat(pad) - o) * inv;
        let tmin = t1.min(t2).max_element();
        let tmax = t1.max(t2).min_element();
        (tmax >= tmin.max(0.0)).then_some(tmin.max(0.0))
    }
}

enum Node {
    Leaf { bb: Aabb, start: u32, len: u32 },
    Inner { bb: Aabb, left: u32, right: u32 },
}

/// BVH over one body's triangles and edge segments.
pub struct BodyPicker {
    pub body: BodyId,
    tris: Vec<[Vec3; 3]>,
    tri_face: Vec<u32>,
    order: Vec<u32>,
    nodes: Vec<Node>,
    segs: Vec<(Vec3, Vec3, u32)>,
    seg_bb: Aabb,
    /// Faces each edge borders, inferred from shared mesh vertices (edge id -> faces).
    /// Edges missing here have unknown adjacency.
    edge_faces: HashMap<u32, Vec<u32>>,
    /// Measured tessellation sag: how far a tessellated face may sit in front of
    /// the exact surface (and so the exact edges on it).
    defl: f32,
}

const LEAF: usize = 4;

impl BodyPicker {
    pub fn new(body: BodyId, mesh: &Mesh) -> Self {
        let p = |i: u32| Vec3::from_array(mesh.positions[i as usize]);
        let tris: Vec<[Vec3; 3]> = mesh.indices.iter().map(|t| [p(t[0]), p(t[1]), p(t[2])]).collect();
        let mut order: Vec<u32> = (0..tris.len() as u32).collect();
        let mut nodes = Vec::new();
        if !tris.is_empty() {
            build(&tris, &mut order, 0, tris.len(), &mut nodes);
        }
        let mut segs = Vec::new();
        let mut seg_bb = Aabb::empty();
        for e in &mesh.edges {
            for w in e.points.windows(2) {
                let (a, b) = (Vec3::from_array(w[0]), Vec3::from_array(w[1]));
                seg_bb.grow(a);
                seg_bb.grow(b);
                segs.push((a, b, e.edge));
            }
        }
        let (edge_faces, defl) = adjacency_and_sag(mesh);
        BodyPicker { body, tris, tri_face: mesh.tri_face.clone(), order, nodes, segs, seg_bb, edge_faces, defl }
    }

    /// Faces the edge borders, when known.
    fn faces_of_edge(&self, edge: u32) -> Option<&[u32]> {
        self.edge_faces.get(&edge).map(|v| v.as_slice())
    }

    /// All hits on this body (unsorted).
    pub fn pick(&self, origin: Vec3, dir: Vec3, tol: EdgeTolerance, out: &mut Vec<Hit>) {
        let d = dir.normalize();
        let inv = d.recip();
        // faces
        if !self.nodes.is_empty() {
            let mut stack = vec![self.nodes.len() as u32 - 1];
            while let Some(n) = stack.pop() {
                match &self.nodes[n as usize] {
                    Node::Leaf { bb, start, len } => {
                        if bb.ray(origin, inv, 0.0).is_none() {
                            continue;
                        }
                        for k in *start..start + len {
                            let ti = self.order[k as usize] as usize;
                            if let Some(t) = ray_tri(origin, d, &self.tris[ti]) {
                                out.push(Hit { body: self.body, kind: HitKind::Face(self.tri_face[ti]), t, point: origin + d * t });
                            }
                        }
                    }
                    Node::Inner { bb, left, right } => {
                        if bb.ray(origin, inv, 0.0).is_some() {
                            stack.push(*left);
                            stack.push(*right);
                        }
                    }
                }
            }
        }
        // edges (closest approach per segment, within the depth-dependent tolerance)
        if !self.segs.is_empty() {
            let far = self.seg_bb.ray(origin, inv, tol.at(1e6).min(1e3));
            if far.is_some() {
                for (a, b, id) in &self.segs {
                    if let Some((t, p, dist)) = ray_segment(origin, d, *a, *b) {
                        if dist <= tol.at(t) {
                            out.push(Hit { body: self.body, kind: HitKind::Edge(*id), t, point: p });
                        }
                    }
                }
            }
        }
    }
}

/// Infer edge -> bordering faces from mesh vertices shared by an edge polyline and
/// the triangles of a face, and estimate the tessellation sag from the vertex
/// normals (`L * angle / 8` per triangle side, the sagitta of a circular chord).
fn adjacency_and_sag(mesh: &Mesh) -> (HashMap<u32, Vec<u32>>, f32) {
    let mut bb = Aabb::empty();
    for p in &mesh.positions {
        bb.grow(Vec3::from_array(*p));
    }
    let diag = if mesh.positions.is_empty() { 0.0 } else { (bb.max - bb.min).length() };
    let cell = (diag * 1e-5).max(1e-6);
    let key = |p: [f32; 3]| [(p[0] / cell).round() as i64, (p[1] / cell).round() as i64, (p[2] / cell).round() as i64];
    let mut at: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    let mut sag = 0.0f32;
    for (ti, t) in mesh.indices.iter().enumerate() {
        let f = mesh.tri_face.get(ti).copied().unwrap_or(0);
        for k in 0..3 {
            let (i, j) = (t[k] as usize, t[(k + 1) % 3] as usize);
            let v = at.entry(key(mesh.positions[i])).or_default();
            if !v.contains(&f) {
                v.push(f);
            }
            if let (Some(ni), Some(nj)) = (mesh.normals.get(i), mesh.normals.get(j)) {
                let ang = Vec3::from_array(*ni).dot(Vec3::from_array(*nj)).clamp(-1.0, 1.0).acos();
                let l = Vec3::from_array(mesh.positions[i]).distance(Vec3::from_array(mesh.positions[j]));
                sag = sag.max(l * ang / 8.0);
            }
        }
    }
    let mut out = HashMap::new();
    for e in &mesh.edges {
        let mut faces: Option<Vec<u32>> = None;
        for p in &e.points {
            let here = at.get(&key(*p)).cloned().unwrap_or_default();
            faces = Some(match faces {
                None => here,
                Some(f) => f.into_iter().filter(|x| here.contains(x)).collect(),
            });
        }
        if let Some(f) = faces.filter(|f| !f.is_empty()) {
            out.insert(e.edge, f);
        }
    }
    (out, sag.min(diag * 0.05))
}

fn centroid(t: &[Vec3; 3]) -> Vec3 {
    (t[0] + t[1] + t[2]) / 3.0
}

fn build(tris: &[[Vec3; 3]], order: &mut [u32], start: usize, end: usize, nodes: &mut Vec<Node>) -> u32 {
    let mut bb = Aabb::empty();
    let mut cb = Aabb::empty();
    for &i in &order[start..end] {
        for v in tris[i as usize] {
            bb.grow(v);
        }
        cb.grow(centroid(&tris[i as usize]));
    }
    if end - start <= LEAF {
        nodes.push(Node::Leaf { bb, start: start as u32, len: (end - start) as u32 });
        return nodes.len() as u32 - 1;
    }
    let ext = cb.max - cb.min;
    let axis = if ext.x >= ext.y && ext.x >= ext.z { 0 } else if ext.y >= ext.z { 1 } else { 2 };
    let mid = (start + end) / 2;
    order[start..end].select_nth_unstable_by(mid - start, |a, b| {
        centroid(&tris[*a as usize])[axis].total_cmp(&centroid(&tris[*b as usize])[axis])
    });
    let left = build(tris, order, start, mid, nodes);
    let right = build(tris, order, mid, end, nodes);
    let mut bb2 = bb;
    bb2.merge(&bb);
    nodes.push(Node::Inner { bb: bb2, left, right });
    nodes.len() as u32 - 1
}

/// Möller–Trumbore, two-sided.
fn ray_tri(o: Vec3, d: Vec3, t: &[Vec3; 3]) -> Option<f32> {
    let e1 = t[1] - t[0];
    let e2 = t[2] - t[0];
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    if !(-1e-6..=1.0 + 1e-6).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < -1e-6 || u + v > 1.0 + 1e-6 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t >= 0.0).then_some(t)
}

/// Closest approach between a ray and a segment: (ray t, point on segment, distance).
fn ray_segment(o: Vec3, d: Vec3, a: Vec3, b: Vec3) -> Option<(f32, Vec3, f32)> {
    let u = b - a;
    let w = o - a;
    let (aa, bb, cc, dd, ee) = (d.dot(d), d.dot(u), u.dot(u), d.dot(w), u.dot(w));
    let den = aa * cc - bb * bb;
    let mut s = if den.abs() > 1e-12 { (aa * ee - bb * dd) / den } else { 0.0 };
    s = s.clamp(0.0, 1.0);
    let q = a + u * s;
    let t = (q - o).dot(d) / aa;
    if t < 0.0 {
        return None;
    }
    let dist = (o + d * t).distance(q);
    Some((t, q, dist))
}

/// How far behind a face hit an edge hit may lie and still beat that face.
/// Edge known to border the face: a few pick radii plus the mesh sag (the
/// tessellated face leads the exact edge by up to the sag, and a grazing view
/// stretches the depth gap). Adjacency unknown: only a sliver, so an edge truly
/// behind other geometry never beats the front face.
fn edge_slack(tol: EdgeTolerance, t: f32, defl: f32, adjacent_known: bool) -> f32 {
    if adjacent_known {
        4.0 * tol.at(t) + 2.0 * defl + 1e-3
    } else {
        0.25 * tol.at(t) + 1e-3
    }
}

/// Picker over several bodies.
pub struct Picker {
    pub bodies: Vec<BodyPicker>,
}

impl Picker {
    pub fn new<'a>(meshes: impl IntoIterator<Item = (BodyId, &'a Mesh)>) -> Self {
        Picker { bodies: meshes.into_iter().map(|(id, m)| BodyPicker::new(id, m)).collect() }
    }

    /// Every face and edge along the ray, nearest first. Repeated hits of the same
    /// face/edge at (nearly) the same depth (shared triangle edges) are merged; a
    /// face crossed twice (front and back of a cylinder) appears twice.
    pub fn pick_all(&self, origin: Vec3, dir: Vec3, tol: EdgeTolerance) -> Vec<Hit> {
        let mut hits = Vec::new();
        for b in &self.bodies {
            b.pick(origin, dir, tol, &mut hits);
        }
        // Nearest first; an exact depth tie goes to the edge (edges sit on faces).
        hits.sort_by(|a, b| {
            a.t.total_cmp(&b.t).then_with(|| matches!(b.kind, HitKind::Edge(_)).cmp(&matches!(a.kind, HitKind::Edge(_))))
        });
        let mut out: Vec<Hit> = Vec::new();
        for h in hits {
            let dup = out.iter().rev().take_while(|o| h.t - o.t < 1e-3 + 1e-6 * h.t).any(|o| o.body == h.body && o.kind == h.kind);
            if !dup {
                out.push(h);
            }
        }
        // Edge priority: an edge hit moves in front of the nearest face hit it
        // outranks. It outranks a face of its own body when the edge borders that
        // face (or adjacency is unknown) and the face is not in front of it by
        // more than the slack (see `edge_slack`). The 3D depth of a tessellated
        // face can lead the exact edge by the mesh sag, so depth alone is not enough.
        let mut i = 0;
        while i < out.len() {
            if let HitKind::Edge(e) = out[i].kind {
                let eh = out[i];
                let bp = self.bodies.iter().find(|b| b.body == eh.body);
                let adj = bp.and_then(|b| b.faces_of_edge(e));
                let slack = edge_slack(tol, eh.t, bp.map_or(0.0, |b| b.defl), adj.is_some());
                let first = (0..i).find(|&j| {
                    let f = out[j];
                    match f.kind {
                        HitKind::Face(fid) => f.body == eh.body && eh.t - f.t <= slack && adj.is_none_or(|a| a.contains(&fid)),
                        HitKind::Edge(_) => false,
                    }
                });
                if let Some(j) = first {
                    let h = out.remove(i);
                    out.insert(j, h);
                }
            }
            i += 1;
        }
        out
    }

    pub fn pick_nearest(&self, origin: Vec3, dir: Vec3, tol: EdgeTolerance) -> Option<Hit> {
        self.pick_all(origin, dir, tol).into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::FeatureId;
    use occt::EdgePolyline;

    /// Two unit squares stacked along z (z=0 face 0, z=5 face 1) with an edge on each.
    fn quads() -> Mesh {
        let mut m = Mesh::default();
        for (f, z) in [(0u32, 0.0f32), (1, 5.0)] {
            let b = m.positions.len() as u32;
            m.positions.extend([[0.0, 0.0, z], [1.0, 0.0, z], [1.0, 1.0, z], [0.0, 1.0, z]]);
            m.normals.extend([[0.0, 0.0, 1.0]; 4]);
            m.indices.extend([[b, b + 1, b + 2], [b, b + 2, b + 3]]);
            m.tri_face.extend([f, f]);
            m.edges.push(EdgePolyline { edge: f, points: vec![[0.0, 0.0, z], [1.0, 0.0, z]] });
        }
        m
    }

    #[test]
    fn all_hits_front_to_back() {
        let m = quads();
        let id = BodyId::of(FeatureId(1));
        let p = Picker::new([(id, &m)]);
        // through the shared diagonal of both quads: one hit per face, not two
        let hits = p.pick_all(Vec3::new(0.5, 0.5, 10.0), Vec3::NEG_Z, EdgeTolerance::ortho(0.01));
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].kind, HitKind::Face(1));
        assert!((hits[0].t - 5.0).abs() < 1e-5);
        assert_eq!(hits[1].kind, HitKind::Face(0));
        // near the y=0 edges: edge hits come before the face at the same depth
        let hits = p.pick_all(Vec3::new(0.5, 0.02, 10.0), Vec3::NEG_Z, EdgeTolerance::ortho(0.05));
        let kinds: Vec<HitKind> = hits.iter().map(|h| h.kind).collect();
        assert_eq!(kinds, vec![HitKind::Edge(1), HitKind::Face(1), HitKind::Edge(0), HitKind::Face(0)]);
        // tolerance too small -> no edge hits
        let hits = p.pick_all(Vec3::new(0.5, 0.02, 10.0), Vec3::NEG_Z, EdgeTolerance::ortho(0.001));
        assert!(hits.iter().all(|h| matches!(h.kind, HitKind::Face(_))));
        // perspective tolerance grows with depth: picks the far edge only
        let tol = EdgeTolerance { constant: 0.0, per_unit_depth: 0.0035 };
        let hits = p.pick_all(Vec3::new(0.5, 0.03, 10.0), Vec3::NEG_Z, tol);
        assert!(!hits.iter().any(|h| h.kind == HitKind::Edge(1)));
        assert!(hits.iter().any(|h| h.kind == HitKind::Edge(0)));
        // miss
        assert!(p.pick_all(Vec3::new(5.0, 5.0, 10.0), Vec3::NEG_Z, EdgeTolerance::ortho(0.01)).is_empty());
    }

    /// A quad at z = 0 and a floating edge (no shared vertices, so adjacency is
    /// unknown) at exactly the same depth: the tie goes to the edge. One 0.02 behind
    /// is outside the sliver slack and the face stays first.
    #[test]
    fn exact_depth_tie_goes_to_the_edge() {
        let mut m = Mesh::default();
        m.positions.extend([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]]);
        m.normals.extend([[0.0, 0.0, 1.0]; 4]);
        m.indices.extend([[0, 1, 2], [0, 2, 3]]);
        m.tri_face.extend([0, 0]);
        m.edges.push(EdgePolyline { edge: 7, points: vec![[0.3, 0.5, 0.0], [0.8, 0.5, 0.0]] });
        let id = BodyId::of(FeatureId(1));
        let p = Picker::new([(id, &m)]);
        let tol = EdgeTolerance::ortho(0.05);
        let hits = p.pick_all(Vec3::new(0.5, 0.52, 10.0), Vec3::NEG_Z, tol);
        assert_eq!(hits[0].kind, HitKind::Edge(7), "{hits:?}");
        assert_eq!(hits[1].kind, HitKind::Face(0));
        m.edges[0].points = vec![[0.3, 0.5, -0.02], [0.8, 0.5, -0.02]];
        let p = Picker::new([(id, &m)]);
        let hits = p.pick_all(Vec3::new(0.5, 0.52, 10.0), Vec3::NEG_Z, tol);
        assert_eq!(hits[0].kind, HitKind::Face(0), "{hits:?}");
    }
}
