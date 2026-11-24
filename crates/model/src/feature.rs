//! The serializable feature list (source of truth for regeneration).

use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

use crate::datum::{AxisDef, PlaneDef, PointDef};
use crate::geom::{Axis, Plane};
use crate::library::HoleChoice;
use crate::params::Expr;
use crate::select::{EdgeSelector, FaceSelector};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FeatureId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SketchId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ImportId(pub u32);

/// A body is named by the feature that created it plus an index, so ids survive
/// edits to unrelated features.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BodyId {
    pub feature: FeatureId,
    pub index: u32,
}

impl BodyId {
    pub fn of(feature: FeatureId) -> Self {
        BodyId { feature, index: 0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoolOp {
    NewBody,
    Join,
    Cut,
    Intersect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ProfileSel {
    /// Every closed region of the sketch.
    All,
    /// By index into `Sketch::profiles()`.
    Indices(Vec<usize>),
    /// The regions containing these sketch-plane points (stable under edits).
    Containing(Vec<DVec2>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RibExtent {
    #[default]
    Height,
    ToBody,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PatternKind {
    Linear { dir: DVec3, spacing: Expr, count: Expr },
    /// `total_deg` 360 spaces copies evenly.
    Circular { axis: Axis, count: Expr, total_deg: Expr },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PatternSource {
    /// Re-apply the tool bodies of earlier features (Fusion's "pattern features").
    Features(Vec<FeatureId>),
    /// Copy a whole body and join the copies to it.
    Body(BodyId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PcbSource {
    /// Circles on DXF layers whose diameter is in `[min_dia, max_dia]`; board
    /// coordinates are shifted by `offset` into the boss plane.
    Dxf { import: ImportId, layers: Vec<String>, min_dia: f64, max_dia: f64, offset: DVec2 },
    /// Holes in the board of an imported STEP (circular edges of the largest planar
    /// face parallel to the boss plane), projected onto the plane.
    Step { import: ImportId, min_dia: f64, max_dia: f64 },
}

/// Where a library part goes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlacementRef {
    /// Snap to a circular edge: its centre and axis; the part's +Z points out of the
    /// planar face the edge bounds.
    CircularEdge { edge: EdgeSelector },
    /// A point on a face; +Z is the face's outward normal there.
    PointOnFace { face: FaceSelector, point: DVec3 },
    Explicit { origin: DVec3, axis: DVec3 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartRef {
    pub source: String,
    pub id: String,
    pub size: String,
    /// Display name / BOM metadata snapshot (so BOMs work without the library).
    pub name: String,
    #[serde(default)]
    pub part_number: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    /// Embedded STEP (sha256 of a document blob) for parts from local libraries.
    #[serde(default)]
    pub blob: Option<String>,
    /// Mate metadata for embedded STEP parts.
    #[serde(default)]
    pub thread: Option<String>,
}

/// How screw threads are drawn. `Cosmetic` (the default) keeps plain cylinders at
/// the nominal or tap-drill diameter: small printed threads come out better tapped or
/// thread-formed, and files stay small. `Modeled` builds ISO 68-1 / UN basic-profile
/// helical threads (`occt::Thread`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Threads {
    #[default]
    Cosmetic,
    Modeled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutoHole {
    pub choice: HoleChoice,
    /// Overrides the default depth (blind holes); `None` = rule default / through.
    #[serde(default)]
    pub depth: Option<Expr>,
    /// Process fit for the hole diameters (default: Hole / Snug).
    #[serde(default)]
    pub fit: Option<crate::process::Fit>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // stored once per feature; size is irrelevant
pub enum FeatureKind {
    Extrude {
        sketch: SketchId,
        profiles: ProfileSel,
        distance: Expr,
        #[serde(default)]
        symmetric: bool,
        /// Extrude against the sketch normal.
        #[serde(default)]
        reverse: bool,
        /// Taper in degrees (positive shrinks away from the sketch plane).
        #[serde(default = "zero")]
        draft_deg: Expr,
        op: BoolOp,
        /// Body for Join/Cut/Intersect (`None` = the last solid body).
        #[serde(default)]
        target: Option<BodyId>,
    },
    /// Extrude an existing planar face of a body: the face (with its holes) is the
    /// profile, its outward normal is +direction. `distance` is signed along that
    /// normal: positive grows out of the body, negative goes into it.
    ExtrudeFace {
        face: FaceSelector,
        distance: Expr,
        op: BoolOp,
        /// Body the face belongs to and the op acts on (`None` = the last solid body).
        #[serde(default)]
        target: Option<BodyId>,
    },
    Revolve {
        sketch: SketchId,
        profiles: ProfileSel,
        axis: Axis,
        angle_deg: Expr,
        op: BoolOp,
        #[serde(default)]
        target: Option<BodyId>,
    },
    /// Edge sets: `(edges, radius, auto_reduce)` (when `edges` is not empty) followed
    /// by `sets`, each with its own radius; see [`FeatureKind::edge_sets`].
    Fillet {
        #[serde(default)]
        body: Option<BodyId>,
        edges: Vec<EdgeSelector>,
        radius: Expr,
        /// Apply the largest workable radius on edges that refuse the requested one.
        #[serde(default)]
        auto_reduce: bool,
        /// More edge sets, each with its own radius.
        #[serde(default)]
        sets: Vec<EdgeSet>,
        /// Edges the user chose to skip (left sharp, listed in the report).
        /// (Documents saved with the former `setback` flag still load: unknown
        /// fields are ignored. OpenCascade has no setback vertex blend; see TODO.md.)
        #[serde(default)]
        skipped: Vec<EdgeSelector>,
    },
    Chamfer {
        #[serde(default)]
        body: Option<BodyId>,
        edges: Vec<EdgeSelector>,
        distance: Expr,
        #[serde(default)]
        auto_reduce: bool,
        #[serde(default)]
        sets: Vec<EdgeSet>,
        #[serde(default)]
        skipped: Vec<EdgeSelector>,
    },
    Shell {
        #[serde(default)]
        body: Option<BodyId>,
        open_faces: Vec<FaceSelector>,
        thickness: Expr,
        #[serde(default)]
        outward: bool,
    },
    /// Delete faces and heal the gap from their neighbours (fusion-style): a fillet face
    /// gives back the sharp edge, a boss's faces give back the flat, a hole wall closes it.
    DeleteFace {
        #[serde(default)]
        body: Option<BodyId>,
        faces: Vec<FaceSelector>,
    },
    Draft {
        #[serde(default)]
        body: Option<BodyId>,
        faces: Vec<FaceSelector>,
        neutral: Plane,
        pull: DVec3,
        angle_deg: Expr,
        /// Moves `neutral` along its normal (param expression).
        #[serde(default = "zero")]
        offset: Expr,
    },
    /// Thin wall grown from the sketch's open chains along the sketch normal.
    Rib {
        sketch: SketchId,
        thickness: Expr,
        height: Expr,
        #[serde(default = "zero")]
        draft_deg: Expr,
        #[serde(default)]
        flip: bool,
        #[serde(default)]
        body: Option<BodyId>,
        /// `Height` uses `height`; `ToBody` grows the rib along its direction until it
        /// meets the body and trims it there.
        #[serde(default)]
        extent: RibExtent,
    },
    ScrewBoss {
        #[serde(default)]
        body: Option<BodyId>,
        plane: Plane,
        points: Vec<DVec2>,
        od: Expr,
        hole_dia: Expr,
        height: Expr,
        /// Hole depth from the boss top (`None` = full height).
        #[serde(default)]
        hole_depth: Option<Expr>,
        #[serde(default = "zero")]
        base_fillet: Expr,
        #[serde(default = "zero")]
        draft_deg: Expr,
        /// Moves `plane` along its normal (param expression).
        #[serde(default = "zero")]
        offset: Expr,
    },
    StandoffsFromPcb {
        #[serde(default)]
        body: Option<BodyId>,
        source: PcbSource,
        plane: Plane,
        od: Expr,
        hole_dia: Expr,
        height: Expr,
        #[serde(default = "zero")]
        base_fillet: Expr,
        #[serde(default = "zero")]
        draft_deg: Expr,
        /// Moves `plane` along its normal (param expression).
        #[serde(default = "zero")]
        offset: Expr,
        /// Board holes left without a standoff: hole centres in plane coordinates
        /// (after the source offset), matched within 0.5 mm.
        #[serde(default)]
        exclude: Vec<DVec2>,
        /// Ribs from each standoff to the nearest wall of the body.
        #[serde(default)]
        gussets: Option<Gussets>,
        /// Matching holes through another body (the lid), counterbored for the
        /// screw head, coaxial with each standoff.
        #[serde(default)]
        lid_holes: Option<LidHoles>,
    },
    Pattern {
        source: PatternSource,
        kind: PatternKind,
    },
    Mirror {
        source: PatternSource,
        plane: Plane,
    },
    /// Split a body by a plane. The body keeps the side below the plane (against the
    /// normal); the side above becomes a new body named `new_name`.
    Split {
        #[serde(default)]
        body: Option<BodyId>,
        plane: Plane,
        new_name: String,
        /// Moves `plane` along its normal (param expression, e.g. `"lid_split"`).
        #[serde(default = "zero")]
        offset: Expr,
        /// Which side(s) to keep. `Below` / `Above` discard the other side (e.g. a
        /// flat bottom: split at the base plane, keep `Above`); `new_name` is then
        /// unused. Works on mesh bodies too.
        #[serde(default)]
        keep: SplitKeep,
    },
    /// Split a (shelled) body at a plane into two halves with a mating joint.
    /// The body keeps the lower half; the upper half becomes `BodyId::of(feature)`.
    PartingJoint {
        #[serde(default)]
        body: Option<BodyId>,
        plane: Plane,
        /// Moves the plane along its normal (param expression, e.g. `"split_z"`).
        #[serde(default = "zero")]
        offset: Expr,
        kind: crate::joint::JointKind,
        #[serde(default)]
        rim: Option<crate::joint::Rim>,
        upper_name: String,
        /// Fit class used for the joint's Hole/Pin/Gap dimensions.
        #[serde(default = "slide")]
        class: crate::process::FitClass,
        /// Closing radius used to bridge wall cutouts (default and minimum 12 mm).
        #[serde(default)]
        closing_radius: Option<Expr>,
    },
    /// Bring an imported STEP into the model. `reference` bodies (e.g. a PCB) are
    /// shown and exported to STEP but never booleaned or printed.
    ImportStep {
        import: ImportId,
        #[serde(default = "yes")]
        reference: bool,
        #[serde(default)]
        translate: DVec3,
    },
    /// Bring an imported triangle mesh (STL / OBJ / GLB / glTF) in as a mesh body:
    /// a closed triangle mesh that can be moved, split and combined with solid
    /// bodies (see `MeshBoolean`), but not filleted, shelled or drafted. Applied in
    /// order: up-axis conversion, `scale` about the file origin, `rotate_deg` (XYZ
    /// Euler), `translate`. With everything at its default the mesh lands exactly
    /// as stored in the file (file units taken as mm).
    ImportMesh {
        import: ImportId,
        #[serde(default = "one")]
        scale: Expr,
        /// The file's up axis. `Y` (glTF / most game and AI-generated meshes) is
        /// turned so it points along +Z.
        #[serde(default)]
        up: MeshUp,
        #[serde(default)]
        rotate_deg: DVec3,
        #[serde(default = "zero3")]
        translate: [Expr; 3],
        /// Shown and exported but never booleaned or printed.
        #[serde(default)]
        reference: bool,
    },
    /// Move, rotate, scale or copy a body (solid or mesh). Applied in order: `scale`
    /// about `pivot` (mesh bodies only), `rotate_deg` (XYZ Euler) about `pivot`,
    /// `translate`. With `copy` the original stays and the moved copy is a new
    /// body `new_name`.
    TransformBody {
        body: BodyId,
        #[serde(default = "zero3")]
        translate: [Expr; 3],
        #[serde(default)]
        rotate_deg: DVec3,
        #[serde(default)]
        pivot: DVec3,
        #[serde(default = "one")]
        scale: Expr,
        #[serde(default)]
        copy: bool,
        #[serde(default)]
        new_name: String,
    },
    /// Boolean between bodies where either side may be a mesh body (Manifold
    /// kernel; solid operands are tessellated at print quality). The target
    /// becomes a mesh body unless every operand is a solid.
    ///
    /// `clearance` grows the tools for `Cut` (the target ends up at least that far
    /// from them) and shrinks them for `Intersect` (the target is trimmed to stay
    /// that far inside them, e.g. a sliding fit in a cavity).
    ///
    /// `positions`: the target's positions to clear, as offsets from where it is
    /// (e.g. `[[0,0,0],[0,0,14]]` for a part that travels 14 mm up). The tools
    /// are applied at each position (moved by the negated offset): `Cut` removes
    /// everything that would hit a tool at any position, `Intersect` keeps only
    /// what is inside the tools at every position. Empty = `[[0,0,0]]`.
    MeshBoolean {
        #[serde(default)]
        target: Option<BodyId>,
        tools: Vec<BodyId>,
        op: BoolOp,
        #[serde(default = "zero")]
        clearance: Expr,
        #[serde(default)]
        positions: Vec<[Expr; 3]>,
        /// Keep the tool bodies (default). `false` removes them from the model.
        #[serde(default = "yes")]
        keep_tools: bool,
    },
    /// Place a library part as a linked reference body at one or more placements,
    /// optionally cutting the matching hole into a target body.
    InsertPart {
        part: PartRef,
        placements: Vec<PlacementRef>,
        /// Flip the part axis.
        #[serde(default)]
        flip: bool,
        /// Offset along the part axis (e.g. to sit a screw head on a boss top).
        #[serde(default = "zero")]
        offset: Expr,
        #[serde(default)]
        auto_hole: Option<AutoHole>,
        #[serde(default)]
        target: Option<BodyId>,
        /// Draw the part's threads (and a tapped auto-hole) as real helical
        /// geometry instead of plain cylinders.
        #[serde(default)]
        threads: Threads,
    },
    /// Construction plane (see `crate::datum`). Makes no body and consumes no sketch.
    DatumPlane {
        def: PlaneDef,
        /// Reverse the plane's normal.
        #[serde(default)]
        flip: bool,
    },
    /// Construction axis.
    DatumAxis { def: AxisDef },
    /// Construction point.
    DatumPoint { def: PointDef },
    /// In-context reference: a body of another occurrence, resolved after that
    /// component regenerates and placed into this component's frame (the frame of
    /// this component's first occurrence). Used as a new body, a reference body, or
    /// a tool for Join/Cut/Intersect on `target`.
    ContextRef {
        /// Occurrence path from the root (`[]` = the root component's own bodies).
        source: Vec<crate::assembly::OccurrenceId>,
        /// Body name in the source component (`None` = its first solid body).
        #[serde(default)]
        body: Option<String>,
        op: BoolOp,
        #[serde(default)]
        target: Option<BodyId>,
        /// 3D offset of the referenced body before use (e.g. a clearance; mm).
        #[serde(default = "zero")]
        offset: Expr,
        /// With `NewBody`: add it as a reference body (shown, never printed).
        #[serde(default)]
        reference: bool,
        /// `None`: `source` starts at the document root. `Some(c)`: `source` starts
        /// at component `c`'s context occurrence (used inside linked assemblies).
        #[serde(default)]
        root: Option<crate::assembly::ComponentId>,
    },
}

fn zero() -> Expr {
    Expr::new("0")
}
fn one() -> Expr {
    Expr::new("1")
}
fn zero3() -> [Expr; 3] {
    [zero(), zero(), zero()]
}

/// Up axis of an imported mesh file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeshUp {
    /// Use the file's coordinates as they are.
    #[default]
    Z,
    /// The file is Y-up (glTF convention): rotate +90° about X so +Y becomes +Z.
    Y,
}

/// Which side(s) of a `Split` to keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitKeep {
    /// Below stays in the body, above becomes `new_name`.
    #[default]
    Both,
    Below,
    Above,
}

/// One set of edges with its own size (fillet radius / chamfer distance).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeSet {
    pub edges: Vec<EdgeSelector>,
    pub size: Expr,
    #[serde(default)]
    pub auto_reduce: bool,
}

/// Gusset ribs from a standoff to the nearest wall.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gussets {
    pub thickness: Expr,
    /// Height up the standoff (from the plane); `None` = 60 % of the standoff height.
    #[serde(default)]
    pub height: Option<Expr>,
    /// Longest gusset (mm from the standoff axis); walls farther away get none.
    #[serde(default = "gusset_reach")]
    pub max_length: Expr,
}

fn gusset_reach() -> Expr {
    Expr::new("25")
}

/// Counterbored screw holes through the lid, coaxial with the standoffs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LidHoles {
    /// The lid body if it already exists at this point of the history; `None`
    /// (usual: standoffs come before the parting joint) cuts the material above
    /// each standoff in the target body, so the holes end up in the lid after the
    /// split.
    #[serde(default)]
    pub body: Option<BodyId>,
    /// Clearance hole (default fit Hole/Snug).
    pub hole_dia: Expr,
    pub cbore_dia: Expr,
    pub cbore_depth: Expr,
}
fn slide() -> crate::process::FitClass {
    crate::process::FitClass::Slide
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    pub id: FeatureId,
    pub name: String,
    #[serde(default)]
    pub suppressed: bool,
    pub kind: FeatureKind,
}

impl FeatureKind {
    /// Fillet/chamfer edge sets in order: `(edges, radius/distance, auto_reduce)`
    /// first when `edges` is not empty, then `sets`. Empty for other features.
    /// `FeatureReport.edge_requests[k].set` indexes this list.
    pub fn edge_sets(&self) -> Vec<EdgeSet> {
        let (edges, size, auto_reduce, sets) = match self {
            FeatureKind::Fillet { edges, radius, auto_reduce, sets, .. } => (edges, radius, auto_reduce, sets),
            FeatureKind::Chamfer { edges, distance, auto_reduce, sets, .. } => (edges, distance, auto_reduce, sets),
            _ => return Vec::new(),
        };
        let mut v = Vec::new();
        if !edges.is_empty() {
            v.push(EdgeSet { edges: edges.clone(), size: size.clone(), auto_reduce: *auto_reduce });
        }
        v.extend(sets.iter().cloned());
        v
    }

    /// The fit a dimension field gets when its expression carries none
    /// (GUI_NEEDS #6). Field names are the struct field names; joint fields are
    /// `kind.<field>` (e.g. `kind.width`), `compression_rib.width`, and
    /// `auto_hole` / `lid_holes.hole_dia`.
    pub fn default_fit(&self, field: &str) -> Option<crate::process::Fit> {
        use crate::joint::JointKind as J;
        use crate::process::{Fit, FitClass};
        match (self, field) {
            (FeatureKind::ScrewBoss { .. } | FeatureKind::StandoffsFromPcb { .. }, "hole_dia") => Some(Fit::hole(FitClass::Snug)),
            (FeatureKind::StandoffsFromPcb { .. }, "lid_holes.hole_dia") => Some(Fit::hole(FitClass::Snug)),
            (FeatureKind::InsertPart { .. }, "auto_hole") => Some(Fit::hole(FitClass::Snug)),
            (FeatureKind::PartingJoint { kind, class, .. }, f) => {
                let c = *class;
                match (kind, f.strip_prefix("kind.").unwrap_or(f)) {
                    (J::TongueGroove { .. } | J::Lip { .. }, "width") => Some(Fit::pin(c)),
                    (J::TongueGroove { .. }, "extra_depth") => Some(Fit::gap(c)),
                    (J::TongueGroove { .. } | J::Lip { .. }, "groove" | "recess") => Some(Fit::hole(c)),
                    (J::ORingGland { .. }, "width" | "depth") => Some(Fit::hole(c)),
                    (J::ORingGland { .. }, "compression_rib.width") => Some(Fit::pin(c)),
                    (J::SnapFit { .. }, "undercut") => Some(Fit::pin(c)),
                    (J::SnapFit { .. }, "clearance") => Some(Fit::gap(c)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Short type label for the timeline.
    pub fn label(&self) -> &'static str {
        match self {
            FeatureKind::Extrude { .. } => "Extrude",
            FeatureKind::ExtrudeFace { .. } => "Extrude face",
            FeatureKind::Revolve { .. } => "Revolve",
            FeatureKind::Fillet { .. } => "Fillet",
            FeatureKind::Chamfer { .. } => "Chamfer",
            FeatureKind::Shell { .. } => "Shell",
            FeatureKind::Draft { .. } => "Draft",
            FeatureKind::DeleteFace { .. } => "Delete face",
            FeatureKind::Rib { .. } => "Rib",
            FeatureKind::ScrewBoss { .. } => "Screw boss",
            FeatureKind::StandoffsFromPcb { .. } => "Standoffs from PCB",
            FeatureKind::Pattern { .. } => "Pattern",
            FeatureKind::Mirror { .. } => "Mirror",
            FeatureKind::Split { .. } => "Split body",
            FeatureKind::PartingJoint { .. } => "Parting joint",
            FeatureKind::ImportStep { .. } => "Import STEP",
            FeatureKind::ImportMesh { .. } => "Import mesh",
            FeatureKind::TransformBody { copy: true, .. } => "Copy body",
            FeatureKind::TransformBody { .. } => "Move body",
            FeatureKind::MeshBoolean { .. } => "Combine",
            FeatureKind::InsertPart { .. } => "Insert part",
            FeatureKind::ContextRef { .. } => "In-context reference",
            FeatureKind::DatumPlane { .. } => "Construction plane",
            FeatureKind::DatumAxis { .. } => "Construction axis",
            FeatureKind::DatumPoint { .. } => "Construction point",
        }
    }
}

impl FeatureKind {
    /// True for construction planes/axes/points.
    pub fn is_datum(&self) -> bool {
        matches!(self, FeatureKind::DatumPlane { .. } | FeatureKind::DatumAxis { .. } | FeatureKind::DatumPoint { .. })
    }

    /// The sketch this feature builds from, if any.
    pub fn sketch(&self) -> Option<SketchId> {
        match self {
            FeatureKind::Extrude { sketch, .. }
            | FeatureKind::Revolve { sketch, .. }
            | FeatureKind::Rib { sketch, .. } => Some(*sketch),
            _ => None,
        }
    }
}
