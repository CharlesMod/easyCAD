//! The serializable feature list (source of truth for regeneration).

use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

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
    Revolve {
        sketch: SketchId,
        profiles: ProfileSel,
        axis: Axis,
        angle_deg: Expr,
        op: BoolOp,
        #[serde(default)]
        target: Option<BodyId>,
    },
    Fillet {
        #[serde(default)]
        body: Option<BodyId>,
        edges: Vec<EdgeSelector>,
        radius: Expr,
        /// Apply the largest workable radius on edges that refuse the requested one.
        #[serde(default)]
        auto_reduce: bool,
    },
    Chamfer {
        #[serde(default)]
        body: Option<BodyId>,
        edges: Vec<EdgeSelector>,
        distance: Expr,
        #[serde(default)]
        auto_reduce: bool,
    },
    Shell {
        #[serde(default)]
        body: Option<BodyId>,
        open_faces: Vec<FaceSelector>,
        thickness: Expr,
        #[serde(default)]
        outward: bool,
    },
    Draft {
        #[serde(default)]
        body: Option<BodyId>,
        faces: Vec<FaceSelector>,
        neutral: Plane,
        pull: DVec3,
        angle_deg: Expr,
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
    },
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
    },
}

fn zero() -> Expr {
    Expr::new("0")
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
    /// Short type label for the timeline.
    pub fn label(&self) -> &'static str {
        match self {
            FeatureKind::Extrude { .. } => "Extrude",
            FeatureKind::Revolve { .. } => "Revolve",
            FeatureKind::Fillet { .. } => "Fillet",
            FeatureKind::Chamfer { .. } => "Chamfer",
            FeatureKind::Shell { .. } => "Shell",
            FeatureKind::Draft { .. } => "Draft",
            FeatureKind::Rib { .. } => "Rib",
            FeatureKind::ScrewBoss { .. } => "Screw boss",
            FeatureKind::StandoffsFromPcb { .. } => "Standoffs from PCB",
            FeatureKind::Pattern { .. } => "Pattern",
            FeatureKind::Mirror { .. } => "Mirror",
            FeatureKind::Split { .. } => "Split body",
            FeatureKind::PartingJoint { .. } => "Parting joint",
            FeatureKind::ImportStep { .. } => "Import STEP",
            FeatureKind::InsertPart { .. } => "Insert part",
            FeatureKind::ContextRef { .. } => "In-context reference",
        }
    }
}
