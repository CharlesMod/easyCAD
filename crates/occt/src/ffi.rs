//! Raw cxx bridge. Every fallible call returns `Result<_, cxx::Exception>`; the C++
//! side converts `Standard_Failure` and all other exceptions (see `shim.h`).

#[cxx::bridge(namespace = "cw")]
pub mod bridge {
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    struct V3 {
        x: f64,
        y: f64,
        z: f64,
    }

    /// Placed frame for planar construction.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Frame {
        origin: V3,
        x_dir: V3,
        normal: V3,
    }

    /// One 3D boundary edge for face construction. `kind`: 0 line (p0→p1),
    /// 1 arc through three points (p0 start, p1 mid, p2 end),
    /// 2 full circle (center p0, normal p1, `radius`). `loop_id` 0 is the outer wire,
    /// higher ids are holes; edges of a loop must be contiguous and ordered.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct EdgeSpec {
        kind: u8,
        loop_id: u32,
        p0: V3,
        p1: V3,
        p2: V3,
        radius: f64,
    }

    #[derive(Clone, Debug, PartialEq)]
    struct FaceInfo {
        index: u32,
        /// 0 plane, 1 cylinder, 2 cone, 3 sphere, 4 torus, 5 other.
        kind: u8,
        centroid: V3,
        normal: V3,
        /// Surface point where `normal` was evaluated.
        point: V3,
        area: f64,
        /// Cylinder/cone axis (zero otherwise).
        axis_origin: V3,
        axis_dir: V3,
        radius: f64,
    }

    #[derive(Clone, Debug, PartialEq)]
    struct EdgeInfo {
        index: u32,
        /// 0 line, 1 circle, 2 ellipse, 3 bspline/bezier, 4 other.
        kind: u8,
        midpoint: V3,
        tangent: V3,
        length: f64,
        start: V3,
        end: V3,
        /// Circle center / axis / radius (zero otherwise).
        center: V3,
        axis: V3,
        radius: f64,
        closed: bool,
        /// Indices (into the face map) of up to two adjacent faces, -1 if none.
        face_a: i32,
        face_b: i32,
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    struct MeshData {
        positions: Vec<f32>,
        normals: Vec<f32>,
        indices: Vec<u32>,
        /// Face index (TopExp::MapShapes order) per triangle.
        tri_face: Vec<u32>,
        /// Concatenated edge polyline points (xyz).
        edge_points: Vec<f32>,
        /// Per edge polyline: start offset (in points) into `edge_points`, then count.
        edge_ranges: Vec<u32>,
        /// Edge index per polyline.
        edge_ids: Vec<u32>,
    }

    /// Hidden-line result: polylines in the projector frame (x along `x_dir`,
    /// y along `dir × x_dir`).
    #[derive(Clone, Debug, Default, PartialEq)]
    struct HlrData {
        /// x, y pairs.
        pts: Vec<f64>,
        /// Per polyline: start (in points), count.
        ranges: Vec<u32>,
        /// Per polyline: bit 0 visible; bits 1-2 kind (0 sharp, 1 smooth, 2 outline).
        flags: Vec<u8>,
        /// True if the exact algorithm failed and the polygonal one was used.
        poly_fallback: bool,
    }

    /// A named, coloured part of an XCAF (assembly) STEP file.
    #[derive(Clone, Debug, Default, PartialEq)]
    struct XcafPart {
        name: String,
        r: f32,
        g: f32,
        b: f32,
        has_color: bool,
    }

    /// A product (part or sub-assembly) of an XCAF assembly. `shape` indexes the
    /// direct children of the accompanying compound (-1 for an assembly).
    #[derive(Clone, Debug, Default, PartialEq)]
    struct AsmNode {
        name: String,
        r: f32,
        g: f32,
        b: f32,
        has_color: bool,
        shape: i32,
    }

    /// An instance of node `child` inside assembly node `parent`; its placement is
    /// the 12 doubles at `12 * index` of the accompanying matrix list (row-major 3×4).
    #[derive(Clone, Debug, Default, PartialEq)]
    struct AsmLink {
        parent: u32,
        child: u32,
        name: String,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    struct BBox {
        min: V3,
        max: V3,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    struct MassProps {
        volume: f64,
        area: f64,
        center: V3,
    }

    unsafe extern "C++" {
        include!("shim.h");

        type Shape;

        fn shape_clone(s: &Shape) -> UniquePtr<Shape>;
        fn shape_is_null(s: &Shape) -> bool;
        /// TopAbs_ShapeEnum as integer (0 compound .. 7 vertex).
        fn shape_type(s: &Shape) -> u32;
        fn shape_is_valid(s: &Shape) -> bool;
        fn count_sub(s: &Shape, kind: u32) -> u32;
        fn sub_shape(s: &Shape, kind: u32, index: u32) -> Result<UniquePtr<Shape>>;
        fn compound_new() -> UniquePtr<Shape>;
        fn compound_add(c: Pin<&mut Shape>, s: &Shape) -> Result<()>;
        /// Number of direct children (TopoDS_Iterator).
        fn child_count(s: &Shape) -> u32;
        fn child_at(s: &Shape, i: u32) -> Result<UniquePtr<Shape>>;

        fn make_box(origin: V3, dx: f64, dy: f64, dz: f64) -> Result<UniquePtr<Shape>>;
        fn make_cylinder(base: V3, dir: V3, r: f64, h: f64) -> Result<UniquePtr<Shape>>;
        fn make_cone(base: V3, dir: V3, r1: f64, r2: f64, h: f64) -> Result<UniquePtr<Shape>>;

        fn face_from_edges(frame: &Frame, edges: &[EdgeSpec]) -> Result<UniquePtr<Shape>>;
        fn extrude(s: &Shape, v: V3) -> Result<UniquePtr<Shape>>;
        fn extrude_draft(s: &Shape, v: V3, angle_rad: f64, neutral: &Frame) -> Result<UniquePtr<Shape>>;
        fn taper_prism(s: &Shape, v: V3, inset: f64) -> Result<UniquePtr<Shape>>;
        fn revolve(s: &Shape, origin: V3, dir: V3, angle_rad: f64) -> Result<UniquePtr<Shape>>;

        /// op: 0 fuse, 1 cut, 2 common.
        fn boolean(a: &Shape, b: &Shape, op: u32) -> Result<UniquePtr<Shape>>;
        /// Fuse all direct children of a compound.
        fn fuse_all(c: &Shape) -> Result<UniquePtr<Shape>>;
        fn unify(s: &Shape) -> Result<UniquePtr<Shape>>;
        /// ShapeFix_Shape + UnifySameDomain (merge coplanar faces / collinear edges).
        fn heal(s: &Shape) -> Result<UniquePtr<Shape>>;
        /// Tangent-chain id per edge index (edges meeting with G1 continuity share an id).
        fn tangent_chains(s: &Shape) -> Result<Vec<u32>>;
        /// 1 convex, -1 concave, 0 smooth/boundary/unknown.
        fn edge_convexity(s: &Shape, edge: u32) -> Result<i32>;
        /// Best edge passing within `tol` of `p` whose tangent there is parallel to
        /// `tangent` (ignored if zero). -1 when none.
        fn match_edge(s: &Shape, p: V3, tangent: V3, tol: f64) -> Result<i32>;

        /// Per-edge radii (`radii.len() == edges.len()`).
        fn fillet(s: &Shape, edges: &[u32], radii: &[f64]) -> Result<UniquePtr<Shape>>;
        fn chamfer(s: &Shape, edges: &[u32], dists: &[f64]) -> Result<UniquePtr<Shape>>;
        fn shell(s: &Shape, faces: &[u32], offset: f64) -> Result<UniquePtr<Shape>>;
        fn draft(
            s: &Shape,
            faces: &[u32],
            pull: V3,
            angle_rad: f64,
            neutral: &Frame,
        ) -> Result<UniquePtr<Shape>>;

        fn translate(s: &Shape, v: V3) -> Result<UniquePtr<Shape>>;
        fn rotate(s: &Shape, origin: V3, dir: V3, angle_rad: f64) -> Result<UniquePtr<Shape>>;
        fn mirror(s: &Shape, origin: V3, normal: V3) -> Result<UniquePtr<Shape>>;
        /// Part of `s` on the positive (`above` = true) or negative side of a plane.
        fn half_space_side(s: &Shape, origin: V3, normal: V3, above: bool) -> Result<UniquePtr<Shape>>;

        /// Planar section region(s) of a solid at a plane (solid ∩ plane), as faces.
        fn section_face(s: &Shape, origin: V3, normal: V3) -> Result<UniquePtr<Shape>>;
        /// Uniform 2D offset of planar face(s) (all boundaries; d < 0 shrinks).
        /// Returns a compound of faces; errors if the region vanishes.
        fn offset_face(s: &Shape, d: f64) -> Result<UniquePtr<Shape>>;
        /// Faces bounded by only the outer wire of each input face (holes filled).
        fn fill_holes(s: &Shape) -> Result<UniquePtr<Shape>>;
        /// Point-in-region test on planar face(s) (boundary counts as inside).
        fn face_contains(s: &Shape, p: V3) -> Result<bool>;
        /// Length of the outer wire of the largest face.
        fn outer_wire_length(s: &Shape) -> Result<f64>;
        /// Point and unit tangent at arc-length fraction `t` (0..1) along the outer
        /// wire of the largest face. Returned as [point, tangent].
        fn outer_wire_point(s: &Shape, t: f64) -> Result<Vec<V3>>;
        /// 3D offset of a solid's skin (BRepOffsetAPI_MakeOffsetShape, d < 0 inward).
        fn offset_shape(s: &Shape, d: f64) -> Result<UniquePtr<Shape>>;
        fn faces(s: &Shape) -> Result<Vec<FaceInfo>>;
        fn edges(s: &Shape) -> Result<Vec<EdgeInfo>>;
        fn face_edges(s: &Shape, face: u32) -> Result<Vec<u32>>;
        fn tessellate(s: &Shape, lin_defl: f64, ang_defl_rad: f64) -> Result<MeshData>;

        /// Hidden-line removal looking along `-dir` (dir points to the viewer).
        fn hlr(s: &Shape, dir: V3, x_dir: V3, exact: bool, defl: f64) -> Result<HlrData>;
        fn mass_props(s: &Shape) -> Result<MassProps>;
        fn bounding_box(s: &Shape) -> Result<BBox>;
        fn extent_along(s: &Shape, origin: V3, dir: V3) -> Result<Vec<f64>>;

        /// STEP with assembly structure, names and colours. Returns a compound whose
        /// direct children are the leaf solids (placed), in the order of `parts`.
        fn read_step_xcaf(data: &str, parts: &mut Vec<XcafPart>) -> Result<UniquePtr<Shape>>;
        /// STEP writer keeping a name and colour per direct child of `c`.
        fn write_step_xcaf(c: &Shape, parts: &[XcafPart]) -> Result<String>;
        /// For each face of `c` (MapShapes order), the index of the direct child owning it.
        fn face_owners(c: &Shape) -> Result<Vec<u32>>;
        /// Write an XCAF product structure: `nodes` (parts reference children of
        /// `shapes`), `links` (instances with placements in `mats`), free `roots`.
        fn write_step_asm(shapes: &Shape, nodes: &[AsmNode], links: &[AsmLink], mats: &[f64], roots: &[u32]) -> Result<String>;
        /// Read the product structure back (inverse of `write_step_asm`); returns the
        /// compound of part shapes (unplaced) indexed by `AsmNode::shape`.
        fn read_step_asm(
            data: &str,
            nodes: &mut Vec<AsmNode>,
            links: &mut Vec<AsmLink>,
            mats: &mut Vec<f64>,
            roots: &mut Vec<u32>,
        ) -> Result<UniquePtr<Shape>>;
        fn read_step_file(path: &str) -> Result<UniquePtr<Shape>>;
        fn read_step_str(data: &str) -> Result<UniquePtr<Shape>>;
        fn write_step_file(s: &Shape, path: &str) -> Result<()>;
        fn write_step_str(s: &Shape) -> Result<String>;
        fn write_stl_file(s: &Shape, path: &str, lin_defl: f64, ascii: bool) -> Result<()>;
    }
}

// SAFETY: `TopoDS_Shape` is a ref-counted handle to topology (OCCT uses atomic ref
// counts). No shim entry point writes into an input's topology: operations that
// would (meshing, ShapeFix healing, unify, offsets, shell, draft, fillet/chamfer)
// run on a private `BRepBuilderAPI_Copy` of their input, and booleans run in
// non-destructive mode. Process-global STEP/XCAF state is behind a lock in the shim.
// See "Concurrency contract" in docs/ENGINE.md and tests/concurrency.rs (model).
unsafe impl Send for bridge::Shape {}
unsafe impl Sync for bridge::Shape {}
