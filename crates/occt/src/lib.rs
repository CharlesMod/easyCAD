//! Safe Rust API over a narrow OpenCascade shim.
//!
//! [`Shape`] is an immutable, cheaply clonable handle to a `TopoDS_Shape`. All
//! modelling operations return new shapes. Kernel failures never abort: every
//! fallible call returns [`Result`], carrying the OCCT exception text.
//!
//! Units are millimetres, angles are degrees unless a name says `_rad`.

// links mimalloc in, which overrides malloc process-wide (see Cargo.toml)
#[cfg(target_os = "macos")]
use libmimalloc_sys as _;

mod ffi;
pub mod dressup;
pub mod thread;

use cxx::UniquePtr;
use ffi::bridge as raw;
pub use glam::DVec3;

pub use dressup::{DressUpKind, DressUpReport, EdgeOutcome, EdgeStatus, Strategy};
pub use thread::Thread;


/// A kernel error (wraps the C++ exception message).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("kernel: {0}")]
pub struct Error(pub String);

impl From<cxx::Exception> for Error {
    fn from(e: cxx::Exception) -> Self {
        Error(e.what().to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn v3(v: DVec3) -> raw::V3 {
    raw::V3 { x: v.x, y: v.y, z: v.z }
}
fn dv(v: raw::V3) -> DVec3 {
    DVec3::new(v.x, v.y, v.z)
}

/// A placed planar frame (origin, x direction, normal).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub origin: DVec3,
    pub x_dir: DVec3,
    pub normal: DVec3,
}

impl Frame {
    pub const XY: Frame = Frame { origin: DVec3::ZERO, x_dir: DVec3::X, normal: DVec3::Z };
    fn raw(&self) -> raw::Frame {
        raw::Frame { origin: v3(self.origin), x_dir: v3(self.x_dir), normal: v3(self.normal) }
    }
}

/// A 3D boundary edge used to build planar faces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Line(DVec3, DVec3),
    /// Arc through start, an interior point, end.
    Arc3(DVec3, DVec3, DVec3),
    Circle { center: DVec3, normal: DVec3, radius: f64 },
}

/// Shape topology kinds (TopAbs_ShapeEnum).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Compound = 0,
    CompSolid = 1,
    Solid = 2,
    Shell = 3,
    Face = 4,
    Wire = 5,
    Edge = 6,
    Vertex = 7,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveKind {
    Line,
    Circle,
    Ellipse,
    Spline,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FaceInfo {
    /// Index within this shape (TopExp::MapShapes order). Stable only for this shape.
    pub index: u32,
    pub kind: SurfaceKind,
    pub centroid: DVec3,
    /// Outward normal at the surface point closest to the centroid.
    pub normal: DVec3,
    /// The surface point where `normal` is evaluated.
    pub point: DVec3,
    pub area: f64,
    /// Cylinder / cone axis and (reference) radius.
    pub axis: Option<(DVec3, DVec3)>,
    pub radius: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EdgeInfo {
    pub index: u32,
    pub kind: CurveKind,
    pub midpoint: DVec3,
    pub tangent: DVec3,
    pub length: f64,
    pub start: DVec3,
    pub end: DVec3,
    /// Circle center, axis and radius.
    pub circle: Option<(DVec3, DVec3, f64)>,
    pub closed: bool,
    /// Adjacent face indices.
    pub faces: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BBox {
    pub min: DVec3,
    pub max: DVec3,
}

impl BBox {
    pub fn size(&self) -> DVec3 {
        self.max - self.min
    }
    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlrKind {
    /// A sharp model edge.
    Sharp,
    /// A smooth (tangent-continuous) model edge.
    Smooth,
    /// A silhouette of a curved surface (no model edge).
    Outline,
}

/// One projected polyline of a hidden-line drawing (projector coordinates).
#[derive(Clone, Debug, PartialEq)]
pub struct HlrEdge {
    pub points: Vec<[f64; 2]>,
    pub visible: bool,
    pub kind: HlrKind,
}

/// A named, optionally coloured (sRGB 0..1) part of a STEP assembly.
#[derive(Clone, Debug, PartialEq)]
pub struct StepPart {
    pub name: String,
    pub color: Option<[f32; 3]>,
}

/// A STEP read through XCAF: leaf solids (direct children of `shape`, following
/// `parts`) and each face's own colour (MapShapes face order of `shape`).
#[derive(Clone, Debug)]
pub struct StyledStep {
    pub shape: Shape,
    pub parts: Vec<StepPart>,
    pub face_colors: Vec<Option<[f32; 3]>>,
}

/// An XCAF product structure (STEP assembly): parts carry a shape, assemblies carry
/// placed instances of other nodes.
#[derive(Clone, Debug, Default)]
pub struct StepTree {
    pub nodes: Vec<StepNode>,
    /// Top-level products (XCAF free shapes).
    pub roots: Vec<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct StepNode {
    pub name: String,
    pub color: Option<[f32; 3]>,
    /// The part's own shape, in its own frame (`None` for an assembly).
    pub shape: Option<Shape>,
    pub children: Vec<StepLink>,
}

/// An instance of `nodes[child]` inside its parent node.
#[derive(Clone, Debug, PartialEq)]
pub struct StepLink {
    pub child: usize,
    /// Instance name (may be empty).
    pub name: String,
    /// Placement, row-major 3×4 `[r00 r01 r02 tx, r10 r11 r12 ty, r20 r21 r22 tz]`.
    pub matrix: [f64; 12],
}

impl StepTree {
    /// Read the product structure of a STEP file (names, colours, placements).
    pub fn read(data: &str) -> Result<StepTree> {
        let (mut nodes, mut links, mut mats, mut roots) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let comp = wrap(raw::read_step_asm(data, &mut nodes, &mut links, &mut mats, &mut roots))?;
        let mut out = StepTree { nodes: Vec::with_capacity(nodes.len()), roots: roots.iter().map(|r| *r as usize).collect() };
        for n in &nodes {
            let shape = if n.shape >= 0 { Some(wrap(raw::child_at(comp.r(), n.shape as u32))?) } else { None };
            out.nodes.push(StepNode { name: n.name.clone(), color: n.has_color.then_some([n.r, n.g, n.b]), shape, children: Vec::new() });
        }
        for (k, l) in links.iter().enumerate() {
            let mut m = [0.0; 12];
            m.copy_from_slice(&mats[12 * k..12 * k + 12]);
            out.nodes[l.parent as usize].children.push(StepLink { child: l.child as usize, name: l.name.clone(), matrix: m });
        }
        Ok(out)
    }

    /// Write as an AP214 STEP assembly (XCAF), keeping names, colours and placements.
    /// A node with children is an assembly (its `shape` is ignored); every other node
    /// must have a shape.
    pub fn to_step_string(&self) -> Result<String> {
        let mut shapes: Vec<&Shape> = Vec::new();
        let mut nodes = Vec::new();
        for n in &self.nodes {
            let shape = match (&n.shape, n.children.is_empty()) {
                (Some(s), true) => {
                    shapes.push(s);
                    shapes.len() as i32 - 1
                }
                _ => -1,
            };
            let c = n.color.unwrap_or([0.0; 3]);
            nodes.push(raw::AsmNode { name: n.name.clone(), r: c[0], g: c[1], b: c[2], has_color: n.color.is_some(), shape });
        }
        let (mut links, mut mats) = (Vec::new(), Vec::new());
        for (p, n) in self.nodes.iter().enumerate() {
            for l in &n.children {
                links.push(raw::AsmLink { parent: p as u32, child: l.child as u32, name: l.name.clone() });
                mats.extend_from_slice(&l.matrix);
            }
        }
        let roots: Vec<u32> = self.roots.iter().map(|r| *r as u32).collect();
        let comp = Shape::compound(&shapes)?;
        Ok(raw::write_step_asm(comp.r(), &nodes, &links, &mats, &roots)?)
    }
}

/// Triangle mesh of a shape with per-triangle face ids and per-edge polylines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<[u32; 3]>,
    /// Face index (see [`Shape::faces`]) per triangle.
    pub tri_face: Vec<u32>,
    pub edges: Vec<EdgePolyline>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EdgePolyline {
    pub edge: u32,
    pub points: Vec<[f32; 3]>,
}

fn chunk3(v: &[f32]) -> Vec<[f32; 3]> {
    v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
}

impl From<raw::MeshData> for Mesh {
    fn from(m: raw::MeshData) -> Self {
        let pts = chunk3(&m.edge_points);
        let edges = m
            .edge_ranges
            .chunks_exact(2)
            .zip(m.edge_ids.iter())
            .map(|(r, &id)| EdgePolyline {
                edge: id,
                points: pts[r[0] as usize..(r[0] + r[1]) as usize].to_vec(),
            })
            .collect();
        Mesh {
            positions: chunk3(&m.positions),
            normals: chunk3(&m.normals),
            indices: m.indices.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
            tri_face: m.tri_face,
            edges,
        }
    }
}

/// Immutable handle to a kernel shape.
pub struct Shape {
    inner: UniquePtr<raw::Shape>,
}

impl Clone for Shape {
    fn clone(&self) -> Self {
        Shape { inner: raw::shape_clone(self.r()) }
    }
}

impl std::fmt::Debug for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shape").field("kind", &self.kind()).finish()
    }
}

fn wrap(p: std::result::Result<UniquePtr<raw::Shape>, cxx::Exception>) -> Result<Shape> {
    let inner = p?;
    if inner.is_null() {
        return Err(Error("null shape".into()));
    }
    Ok(Shape { inner })
}

impl Shape {
    fn r(&self) -> &raw::Shape {
        self.inner.as_ref().expect("Shape is never null")
    }

    // ------------------------------------------------------------ primitives

    pub fn make_box(origin: DVec3, size: DVec3) -> Result<Shape> {
        wrap(raw::make_box(v3(origin), size.x, size.y, size.z))
    }

    pub fn cylinder(base: DVec3, dir: DVec3, radius: f64, height: f64) -> Result<Shape> {
        wrap(raw::make_cylinder(v3(base), v3(dir), radius, height))
    }

    /// Truncated cone from `r_base` at `base` to `r_top` at `base + dir*height`.
    pub fn cone(base: DVec3, dir: DVec3, r_base: f64, r_top: f64, height: f64) -> Result<Shape> {
        wrap(raw::make_cone(v3(base), v3(dir), r_base, r_top, height))
    }

    /// An empty compound to collect shapes into.
    pub fn compound(shapes: &[&Shape]) -> Result<Shape> {
        let mut c = raw::compound_new();
        for s in shapes {
            raw::compound_add(c.pin_mut(), s.r())?;
        }
        wrap(Ok(c))
    }

    /// Planar face on `frame` from an outer loop and hole loops. Each loop's edges
    /// must be ordered and chained end to start.
    pub fn face(frame: &Frame, outer: &[Edge], holes: &[Vec<Edge>]) -> Result<Shape> {
        let mut specs = Vec::new();
        let mut push = |loop_id: u32, e: &Edge| {
            let z = raw::V3::default();
            specs.push(match *e {
                Edge::Line(a, b) => raw::EdgeSpec { kind: 0, loop_id, p0: v3(a), p1: v3(b), p2: z, radius: 0.0 },
                Edge::Arc3(a, m, b) => {
                    raw::EdgeSpec { kind: 1, loop_id, p0: v3(a), p1: v3(m), p2: v3(b), radius: 0.0 }
                }
                Edge::Circle { center, normal, radius } => {
                    raw::EdgeSpec { kind: 2, loop_id, p0: v3(center), p1: v3(normal), p2: z, radius }
                }
            });
        };
        for e in outer {
            push(0, e);
        }
        for (i, h) in holes.iter().enumerate() {
            for e in h {
                push(i as u32 + 1, e);
            }
        }
        wrap(raw::face_from_edges(&frame.raw(), &specs))
    }

    // ------------------------------------------------------------ sweeps

    /// Linear sweep by vector `v`.
    pub fn extrude(&self, v: DVec3) -> Result<Shape> {
        wrap(raw::extrude(self.r(), v3(v)))
    }

    /// Extrude a planar face along `dir` by `distance`; `symmetric` centres it on
    /// the profile plane; `draft_deg` tapers side faces (positive = shrinking
    /// away from the profile plane), with the neutral plane at the profile.
    pub fn extrude_ext(&self, frame: &Frame, dir: DVec3, distance: f64, symmetric: bool, draft_deg: f64) -> Result<Shape> {
        let dir = dir.normalize_or_zero();
        if dir == DVec3::ZERO || distance.abs() < 1e-9 {
            return Err(Error("extrude: zero direction or distance".into()));
        }
        let v = dir * distance;
        if symmetric {
            // Two half extrusions so the draft neutral plane stays at the profile.
            let a = self.extrude_ext(frame, dir, distance / 2.0, false, draft_deg)?;
            let b = self.extrude_ext(frame, -dir, distance / 2.0, false, draft_deg)?;
            return a.fuse(&b);
        }
        if draft_deg.abs() < 1e-9 {
            return self.extrude(v);
        }
        let neutral = Frame { origin: frame.origin, x_dir: frame.x_dir, normal: frame.normal };
        wrap(raw::extrude_draft(self.r(), v3(v), draft_deg.to_radians(), &neutral.raw()))
    }

    /// Planar region(s) extruded by `v` while every boundary moves inward by
    /// `inset` (outer loops shrink, holes grow): a ruled taper built from the 2D
    /// offset of the region, without OCCT's draft algorithm (which can hang on
    /// near-tangent sections). `inset == |v|` is a 45° lead-in. Errors when the
    /// offset changes a loop's edge count (a corner arc vanished) or the region.
    pub fn taper(&self, v: DVec3, inset: f64) -> Result<Shape> {
        wrap(raw::taper_prism(self.r(), v3(v), inset))
    }

    pub fn revolve(&self, axis_origin: DVec3, axis_dir: DVec3, angle_deg: f64) -> Result<Shape> {
        wrap(raw::revolve(self.r(), v3(axis_origin), v3(axis_dir), angle_deg.to_radians()))
    }

    // ------------------------------------------------------------ booleans

    pub fn fuse(&self, other: &Shape) -> Result<Shape> {
        wrap(raw::boolean(self.r(), other.r(), 0))
    }
    pub fn cut(&self, other: &Shape) -> Result<Shape> {
        wrap(raw::boolean(self.r(), other.r(), 1))
    }
    pub fn common(&self, other: &Shape) -> Result<Shape> {
        wrap(raw::boolean(self.r(), other.r(), 2))
    }
    /// Fuse many shapes in one general-fuse pass.
    pub fn fuse_all(shapes: &[&Shape]) -> Result<Shape> {
        let c = Shape::compound(shapes)?;
        wrap(raw::fuse_all(c.r()))
    }
    /// Merge coplanar faces / collinear edges (ShapeUpgrade_UnifySameDomain).
    pub fn unify(&self) -> Result<Shape> {
        wrap(raw::unify(self.r()))
    }
    /// ShapeFix + unify.
    pub fn heal(&self) -> Result<Shape> {
        wrap(raw::heal(self.r()))
    }

    // ------------------------------------------------------------ dress-up

    /// Single-shot fillet (no fallback). Prefer [`dressup::fillet`].
    pub fn fillet_raw(&self, edges: &[u32], radii: &[f64]) -> Result<Shape> {
        wrap(raw::fillet(self.r(), edges, radii))
    }
    /// Single-shot chamfer (no fallback). Prefer [`dressup::chamfer`].
    pub fn chamfer_raw(&self, edges: &[u32], distances: &[f64]) -> Result<Shape> {
        wrap(raw::chamfer(self.r(), edges, distances))
    }
    /// Hollow the solid, removing `open_faces`. Inward keeps the outer skin.
    pub fn shell(&self, open_faces: &[u32], thickness: f64, outward: bool) -> Result<Shape> {
        self.shell_report(open_faces, thickness, outward).map(|(s, _)| s)
    }
    /// [`Shape::shell`] plus the strategy that built it. On failure the error names
    /// the faces / creases that likely blocked the offset ([`Shape::shell_blockers`]).
    pub fn shell_report(&self, open_faces: &[u32], thickness: f64, outward: bool) -> Result<(Shape, ShellStrategy)> {
        let off = if outward { thickness.abs() } else { -thickness.abs() };
        let mut code = 0u32;
        match raw::shell(self.r(), open_faces, off, &mut code) {
            Ok(p) => Ok((wrap(Ok(p))?, ShellStrategy::from_code(code))),
            Err(e) => {
                let Error(mut msg) = Error::from(e);
                msg += &self.shell_max_note(open_faces, thickness, outward);
                if let Ok(b) = self.shell_blockers(open_faces, thickness) {
                    msg += &ShellBlocker::summarize(&b, thickness.abs(), |i| format!("face {i}"));
                }
                Err(Error(msg))
            }
        }
    }
    /// Largest wall thickness below `thickness` that shells with `open_faces`
    /// (bisection to about 1/64 of `thickness` with the fast, uncapped half of the
    /// ladder, rounded down to 0.01 mm and itself shelled; no clock involved, so the
    /// answer is the same on a busy machine), or `None` when none was found. Only
    /// worth calling after a failure: up to ten shells.
    pub fn shell_max_thickness(&self, open_faces: &[u32], thickness: f64, outward: bool) -> Option<f64> {
        let off = if outward { thickness.abs() } else { -thickness.abs() };
        let t = raw::shell_max_thickness(self.r(), open_faces, off, 6).ok()?;
        (t > 0.0).then_some(t)
    }
    /// "; largest workable thickness: 0.62 mm" (or that none was found).
    fn shell_max_note(&self, open_faces: &[u32], thickness: f64, outward: bool) -> String {
        if open_faces.is_empty() {
            return String::new();
        }
        match self.shell_max_thickness(open_faces, thickness, outward) {
            Some(t) => format!("; largest workable thickness: {t:.2} mm"),
            None => format!("; no thickness down to {:.2} mm works", thickness.abs() / 64.0),
        }
    }
    /// Faces narrower than the wall and near-tangent creases (under 8°), the two
    /// shapes that most often stop a shell of `thickness`. Open faces are skipped.
    pub fn shell_blockers(&self, open_faces: &[u32], thickness: f64) -> Result<Vec<ShellBlocker>> {
        Ok(raw::shell_blockers(self.r(), open_faces, thickness.abs())?
            .into_iter()
            .map(|b| match b.kind {
                0 => ShellBlocker::NarrowFace { face: b.face, width: b.value },
                _ => ShellBlocker::Crease { faces: (b.face, b.other.max(0) as u32), angle_deg: b.value },
            })
            .collect())
    }
    /// Draft `faces` by `angle_deg` about the neutral plane, pulling along `pull`.
    pub fn draft(&self, faces: &[u32], pull: DVec3, angle_deg: f64, neutral: &Frame) -> Result<Shape> {
        wrap(raw::draft(self.r(), faces, v3(pull), angle_deg.to_radians(), &neutral.raw()))
    }

    // ------------------------------------------------------------ topology helpers

    pub fn tangent_chains(&self) -> Result<Vec<u32>> {
        Ok(raw::tangent_chains(self.r())?)
    }
    /// 1 convex, -1 concave, 0 smooth or unknown.
    pub fn edge_convexity(&self, edge: u32) -> Result<i32> {
        Ok(raw::edge_convexity(self.r(), edge)?)
    }
    /// Edge passing within `tol` of `p` with a tangent parallel to `tangent`.
    pub fn match_edge(&self, p: DVec3, tangent: DVec3, tol: f64) -> Result<Option<u32>> {
        let i = raw::match_edge(self.r(), v3(p), v3(tangent), tol)?;
        Ok(u32::try_from(i).ok())
    }

    // ------------------------------------------------------------ transforms

    pub fn translated(&self, v: DVec3) -> Result<Shape> {
        wrap(raw::translate(self.r(), v3(v)))
    }
    pub fn rotated(&self, origin: DVec3, dir: DVec3, angle_deg: f64) -> Result<Shape> {
        wrap(raw::rotate(self.r(), v3(origin), v3(dir), angle_deg.to_radians()))
    }
    pub fn mirrored(&self, origin: DVec3, normal: DVec3) -> Result<Shape> {
        wrap(raw::mirror(self.r(), v3(origin), v3(normal)))
    }
    /// `count` copies (including the original) along `dir` every `spacing`, fused.
    pub fn linear_pattern(&self, dir: DVec3, spacing: f64, count: u32) -> Result<Shape> {
        let d = dir.normalize_or_zero();
        let copies: Vec<Shape> =
            (1..count).map(|i| self.translated(d * spacing * i as f64)).collect::<Result<_>>()?;
        let mut all = vec![self];
        all.extend(copies.iter());
        Shape::fuse_all(&all)
    }
    /// `count` copies spread over `total_deg` around an axis (360 → evenly spaced).
    pub fn circular_pattern(&self, origin: DVec3, dir: DVec3, count: u32, total_deg: f64) -> Result<Shape> {
        let step = circular_step(count, total_deg);
        let copies: Vec<Shape> =
            (1..count).map(|i| self.rotated(origin, dir, step * i as f64)).collect::<Result<_>>()?;
        let mut all = vec![self];
        all.extend(copies.iter());
        Shape::fuse_all(&all)
    }
    /// Split by a plane: `(below, above)` relative to `normal`. Either side may be
    /// `None` if the plane misses the shape.
    pub fn split(&self, origin: DVec3, normal: DVec3) -> Result<(Option<Shape>, Option<Shape>)> {
        let side = |above: bool| -> Result<Option<Shape>> {
            match wrap(raw::half_space_side(self.r(), v3(origin), v3(normal), above)) {
                Ok(s) => Ok(Some(s)),
                Err(e) if e.0.contains("nothing on that side") => Ok(None),
                Err(e) => Err(e),
            }
        };
        // the two sides are independent booleans: run them concurrently
        let (below, above) = std::thread::scope(|sc| {
            let h = sc.spawn(|| side(true));
            let below = side(false);
            (below, h.join().unwrap_or_else(|_| Err(Error("split worker panicked".into()))))
        });
        let (below, above) = (below?, above?);
        if below.is_none() && above.is_none() {
            return Err(Error("split produced nothing".into()));
        }
        Ok((below, above))
    }

    // ------------------------------------------------------------ planar regions

    /// Planar section of a solid at a plane, as face(s).
    pub fn section(&self, origin: DVec3, normal: DVec3) -> Result<Shape> {
        wrap(raw::section_face(self.r(), v3(origin), v3(normal)))
    }
    /// Uniform 2D offset of planar face(s); `d < 0` shrinks. Errors if it vanishes.
    pub fn offset_region(&self, d: f64) -> Result<Shape> {
        wrap(raw::offset_face(self.r(), d))
    }
    /// The same faces with their holes filled.
    pub fn fill_holes(&self) -> Result<Shape> {
        wrap(raw::fill_holes(self.r()))
    }
    pub fn region_contains(&self, p: DVec3) -> Result<bool> {
        Ok(raw::face_contains(self.r(), v3(p))?)
    }
    pub fn outer_wire_length(&self) -> Result<f64> {
        Ok(raw::outer_wire_length(self.r())?)
    }
    /// Point and unit tangent at arc-length fraction `t` along the outer boundary.
    pub fn outer_wire_point(&self, t: f64) -> Result<(DVec3, DVec3)> {
        let v = raw::outer_wire_point(self.r(), t)?;
        Ok((dv(v[0]), dv(v[1])))
    }
    /// 3D skin offset of a solid (`d < 0` inward), e.g. to hollow a body.
    pub fn offset_solid(&self, d: f64) -> Result<Shape> {
        wrap(raw::offset_shape(self.r(), d))
    }

    // ------------------------------------------------------------ queries

    pub fn kind(&self) -> Option<ShapeKind> {
        use ShapeKind::*;
        Some(match raw::shape_type(self.r()) {
            0 => Compound,
            1 => CompSolid,
            2 => Solid,
            3 => Shell,
            4 => Face,
            5 => Wire,
            6 => Edge,
            7 => Vertex,
            _ => return None,
        })
    }
    /// True for an empty (null) shape.
    pub fn is_null(&self) -> bool {
        raw::shape_is_null(self.r())
    }
    pub fn is_valid(&self) -> bool {
        raw::shape_is_valid(self.r())
    }
    pub fn count(&self, kind: ShapeKind) -> u32 {
        raw::count_sub(self.r(), kind as u32)
    }
    /// Direct children (of a compound, solid, shell, ...), in order.
    pub fn children(&self) -> Result<Vec<Shape>> {
        (0..raw::child_count(self.r())).map(|i| wrap(raw::child_at(self.r(), i))).collect()
    }
    pub fn solids(&self) -> Result<Vec<Shape>> {
        (0..self.count(ShapeKind::Solid)).map(|i| wrap(raw::sub_shape(self.r(), ShapeKind::Solid as u32, i))).collect()
    }
    pub fn face_shape(&self, index: u32) -> Result<Shape> {
        wrap(raw::sub_shape(self.r(), ShapeKind::Face as u32, index))
    }

    pub fn faces(&self) -> Result<Vec<FaceInfo>> {
        Ok(raw::faces(self.r())?
            .into_iter()
            .map(|f| FaceInfo {
                index: f.index,
                kind: match f.kind {
                    0 => SurfaceKind::Plane,
                    1 => SurfaceKind::Cylinder,
                    2 => SurfaceKind::Cone,
                    3 => SurfaceKind::Sphere,
                    4 => SurfaceKind::Torus,
                    _ => SurfaceKind::Other,
                },
                centroid: dv(f.centroid),
                normal: dv(f.normal),
                point: dv(f.point),
                area: f.area,
                axis: (f.kind == 1 || f.kind == 2).then(|| (dv(f.axis_origin), dv(f.axis_dir))),
                radius: f.radius,
            })
            .collect())
    }

    pub fn edges(&self) -> Result<Vec<EdgeInfo>> {
        Ok(raw::edges(self.r())?
            .into_iter()
            .map(|e| EdgeInfo {
                index: e.index,
                kind: match e.kind {
                    0 => CurveKind::Line,
                    1 => CurveKind::Circle,
                    2 => CurveKind::Ellipse,
                    3 => CurveKind::Spline,
                    _ => CurveKind::Other,
                },
                midpoint: dv(e.midpoint),
                tangent: dv(e.tangent),
                length: e.length,
                start: dv(e.start),
                end: dv(e.end),
                circle: (e.kind == 1).then(|| (dv(e.center), dv(e.axis), e.radius)),
                closed: e.closed,
                faces: [e.face_a, e.face_b].into_iter().filter_map(|i| u32::try_from(i).ok()).collect(),
            })
            .collect())
    }

    /// Edge indices bounding a face.
    pub fn face_edges(&self, face: u32) -> Result<Vec<u32>> {
        Ok(raw::face_edges(self.r(), face)?.into_iter().collect())
    }

    /// Triangulate. `lin_defl` in mm (e.g. 0.05–0.2), `ang_defl_deg` (e.g. 15).
    pub fn tessellate(&self, lin_defl: f64, ang_defl_deg: f64) -> Result<Mesh> {
        Ok(raw::tessellate(self.r(), lin_defl, ang_defl_deg.to_radians())?.into())
    }

    /// Hidden-line projection. `dir` points from the model toward the viewer;
    /// output x runs along `x_dir`, y along `dir × x_dir`. `exact` uses
    /// HLRBRep_Algo (falls back to the polygonal algorithm on failure; the flag is
    /// returned).
    pub fn hlr(&self, dir: DVec3, x_dir: DVec3, exact: bool, defl: f64) -> Result<(Vec<HlrEdge>, bool)> {
        let d = raw::hlr(self.r(), v3(dir), v3(x_dir), exact, defl)?;
        let edges = d
            .ranges
            .chunks_exact(2)
            .zip(d.flags.iter())
            .map(|(r, f)| HlrEdge {
                points: (r[0]..r[0] + r[1]).map(|i| [d.pts[2 * i as usize], d.pts[2 * i as usize + 1]]).collect(),
                visible: f & 1 == 1,
                kind: match (f >> 1) & 3 {
                    0 => HlrKind::Sharp,
                    1 => HlrKind::Smooth,
                    _ => HlrKind::Outline,
                },
            })
            .collect();
        Ok((edges, d.poly_fallback))
    }

    pub fn volume(&self) -> Result<f64> {
        Ok(raw::mass_props(self.r())?.volume)
    }
    pub fn area(&self) -> Result<f64> {
        Ok(raw::mass_props(self.r())?.area)
    }
    pub fn center_of_mass(&self) -> Result<DVec3> {
        Ok(dv(raw::mass_props(self.r())?.center))
    }
    /// `(min, max)` of `(p - origin)·dir` over the shape's vertices and points
    /// sampled along its edges: cheap (no surface optimisation), exact for shapes
    /// whose extremes lie on edges (prisms, shells), an underestimate within the
    /// sampling for faces bulging past their edges.
    pub fn extent_along(&self, origin: DVec3, dir: DVec3) -> Result<(f64, f64)> {
        let v = raw::extent_along(self.r(), v3(origin), v3(dir))?;
        Ok((v[0], v[1]))
    }

    /// The smallest distance between (a part of) this shape and (a part of)
    /// `other` (BRepExtrema), with the closest points. 0 when they touch or
    /// overlap; `inside` when one solid contains the other's nearest point.
    pub fn min_distance(&self, part: SubShape, other: &Shape, other_part: SubShape) -> Result<Distance> {
        let (ak, ai) = part.raw();
        let (bk, bi) = other_part.raw();
        let v = raw::min_distance(self.r(), ak, ai, other.r(), bk, bi)?;
        Ok(Distance {
            distance: v[0],
            on_self: DVec3::new(v[1], v[2], v[3]),
            on_other: DVec3::new(v[4], v[5], v[6]),
            inside: v[7] != 0.0,
        })
    }

    pub fn bbox(&self) -> Result<BBox> {
        let b = raw::bounding_box(self.r())?;
        Ok(BBox { min: dv(b.min), max: dv(b.max) })
    }

    // ------------------------------------------------------------ I/O

    pub fn read_step(path: &std::path::Path) -> Result<Shape> {
        wrap(raw::read_step_file(&path.to_string_lossy()))
    }
    /// Read a STEP with its assembly tree flattened to leaf solids, keeping each
    /// solid's name and colour. The compound's children follow `parts`.
    pub fn read_step_xcaf(data: &str) -> Result<(Shape, Vec<StepPart>)> {
        Shape::read_step_styled(data).map(|s| (s.shape, s.parts))
    }
    /// [`Shape::read_step_xcaf`] plus the colour each face carries of its own
    /// (AP214 `STYLED_ITEM` on an `ADVANCED_FACE`), which overrides its part's.
    pub fn read_step_styled(data: &str) -> Result<StyledStep> {
        let (mut parts, mut rgba) = (Vec::new(), Vec::new());
        let shape = wrap(raw::read_step_xcaf(data, &mut parts, &mut rgba))?;
        Ok(StyledStep {
            shape,
            parts: parts.into_iter().map(|p| StepPart { name: p.name, color: p.has_color.then_some([p.r, p.g, p.b]) }).collect(),
            face_colors: rgba.as_chunks::<4>().0.iter().map(|c| (c[3] > 0.0).then_some([c[0], c[1], c[2]])).collect(),
        })
    }
    /// Write the direct children of this compound as named, coloured STEP parts.
    pub fn to_step_string_named(&self, parts: &[StepPart]) -> Result<String> {
        self.to_step_string_styled(parts, &[])
    }
    /// As [`Shape::to_step_string_named`], also writing a colour on each face that has
    /// one (`face_colors` follows the compound's face order; empty = none).
    pub fn to_step_string_styled(&self, parts: &[StepPart], face_colors: &[Option<[f32; 3]>]) -> Result<String> {
        let raw_parts: Vec<raw::XcafPart> = parts
            .iter()
            .map(|p| {
                let c = p.color.unwrap_or([0.7, 0.7, 0.7]);
                raw::XcafPart { name: p.name.clone(), r: c[0], g: c[1], b: c[2], has_color: p.color.is_some() }
            })
            .collect();
        let rgba: Vec<f32> = if face_colors.iter().any(|c| c.is_some()) {
            face_colors.iter().flat_map(|c| c.map_or([0.0; 4], |c| [c[0], c[1], c[2], 1.0])).collect()
        } else {
            Vec::new()
        };
        Ok(raw::write_step_xcaf(self.r(), &raw_parts, &rgba)?)
    }
    /// Owning direct child of every face (MapShapes face order).
    pub fn face_owners(&self) -> Result<Vec<u32>> {
        Ok(raw::face_owners(self.r())?.into_iter().collect())
    }
    pub fn read_step_str(data: &str) -> Result<Shape> {
        wrap(raw::read_step_str(data))
    }
    pub fn write_step(&self, path: &std::path::Path) -> Result<()> {
        Ok(raw::write_step_file(self.r(), &path.to_string_lossy())?)
    }
    pub fn to_step_string(&self) -> Result<String> {
        Ok(raw::write_step_str(self.r())?)
    }
    pub fn write_stl(&self, path: &std::path::Path, lin_defl: f64, ascii: bool) -> Result<()> {
        Ok(raw::write_stl_file(self.r(), &path.to_string_lossy(), lin_defl, ascii)?)
    }
}

/// Angular step of a circular pattern: a full turn spaces copies evenly.
pub fn circular_step(count: u32, total_deg: f64) -> f64 {
    if count <= 1 {
        return 0.0;
    }
    if (total_deg.abs() - 360.0).abs() < 1e-9 {
        total_deg / count as f64
    } else {
        total_deg / (count - 1) as f64
    }
}

/// How [`Shape::shell_report`] built the shell (see ENGINE.md, Robustness).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellStrategy {
    /// MakeThickSolid with its defaults (or, closed: the direct inner offset).
    Default,
    /// Complete intersection of the offset faces (steps narrower than the wall).
    CompleteIntersection,
    /// Complete intersection with a 0.01 mm tolerance (near-tangent creases).
    LooseTolerance,
    /// The body healed and unified (ShapeFix + UnifySameDomain) first.
    Healed,
    /// Complete intersection, 0.01 mm tolerance, sharp inner joins instead of arcs.
    SharpJoins,
    /// The same on the healed body.
    HealedSharp,
    /// No thick solid: the body minus its inner 3D offset, punched open under each
    /// (flat) open face.
    OffsetAndCut,
    /// Closed shell built by opening the largest planar face and closing it again;
    /// the inner value is the thick-solid step used.
    OpenAndClose(u8),
}

impl ShellStrategy {
    fn from_code(c: u32) -> Self {
        match c {
            0 => ShellStrategy::Default,
            1 => ShellStrategy::CompleteIntersection,
            2 => ShellStrategy::LooseTolerance,
            3 => ShellStrategy::Healed,
            4 => ShellStrategy::SharpJoins,
            5 => ShellStrategy::HealedSharp,
            6 => ShellStrategy::OffsetAndCut,
            c => ShellStrategy::OpenAndClose(c.saturating_sub(100).min(255) as u8),
        }
    }
    /// A fallback was needed.
    pub fn is_fallback(&self) -> bool {
        !matches!(self, ShellStrategy::Default)
    }
    /// Plain words for the feature report.
    pub fn describe(&self) -> String {
        match self {
            ShellStrategy::Default => "default offset".into(),
            ShellStrategy::CompleteIntersection => "complete-intersection offset (the default offset failed)".into(),
            ShellStrategy::LooseTolerance => "complete-intersection offset at 0.01 mm tolerance (the default offset failed)".into(),
            ShellStrategy::SharpJoins => "complete-intersection offset with sharp inner corners at 0.01 mm tolerance (the default offset failed)".into(),
            ShellStrategy::Healed => "healed body, complete-intersection offset (the default offset failed)".into(),
            ShellStrategy::HealedSharp => "healed body, sharp inner corners, 0.01 mm tolerance (the default offset failed)".into(),
            ShellStrategy::OffsetAndCut => "inner offset cut out of the body (the thick-solid offset failed)".into(),
            ShellStrategy::OpenAndClose(k) => {
                let inner = ShellStrategy::from_code(*k as u32);
                format!("opened at the largest flat face and closed again ({})", inner.describe())
            }
        }
    }
}

/// Which part of a shape [`Shape::min_distance`] measures from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubShape {
    Whole,
    /// Face index (as in `faces()` / `FaceInfo`).
    Face(u32),
    /// Edge index (as in `edges()` / `EdgeInfo`).
    Edge(u32),
}

impl SubShape {
    fn raw(self) -> (u32, u32) {
        match self {
            SubShape::Whole => (0, 0),
            SubShape::Face(i) => (1, i),
            SubShape::Edge(i) => (2, i),
        }
    }
}

/// A face or crease likely to stop a shell ([`Shape::shell_blockers`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShellBlocker {
    /// A face narrower than the wall (a ledge or inset ring between two features).
    NarrowFace { face: u32, width: f64 },
    /// Two faces meeting at a shallow angle (neither tangent nor a clear corner).
    Crease { faces: (u32, u32), angle_deg: f64 },
}

impl ShellBlocker {
    /// "; likely blockers: …" and what to change, or "" when there are none. At most
    /// four are listed.
    pub fn summarize(blockers: &[ShellBlocker], thickness: f64, name: impl Fn(u32) -> String) -> String {
        if blockers.is_empty() {
            return String::new();
        }
        let list: Vec<String> = blockers.iter().take(4).map(|b| b.describe(thickness, &name)).collect();
        let more = if blockers.len() > 4 { format!(" (+{} more)", blockers.len() - 4) } else { String::new() };
        let narrow = blockers.iter().any(|b| matches!(b, ShellBlocker::NarrowFace { .. }));
        let crease = blockers.iter().any(|b| matches!(b, ShellBlocker::Crease { .. }));
        let fix = match (narrow, crease) {
            (true, false) => "a step narrower than the wall, often where a joined feature's outline almost matches the body under it: make the outlines match exactly, step out by more than the wall, or use a thinner wall",
            (false, true) => "faces meeting almost flat: make them tangent (same draft) or give the corner a clear angle",
            _ => "narrow steps and almost-flat creases, often where a joined feature's outline almost matches the body under it: make the outlines match exactly or step out by more than the wall",
        };
        format!("; likely blockers: {}{more} ({fix})", list.join("; "))
    }
    /// The faces involved.
    pub fn faces(&self) -> Vec<u32> {
        match *self {
            ShellBlocker::NarrowFace { face, .. } => vec![face],
            ShellBlocker::Crease { faces: (a, b), .. } => vec![a, b],
        }
    }
    /// One clause, with faces named by `name`.
    pub fn describe(&self, thickness: f64, name: impl Fn(u32) -> String) -> String {
        match *self {
            ShellBlocker::NarrowFace { face, width } => {
                format!("{} is only {width:.3} mm wide (the wall is {thickness} mm)", name(face))
            }
            ShellBlocker::Crease { faces: (a, b), angle_deg } => {
                format!("{} meets {} at a shallow {angle_deg:.1}° crease", name(a), name(b))
            }
        }
    }
}

/// [`Shape::min_distance`]'s answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Distance {
    pub distance: f64,
    pub on_self: DVec3,
    pub on_other: DVec3,
    pub inside: bool,
}
