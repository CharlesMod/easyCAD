//! Mesh bodies: closed triangle meshes (an imported STL / OBJ / glTF, or the result
//! of a boolean with one) carried next to the kernel's BRep bodies. Booleans,
//! plane splits and clearances on them run in Manifold (`manifold-csg`), which is
//! robust on the 50k–300k triangle meshes that scanners and generators produce.
//! BRep operands are tessellated at print quality first.

use std::collections::{HashMap, HashSet, VecDeque};

use glam::{DAffine3, DMat3, DVec3};
use manifold_csg::Manifold;

/// A closed, oriented (outward normals) triangle mesh in model millimetres.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshSolid {
    pub positions: Vec<DVec3>,
    pub tris: Vec<[u32; 3]>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum MeshError {
    #[error("unsupported mesh format `{0}` (STL, OBJ, GLB or glTF)")]
    Format(String),
    #[error("could not read the mesh: {0}")]
    Parse(String),
    #[error("the file has no triangles")]
    Empty,
    #[error("not a closed solid: {open} open edges and {nonmanifold} edges shared by more than two triangles remain after repair (fix it in the source app, e.g. Meshy's 3D-print prep or the slicer's 'Fix model')")]
    NotClosed { open: usize, nonmanifold: usize },
    #[error("mesh kernel: {0}")]
    Kernel(String),
    #[error("the result is empty")]
    EmptyResult,
}

type R<T> = Result<T, MeshError>;

/// Raw triangles as read from a file (unwelded, unchecked).
#[derive(Clone, Debug, Default)]
pub struct RawMesh {
    pub positions: Vec<DVec3>,
    pub tris: Vec<[u32; 3]>,
}

impl RawMesh {
    fn push_tri(&mut self, a: DVec3, b: DVec3, c: DVec3) {
        let n = self.positions.len() as u32;
        self.positions.extend([a, b, c]);
        self.tris.push([n, n + 1, n + 2]);
    }
}

/// File extensions that [`read_mesh`] understands.
pub const MESH_EXTENSIONS: &[&str] = &["stl", "obj", "glb", "gltf"];

/// Read an STL (binary or ASCII), OBJ, GLB or self-contained glTF.
pub fn read_mesh(data: &[u8], ext: &str) -> R<RawMesh> {
    let m = match ext.to_ascii_lowercase().as_str() {
        "stl" => read_stl(data)?,
        "obj" => read_obj(data)?,
        "glb" | "gltf" => read_gltf(data)?,
        other => return Err(MeshError::Format(other.to_string())),
    };
    if m.tris.is_empty() {
        return Err(MeshError::Empty);
    }
    Ok(m)
}

fn read_stl(data: &[u8]) -> R<RawMesh> {
    let mut m = RawMesh::default();
    let binary_len = |n: usize| 84 + 50 * n;
    if data.len() >= 84 {
        let n = u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as usize;
        if binary_len(n) == data.len() {
            let f = |o: usize| f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]) as f64;
            for t in 0..n {
                let o = 84 + 50 * t + 12;
                let v = |k: usize| DVec3::new(f(o + 12 * k), f(o + 12 * k + 4), f(o + 12 * k + 8));
                m.push_tri(v(0), v(1), v(2));
            }
            return Ok(m);
        }
    }
    let text = std::str::from_utf8(data).map_err(|_| MeshError::Parse("STL is neither binary nor ASCII".into()))?;
    let mut corner = Vec::with_capacity(3);
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() != Some("vertex") {
            continue;
        }
        let xs: Vec<f64> = it.filter_map(|s| s.parse().ok()).collect();
        if xs.len() != 3 {
            return Err(MeshError::Parse(format!("bad STL vertex line `{line}`")));
        }
        corner.push(DVec3::new(xs[0], xs[1], xs[2]));
        if corner.len() == 3 {
            m.push_tri(corner[0], corner[1], corner[2]);
            corner.clear();
        }
    }
    Ok(m)
}

fn read_obj(data: &[u8]) -> R<RawMesh> {
    let opts = tobj::LoadOptions { triangulate: true, single_index: true, ignore_points: true, ignore_lines: true };
    let (models, _) = tobj::load_obj_buf(&mut std::io::BufReader::new(data), &opts, |_| Err(tobj::LoadError::OpenFileFailed))
        .map_err(|e| MeshError::Parse(e.to_string()))?;
    let mut m = RawMesh::default();
    for model in models {
        let base = m.positions.len() as u32;
        let p = &model.mesh.positions;
        m.positions.extend(p.chunks_exact(3).map(|c| DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64)));
        m.tris.extend(model.mesh.indices.chunks_exact(3).map(|c| [base + c[0], base + c[1], base + c[2]]));
    }
    Ok(m)
}

fn read_gltf(data: &[u8]) -> R<RawMesh> {
    let (doc, buffers, _) = gltf::import_slice(data).map_err(|e| MeshError::Parse(e.to_string()))?;
    let mut m = RawMesh::default();
    fn walk(node: gltf::Node, parent: glam::DMat4, buffers: &[gltf::buffer::Data], m: &mut RawMesh) {
        let local = glam::Mat4::from_cols_array_2d(&node.transform().matrix()).as_dmat4();
        let world = parent * local;
        if let Some(mesh) = node.mesh() {
            for prim in mesh.primitives() {
                if prim.mode() != gltf::mesh::Mode::Triangles {
                    continue;
                }
                let r = prim.reader(|b| Some(&buffers[b.index()]));
                let Some(pos) = r.read_positions() else { continue };
                let base = m.positions.len() as u32;
                m.positions.extend(pos.map(|p| world.transform_point3(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))));
                let n = m.positions.len() as u32 - base;
                let idx: Vec<u32> = match r.read_indices() {
                    Some(i) => i.into_u32().collect(),
                    None => (0..n).collect(),
                };
                let flip = world.determinant() < 0.0;
                for c in idx.chunks_exact(3) {
                    let t = [base + c[0], base + c[1], base + c[2]];
                    m.tris.push(if flip { [t[0], t[2], t[1]] } else { t });
                }
            }
        }
        for c in node.children() {
            walk(c, world, buffers, m);
        }
    }
    let scene = doc.default_scene().or_else(|| doc.scenes().next()).ok_or_else(|| MeshError::Parse("glTF has no scene".into()))?;
    for n in scene.nodes() {
        walk(n, glam::DMat4::IDENTITY, &buffers, &mut m);
    }
    Ok(m)
}

/// What [`MeshSolid::repair`] had to do.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct RepairReport {
    pub input_triangles: usize,
    pub welded_vertices: usize,
    pub dropped_degenerate: usize,
    pub dropped_duplicate: usize,
    pub flipped_triangles: usize,
    pub filled_holes: usize,
}

impl RepairReport {
    pub fn summary(&self) -> Option<String> {
        let mut v = Vec::new();
        if self.dropped_degenerate + self.dropped_duplicate > 0 {
            v.push(format!("dropped {} degenerate/duplicate triangles", self.dropped_degenerate + self.dropped_duplicate));
        }
        if self.flipped_triangles > 0 {
            v.push(format!("reoriented {} triangles", self.flipped_triangles));
        }
        if self.filled_holes > 0 {
            v.push(format!("filled {} holes", self.filled_holes));
        }
        (!v.is_empty()).then(|| format!("mesh repaired: {}", v.join(", ")))
    }
}

fn edge_key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

impl MeshSolid {
    /// Weld, clean up and close a raw triangle soup (the steps `fit_pile.py` took
    /// with trimesh: merge vertices, fix winding, fill holes), then check it is a
    /// closed 2-manifold in Manifold.
    pub fn repair(raw: &RawMesh) -> R<(MeshSolid, RepairReport)> {
        let mut rep = RepairReport { input_triangles: raw.tris.len(), ..Default::default() };
        if raw.tris.is_empty() {
            return Err(MeshError::Empty);
        }
        // weld on a grid relative to the model size
        let (lo, hi) = bounds(&raw.positions);
        let tol = ((hi - lo).length() * 1e-7).max(1e-9);
        let mut map: HashMap<[i64; 3], u32> = HashMap::new();
        let mut positions = Vec::new();
        let remap: Vec<u32> = raw
            .positions
            .iter()
            .map(|p| {
                let k = [(p.x / tol).round() as i64, (p.y / tol).round() as i64, (p.z / tol).round() as i64];
                *map.entry(k).or_insert_with(|| {
                    positions.push(*p);
                    positions.len() as u32 - 1
                })
            })
            .collect();
        rep.welded_vertices = positions.len();
        let mut tris = Vec::with_capacity(raw.tris.len());
        let mut seen: HashSet<[u32; 3]> = HashSet::new();
        for t in &raw.tris {
            let t = [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]];
            if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                rep.dropped_degenerate += 1;
                continue;
            }
            let mut s = t;
            s.sort_unstable();
            if !seen.insert(s) {
                rep.dropped_duplicate += 1;
                continue;
            }
            tris.push(t);
        }
        let mut m = MeshSolid { positions, tris };
        if let Ok(ok) = m.validated() {
            return Ok((ok, rep));
        }
        rep.flipped_triangles = m.orient_consistently();
        rep.filled_holes = m.fill_holes();
        if m.signed_volume() < 0.0 {
            for t in m.tris.iter_mut() {
                t.swap(1, 2);
            }
        }
        match m.validated() {
            Ok(ok) => Ok((ok, rep)),
            Err(_) => {
                let (open, nonmanifold) = m.edge_defects();
                Err(MeshError::NotClosed { open, nonmanifold })
            }
        }
    }

    /// Round-trip through Manifold: checks closedness and returns its cleaned mesh.
    fn validated(&self) -> R<MeshSolid> {
        let mf = self.to_manifold()?;
        if mf.is_empty() {
            return Err(MeshError::EmptyResult);
        }
        Ok(MeshSolid::from_manifold(&mf))
    }

    /// (open edges, edges with more than two triangles).
    pub fn edge_defects(&self) -> (usize, usize) {
        let mut count: HashMap<(u32, u32), u32> = HashMap::new();
        for t in &self.tris {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *count.entry(edge_key(a, b)).or_default() += 1;
            }
        }
        (count.values().filter(|c| **c == 1).count(), count.values().filter(|c| **c > 2).count())
    }

    /// Make neighbouring triangles agree on winding (BFS over shared edges).
    /// Returns how many were flipped.
    fn orient_consistently(&mut self) -> usize {
        let mut by_edge: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        for (i, t) in self.tris.iter().enumerate() {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                by_edge.entry(edge_key(a, b)).or_default().push(i);
            }
        }
        let has_dir = |t: &[u32; 3], a: u32, b: u32| (t[0] == a && t[1] == b) || (t[1] == a && t[2] == b) || (t[2] == a && t[0] == b);
        let mut done = vec![false; self.tris.len()];
        let mut flipped = 0;
        for start in 0..self.tris.len() {
            if done[start] {
                continue;
            }
            done[start] = true;
            let mut q = VecDeque::from([start]);
            while let Some(i) = q.pop_front() {
                let t = self.tris[i];
                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    let Some(ns) = by_edge.get(&edge_key(a, b)) else { continue };
                    if ns.len() != 2 {
                        continue;
                    }
                    for &j in ns {
                        if j == i || done[j] {
                            continue;
                        }
                        // a consistent neighbour runs the shared edge the other way
                        if has_dir(&self.tris[j], a, b) {
                            self.tris[j].swap(1, 2);
                            flipped += 1;
                        }
                        done[j] = true;
                        q.push_back(j);
                    }
                }
            }
        }
        flipped
    }

    /// Close boundary loops with a fan around their centroid. Returns loops filled.
    fn fill_holes(&mut self) -> usize {
        // directed boundary half-edges: an edge used once, kept in its triangle's direction
        let mut count: HashMap<(u32, u32), u32> = HashMap::new();
        for t in &self.tris {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *count.entry(edge_key(a, b)).or_default() += 1;
            }
        }
        let mut next: HashMap<u32, u32> = HashMap::new();
        for t in &self.tris {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if count[&edge_key(a, b)] == 1 {
                    // the hole runs the other way round
                    if next.insert(b, a).is_some() {
                        return 0; // a vertex on two boundary loops: leave it alone
                    }
                }
            }
        }
        let mut filled = 0;
        let mut visited: HashSet<u32> = HashSet::new();
        let starts: Vec<u32> = next.keys().copied().collect();
        for s in starts {
            if visited.contains(&s) {
                continue;
            }
            let mut ring = vec![s];
            visited.insert(s);
            let mut cur = s;
            let closed = loop {
                let Some(&n) = next.get(&cur) else { break false };
                if n == s {
                    break true;
                }
                if !visited.insert(n) {
                    break false;
                }
                ring.push(n);
                cur = n;
            };
            if !closed || ring.len() < 3 {
                continue;
            }
            if ring.len() == 3 {
                self.tris.push([ring[0], ring[1], ring[2]]);
            } else {
                let c = ring.iter().map(|i| self.positions[*i as usize]).sum::<DVec3>() / ring.len() as f64;
                self.positions.push(c);
                let ci = self.positions.len() as u32 - 1;
                for k in 0..ring.len() {
                    self.tris.push([ring[k], ring[(k + 1) % ring.len()], ci]);
                }
            }
            filled += 1;
        }
        filled
    }

    pub fn to_manifold(&self) -> R<Manifold> {
        let vp: Vec<f64> = self.positions.iter().flat_map(|p| [p.x, p.y, p.z]).collect();
        let ix: Vec<u64> = self.tris.iter().flat_map(|t| t.iter().map(|i| *i as u64)).collect();
        Manifold::from_mesh_f64(&vp, 3, &ix).map_err(|e| MeshError::Kernel(e.to_string()))
    }

    pub fn from_manifold(m: &Manifold) -> MeshSolid {
        let (vp, np, ix) = m.to_mesh_f64();
        MeshSolid {
            positions: vp.chunks_exact(np).map(|c| DVec3::new(c[0], c[1], c[2])).collect(),
            tris: ix.chunks_exact(3).map(|c| [c[0] as u32, c[1] as u32, c[2] as u32]).collect(),
        }
    }

    /// Tessellate a kernel solid at print quality into a closed mesh.
    pub fn from_shape(shape: &occt::Shape) -> R<MeshSolid> {
        let m = shape
            .tessellate(crate::export::PRINT_LIN_DEFL, crate::export::PRINT_ANG_DEFL_DEG)
            .map_err(|e| MeshError::Kernel(e.0))?;
        let raw = RawMesh {
            positions: m.positions.iter().map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect(),
            tris: m.indices.clone(),
        };
        Ok(MeshSolid::repair(&raw)?.0)
    }

    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    pub fn signed_volume(&self) -> f64 {
        self.tris
            .iter()
            .map(|t| {
                let (a, b, c) = (self.positions[t[0] as usize], self.positions[t[1] as usize], self.positions[t[2] as usize]);
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }

    pub fn volume(&self) -> f64 {
        self.signed_volume().abs()
    }

    /// (min, max) corners.
    pub fn bounds(&self) -> (DVec3, DVec3) {
        bounds(&self.positions)
    }

    pub fn bbox(&self) -> occt::BBox {
        let (min, max) = self.bounds();
        occt::BBox { min, max }
    }

    /// Apply an affine map (flips winding for mirroring maps).
    pub fn transformed(&self, m: &DAffine3) -> MeshSolid {
        let positions = self.positions.iter().map(|p| m.transform_point3(*p)).collect();
        let mut tris = self.tris.clone();
        if m.matrix3.determinant() < 0.0 {
            for t in tris.iter_mut() {
                t.swap(1, 2);
            }
        }
        MeshSolid { positions, tris }
    }

    pub fn translated(&self, v: DVec3) -> MeshSolid {
        self.transformed(&DAffine3::from_translation(v))
    }

    /// Split by the plane through `origin` with `normal`: (below, above), i.e.
    /// against and along the normal. `None` for an empty side.
    pub fn split(&self, origin: DVec3, normal: DVec3) -> R<(Option<MeshSolid>, Option<MeshSolid>)> {
        let n = normal.normalize();
        let mf = self.to_manifold()?;
        let (pos, neg) = mf.split_by_plane([n.x, n.y, n.z], n.dot(origin));
        let side = |m: Manifold| {
            let s = MeshSolid::from_manifold(&m);
            (!s.is_empty()).then_some(s)
        };
        Ok((side(neg), side(pos)))
    }

    /// Grow (`d` > 0) by a Minkowski sum with a ball, or shrink (`d` < 0) by a
    /// Minkowski difference. Slow on large non-convex meshes.
    pub fn offset(&self, d: f64) -> R<MeshSolid> {
        if d.abs() < 1e-9 {
            return Ok(self.clone());
        }
        let ball = Manifold::sphere(d.abs(), 16);
        let mf = self.to_manifold()?;
        let r = if d > 0.0 { mf.minkowski_sum(&ball) } else { mf.minkowski_difference(&ball) };
        Ok(MeshSolid::from_manifold(&r))
    }

    /// Triangles for the viewport: one pick face (0), no edges, vertices split
    /// where neighbouring triangles meet at more than 30° so cut faces stay flat.
    pub fn display_mesh(&self) -> occt::Mesh {
        let fn_: Vec<DVec3> = self
            .tris
            .iter()
            .map(|t| {
                let (a, b, c) = (self.positions[t[0] as usize], self.positions[t[1] as usize], self.positions[t[2] as usize]);
                (b - a).cross(c - a) // area weighted
            })
            .collect();
        let mut incident: Vec<Vec<u32>> = vec![Vec::new(); self.positions.len()];
        for (i, t) in self.tris.iter().enumerate() {
            for v in t {
                incident[*v as usize].push(i as u32);
            }
        }
        let cos_crease = 30f64.to_radians().cos();
        let mut out = occt::Mesh::default();
        let mut key: HashMap<(u32, [i32; 3]), u32> = HashMap::new();
        for (i, t) in self.tris.iter().enumerate() {
            let own = fn_[i].normalize_or_zero();
            let mut tri = [0u32; 3];
            for (k, v) in t.iter().enumerate() {
                let mut n = DVec3::ZERO;
                for &j in &incident[*v as usize] {
                    if fn_[j as usize].normalize_or_zero().dot(own) >= cos_crease {
                        n += fn_[j as usize];
                    }
                }
                let n = n.try_normalize().unwrap_or(own);
                let q = [(n.x * 1000.0) as i32, (n.y * 1000.0) as i32, (n.z * 1000.0) as i32];
                tri[k] = *key.entry((*v, q)).or_insert_with(|| {
                    let p = self.positions[*v as usize];
                    out.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                    out.normals.push([n.x as f32, n.y as f32, n.z as f32]);
                    out.positions.len() as u32 - 1
                });
            }
            out.indices.push(tri);
            out.tri_face.push(0);
        }
        out
    }

    /// Unwelded export mesh for the STL / 3MF writers.
    pub fn to_occt_mesh(&self) -> occt::Mesh {
        occt::Mesh {
            positions: self.positions.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(),
            normals: Vec::new(),
            indices: self.tris.clone(),
            tri_face: vec![0; self.tris.len()],
            edges: Vec::new(),
        }
    }
}

fn bounds(ps: &[DVec3]) -> (DVec3, DVec3) {
    ps.iter().fold((DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)), |(lo, hi), p| (lo.min(*p), hi.max(*p)))
}

/// Boolean of mesh operands in Manifold. `Join`/`Cut` combine all tools;
/// `Intersect` keeps what is inside every tool.
pub fn boolean(target: &MeshSolid, tools: &[MeshSolid], op: crate::feature::BoolOp) -> R<MeshSolid> {
    use crate::feature::BoolOp;
    let mut m = target.to_manifold()?;
    let ts: Vec<Manifold> = tools.iter().map(|t| t.to_manifold()).collect::<R<_>>()?;
    match op {
        BoolOp::Join => {
            let mut all = vec![m];
            all.extend(ts);
            m = Manifold::batch_union(&all);
        }
        BoolOp::Cut => {
            if !ts.is_empty() {
                m = m.difference(&Manifold::batch_union(&ts));
            }
        }
        BoolOp::Intersect => {
            for t in &ts {
                m = m.intersection(t);
            }
        }
        BoolOp::NewBody => return Err(MeshError::Kernel("NewBody is not a boolean".into())),
    }
    m.status().map_err(|e| MeshError::Kernel(e.to_string()))?;
    let out = MeshSolid::from_manifold(&m);
    if out.is_empty() {
        return Err(MeshError::EmptyResult);
    }
    Ok(out)
}

/// Rotation from XYZ Euler angles in degrees (applied X, then Y, then Z).
pub fn euler_deg(r: DVec3) -> DMat3 {
    DMat3::from_rotation_z(r.z.to_radians()) * DMat3::from_rotation_y(r.y.to_radians()) * DMat3::from_rotation_x(r.x.to_radians())
}

/// Write a binary STL (for tests and tools; the app's exporter is `export::write_stl`).
pub fn write_stl_bytes(m: &MeshSolid) -> Vec<u8> {
    let mut out = vec![0u8; 80];
    out.extend((m.tris.len() as u32).to_le_bytes());
    for t in &m.tris {
        let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
        let n = (b - a).cross(c - a).normalize_or_zero();
        for v in [n, a, b, c] {
            for x in [v.x, v.y, v.z] {
                out.extend((x as f32).to_le_bytes());
            }
        }
        out.extend([0, 0]);
    }
    out
}

/// A lumpy closed blob (icosphere with `subdivisions` levels, radius `r` about `c`,
/// ±18 % bumps): a stand-in for a scanned or AI-generated mesh in tests and demos.
pub fn sample_blob(c: DVec3, r: f64, subdivisions: u32) -> MeshSolid {
    let t = (1.0 + 5f64.sqrt()) / 2.0;
    let mut ps: Vec<DVec3> = [
        (-1.0, t, 0.0), (1.0, t, 0.0), (-1.0, -t, 0.0), (1.0, -t, 0.0),
        (0.0, -1.0, t), (0.0, 1.0, t), (0.0, -1.0, -t), (0.0, 1.0, -t),
        (t, 0.0, -1.0), (t, 0.0, 1.0), (-t, 0.0, -1.0), (-t, 0.0, 1.0),
    ]
    .iter()
    .map(|p| DVec3::new(p.0, p.1, p.2).normalize())
    .collect();
    let mut tris: Vec<[u32; 3]> = vec![
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11], [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9], [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ];
    for _ in 0..subdivisions {
        let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
        let mut m = |a: u32, b: u32, ps: &mut Vec<DVec3>| {
            *mid.entry((a.min(b), a.max(b))).or_insert_with(|| {
                ps.push(((ps[a as usize] + ps[b as usize]) / 2.0).normalize());
                ps.len() as u32 - 1
            })
        };
        let mut next = Vec::with_capacity(tris.len() * 4);
        for [a, b, cc] in tris {
            let (ab, bc, ca) = (m(a, b, &mut ps), m(b, cc, &mut ps), m(cc, a, &mut ps));
            next.extend([[a, ab, ca], [b, bc, ab], [cc, ca, bc], [ab, bc, ca]]);
        }
        tris = next;
    }
    let positions = ps.iter().map(|d| c + *d * r * (1.0 + 0.12 * (3.0 * d.x).sin() * (2.0 * d.y).cos() + 0.06 * (5.0 * d.z).sin())).collect();
    MeshSolid { positions, tris }
}
