//! 2D drawings: sheets with a title block bound to document params/metadata, views
//! (cube face, iso corner or arbitrary rotation, plus in-sheet rotation, scale and a
//! hidden-line toggle) computed with OCCT hidden-line removal, associative
//! annotations, and SVG / DXF / PDF export.
//!
//! Annotations reference model geometry through `(view, DimRef)`, where `DimRef`
//! holds a geometric edge selector or a vertex position, never a raw index. They are
//! re-resolved on every computation. When a reference disappears the annotation is
//! reported `Dangling` and stays in the document.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{DMat3, DQuat, DVec2, DVec3};
use occt::{CurveKind, EdgeInfo, HlrKind};
use serde::{Deserialize, Serialize};

use crate::assembly::{AssemblyBom, AssemblyResult, OccPath, Scope, ScopeItem, Transform};
use crate::document::{BomLine, DocData};
use crate::feature::{BodyId, FeatureId, FeatureKind};
use crate::params::Values;
use crate::process::CubeFace;
use crate::regen::{Body, BodyGeom, RegenResult};
use crate::select::EdgeSelector;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DrawingId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ViewId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AnnotationId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaperSize {
    A4,
    A3,
    A2,
    A1,
    A0,
    AnsiA,
    AnsiB,
    AnsiC,
    AnsiD,
    AnsiE,
}

impl PaperSize {
    /// Landscape (width, height) in mm.
    pub fn landscape_mm(self) -> (f64, f64) {
        match self {
            PaperSize::A4 => (297.0, 210.0),
            PaperSize::A3 => (420.0, 297.0),
            PaperSize::A2 => (594.0, 420.0),
            PaperSize::A1 => (841.0, 594.0),
            PaperSize::A0 => (1189.0, 841.0),
            PaperSize::AnsiA => (279.4, 215.9),
            PaperSize::AnsiB => (431.8, 279.4),
            PaperSize::AnsiC => (558.8, 431.8),
            PaperSize::AnsiD => (863.6, 558.8),
            PaperSize::AnsiE => (1117.6, 863.6),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaperOrientation {
    Landscape,
    Portrait,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitleField {
    pub label: String,
    /// Text with `{param}`, `{meta.key}`, `{doc.name}`, `{doc.revision}`, `{sheet.scale}`.
    pub template: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitleBlock {
    pub fields: Vec<TitleField>,
}

impl Default for TitleBlock {
    fn default() -> Self {
        let f = |l: &str, t: &str| TitleField { label: l.into(), template: t.into() };
        TitleBlock {
            fields: vec![
                f("Title", "{doc.name}"),
                f("Part no.", "{meta.part_number}"),
                f("Material", "{meta.material}"),
                f("Drawn", "{meta.author}"),
                f("Scale", "{sheet.scale}"),
                f("Rev", "{doc.revision}"),
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    pub size: PaperSize,
    pub orientation: PaperOrientation,
    #[serde(default = "ten")]
    pub margin: f64,
    #[serde(default)]
    pub title_block: TitleBlock,
}

fn ten() -> f64 {
    10.0
}

impl Sheet {
    pub fn new(size: PaperSize, orientation: PaperOrientation) -> Self {
        Sheet { size, orientation, margin: 10.0, title_block: TitleBlock::default() }
    }
    pub fn size_mm(&self) -> DVec2 {
        let (w, h) = self.size.landscape_mm();
        match self.orientation {
            PaperOrientation::Landscape => DVec2::new(w, h),
            PaperOrientation::Portrait => DVec2::new(h, w),
        }
    }
}

/// An iso corner given by the signs of the direction toward the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsoCorner {
    pub x: i8,
    pub y: i8,
    pub z: i8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ViewOrientation {
    /// Look at this cube face (its normal points to the viewer). Front = `NegY`.
    Face(CubeFace),
    Iso(IsoCorner),
    /// Camera rotation: model → view coordinates (view +Z toward the viewer,
    /// +X right, +Y up).
    Rotation(DQuat),
}

impl ViewOrientation {
    /// (toward viewer, sheet-right, sheet-up) in model coordinates.
    pub fn basis(&self) -> (DVec3, DVec3, DVec3) {
        let (n, up) = match *self {
            ViewOrientation::Face(f) => {
                let n = f.dir();
                let up = match f {
                    CubeFace::PosZ => DVec3::Y,
                    CubeFace::NegZ => DVec3::NEG_Y,
                    _ => DVec3::Z,
                };
                (n, up)
            }
            ViewOrientation::Iso(c) => {
                let n = DVec3::new(c.x.signum() as f64, c.y.signum() as f64, c.z.signum() as f64).normalize();
                let up = (DVec3::Z - n * n.z).normalize();
                (n, up)
            }
            ViewOrientation::Rotation(q) => {
                let inv = q.inverse();
                (inv * DVec3::Z, inv * DVec3::Y)
            }
        };
        let x = up.cross(n).normalize();
        (n, x, n.cross(x))
    }
    /// The model → view rotation (what `Rotation` holds) of any orientation.
    pub fn quat(&self) -> DQuat {
        let (n, x, up) = self.basis();
        DQuat::from_mat3(&DMat3::from_cols(x, up, n)).inverse().normalize()
    }
}

/// The cutting plane of a section view. The view keeps the material behind the
/// plane (opposite `normal`) and looks at the cut along `-normal`; the parent
/// view, which must see the plane edge-on, shows the cutting line with arrows
/// and `label` at both ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SectionCut {
    pub parent: ViewId,
    /// A point of the plane and its normal toward the viewer, in the view's scope
    /// frame (model coordinates for root views).
    pub origin: DVec3,
    pub normal: DVec3,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: ViewId,
    pub name: String,
    /// Bodies shown (empty = all solid, non-reference bodies).
    #[serde(default)]
    pub bodies: Vec<BodyId>,
    pub orientation: ViewOrientation,
    /// Rotation of the projected view on the sheet (degrees, CCW).
    #[serde(default)]
    pub rotation_deg: f64,
    pub scale: f64,
    /// Centre of the view on the sheet (mm).
    pub position: DVec2,
    #[serde(default)]
    pub hidden_lines: bool,
    /// Exact HLR (default) or the fast polygonal algorithm.
    #[serde(default = "yes")]
    pub exact: bool,
    /// Draw tangent (smooth) edges.
    #[serde(default)]
    pub smooth_edges: bool,
    /// What the view shows: the root's bodies (default), the whole assembly, one
    /// component or one occurrence (see [`Scope`]). Non-root scopes draw every
    /// visible body including reference bodies (purchased parts) and hide lines
    /// between parts; `bodies` then filters by body id in every component.
    #[serde(default)]
    pub scope: Scope,
    /// Non-root scopes: apply the occurrences' explode offsets.
    #[serde(default)]
    pub exploded: bool,
    /// A section view: the bodies cut by this plane, cut faces hatched.
    #[serde(default)]
    pub section: Option<SectionCut>,
}

fn yes() -> bool {
    true
}

/// A model reference used by annotations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DimRef {
    Edge { body: BodyId, edge: EdgeSelector },
    /// A model vertex (edge endpoint) near this point.
    Vertex { body: BodyId, near: DVec3 },
    /// `base` (an `Edge` or `Vertex`, in its component's frame) on the instance at
    /// `path` of an assembly-scoped view (path relative to the view's scope).
    Placed { path: OccPath, base: Box<DimRef> },
}

impl DimRef {
    /// The reference for an edge of a computed view (GUI_NEEDS #16): an edge hint
    /// selector on the source edge, `Placed` when the view shows an assembly.
    /// `None` for silhouettes (no source edge).
    pub fn from_view_edge(e: &ViewEdge) -> Option<DimRef> {
        e.reference.clone()
    }
    /// Wrap a body reference for the instance at `path` (`[]` = unchanged).
    pub fn placed(path: OccPath, base: DimRef) -> DimRef {
        if path.is_empty() {
            base
        } else {
            DimRef::Placed { path, base: Box::new(base) }
        }
    }
    fn split(&self) -> (&[crate::assembly::OccurrenceId], &DimRef) {
        match self {
            DimRef::Placed { path, base } => (path, base),
            r => (&[], r),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinearKind {
    Horizontal,
    Vertical,
    Aligned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SheetAxis {
    X,
    Y,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AnnotationKind {
    /// Between two references, or the length of one line edge (`b = None`).
    Linear { view: ViewId, a: DimRef, b: Option<DimRef>, kind: LinearKind, offset: f64 },
    Ordinate { view: ViewId, origin: DimRef, target: DimRef, axis: SheetAxis, offset: f64 },
    Radius { view: ViewId, circle: DimRef, diameter: bool, leader_deg: f64 },
    Angle { view: ViewId, a: DimRef, b: DimRef, radius: f64 },
    Chamfer { view: ViewId, edge: DimRef, offset: DVec2 },
    /// Free text with live `{param}` substitution.
    Note { position: DVec2, text: String, height: f64 },
    Leader { view: ViewId, target: DimRef, text: String, position: DVec2 },
    /// Balloon pointing at a library-part body, numbered by its BOM row.
    Balloon { view: ViewId, body: BodyId, position: DVec2 },
    CenterMark { view: ViewId, circle: DimRef },
    Centerline { view: ViewId, a: DimRef, b: DimRef },
    /// The parting plane of a `PartingJoint` / `Split` feature, seen edge-on.
    PartingLine { view: ViewId, feature: FeatureId },
    /// Balloon numbered by the flat assembly BOM (`bom_tree().flat` row + 1):
    /// the occurrence at `path` (relative to the view's scope), or, with `body`, that
    /// body of it (a library-part body gets its part's row).
    ItemBalloon { view: ViewId, path: OccPath, body: Option<BodyId>, position: DVec2 },
    /// Bill of materials table (item, qty, name, kind / part number) from the
    /// assembly BOM; `position` is its top-left corner on the sheet. Items are the
    /// flat BOM rows (the numbers `ItemBalloon`s show); `indented` lists the tree
    /// with per-parent quantities instead.
    BomTable { position: DVec2, indented: bool },
    /// GD&T feature control frame (ASME Y14.5 / ISO 1101): the characteristic's
    /// symbol, the tolerance (`{param}`s allowed, ⌀ and a material condition
    /// optional) and the datum references, in boxes whose left middle is at
    /// `position`; a leader runs to `target` when set.
    FeatureControl {
        view: ViewId,
        target: Option<DimRef>,
        position: DVec2,
        symbol: GdtSymbol,
        tolerance: String,
        diameter: bool,
        modifier: Option<MaterialCondition>,
        datums: Vec<String>,
    },
    /// Datum feature symbol: `label` in a box centred at `position`, joined to a
    /// filled triangle standing on `target`.
    Datum { view: ViewId, target: DimRef, label: String, position: DVec2 },
}

/// The geometric characteristics of a feature control frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GdtSymbol {
    Straightness,
    Flatness,
    Circularity,
    Cylindricity,
    ProfileOfLine,
    ProfileOfSurface,
    Angularity,
    Perpendicularity,
    Parallelism,
    Position,
    Concentricity,
    Symmetry,
    CircularRunout,
    TotalRunout,
}

impl GdtSymbol {
    pub const ALL: [GdtSymbol; 14] = [
        GdtSymbol::Straightness,
        GdtSymbol::Flatness,
        GdtSymbol::Circularity,
        GdtSymbol::Cylindricity,
        GdtSymbol::ProfileOfLine,
        GdtSymbol::ProfileOfSurface,
        GdtSymbol::Angularity,
        GdtSymbol::Perpendicularity,
        GdtSymbol::Parallelism,
        GdtSymbol::Position,
        GdtSymbol::Concentricity,
        GdtSymbol::Symmetry,
        GdtSymbol::CircularRunout,
        GdtSymbol::TotalRunout,
    ];
    /// The Unicode character (for lists and `AnnotationResult::text`; the sheet
    /// draws the symbol as lines, which every export and font can show).
    pub fn glyph(self) -> &'static str {
        match self {
            GdtSymbol::Straightness => "⏤",
            GdtSymbol::Flatness => "⏥",
            GdtSymbol::Circularity => "○",
            GdtSymbol::Cylindricity => "⌭",
            GdtSymbol::ProfileOfLine => "⌒",
            GdtSymbol::ProfileOfSurface => "⌓",
            GdtSymbol::Angularity => "∠",
            GdtSymbol::Perpendicularity => "⟂",
            GdtSymbol::Parallelism => "∥",
            GdtSymbol::Position => "⌖",
            GdtSymbol::Concentricity => "◎",
            GdtSymbol::Symmetry => "⌯",
            GdtSymbol::CircularRunout => "↗",
            GdtSymbol::TotalRunout => "⌰",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            GdtSymbol::Straightness => "Straightness",
            GdtSymbol::Flatness => "Flatness",
            GdtSymbol::Circularity => "Circularity",
            GdtSymbol::Cylindricity => "Cylindricity",
            GdtSymbol::ProfileOfLine => "Profile of a line",
            GdtSymbol::ProfileOfSurface => "Profile of a surface",
            GdtSymbol::Angularity => "Angularity",
            GdtSymbol::Perpendicularity => "Perpendicularity",
            GdtSymbol::Parallelism => "Parallelism",
            GdtSymbol::Position => "Position",
            GdtSymbol::Concentricity => "Concentricity",
            GdtSymbol::Symmetry => "Symmetry",
            GdtSymbol::CircularRunout => "Circular runout",
            GdtSymbol::TotalRunout => "Total runout",
        }
    }
    /// Form tolerances take no datum.
    pub fn takes_datums(self) -> bool {
        !matches!(self, GdtSymbol::Straightness | GdtSymbol::Flatness | GdtSymbol::Circularity | GdtSymbol::Cylindricity)
    }
}

/// Material condition modifier after the tolerance: Ⓜ or Ⓛ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterialCondition {
    Maximum,
    Least,
}

impl AnnotationKind {
    /// The view an annotation belongs to (`None` for sheet-level ones).
    pub fn view(&self) -> Option<ViewId> {
        use AnnotationKind as K;
        match self {
            K::Linear { view, .. }
            | K::Ordinate { view, .. }
            | K::Radius { view, .. }
            | K::Angle { view, .. }
            | K::Chamfer { view, .. }
            | K::Leader { view, .. }
            | K::Balloon { view, .. }
            | K::CenterMark { view, .. }
            | K::Centerline { view, .. }
            | K::PartingLine { view, .. }
            | K::ItemBalloon { view, .. }
            | K::FeatureControl { view, .. }
            | K::Datum { view, .. } => Some(*view),
            K::Note { .. } | K::BomTable { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: AnnotationId,
    pub kind: AnnotationKind,
    /// Replace the measured text (the measured value is still reported).
    #[serde(default)]
    pub text_override: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    pub id: DrawingId,
    pub name: String,
    pub sheet: Sheet,
    #[serde(default)]
    pub views: Vec<View>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(default)]
    pub next_id: u32,
}

impl Drawing {
    pub fn new(id: DrawingId, name: &str, sheet: Sheet) -> Self {
        Drawing { id, name: name.into(), sheet, views: Vec::new(), annotations: Vec::new(), next_id: 1 }
    }
    fn alloc(&mut self) -> u32 {
        let i = self.next_id.max(1);
        self.next_id = i + 1;
        i
    }
    /// Add a view (scale 1, no rotation, hidden lines off).
    pub fn add_view(&mut self, name: &str, orientation: ViewOrientation, position: DVec2, scale: f64) -> ViewId {
        let id = ViewId(self.alloc());
        self.views.push(View {
            id,
            name: name.into(),
            bodies: Vec::new(),
            orientation,
            rotation_deg: 0.0,
            scale,
            position,
            hidden_lines: false,
            exact: true,
            smooth_edges: false,
            scope: Scope::Root,
            exploded: false,
            section: None,
        });
        id
    }
    /// Add a section view cut along the sheet line `a`–`b` drawn across `parent`
    /// (`frame` is its computed [`ViewResult::frame`]), placed at `position`.
    /// The arrows point away from the side of the line the new view is placed on,
    /// as with a projected view; the view copies the parent's scale and contents
    /// and is labelled with the next free letter (A, B, …, no I, O or Q).
    pub fn add_section_view(&mut self, parent: ViewId, frame: &ViewFrame, a: DVec2, b: DVec2, position: DVec2) -> Option<ViewId> {
        let pv = self.views.iter().find(|v| v.id == parent)?.clone();
        let s = b - a;
        if s.length() < 1e-6 {
            return None;
        }
        let inv = DVec2::from_angle(-frame.rot);
        let model_dir = |v: DVec2| {
            let l = inv.rotate(v);
            frame.x * l.x + frame.up * l.y
        };
        let l = frame.center + inv.rotate(a - frame.position) / frame.scale;
        let origin = frame.x * l.x + frame.up * l.y;
        let mut p = s.perp().normalize();
        if (position - a).dot(p) < 0.0 {
            p = -p;
        }
        let normal = model_dir(p).normalize();
        let m = model_dir(s).normalize();
        // like a projected view: a vertical cut keeps the parent's up, a horizontal
        // one looks down (placed above) or up (placed below) at it
        let up = if s.y.abs() > s.x.abs() {
            if s.y > 0.0 {
                m
            } else {
                -m
            }
        } else if p.y > 0.0 {
            -frame.n
        } else {
            frame.n
        };
        let q = DQuat::from_mat3(&DMat3::from_cols(up.cross(normal), up, normal)).inverse().normalize();
        let used: Vec<&str> = self.views.iter().filter_map(|v| v.section.as_ref().map(|s| s.label.as_str())).collect();
        let label = "ABCDEFGHJKLMNPRSTUVWXYZ".chars().map(String::from).find(|c| !used.contains(&c.as_str())).unwrap_or_else(|| "Z".into());
        let id = self.add_view(&format!("Section {label}-{label}"), ViewOrientation::Rotation(q), position, pv.scale);
        let v = self.view_mut(id)?;
        v.bodies = pv.bodies;
        v.scope = pv.scope;
        v.exploded = pv.exploded;
        v.exact = pv.exact;
        v.smooth_edges = pv.smooth_edges;
        v.section = Some(SectionCut { parent, origin, normal, label });
        Some(id)
    }
    pub fn view_mut(&mut self, id: ViewId) -> Option<&mut View> {
        self.views.iter_mut().find(|v| v.id == id)
    }
    pub fn annotate(&mut self, kind: AnnotationKind) -> AnnotationId {
        let id = AnnotationId(self.alloc());
        self.annotations.push(Annotation { id, kind, text_override: None });
        id
    }
}

// ---------------------------------------------------------------------------
// Results

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum LineStyle {
    Visible,
    Hidden,
    /// Dimension and leader lines.
    Thin,
    /// Centre lines / marks (chain).
    Center,
    /// Parting line (phantom).
    Phantom,
    Frame,
    /// Section hatching on cut faces.
    Hatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Prim {
    Polyline { pts: Vec<DVec2>, style: LineStyle },
    Circle { c: DVec2, r: f64, style: LineStyle },
    Text { pos: DVec2, height: f64, angle_deg: f64, text: String, anchor: Anchor },
    /// Filled arrow head with its tip at `tip`, pointing along `dir`.
    Arrow { tip: DVec2, dir: DVec2, size: f64 },
    /// Filled convex polygon (datum triangles).
    Fill { pts: Vec<DVec2> },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ViewEdge {
    /// Sheet coordinates (mm).
    pub points: Vec<DVec2>,
    pub visible: bool,
    pub kind: HlrKindS,
    /// Source model edge (body, edge index in that body's shape) — `None` for
    /// silhouettes of curved faces.
    pub source: Option<(BodyId, u32)>,
    /// Occurrence path of the source body, relative to the view's scope (`[]` for
    /// root views and the scope's own bodies).
    pub path: OccPath,
    /// A ready-made annotation reference to the source edge
    /// ([`DimRef::from_view_edge`]).
    pub reference: Option<DimRef>,
}

/// Serializable mirror of `occt::HlrKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum HlrKindS {
    Sharp,
    Smooth,
    Outline,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ViewResult {
    pub id: ViewId,
    pub edges: Vec<ViewEdge>,
    /// Model → sheet placement of this view.
    pub frame: ViewFrame,
    /// Section views: hatch lines on the cut faces (sheet mm).
    pub hatch: Vec<Vec<DVec2>>,
    /// Section views: the cutting line with arrows and letters on the parent view,
    /// and the "SECTION A-A" caption.
    pub marks: Vec<Prim>,
    /// Sheet-space bounding box of the drawn edges.
    pub min: DVec2,
    pub max: DVec2,
    pub poly_fallback: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum AnnotationStatus {
    Ok,
    /// The model reference is gone or not shown in the view; kept in the document.
    Dangling(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AnnotationResult {
    pub id: AnnotationId,
    pub status: AnnotationStatus,
    /// Measured value (mm or degrees) for dimensions.
    pub value: Option<f64>,
    pub text: String,
    pub prims: Vec<Prim>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DrawingResult {
    pub size: DVec2,
    pub views: Vec<ViewResult>,
    pub annotations: Vec<AnnotationResult>,
    /// Border and title block.
    pub frame: Vec<Prim>,
    pub title: Vec<(String, String)>,
}

impl DrawingResult {
    pub fn view(&self, id: ViewId) -> Option<&ViewResult> {
        self.views.iter().find(|v| v.id == id)
    }
    pub fn annotation(&self, id: AnnotationId) -> Option<&AnnotationResult> {
        self.annotations.iter().find(|a| a.id == id)
    }
}

// ---------------------------------------------------------------------------
// Computation

/// Placement of one view: model → sheet.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ViewFrame {
    pub n: DVec3,
    pub x: DVec3,
    pub up: DVec3,
    pub rot: f64,
    pub scale: f64,
    pub position: DVec2,
    pub center: DVec2,
}

impl ViewFrame {
    pub fn local(&self, p: DVec3) -> DVec2 {
        DVec2::new(p.dot(self.x), p.dot(self.up))
    }
    pub fn local_to_sheet(&self, l: DVec2) -> DVec2 {
        let d = (l - self.center) * self.scale;
        self.position + DVec2::from_angle(self.rot).rotate(d)
    }
    pub fn sheet(&self, p: DVec3) -> DVec2 {
        self.local_to_sheet(self.local(p))
    }
    /// Direction of a model vector on the sheet (not normalised).
    pub fn sheet_dir(&self, v: DVec3) -> DVec2 {
        DVec2::from_angle(self.rot).rotate(DVec2::new(v.dot(self.x), v.dot(self.up)))
    }
}


/// HLR output of one body in one orientation (local coordinates), with sources.
struct HlrBody {
    _geom: Arc<BodyGeom>,
    edges: Vec<(Vec<DVec2>, bool, HlrKindS, Option<u32>)>,
    poly: bool,
}

/// HLR output of several placed bodies hidden against each other.
struct HlrSet {
    _geoms: Vec<Arc<BodyGeom>>,
    edges: Vec<PlacedEdge>,
    poly: bool,
}

/// Caches hidden-line results per (body geometry, orientation, algorithm), and per
/// placed set for assembly views.
#[derive(Default)]
pub struct DrawingCache {
    hlr: HashMap<(usize, [u64; 6], bool), Arc<HlrBody>>,
    sets: HashMap<u64, Arc<HlrSet>>,
    /// Section face outlines per (body geometry, placement, plane).
    cuts: HashMap<u64, Arc<Vec<Vec<DVec3>>>>,
}

impl DrawingCache {
    pub fn clear(&mut self) {
        self.hlr.clear();
        self.sets.clear();
        self.cuts.clear();
    }
    pub fn len(&self) -> usize {
        self.hlr.len() + self.sets.len() + self.cuts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.hlr.is_empty() && self.sets.is_empty() && self.cuts.is_empty()
    }
}

fn placed_key(it: &ScopeItem, hs: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    (Arc::as_ptr(&it.body.geom) as usize).hash(hs);
    it.transform.to_rows().iter().for_each(|v| v.to_bits().hash(hs));
}

/// The outline loops of one item's cut face (scope frame), cached.
fn cut_outline(cache: &mut DrawingCache, it: &ScopeItem, origin: DVec3, normal: DVec3) -> Option<Arc<Vec<Vec<DVec3>>>> {
    use std::hash::{Hash, Hasher};
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    placed_key(it, &mut hs);
    [origin, normal].iter().flat_map(|v| v.to_array()).for_each(|v| v.to_bits().hash(&mut hs));
    let key = hs.finish();
    if let Some(c) = cache.cuts.get(&key) {
        return Some(c.clone());
    }
    let face = it.placed_shape().ok()?.section(origin, normal).ok()?;
    let infos = face.edges().ok()?;
    let diag = face.bbox().map(|b| b.size().length()).unwrap_or(10.0).max(1.0);
    let mesh = face.tessellate((diag * 1e-3).max(0.01), 10.0).ok();
    let out = Arc::new(source_polylines(&infos, mesh.as_ref()));
    cache.cuts.insert(key, out.clone());
    Some(out)
}

/// Hatch lines (`angle_deg`, every `spacing` mm) filling the even-odd interior of
/// closed sheet polylines.
pub fn hatch_lines(loops: &[Vec<DVec2>], angle_deg: f64, spacing: f64) -> Vec<Vec<DVec2>> {
    let r = DVec2::from_angle(angle_deg.to_radians());
    // hatch frame: u along the lines, v across them
    let to = |p: DVec2| DVec2::new(p.dot(r), p.dot(r.perp()));
    let segs: Vec<(DVec2, DVec2)> = loops.iter().flat_map(|l| l.windows(2).map(|w| (to(w[0]), to(w[1])))).collect();
    let (lo, hi) = segs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (a, b)| (lo.min(a.y).min(b.y), hi.max(a.y).max(b.y)));
    let mut out = Vec::new();
    if !lo.is_finite() || spacing <= 0.0 {
        return out;
    }
    let mut v = (lo / spacing).floor() * spacing + spacing * 0.5;
    while v < hi && out.len() < 20_000 {
        let mut us: Vec<f64> = segs
            .iter()
            .filter(|(a, b)| (a.y <= v) != (b.y <= v))
            .map(|(a, b)| a.x + (b.x - a.x) * (v - a.y) / (b.y - a.y))
            .collect();
        us.sort_by(f64::total_cmp);
        for pair in us.as_chunks::<2>().0 {
            if pair[1] - pair[0] > 1e-6 {
                out.push(vec![r * pair[0] + r.perp() * v, r * pair[1] + r.perp() * v]);
            }
        }
        v += spacing;
    }
    out
}

/// HLR of one body placed by `t`: computed in the body's frame (cached per body
/// geometry and local direction) and offset by the translation.
fn hlr_placed(cache: &mut DrawingCache, body: &Body, t: Transform, n: DVec3, x: DVec3, exact: bool) -> Result<(Arc<HlrBody>, DVec2), String> {
    let inv = t.rotation.inverse();
    let h = hlr_body(cache, body, inv * n, inv * x, exact)?;
    let up = n.cross(x);
    Ok((h, DVec2::new(t.translation.dot(x), t.translation.dot(up))))
}

/// HLR of all items together (parts hide each other), sources matched per item.
/// With `cut` (a plane point and normal), only what lies behind the plane is drawn.
fn hlr_combined(cache: &mut DrawingCache, items: &[ScopeItem], n: DVec3, x: DVec3, exact: bool, cut: Option<(DVec3, DVec3)>) -> Result<Arc<HlrSet>, String> {
    use std::hash::{Hash, Hasher};
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    for it in items {
        placed_key(it, &mut hs);
    }
    [n.x, n.y, n.z, x.x, x.y, x.z].iter().for_each(|v| v.to_bits().hash(&mut hs));
    exact.hash(&mut hs);
    if let Some((o, nn)) = cut {
        [o, nn].iter().flat_map(|v| v.to_array()).for_each(|v| v.to_bits().hash(&mut hs));
    }
    let key = hs.finish();
    if let Some(h) = cache.sets.get(&key) {
        return Ok(h.clone());
    }
    let mut shapes: Vec<occt::Shape> = Vec::new();
    for it in items {
        let s = it.placed_shape().map_err(|e| e.0)?;
        match cut {
            None => shapes.push(s),
            Some((o, nn)) => match s.split(o, nn) {
                Ok((Some(behind), _)) => shapes.push(behind),
                Ok((None, _)) => {} // wholly in front of the plane: cut away
                Err(_) => shapes.push(s),
            },
        }
    }
    if shapes.is_empty() {
        let h = Arc::new(HlrSet { _geoms: Vec::new(), edges: Vec::new(), poly: false });
        cache.sets.insert(key, h.clone());
        return Ok(h);
    }
    let refs: Vec<&occt::Shape> = shapes.iter().collect();
    let comp = occt::Shape::compound(&refs).map_err(|e| e.0)?;
    let diag = comp.bbox().map_err(|e| e.0)?.size().length().max(1.0);
    let (edges, poly) = comp.hlr(n, x, exact, (diag * 2e-4).max(0.002)).map_err(|e| e.0)?;
    let up = n.cross(x);
    struct Src {
        item: usize,
        edge: u32,
        pts: Vec<DVec2>,
        depth: f64,
        lo: DVec2,
        hi: DVec2,
    }
    let mut srcs = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let infos = it.body.shape().edges().map_err(|e| e.0)?;
        let mesh = it.body.mesh().ok();
        for (k, pl) in source_polylines(&infos, mesh.as_deref()).into_iter().enumerate() {
            let w: Vec<DVec3> = pl.iter().map(|p| it.transform.point(*p)).collect();
            let depth = w.iter().map(|p| p.dot(n)).sum::<f64>() / w.len().max(1) as f64;
            let pts: Vec<DVec2> = w.iter().map(|p| DVec2::new(p.dot(x), p.dot(up))).collect();
            let lo = pts.iter().fold(DVec2::splat(f64::INFINITY), |a, p| a.min(*p));
            let hi = pts.iter().fold(DVec2::splat(f64::NEG_INFINITY), |a, p| a.max(*p));
            srcs.push(Src { item: i, edge: k as u32, pts, depth, lo, hi });
        }
    }
    let tol = diag * 1e-4 + 0.005;
    let out = edges
        .into_iter()
        .filter(|e| !e.points.is_empty())
        .map(|e| {
            let pts: Vec<DVec2> = e.points.iter().map(|p| DVec2::new(p[0], p[1])).collect();
            let kind = match e.kind {
                HlrKind::Sharp => HlrKindS::Sharp,
                HlrKind::Smooth => HlrKindS::Smooth,
                HlrKind::Outline => HlrKindS::Outline,
            };
            let source = if kind == HlrKindS::Outline {
                None
            } else {
                let samples = [pts[0], pts[pts.len() / 2], pts[pts.len() - 1]];
                let sign = if e.visible { 1.0 } else { -1.0 };
                srcs.iter()
                    .filter(|s| samples.iter().all(|p| p.x >= s.lo.x - tol && p.y >= s.lo.y - tol && p.x <= s.hi.x + tol && p.y <= s.hi.y + tol))
                    .map(|s| (s, samples.iter().map(|p| dist_point_polyline(*p, &s.pts)).fold(0.0, f64::max)))
                    .filter(|(_, d)| *d < tol)
                    .max_by(|a, b| (sign * a.0.depth).total_cmp(&(sign * b.0.depth)).then(b.1.total_cmp(&a.1)))
                    .map(|(s, _)| (s.item, s.edge))
            };
            (pts, e.visible, kind, source)
        })
        .collect();
    let h = Arc::new(HlrSet { _geoms: items.iter().map(|i| i.body.geom.clone()).collect(), edges: out, poly });
    cache.sets.insert(key, h.clone());
    Ok(h)
}

fn source_polylines(info: &[EdgeInfo], mesh: Option<&occt::Mesh>) -> Vec<Vec<DVec3>> {
    info.iter()
        .map(|e| match (e.kind, e.circle) {
            (CurveKind::Line, _) => vec![e.start, e.end],
            (CurveKind::Circle, Some((c, ax, r))) => {
                let ax = ax.normalize();
                let u = (e.start - c).normalize_or_zero();
                if u == DVec3::ZERO {
                    return vec![e.start];
                }
                let v = ax.cross(u);
                let ang = |p: DVec3| {
                    let d = p - c;
                    d.dot(v).atan2(d.dot(u))
                };
                let tau = std::f64::consts::TAU;
                let (am, ae) = (ang(e.midpoint).rem_euclid(tau), ang(e.end).rem_euclid(tau));
                // sweep from start (0) through mid to end, in either direction
                let sweep = if e.closed {
                    tau
                } else if am <= ae || ae < 1e-9 {
                    if ae < 1e-9 {
                        tau
                    } else {
                        ae
                    }
                } else {
                    ae - tau
                };
                (0..=96).map(|i| c + (u * (sweep * i as f64 / 96.0).cos() + v * (sweep * i as f64 / 96.0).sin()) * r).collect()
            }
            _ => mesh
                .and_then(|m| m.edges.iter().find(|p| p.edge == e.index))
                .map(|p| p.points.iter().map(|q| DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64)).collect())
                .unwrap_or_else(|| vec![e.start, e.midpoint, e.end]),
        })
        .collect()
}

fn dist_point_polyline(p: DVec2, pl: &[DVec2]) -> f64 {
    if pl.len() == 1 {
        return p.distance(pl[0]);
    }
    pl.windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let ab = b - a;
            let t = if ab.length_squared() > 1e-18 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            p.distance(a + ab * t)
        })
        .fold(f64::INFINITY, f64::min)
}

fn hlr_body(cache: &mut DrawingCache, body: &Body, n: DVec3, x: DVec3, exact: bool) -> Result<Arc<HlrBody>, String> {
    let key = (Arc::as_ptr(&body.geom) as usize, [n.x.to_bits(), n.y.to_bits(), n.z.to_bits(), x.x.to_bits(), x.y.to_bits(), x.z.to_bits()], exact);
    if let Some(h) = cache.hlr.get(&key) {
        return Ok(h.clone());
    }
    let shape = body.shape();
    let bb = shape.bbox().map_err(|e| e.0)?;
    let diag = bb.size().length().max(1.0);
    let (edges, poly) = shape.hlr(n, x, exact, (diag * 2e-4).max(0.002)).map_err(|e| e.0)?;
    let infos = shape.edges().map_err(|e| e.0)?;
    let mesh = body.mesh().ok();
    let up = n.cross(x);
    let src3 = source_polylines(&infos, mesh.as_deref());
    let depth: Vec<f64> = src3.iter().map(|pl| pl.iter().map(|p| p.dot(n)).sum::<f64>() / pl.len().max(1) as f64).collect();
    let src: Vec<Vec<DVec2>> = src3.into_iter().map(|pl| pl.into_iter().map(|p| DVec2::new(p.dot(x), p.dot(up))).collect()).collect();
    let tol = diag * 1e-4 + 0.005;
    let out = edges
        .into_iter()
        .map(|e| {
            let pts: Vec<DVec2> = e.points.iter().map(|p| DVec2::new(p[0], p[1])).collect();
            let kind = match e.kind {
                HlrKind::Sharp => HlrKindS::Sharp,
                HlrKind::Smooth => HlrKindS::Smooth,
                HlrKind::Outline => HlrKindS::Outline,
            };
            let source = if kind == HlrKindS::Outline {
                None
            } else {
                let samples = [pts[0], pts[pts.len() / 2], pts[pts.len() - 1]];
                // edges that project onto each other (e.g. a hole's top and bottom
                // circles): visible output takes the frontmost, hidden the backmost
                let sign = if e.visible { 1.0 } else { -1.0 };
                src.iter()
                    .enumerate()
                    .map(|(i, pl)| (i, samples.iter().map(|s| dist_point_polyline(*s, pl)).fold(0.0, f64::max)))
                    .filter(|(_, d)| *d < tol)
                    .max_by(|a, b| (sign * depth[a.0]).total_cmp(&(sign * depth[b.0])).then(b.1.total_cmp(&a.1)))
                    .map(|(i, _)| i as u32)
            };
            (pts, e.visible, kind, source)
        })
        .collect();
    let h = Arc::new(HlrBody { _geom: body.geom.clone(), edges: out, poly });
    cache.hlr.insert(key, h.clone());
    Ok(h)
}

/// Format a measured value: up to 2 decimals, trailing zeros trimmed.
pub fn fmt_value(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" {
        "0".into()
    } else {
        s
    }
}

/// Resolve `{...}` placeholders: params, `meta.key`, `doc.name`, `doc.revision`,
/// `sheet.scale`. Unknown names render empty.
pub fn substitute(template: &str, data: &DocData, vals: &Values, scale: f64) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out += &rest[..i];
        let Some(j) = rest[i..].find('}') else {
            out += &rest[i..];
            return out;
        };
        let key = rest[i + 1..i + j].trim();
        let v = if let Some(m) = key.strip_prefix("meta.") {
            data.meta.get(m).cloned().unwrap_or_default()
        } else {
            match key {
                "doc.name" => data.name.clone(),
                "doc.revision" => data.revision.to_string(),
                "sheet.scale" => scale_text(scale),
                k => vals.get(k).map(|v| fmt_value(*v)).unwrap_or_default(),
            }
        };
        out += &v;
        rest = &rest[i + j + 1..];
    }
    out + rest
}

fn scale_text(s: f64) -> String {
    if s >= 1.0 {
        format!("{}:1", fmt_value(s))
    } else {
        format!("1:{}", fmt_value(1.0 / s))
    }
}

enum Resolved {
    Line { a: DVec2, b: DVec2 },
    Circle { c: DVec2, r: f64, face_on: bool },
    Point(DVec2),
    Other { mid: DVec2 },
}

impl Resolved {
    fn point(&self) -> DVec2 {
        match self {
            Resolved::Line { a, b } => (*a + *b) / 2.0,
            Resolved::Circle { c, .. } => *c,
            Resolved::Point(p) => *p,
            Resolved::Other { mid } => *mid,
        }
    }
}

struct ViewCtx<'a> {
    frame: ViewFrame,
    /// The bodies drawn in this view, placed in the view's scope frame.
    items: Vec<ScopeItem>,
    view: &'a View,
}

/// What a drawing is computed against.
pub struct DrawingSources<'a> {
    /// The root component's regeneration (root-scope views, `Balloon`, `PartingLine`).
    pub regen: &'a RegenResult,
    /// Purchased-part BOM (`Balloon` numbers).
    pub bom: &'a [BomLine],
    /// The assembly, for views with a non-root [`Scope`].
    pub assembly: Option<&'a AssemblyResult>,
    /// The assembly BOM, for `ItemBalloon` and `BomTable`.
    pub bom_tree: Option<&'a AssemblyBom>,
}

struct Ctx<'a> {
    data: &'a DocData,
    regen: &'a RegenResult,
    bom_tree: Option<&'a AssemblyBom>,
    views: HashMap<ViewId, ViewCtx<'a>>,
    view_sources: HashMap<ViewId, std::collections::HashSet<(OccPath, BodyId, u32)>>,
}

impl Ctx<'_> {
    fn view(&self, v: ViewId) -> Result<&ViewCtx<'_>, String> {
        self.views.get(&v).ok_or_else(|| format!("view {v:?} does not exist"))
    }
    fn frame(&self, v: ViewId) -> Result<ViewFrame, String> {
        self.view(v).map(|c| c.frame)
    }

    fn resolve(&self, view: ViewId, r: &DimRef) -> Result<Resolved, String> {
        let vc = self.view(view)?;
        let f = vc.frame;
        let (path, base) = r.split();
        let body_id = match base {
            DimRef::Edge { body, .. } | DimRef::Vertex { body, .. } => *body,
            DimRef::Placed { .. } => return Err("a placed reference cannot contain another one".into()),
        };
        let item = vc.items.iter().find(|i| i.path == path && i.body.id == body_id).ok_or_else(|| {
            if path.is_empty() && vc.view.scope == Scope::Root && vc.view.bodies.contains(&body_id) {
                "the referenced body no longer exists".to_string()
            } else {
                "the referenced body is not shown in this view".to_string()
            }
        })?;
        let t = item.transform;
        let shape = item.body.shape();
        let sh = |p: DVec3| f.sheet(t.point(p));
        match base {
            DimRef::Edge { edge, .. } => {
                let ids = edge.resolve(shape).map_err(|e| format!("reference lost: {e}"))?;
                let infos = shape.edges().map_err(|e| e.0)?;
                let e = &infos[ids[0] as usize];
                let shown = self.view_sources.get(&view).is_some_and(|s| s.contains(&(path.to_vec(), body_id, e.index)));
                let circle = e.circle.map(|(c, ax, r)| (c, t.vector(ax).normalize(), r));
                let face_on_circle = matches!(circle, Some((_, ax, _)) if ax.dot(f.n).abs() > 0.999);
                if !shown && !face_on_circle {
                    return Err(if e.circle.is_some() {
                        "the circle is not seen face-on in this view".into()
                    } else {
                        "the referenced edge is not drawn in this view".into()
                    });
                }
                Ok(match (e.kind, circle) {
                    (CurveKind::Line, _) => Resolved::Line { a: sh(e.start), b: sh(e.end) },
                    (CurveKind::Circle, Some((c, ax, r))) => Resolved::Circle { c: sh(c), r: r * f.scale, face_on: ax.dot(f.n).abs() > 0.999 },
                    _ => Resolved::Other { mid: sh(e.midpoint) },
                })
            }
            DimRef::Vertex { near, .. } => {
                let infos = shape.edges().map_err(|e| e.0)?;
                let tol = (shape.bbox().map_err(|e| e.0)?.size().length() * 0.005).max(0.1);
                infos
                    .iter()
                    .flat_map(|e| [e.start, e.end])
                    .map(|p| (p, p.distance(*near)))
                    .filter(|(_, d)| *d <= tol)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(p, _)| Resolved::Point(sh(p)))
                    .ok_or_else(|| "the referenced vertex no longer exists".into())
            }
            DimRef::Placed { .. } => unreachable!(),
        }
    }
}

const TEXT_H: f64 = 3.5;
const ARROW: f64 = 2.5;

/// Font size per unit of text height: a `Prim::Text` height is the cap height, and
/// Helvetica's cap height is 0.718 em, so the em is 1.4 × the height.
pub const FONT_EM: f64 = 1.4;

/// Helvetica advance widths (1/1000 em, from the base-14 AFM) for ' '..='~'.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // ' '..'/'
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, // '0'..'?'
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, // '@'..'O'
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, // 'P'..'_'
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // '`'..'o'
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, // 'p'..'~'
];

/// Width (sheet mm) of `s` at cap height `height` in Helvetica, the font the PDF
/// uses and the SVG asks for: what centring, boxes and collision tests measure.
pub fn text_width(s: &str, height: f64) -> f64 {
    let em: u32 = s
        .chars()
        .map(|c| match c {
            ' '..='~' => HELVETICA[c as usize - 32] as u32,
            '⌀' | 'Ø' => 778,
            '°' => 400,
            '×' | '±' => 584,
            _ => 556, // shown as '?' in the PDF
        })
        .sum();
    em as f64 / 1000.0 * height * FONT_EM
}

/// The corners of a text's box on the sheet (counter-clockwise from bottom-left),
/// from a little below the baseline to a little above the cap height.
pub fn text_quad(pos: DVec2, height: f64, angle_deg: f64, s: &str, anchor: Anchor) -> [DVec2; 4] {
    let w = text_width(s, height);
    let x0 = match anchor {
        Anchor::Start => 0.0,
        Anchor::Middle => -w / 2.0,
        Anchor::End => -w,
    };
    let (lo, hi) = (DVec2::new(x0 - 0.3, -0.25 * height), DVec2::new(x0 + w + 0.3, 1.15 * height));
    let r = DVec2::from_angle(angle_deg.to_radians());
    [DVec2::new(lo.x, lo.y), DVec2::new(hi.x, lo.y), DVec2::new(hi.x, hi.y), DVec2::new(lo.x, hi.y)].map(|c| pos + r.rotate(c))
}

fn line(a: DVec2, b: DVec2, style: LineStyle) -> Prim {
    Prim::Polyline { pts: vec![a, b], style }
}

fn text(pos: DVec2, s: &str, angle: f64, anchor: Anchor) -> Prim {
    Prim::Text { pos, height: TEXT_H, angle_deg: angle, text: s.into(), anchor }
}

fn arrow(tip: DVec2, dir: DVec2) -> Prim {
    Prim::Arrow { tip, dir: dir.normalize_or_zero(), size: ARROW }
}

/// Compute a drawing against a regenerated model (root-scope views only; views of
/// the assembly report an error, use [`compute_with`]).
pub fn compute(data: &DocData, regen: &RegenResult, bom: &[BomLine], drawing: &Drawing, cache: &mut DrawingCache) -> DrawingResult {
    compute_with(data, &DrawingSources { regen, bom, assembly: None, bom_tree: None }, drawing, cache)
}

type PlacedEdge = (Vec<DVec2>, bool, HlrKindS, Option<(usize, u32)>);

/// HLR of each item on its own (no hiding between items).
fn hlr_each(cache: &mut DrawingCache, items: &[ScopeItem], n: DVec3, x: DVec3, exact: bool, out: &mut Vec<PlacedEdge>, err: &mut Option<String>) -> bool {
    let mut poly = false;
    for (i, it) in items.iter().enumerate() {
        match hlr_placed(cache, &it.body, it.transform, n, x, exact) {
            Ok((h, off)) => {
                poly |= h.poly;
                for (pts, vis, kind, src) in &h.edges {
                    out.push((pts.iter().map(|p| *p + off).collect(), *vis, *kind, src.map(|s| (i, s))));
                }
            }
            Err(e) => *err = Some(e),
        }
    }
    poly
}

/// Compute a drawing; views may show the assembly, a component or an occurrence.
pub fn compute_with(data: &DocData, src: &DrawingSources, drawing: &Drawing, cache: &mut DrawingCache) -> DrawingResult {
    let regen = src.regen;
    let vals = data.values().unwrap_or_default();
    let size = drawing.sheet.size_mm();
    let mut views = Vec::new();
    let mut ctx = Ctx { data, regen, bom_tree: src.bom_tree, views: HashMap::new(), view_sources: HashMap::new() };
    for v in &drawing.views {
        let (n, x, up) = v.orientation.basis();
        let mut err = None;
        let items: Vec<ScopeItem> = match (&v.scope, src.assembly) {
            (Scope::Root, _) => {
                let ids: Vec<BodyId> = if v.bodies.is_empty() { regen.solids().map(|b| b.id).collect() } else { v.bodies.clone() };
                ids.iter()
                    .filter_map(|bid| match regen.body_by_id(*bid) {
                        Some(b) => Some(ScopeItem { path: Vec::new(), component: crate::assembly::ComponentId::ROOT, body: b.clone(), transform: Transform::IDENTITY }),
                        None => {
                            err = Some(format!("body {bid:?} no longer exists"));
                            None
                        }
                    })
                    .collect()
            }
            (_, None) => {
                err = Some("this view shows the assembly: compute the drawing with the assembly result (Document::drawing_result or drawing::compute_with)".into());
                Vec::new()
            }
            (scope, Some(a)) => match crate::assembly::scope_items(data, a, scope, v.exploded) {
                Ok(mut its) => {
                    if !v.bodies.is_empty() {
                        its.retain(|i| v.bodies.contains(&i.body.id));
                    }
                    its
                }
                Err(e) => {
                    err = Some(e);
                    Vec::new()
                }
            },
        };
        let mut local: Vec<PlacedEdge> = Vec::new();
        let cut = v.section.as_ref().map(|s| (s.origin, s.normal.normalize_or_zero()));
        let poly = if cut.is_none() && (v.scope == Scope::Root || items.len() <= 1) {
            hlr_each(cache, &items, n, x, v.exact, &mut local, &mut err)
        } else {
            match hlr_combined(cache, &items, n, x, v.exact, cut) {
                Ok(h) => {
                    local.extend(h.edges.iter().cloned());
                    h.poly
                }
                Err(e) => {
                    err = Some(format!("hidden lines between parts failed ({e}); each part is drawn on its own"));
                    hlr_each(cache, &items, n, x, v.exact, &mut local, &mut err)
                }
            }
        };
        let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for (pts, ..) in &local {
            for p in pts {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        let center = if lo.x.is_finite() { (lo + hi) / 2.0 } else { DVec2::ZERO };
        let frame = ViewFrame { n, x, up, rot: v.rotation_deg.to_radians(), scale: v.scale, position: v.position, center };
        let mut infos: HashMap<usize, Vec<EdgeInfo>> = HashMap::new();
        let edges: Vec<ViewEdge> = local
            .into_iter()
            .map(|(pts, visible, kind, s)| {
                let (source, path, reference) = match s {
                    Some((i, e)) => {
                        let it = &items[i];
                        let inf = infos.entry(i).or_insert_with(|| it.body.shape().edges().unwrap_or_default());
                        let r = inf.get(e as usize).map(|info| DimRef::placed(it.path.clone(), DimRef::Edge { body: it.body.id, edge: EdgeSelector::from_pick(info) }));
                        (Some((it.body.id, e)), it.path.clone(), r)
                    }
                    None => (None, Vec::new(), None),
                };
                ViewEdge { points: pts.iter().map(|p| frame.local_to_sheet(*p)).collect(), visible, kind, source, path, reference }
            })
            .collect();
        let (mut smin, mut smax) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for e in edges.iter().filter(|e| e.visible || v.hidden_lines) {
            for p in &e.points {
                smin = smin.min(*p);
                smax = smax.max(*p);
            }
        }
        ctx.view_sources.insert(v.id, edges.iter().filter_map(|e| e.source.map(|(b, i)| (e.path.clone(), b, i))).collect());
        // cut faces, hatched per part (alternating direction between parts)
        let mut hatch = Vec::new();
        if let Some((o, nn)) = cut {
            for (k, it) in items.iter().enumerate() {
                if let Some(loops) = cut_outline(cache, it, o, nn) {
                    let sheet: Vec<Vec<DVec2>> = loops.iter().map(|l| l.iter().map(|p| frame.sheet(*p)).collect()).collect();
                    for l in &sheet {
                        for p in l {
                            smin = smin.min(*p);
                            smax = smax.max(*p);
                        }
                    }
                    hatch.extend(hatch_lines(&sheet, if k % 2 == 0 { 45.0 } else { 135.0 }, 2.0 + 0.5 * (k % 3) as f64));
                }
            }
        }
        ctx.view_sources.insert(v.id, edges.iter().filter_map(|e| e.source.map(|(b, i)| (e.path.clone(), b, i))).collect());
        ctx.views.insert(v.id, ViewCtx { frame, items, view: v });
        views.push(ViewResult { id: v.id, edges, frame, hatch, marks: Vec::new(), min: smin, max: smax, poly_fallback: poly, error: err });
    }
    for (i, v) in drawing.views.iter().enumerate() {
        if let Some(sc) = &v.section {
            match section_marks(&ctx, &views, sc) {
                Ok(m) => views[i].marks = m,
                Err(e) => {
                    views[i].error.get_or_insert(e);
                }
            }
            let vr = &views[i];
            if vr.min.x.is_finite() {
                let c = DVec2::new((vr.min.x + vr.max.x) / 2.0, vr.min.y - 9.0);
                let cap = format!("SECTION {0}-{0}", sc.label);
                views[i].marks.push(text(c, &cap, 0.0, Anchor::Middle));
            }
        }
    }

    let first_scale = drawing.views.first().map(|v| v.scale).unwrap_or(1.0);
    let (mut annotations, slots): (Vec<AnnotationResult>, Vec<Option<TextSlot>>) = drawing.annotations.iter().map(|a| annotate(&ctx, a, &vals, src.bom, first_scale)).unzip();

    // frame and title block
    let m = drawing.sheet.margin;
    let mut frame = vec![Prim::Polyline {
        pts: vec![DVec2::new(m, m), DVec2::new(size.x - m, m), DVec2::new(size.x - m, size.y - m), DVec2::new(m, size.y - m), DVec2::new(m, m)],
        style: LineStyle::Frame,
    }];
    let title: Vec<(String, String)> =
        drawing.sheet.title_block.fields.iter().map(|f| (f.label.clone(), substitute(&f.template, data, &vals, first_scale))).collect();
    let (bw, rh) = ((size.x - 2.0 * m).min(180.0), 7.0);
    let x0 = size.x - m - bw;
    for (i, (l, t)) in title.iter().enumerate() {
        let y = m + rh * i as f64;
        frame.push(Prim::Polyline {
            pts: vec![DVec2::new(x0, y), DVec2::new(x0 + bw, y), DVec2::new(x0 + bw, y + rh), DVec2::new(x0, y + rh), DVec2::new(x0, y)],
            style: LineStyle::Frame,
        });
        frame.push(line(DVec2::new(x0 + 35.0, y), DVec2::new(x0 + 35.0, y + rh), LineStyle::Thin));
        frame.push(Prim::Text { pos: DVec2::new(x0 + 2.0, y + 2.0), height: 2.5, angle_deg: 0.0, text: l.clone(), anchor: Anchor::Start });
        frame.push(Prim::Text { pos: DVec2::new(x0 + 37.0, y + 2.0), height: 3.5, angle_deg: 0.0, text: t.clone(), anchor: Anchor::Start });
    }
    declutter(&mut annotations, &slots, &views, drawing, &frame);
    DrawingResult { size, views, annotations, frame, title }
}

/// How a dimension's text may move to clear other text and the view geometry.
struct TextSlot {
    /// Index of the text in the annotation's prims.
    idx: usize,
    /// Unit direction it slides along.
    dir: DVec2,
    mode: SlotMode,
}

enum SlotMode {
    /// Along the dimension line `a`–`b` either way (the line is extended under
    /// text moved past its ends), or by `flip` to the other side of the line.
    Line { a: DVec2, b: DVec2, flip: DVec2 },
    /// Outward along a leader, which is extended from `end`.
    Leader { end: DVec2 },
    /// Outward, nothing to extend.
    Free,
}

fn quad_aabb(q: &[DVec2; 4]) -> (DVec2, DVec2) {
    (q.iter().fold(DVec2::splat(f64::INFINITY), |a, p| a.min(*p)), q.iter().fold(DVec2::splat(f64::NEG_INFINITY), |a, p| a.max(*p)))
}

/// Separating-axis test of two convex quads.
fn quads_overlap(a: &[DVec2; 4], b: &[DVec2; 4]) -> bool {
    for q in [a, b] {
        for i in 0..4 {
            let ax = (q[(i + 1) % 4] - q[i]).perp();
            let (amin, amax) = a.iter().map(|p| p.dot(ax)).fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| (l.min(v), h.max(v)));
            let (bmin, bmax) = b.iter().map(|p| p.dot(ax)).fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| (l.min(v), h.max(v)));
            if amax < bmin || bmax < amin {
                return false;
            }
        }
    }
    true
}

/// A segment crosses or lies in a convex quad (corners counter-clockwise).
fn segment_hits_quad(p: DVec2, q: DVec2, quad: &[DVec2; 4]) -> bool {
    let inside = |x: DVec2| (0..4).all(|i| (quad[(i + 1) % 4] - quad[i]).perp_dot(x - quad[i]) >= 0.0);
    if inside(p) || inside(q) {
        return true;
    }
    let cross = |a: DVec2, b: DVec2, c: DVec2, d: DVec2| {
        let (r, s) = (b - a, d - c);
        let den = r.perp_dot(s);
        if den.abs() < 1e-15 {
            return false;
        }
        let t = (c - a).perp_dot(s) / den;
        let u = (c - a).perp_dot(r) / den;
        (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)
    };
    (0..4).any(|i| cross(p, q, quad[i], quad[(i + 1) % 4]))
}

fn prim_quad(p: &Prim) -> Option<[DVec2; 4]> {
    match p {
        Prim::Text { pos, height, angle_deg, text, anchor } if !text.is_empty() => Some(text_quad(*pos, *height, *angle_deg, text, *anchor)),
        _ => None,
    }
}

/// Move dimension text that would overlap other text or the drawn view geometry:
/// along its dimension line or leader (extending the line), then to the line's
/// other side; text with no free spot stays where it was.
fn declutter(anns: &mut [AnnotationResult], slots: &[Option<TextSlot>], views: &[ViewResult], drawing: &Drawing, frame: &[Prim]) {
    if slots.iter().all(|s| s.is_none()) {
        return;
    }
    // view geometry as segments with their bounding boxes
    let mut segs: Vec<(DVec2, DVec2, DVec2, DVec2)> = Vec::new();
    for vr in views {
        let v = drawing.views.iter().find(|x| x.id == vr.id);
        let hidden = v.is_some_and(|x| x.hidden_lines);
        let smooth = v.is_some_and(|x| x.smooth_edges);
        let shown = vr.edges.iter().filter(|e| (e.visible || hidden) && (e.kind != HlrKindS::Smooth || smooth)).map(|e| &e.points);
        for pts in shown.chain(vr.hatch.iter()) {
            for w in pts.windows(2) {
                segs.push((w[0], w[1], w[0].min(w[1]), w[0].max(w[1])));
            }
        }
    }
    let mut texts: Vec<[DVec2; 4]> = frame.iter().filter_map(prim_quad).collect();
    for (a, s) in anns.iter().zip(slots) {
        for (i, p) in a.prims.iter().enumerate() {
            if s.as_ref().is_none_or(|s| s.idx != i) {
                texts.extend(prim_quad(p));
            }
        }
    }
    let free = |q: &[DVec2; 4], texts: &[[DVec2; 4]]| {
        let (lo, hi) = quad_aabb(q);
        !texts.iter().any(|t| quads_overlap(q, t))
            && !segs.iter().any(|(p0, p1, slo, shi)| slo.x <= hi.x && shi.x >= lo.x && slo.y <= hi.y && shi.y >= lo.y && segment_hits_quad(*p0, *p1, q))
    };
    for (a, s) in anns.iter_mut().zip(slots) {
        let Some(s) = s else { continue };
        let Some(Prim::Text { pos, height, angle_deg, text, anchor }) = a.prims.get(s.idx).cloned() else { continue };
        let quad_at = |d: DVec2| text_quad(pos + d, height, angle_deg, &text, anchor);
        let w = text_width(&text, height);
        let mut cands = vec![DVec2::ZERO];
        match &s.mode {
            SlotMode::Line { flip, .. } => {
                let step = (w * 0.6).max(3.0);
                for side in [DVec2::ZERO, *flip] {
                    for k in 0..=8 {
                        for sg in [1.0, -1.0] {
                            if k > 0 || side != DVec2::ZERO {
                                cands.push(side + s.dir * (sg * step * k as f64));
                            }
                            if k == 0 {
                                break;
                            }
                        }
                    }
                }
            }
            SlotMode::Leader { .. } | SlotMode::Free => cands.extend((1..=12).map(|k| s.dir * (2.5 * k as f64))),
        }
        let Some(d) = cands.into_iter().find(|d| free(&quad_at(*d), &texts)) else {
            texts.push(quad_at(DVec2::ZERO));
            continue;
        };
        texts.push(quad_at(d));
        if d == DVec2::ZERO {
            continue;
        }
        if let Some(Prim::Text { pos: p, .. }) = a.prims.get_mut(s.idx) {
            *p += d;
        }
        match &s.mode {
            SlotMode::Line { a: d1, b: d2, .. } => {
                let (len, mid) = (d1.distance(*d2), (*d1 + *d2) / 2.0);
                let t = d.dot(s.dir);
                if t + w / 2.0 + 1.0 > len / 2.0 {
                    a.prims.push(line(*d2, mid + s.dir * (t + w / 2.0 + 1.0), LineStyle::Thin));
                }
                if t - w / 2.0 - 1.0 < -len / 2.0 {
                    a.prims.push(line(*d1, mid + s.dir * (t - w / 2.0 - 1.0), LineStyle::Thin));
                }
            }
            SlotMode::Leader { end } => a.prims.push(line(*end, *end + d, LineStyle::Thin)),
            SlotMode::Free => {}
        }
    }
}

/// The cutting line of a section on its parent view: chain line with thick ends,
/// arrows in the viewing direction and the letter at each end.
fn section_marks(ctx: &Ctx, views: &[ViewResult], sc: &SectionCut) -> Result<Vec<Prim>, String> {
    let pf = ctx.frame(sc.parent).map_err(|_| "the parent view of this section no longer exists".to_string())?;
    let nn = sc.normal.normalize_or_zero();
    if nn.dot(pf.n).abs() > 1e-3 {
        return Err("the cutting plane is not edge-on in the parent view any more".into());
    }
    let o = pf.sheet(sc.origin);
    let d = pf.sheet_dir(nn.cross(pf.n)).normalize_or_zero();
    let toward = pf.sheet_dir(nn).normalize_or_zero();
    // across the parent's extents plus a margin
    let (mut t0, mut t1) = (-20.0f64, 20.0f64);
    if let Some(pr) = views.iter().find(|r| r.id == sc.parent).filter(|r| r.min.x.is_finite()) {
        let (lo, hi) = (pr.min - DVec2::splat(6.0), pr.max + DVec2::splat(6.0));
        let (mut a, mut b) = (f64::NEG_INFINITY, f64::INFINITY);
        for k in 0..2 {
            if d[k].abs() < 1e-12 {
                if o[k] < lo[k] || o[k] > hi[k] {
                    a = f64::INFINITY;
                }
            } else {
                let (u, w) = ((lo[k] - o[k]) / d[k], (hi[k] - o[k]) / d[k]);
                a = a.max(u.min(w));
                b = b.min(u.max(w));
            }
        }
        if a < b {
            (t0, t1) = (a, b);
        }
    }
    let (s, e) = (o + d * t0, o + d * t1);
    let mut out = vec![line(s, e, LineStyle::Center), line(s, s + d * 5.0, LineStyle::Visible), line(e - d * 5.0, e, LineStyle::Visible)];
    for q in [s, e] {
        let tip = q - toward * 7.0;
        out.push(line(q, tip, LineStyle::Thin));
        out.push(arrow(tip, -toward));
        out.push(text(tip - toward * 2.5 - DVec2::new(0.0, TEXT_H / 2.0) - toward * (TEXT_H / 2.0), &sc.label, 0.0, Anchor::Middle));
    }
    Ok(out)
}

fn annotate(ctx: &Ctx, a: &Annotation, vals: &Values, bom: &[BomLine], scale0: f64) -> (AnnotationResult, Option<TextSlot>) {
    let mut res = AnnotationResult { id: a.id, status: AnnotationStatus::Ok, value: None, text: String::new(), prims: Vec::new() };
    let mut slot = None;
    if let Err(e) = annotate_inner(ctx, a, vals, bom, scale0, &mut res, &mut slot) {
        res.status = AnnotationStatus::Dangling(e);
        res.prims.clear();
        slot = None;
    }
    if let (Some(t), AnnotationStatus::Ok) = (&a.text_override, &res.status) {
        let t = substitute(t, ctx.data, vals, scale0);
        for p in res.prims.iter_mut() {
            if let Prim::Text { text, .. } = p {
                if *text == res.text {
                    *text = t.clone();
                }
            }
        }
        res.text = t;
    }
    (res, slot)
}

fn annotate_inner(ctx: &Ctx, a: &Annotation, vals: &Values, bom: &[BomLine], scale0: f64, res: &mut AnnotationResult, slot: &mut Option<TextSlot>) -> Result<(), String> {
    use AnnotationKind as K;
    let push_text = |res: &mut AnnotationResult, pos: DVec2, s: String, angle: f64| {
        res.prims.push(text(pos, &s, angle, Anchor::Middle));
        res.text = s;
    };
    match &a.kind {
        K::Linear { view, a: ra, b: rb, kind, offset } => {
            let f = ctx.frame(*view)?;
            let r1 = ctx.resolve(*view, ra)?;
            let (p1, p2) = match rb {
                Some(rb) => (r1.point(), ctx.resolve(*view, rb)?.point()),
                None => match r1 {
                    Resolved::Line { a, b } => (a, b),
                    _ => return Err("a single-reference linear dimension needs a straight edge".into()),
                },
            };
            let (d1, d2, value, angle) = match kind {
                LinearKind::Horizontal => {
                    let y = p1.y.max(p2.y) + offset;
                    (DVec2::new(p1.x, y), DVec2::new(p2.x, y), (p2.x - p1.x).abs(), 0.0)
                }
                LinearKind::Vertical => {
                    let x = p1.x.max(p2.x) + offset;
                    (DVec2::new(x, p1.y), DVec2::new(x, p2.y), (p2.y - p1.y).abs(), 90.0)
                }
                LinearKind::Aligned => {
                    let d = (p2 - p1).normalize_or_zero();
                    let nrm = d.perp() * *offset;
                    (p1 + nrm, p2 + nrm, p1.distance(p2), d.y.atan2(d.x).to_degrees())
                }
            };
            let value = value / f.scale;
            res.value = Some(value);
            res.prims.push(line(p1, d1, LineStyle::Thin));
            res.prims.push(line(p2, d2, LineStyle::Thin));
            res.prims.push(line(d1, d2, LineStyle::Thin));
            let mid = (d1 + d2) / 2.0;
            res.prims.push(arrow(d1, d1 - mid));
            res.prims.push(arrow(d2, d2 - mid));
            let up = DVec2::from_angle(angle.to_radians()).perp();
            push_text(res, mid + up * 1.5, fmt_value(value), angle);
            let dir = (d2 - d1).try_normalize().unwrap_or(DVec2::from_angle(angle.to_radians()));
            *slot = Some(TextSlot { idx: res.prims.len() - 1, dir, mode: SlotMode::Line { a: d1, b: d2, flip: -up * (3.0 + TEXT_H) } });
        }
        K::Ordinate { view, origin, target, axis, offset } => {
            let f = ctx.frame(*view)?;
            let o = ctx.resolve(*view, origin)?.point();
            let t = ctx.resolve(*view, target)?.point();
            let (value, end, ang) = match axis {
                SheetAxis::X => ((t.x - o.x) / f.scale, DVec2::new(t.x, t.y.max(o.y) + offset), 90.0),
                SheetAxis::Y => ((t.y - o.y) / f.scale, DVec2::new(t.x.max(o.x) + offset, t.y), 0.0),
            };
            res.value = Some(value);
            res.prims.push(line(t, end, LineStyle::Thin));
            let dir = (end - t).normalize_or_zero();
            push_text(res, end + dir * 1.5, fmt_value(value), ang);
            if dir != DVec2::ZERO {
                *slot = Some(TextSlot { idx: res.prims.len() - 1, dir, mode: SlotMode::Leader { end } });
            }
        }
        K::Radius { view, circle, diameter, leader_deg } => {
            let f = ctx.frame(*view)?;
            let Resolved::Circle { c, r, face_on } = ctx.resolve(*view, circle)? else {
                return Err("radius dimension needs a circular edge".into());
            };
            if !face_on {
                return Err("the circle is not seen face-on in this view".into());
            }
            let d = DVec2::from_angle(leader_deg.to_radians());
            let on = c + d * r;
            let out = c + d * (r + 8.0);
            let value = if *diameter { 2.0 * r / f.scale } else { r / f.scale };
            res.value = Some(value);
            res.prims.push(line(if *diameter { c - d * r } else { c }, out, LineStyle::Thin));
            res.prims.push(arrow(on, d));
            if *diameter {
                res.prims.push(arrow(c - d * r, -d));
            }
            let s = if *diameter { format!("⌀{}", fmt_value(value)) } else { format!("R{}", fmt_value(value)) };
            push_text(res, out + d * 4.0, s, 0.0);
            *slot = Some(TextSlot { idx: res.prims.len() - 1, dir: d, mode: SlotMode::Leader { end: out } });
        }
        K::Angle { view, a: ra, b: rb, radius } => {
            let (Resolved::Line { a: a1, b: a2 }, Resolved::Line { a: b1, b: b2 }) = (ctx.resolve(*view, ra)?, ctx.resolve(*view, rb)?) else {
                return Err("angle dimension needs two straight edges".into());
            };
            let (u, v) = ((a2 - a1).normalize_or_zero(), (b2 - b1).normalize_or_zero());
            let den = u.perp_dot(v);
            if den.abs() < 1e-9 {
                return Err("the edges are parallel".into());
            }
            let t = (b1 - a1).perp_dot(v) / den;
            let x = a1 + u * t;
            // point each direction from the vertex toward its edge's midpoint
            let orient = |d: DVec2, m: DVec2| if (m - x).dot(d) < 0.0 { -d } else { d };
            let (u, v) = (orient(u, (a1 + a2) / 2.0), orient(v, (b1 + b2) / 2.0));
            let ang = u.dot(v).clamp(-1.0, 1.0).acos();
            res.value = Some(ang.to_degrees());
            let a0 = u.y.atan2(u.x);
            let sgn = if u.perp_dot(v) >= 0.0 { 1.0 } else { -1.0 };
            let pts: Vec<DVec2> = (0..=24).map(|i| x + DVec2::from_angle(a0 + sgn * ang * i as f64 / 24.0) * *radius).collect();
            let (s, e) = (pts[0], *pts.last().unwrap());
            res.prims.push(Prim::Polyline { pts: pts.clone(), style: LineStyle::Thin });
            res.prims.push(arrow(s, DVec2::from_angle(a0).perp() * -sgn));
            res.prims.push(arrow(e, DVec2::from_angle(a0 + sgn * ang).perp() * sgn));
            push_text(res, x + DVec2::from_angle(a0 + sgn * ang / 2.0) * (radius + 3.0), format!("{}°", fmt_value(ang.to_degrees())), 0.0);
            *slot = Some(TextSlot { idx: res.prims.len() - 1, dir: DVec2::from_angle(a0 + sgn * ang / 2.0), mode: SlotMode::Free });
        }
        K::Chamfer { view, edge, offset } => {
            let f = ctx.frame(*view)?;
            let Resolved::Line { a: p, b: q } = ctx.resolve(*view, edge)? else {
                return Err("chamfer callout needs a straight edge".into());
            };
            let d = q - p;
            let ang = d.y.abs().atan2(d.x.abs());
            let leg = d.length() * ang.cos() / f.scale;
            res.value = Some(leg);
            let mid = (p + q) / 2.0;
            let tp = mid + *offset;
            res.prims.push(line(mid, tp, LineStyle::Thin));
            res.prims.push(arrow(mid, mid - tp));
            push_text(res, tp + DVec2::new(0.0, 1.5), format!("{} × {}°", fmt_value(leg), fmt_value(ang.to_degrees())), 0.0);
            if let Some(dir) = (tp - mid).try_normalize() {
                *slot = Some(TextSlot { idx: res.prims.len() - 1, dir, mode: SlotMode::Leader { end: tp } });
            }
        }
        K::Note { position, text: t, height } => {
            let s = substitute(t, ctx.data, vals, scale0);
            res.prims.push(Prim::Text { pos: *position, height: *height, angle_deg: 0.0, text: s.clone(), anchor: Anchor::Start });
            res.text = s;
        }
        K::Leader { view, target, text: t, position } => {
            let p = ctx.resolve(*view, target)?.point();
            res.prims.push(line(p, *position, LineStyle::Thin));
            res.prims.push(arrow(p, p - *position));
            let s = substitute(t, ctx.data, vals, scale0);
            res.prims.push(text(*position + DVec2::new(1.0, 1.0), &s, 0.0, Anchor::Start));
            res.text = s;
        }
        K::Balloon { view, body, position } => {
            let f = ctx.frame(*view)?;
            let b = ctx.regen.body_by_id(*body).ok_or("the ballooned body no longer exists")?;
            let pu = b.part.as_ref().ok_or("the ballooned body is not a library part")?;
            let row = bom
                .iter()
                .position(|l| l.source == pu.part.source && l.part == pu.part.id && l.size == pu.part.size)
                .ok_or("the part is not in the BOM")?;
            let anchor = f.sheet(b.shape().bbox().map_err(|e| e.0)?.center());
            balloon(res, anchor, *position, row + 1);
        }
        K::ItemBalloon { view, path, body, position } => {
            let vc = ctx.view(*view)?;
            let flat = &ctx.bom_tree.ok_or("item balloons need the assembly BOM (compute the drawing with the assembly)")?.flat;
            let items: Vec<&ScopeItem> = vc.items.iter().filter(|i| if let Some(b) = body { i.path == *path && i.body.id == *b } else { i.path.starts_with(path) }).collect();
            if items.is_empty() {
                return Err("the ballooned occurrence is not shown in this view".into());
            }
            let (top, abs) = match &vc.view.scope {
                Scope::Root | Scope::Assembly => (crate::assembly::ComponentId::ROOT, path.clone()),
                Scope::Component(c) => (*c, path.clone()),
                Scope::Occurrence(p) => (crate::assembly::ComponentId::ROOT, p.iter().chain(path).copied().collect()),
            };
            let part = body.and_then(|_| items[0].body.part.as_ref().map(|p| &p.part));
            let row = ctx.data.bom_item(flat, top, &abs, part)?;
            let f = vc.frame;
            let mut sum = DVec2::ZERO;
            for it in &items {
                sum += f.sheet(it.transform.point(it.body.shape().bbox().map_err(|e| e.0)?.center()));
            }
            balloon(res, sum / items.len() as f64, *position, row + 1);
        }
        K::BomTable { position, indented } => {
            let bt = ctx.bom_tree.ok_or("a BOM table needs the assembly BOM (compute the drawing with the assembly)")?;
            let item_of = |r: &crate::assembly::AsmBomRow| bt.flat.iter().position(|f| f.component == r.component && (r.component.is_some() || f.kind == r.kind)).map(|i| i + 1);
            let kind_text = |k: &crate::assembly::BomKind| match k {
                crate::assembly::BomKind::Made { process } => process.clone(),
                crate::assembly::BomKind::Assembly => "Assembly".into(),
                crate::assembly::BomKind::Purchased { source, part, part_number, vendor, .. } => match (part_number, vendor) {
                    (Some(pn), Some(v)) => format!("{v} {pn}"),
                    (Some(pn), None) => pn.clone(),
                    _ => format!("{source} {part}"),
                },
            };
            let mut rows: Vec<[String; 4]> = vec![["Item".into(), "Qty".into(), "Name".into(), "Process / part no.".into()]];
            if *indented {
                for r in &bt.indented {
                    rows.push([item_of(r).map(|i| i.to_string()).unwrap_or_default(), r.qty.to_string(), format!("{}{}", "  ".repeat(r.level), r.name), kind_text(&r.kind)]);
                }
            } else {
                for (i, r) in bt.flat.iter().enumerate() {
                    rows.push([(i + 1).to_string(), r.total.to_string(), r.name.clone(), kind_text(&r.kind)]);
                }
            }
            // columns fit their widest cell (Helvetica metrics), never narrower than the header
            let cols: Vec<f64> = (0..4).map(|ci| rows.iter().map(|r| text_width(&r[ci], 2.5)).fold(8.0, f64::max) + 3.0).collect();
            let (w, rh): (f64, f64) = (cols.iter().sum(), 6.0);
            let p0 = *position;
            let h = rh * rows.len() as f64;
            res.prims.push(Prim::Polyline { pts: vec![p0, p0 + DVec2::new(w, 0.0), p0 + DVec2::new(w, -h), p0 + DVec2::new(0.0, -h), p0], style: LineStyle::Frame });
            for i in 1..rows.len() {
                let y = p0.y - rh * i as f64;
                res.prims.push(line(DVec2::new(p0.x, y), DVec2::new(p0.x + w, y), LineStyle::Thin));
            }
            let mut cx = p0.x;
            for (ci, cw) in cols.iter().enumerate() {
                if ci > 0 {
                    res.prims.push(line(DVec2::new(cx, p0.y), DVec2::new(cx, p0.y - h), LineStyle::Thin));
                }
                for (ri, r) in rows.iter().enumerate() {
                    let pos = DVec2::new(cx + 1.5, p0.y - rh * (ri + 1) as f64 + 1.75);
                    res.prims.push(Prim::Text { pos, height: 2.5, angle_deg: 0.0, text: r[ci].clone(), anchor: Anchor::Start });
                }
                cx += cw;
            }
            res.value = Some((rows.len() - 1) as f64);
            res.text = format!("BOM ({} rows)", rows.len() - 1);
        }
        K::FeatureControl { view, target, position, symbol, tolerance, diameter, modifier, datums } => {
            let (h, th) = (FCF_H, FCF_TEXT);
            let p0 = *position;
            if let Some(t) = target {
                let tp = nearest_on(&ctx.resolve(*view, t)?, p0);
                res.prims.push(line(p0, tp, LineStyle::Thin));
                res.prims.push(arrow(tp, tp - p0));
            }
            let tol = format!("{}{}", if *diameter { "⌀" } else { "" }, substitute(tolerance, ctx.data, vals, scale0).trim());
            let mc = modifier.map(|m| if m == MaterialCondition::Maximum { "M" } else { "L" });
            let datums: Vec<&String> = if symbol.takes_datums() { datums.iter().filter(|d| !d.trim().is_empty()).collect() } else { Vec::new() };
            let (y0, y1) = (p0.y - h / 2.0, p0.y + h / 2.0);
            let base = p0.y - th / 2.0;
            let mut x = p0.x;
            // symbol cell
            gdt_symbol(&mut res.prims, *symbol, DVec2::new(x + h / 2.0, p0.y), h * 0.32);
            x += h;
            let mut seps = vec![x];
            // tolerance cell, with the circled modifier after the value
            let tw = text_width(&tol, th);
            res.prims.push(Prim::Text { pos: DVec2::new(x + 1.5, base), height: th, angle_deg: 0.0, text: tol.clone(), anchor: Anchor::Start });
            x += tw + 3.0;
            if let Some(m) = mc {
                let r = th * 0.62;
                res.prims.push(Prim::Circle { c: DVec2::new(x + r - 0.5, p0.y), r, style: LineStyle::Thin });
                res.prims.push(Prim::Text { pos: DVec2::new(x + r - 0.5, p0.y - th * 0.35), height: th * 0.7, angle_deg: 0.0, text: m.into(), anchor: Anchor::Middle });
                x += 2.0 * r + 1.0;
            }
            for d in &datums {
                seps.push(x);
                let w = (text_width(d, th) + 3.0).max(h);
                res.prims.push(Prim::Text { pos: DVec2::new(x + w / 2.0, base), height: th, angle_deg: 0.0, text: d.trim().to_string(), anchor: Anchor::Middle });
                x += w;
            }
            res.prims.push(Prim::Polyline { pts: vec![DVec2::new(p0.x, y0), DVec2::new(x, y0), DVec2::new(x, y1), DVec2::new(p0.x, y1), DVec2::new(p0.x, y0)], style: LineStyle::Thin });
            for sx in seps {
                res.prims.push(line(DVec2::new(sx, y0), DVec2::new(sx, y1), LineStyle::Thin));
            }
            res.value = tol.trim_start_matches('⌀').parse().ok();
            let mut t = format!("{} {tol}", symbol.glyph());
            if let Some(m) = mc {
                t += &format!("({m})");
            }
            for d in &datums {
                t += &format!(" | {}", d.trim());
            }
            res.text = t;
        }
        K::Datum { view, target, label, position } => {
            let foot = nearest_on(&ctx.resolve(*view, target)?, *position);
            let mut dir = (*position - foot).normalize_or_zero();
            if dir == DVec2::ZERO {
                dir = DVec2::Y;
            }
            // filled triangle standing on the feature, apex toward the box
            let (half, tall) = (1.6, 2.8);
            let apex = foot + dir * tall;
            res.prims.push(Prim::Fill { pts: vec![foot + dir.perp() * half, foot - dir.perp() * half, apex] });
            let s = 7.0;
            // where the leader from the triangle meets the square
            let edge = *position - dir * (s / 2.0 / dir.x.abs().max(dir.y.abs()));
            if (edge - foot).dot(dir) > tall {
                res.prims.push(line(apex, edge, LineStyle::Thin));
            }
            let (lo, hi) = (*position - DVec2::splat(s / 2.0), *position + DVec2::splat(s / 2.0));
            res.prims.push(Prim::Polyline { pts: vec![lo, DVec2::new(hi.x, lo.y), hi, DVec2::new(lo.x, hi.y), lo], style: LineStyle::Thin });
            let l = label.trim().to_string();
            res.prims.push(text(*position - DVec2::new(0.0, TEXT_H / 2.0), &l, 0.0, Anchor::Middle));
            res.text = l;
        }
        K::CenterMark { view, circle } => {
            let Resolved::Circle { c, r, face_on } = ctx.resolve(*view, circle)? else {
                return Err("center mark needs a circular edge".into());
            };
            if !face_on {
                return Err("the circle is not seen face-on in this view".into());
            }
            let e = r + 2.0;
            res.prims.push(line(c - DVec2::X * e, c + DVec2::X * e, LineStyle::Center));
            res.prims.push(line(c - DVec2::Y * e, c + DVec2::Y * e, LineStyle::Center));
        }
        K::Centerline { view, a: ra, b: rb } => {
            let (p, q) = (ctx.resolve(*view, ra)?.point(), ctx.resolve(*view, rb)?.point());
            let d = (q - p).normalize_or_zero();
            if d == DVec2::ZERO {
                return Err("the centre line references coincide".into());
            }
            res.prims.push(line(p - d * 2.0, q + d * 2.0, LineStyle::Center));
        }
        K::PartingLine { view, feature } => {
            let f = ctx.frame(*view)?;
            let feat = ctx.data.feature(*feature).ok_or("the parting feature no longer exists")?;
            let (plane, off) = match &feat.kind {
                FeatureKind::PartingJoint { plane, offset, .. } => (*plane, offset.eval(vals).map_err(|e| e.to_string())?),
                FeatureKind::Split { plane, .. } => (*plane, 0.0),
                _ => return Err("the feature is not a parting joint or split".into()),
            };
            if feat.suppressed {
                return Err("the parting feature is suppressed".into());
            }
            let n = plane.normal.normalize();
            if n.dot(f.n).abs() > 0.01 {
                return Err("the parting plane is not seen edge-on in this view".into());
            }
            let p0 = f.sheet(plane.origin + n * off);
            let d = f.sheet_dir(n.cross(f.n)).normalize_or_zero();
            let reach = ctx.regen.solids().filter_map(|b| b.shape().bbox().ok()).map(|bb| bb.size().length()).fold(0.0, f64::max);
            let half = reach * f.scale / 2.0 + 5.0;
            // centre the trace on the projected model centre along the line
            let c3 = ctx.regen.solids().filter_map(|b| b.shape().bbox().ok()).map(|bb| bb.center()).next().unwrap_or(DVec3::ZERO);
            let cc = f.sheet(c3);
            let mid = p0 + d * (cc - p0).dot(d);
            let (s, e) = (mid - d * half, mid + d * half);
            res.prims.push(line(s, e, LineStyle::Phantom));
            res.value = Some(off);
            push_text(res, e + d * 4.0, "PL".into(), 0.0);
        }
    }
    Ok(())
}

const FCF_H: f64 = 6.0;
const FCF_TEXT: f64 = 3.0;

/// The point of a resolved reference nearest `p` (a line's foot, a circle's rim).
fn nearest_on(r: &Resolved, p: DVec2) -> DVec2 {
    match r {
        Resolved::Line { a, b } => {
            let ab = *b - *a;
            let t = if ab.length_squared() > 1e-18 { ((p - *a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            *a + ab * t
        }
        Resolved::Circle { c, r, .. } => {
            let d = (p - *c).normalize_or_zero();
            *c + if d == DVec2::ZERO { DVec2::X } else { d } * *r
        }
        other => other.point(),
    }
}

/// A characteristic symbol drawn as lines, centred at `c`, about `2a` across.
fn gdt_symbol(out: &mut Vec<Prim>, sym: GdtSymbol, c: DVec2, a: f64) {
    let p = |x: f64, y: f64| c + DVec2::new(x, y) * a;
    let pl = |out: &mut Vec<Prim>, pts: Vec<DVec2>| out.push(Prim::Polyline { pts, style: LineStyle::Thin });
    let arc = |cx: f64, cy: f64, r: f64, a0: f64, a1: f64| -> Vec<DVec2> { (0..=20).map(|i| p(cx, cy) + DVec2::from_angle(a0 + (a1 - a0) * i as f64 / 20.0) * r * a).collect() };
    let circle = |out: &mut Vec<Prim>, r: f64| out.push(Prim::Circle { c, r: r * a, style: LineStyle::Thin });
    let pi = std::f64::consts::PI;
    match sym {
        GdtSymbol::Straightness => pl(out, vec![p(-1.0, 0.0), p(1.0, 0.0)]),
        GdtSymbol::Flatness => pl(out, vec![p(-1.0, -0.5), p(0.55, -0.5), p(1.0, 0.5), p(-0.55, 0.5), p(-1.0, -0.5)]),
        GdtSymbol::Circularity => circle(out, 0.85),
        GdtSymbol::Cylindricity => {
            circle(out, 0.6);
            let (u, n) = (DVec2::from_angle(pi / 3.0), DVec2::from_angle(pi / 3.0).perp());
            for s in [-1.0, 1.0] {
                let m = n * 0.6 * s;
                pl(out, vec![p(m.x - u.x * 1.1, m.y - u.y * 1.1), p(m.x + u.x * 1.1, m.y + u.y * 1.1)]);
            }
        }
        GdtSymbol::ProfileOfLine => pl(out, arc(0.0, -0.45, 0.95, 0.0, pi)),
        GdtSymbol::ProfileOfSurface => {
            let mut v = arc(0.0, -0.45, 0.95, 0.0, pi);
            v.push(v[0]);
            pl(out, v);
        }
        GdtSymbol::Angularity => pl(out, vec![p(0.9, 0.85), p(-1.0, -0.7), p(1.0, -0.7)]),
        GdtSymbol::Perpendicularity => {
            pl(out, vec![p(-1.0, -0.8), p(1.0, -0.8)]);
            pl(out, vec![p(0.0, -0.8), p(0.0, 1.0)]);
        }
        GdtSymbol::Parallelism => {
            pl(out, vec![p(-0.8, -1.0), p(-0.1, 1.0)]);
            pl(out, vec![p(0.1, -1.0), p(0.8, 1.0)]);
        }
        GdtSymbol::Position => {
            circle(out, 0.6);
            pl(out, vec![p(-1.0, 0.0), p(1.0, 0.0)]);
            pl(out, vec![p(0.0, -1.0), p(0.0, 1.0)]);
        }
        GdtSymbol::Concentricity => {
            circle(out, 0.45);
            circle(out, 0.9);
        }
        GdtSymbol::Symmetry => {
            pl(out, vec![p(-0.6, 0.5), p(0.6, 0.5)]);
            pl(out, vec![p(-1.0, 0.0), p(1.0, 0.0)]);
            pl(out, vec![p(-0.6, -0.5), p(0.6, -0.5)]);
        }
        GdtSymbol::CircularRunout => {
            pl(out, vec![p(-0.6, -1.0), p(0.6, 1.0)]);
            out.push(Prim::Arrow { tip: p(0.6, 1.0), dir: DVec2::new(0.6, 1.0).normalize(), size: a * 0.9 });
        }
        GdtSymbol::TotalRunout => {
            pl(out, vec![p(-1.0, -0.7), p(0.35, -0.7)]);
            for x in [-1.0, -0.15] {
                pl(out, vec![p(x, -0.7), p(x + 0.65, 1.0)]);
                out.push(Prim::Arrow { tip: p(x + 0.65, 1.0), dir: DVec2::new(0.65, 1.7).normalize(), size: a * 0.8 });
            }
        }
    }
}

fn balloon(res: &mut AnnotationResult, anchor: DVec2, position: DVec2, n: usize) {
    let r = 4.0;
    let d = (position - anchor).normalize_or_zero();
    res.prims.push(line(anchor, position - d * r, LineStyle::Thin));
    res.prims.push(Prim::Circle { c: position, r, style: LineStyle::Thin });
    res.prims.push(Prim::Circle { c: anchor, r: 0.5, style: LineStyle::Thin });
    res.value = Some(n as f64);
    res.prims.push(text(position - DVec2::new(0.0, TEXT_H * 0.35), &n.to_string(), 0.0, Anchor::Middle));
    res.text = n.to_string();
}

// ---------------------------------------------------------------------------
// Export

/// Visible output primitives of a computed drawing in drawing order.
pub fn primitives(r: &DrawingResult, d: &Drawing) -> Vec<Prim> {
    let mut v = r.frame.clone();
    for vr in &r.views {
        let view = d.views.iter().find(|x| x.id == vr.id);
        let hidden = view.is_some_and(|x| x.hidden_lines);
        let smooth = view.is_some_and(|x| x.smooth_edges);
        for e in &vr.edges {
            if !e.visible && !hidden {
                continue;
            }
            if e.kind == HlrKindS::Smooth && !smooth {
                continue;
            }
            v.push(Prim::Polyline { pts: e.points.clone(), style: if e.visible { LineStyle::Visible } else { LineStyle::Hidden } });
        }
        v.extend(vr.hatch.iter().map(|h| Prim::Polyline { pts: h.clone(), style: LineStyle::Hatch }));
        v.extend(vr.marks.iter().cloned());
    }
    for a in &r.annotations {
        v.extend(a.prims.iter().cloned());
    }
    v
}

fn style_svg(s: LineStyle) -> (&'static str, f64, &'static str) {
    match s {
        LineStyle::Visible => ("#000", 0.35, ""),
        LineStyle::Hidden => ("#000", 0.25, "1.5 0.75"),
        LineStyle::Thin => ("#000", 0.18, ""),
        LineStyle::Center => ("#000", 0.18, "6 1.5 1 1.5"),
        LineStyle::Phantom => ("#000", 0.25, "8 1.5 1 1.5 1 1.5"),
        LineStyle::Frame => ("#000", 0.5, ""),
        LineStyle::Hatch => ("#000", 0.13, ""),
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn arrow_pts(tip: DVec2, dir: DVec2, size: f64) -> [DVec2; 3] {
    let back = tip - dir * size;
    let w = dir.perp() * size * 0.3;
    [tip, back + w, back - w]
}

/// SVG (mm units, y up flipped to SVG's y down).
pub fn to_svg(r: &DrawingResult, d: &Drawing) -> String {
    let h = r.size.y;
    let f = |p: DVec2| format!("{:.3},{:.3}", p.x, h - p.y);
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}mm\" height=\"{h}mm\" viewBox=\"0 0 {w} {h}\">\n<title>{t}</title>\n<rect width=\"{w}\" height=\"{h}\" fill=\"#fff\"/>\n",
        w = r.size.x,
        h = h,
        t = xml(&d.name)
    );
    for p in primitives(r, d) {
        match p {
            Prim::Polyline { pts, style } => {
                let (c, w, dash) = style_svg(style);
                let pts: Vec<String> = pts.iter().map(|p| f(*p)).collect();
                let dash = if dash.is_empty() { String::new() } else { format!(" stroke-dasharray=\"{dash}\"") };
                s += &format!("<polyline points=\"{}\" fill=\"none\" stroke=\"{c}\" stroke-width=\"{w}\"{dash}/>\n", pts.join(" "));
            }
            Prim::Circle { c, r: rad, style } => {
                let (col, w, _) = style_svg(style);
                s += &format!("<circle cx=\"{:.3}\" cy=\"{:.3}\" r=\"{rad:.3}\" fill=\"none\" stroke=\"{col}\" stroke-width=\"{w}\"/>\n", c.x, h - c.y);
            }
            Prim::Text { pos, height, angle_deg, text, anchor } => {
                let a = match anchor {
                    Anchor::Start => "start",
                    Anchor::Middle => "middle",
                    Anchor::End => "end",
                };
                s += &format!(
                    "<text x=\"{:.3}\" y=\"{:.3}\" font-family=\"Helvetica, Arial, sans-serif\" font-size=\"{:.3}\" text-anchor=\"{a}\" transform=\"rotate({:.3} {:.3} {:.3})\">{}</text>\n",
                    pos.x,
                    h - pos.y,
                    height * FONT_EM,
                    -angle_deg,
                    pos.x,
                    h - pos.y,
                    xml(&text)
                );
            }
            Prim::Arrow { tip, dir, size } => {
                let pts: Vec<String> = arrow_pts(tip, dir, size).iter().map(|p| f(*p)).collect();
                s += &format!("<polygon points=\"{}\" fill=\"#000\"/>\n", pts.join(" "));
            }
            Prim::Fill { pts } => {
                let pts: Vec<String> = pts.iter().map(|p| f(*p)).collect();
                s += &format!("<polygon points=\"{}\" fill=\"#000\"/>\n", pts.join(" "));
            }
        }
    }
    s + "</svg>\n"
}

fn layer(s: LineStyle) -> &'static str {
    match s {
        LineStyle::Visible => "VISIBLE",
        LineStyle::Hidden => "HIDDEN",
        LineStyle::Thin => "DIM",
        LineStyle::Center => "CENTER",
        LineStyle::Phantom => "PARTING",
        LineStyle::Frame => "FRAME",
        LineStyle::Hatch => "HATCH",
    }
}

/// ASCII DXF (R12 entities: LINE, CIRCLE, TEXT, SOLID), mm, layered by line style.
pub fn to_dxf(r: &DrawingResult, d: &Drawing) -> String {
    let mut s = String::from("0\nSECTION\n2\nHEADER\n9\n$INSUNITS\n70\n4\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n");
    let ln = |s: &mut String, a: DVec2, b: DVec2, l: &str| {
        *s += &format!("0\nLINE\n8\n{l}\n10\n{:.4}\n20\n{:.4}\n30\n0\n11\n{:.4}\n21\n{:.4}\n31\n0\n", a.x, a.y, b.x, b.y);
    };
    for p in primitives(r, d) {
        match p {
            Prim::Polyline { pts, style } => {
                for w in pts.windows(2) {
                    ln(&mut s, w[0], w[1], layer(style));
                }
            }
            Prim::Circle { c, r: rad, style } => {
                s += &format!("0\nCIRCLE\n8\n{}\n10\n{:.4}\n20\n{:.4}\n30\n0\n40\n{:.4}\n", layer(style), c.x, c.y, rad);
            }
            Prim::Text { pos, height, angle_deg, text, anchor } => {
                let h = match anchor {
                    Anchor::Start => 0,
                    Anchor::Middle => 1,
                    Anchor::End => 2,
                };
                let t = text.replace('⌀', "%%c").replace('°', "%%d");
                s += &format!(
                    "0\nTEXT\n8\nTEXT\n10\n{:.4}\n20\n{:.4}\n30\n0\n40\n{height}\n1\n{t}\n50\n{angle_deg:.3}\n72\n{h}\n11\n{:.4}\n21\n{:.4}\n31\n0\n",
                    pos.x, pos.y, pos.x, pos.y
                );
            }
            Prim::Arrow { tip, dir, size } => {
                let [a, b, c] = arrow_pts(tip, dir, size);
                s += &format!(
                    "0\nSOLID\n8\nDIM\n10\n{:.4}\n20\n{:.4}\n30\n0\n11\n{:.4}\n21\n{:.4}\n31\n0\n12\n{:.4}\n22\n{:.4}\n32\n0\n13\n{:.4}\n23\n{:.4}\n33\n0\n",
                    a.x, a.y, b.x, b.y, c.x, c.y, c.x, c.y
                );
            }
            Prim::Fill { pts } => {
                // SOLID takes 3 or 4 corners (in Z order); fan larger polygons
                for i in 1..pts.len().saturating_sub(1) {
                    let (a, b, c) = (pts[0], pts[i], pts[i + 1]);
                    s += &format!(
                        "0\nSOLID\n8\nDIM\n10\n{:.4}\n20\n{:.4}\n30\n0\n11\n{:.4}\n21\n{:.4}\n31\n0\n12\n{:.4}\n22\n{:.4}\n32\n0\n13\n{:.4}\n23\n{:.4}\n33\n0\n",
                        a.x, a.y, b.x, b.y, c.x, c.y, c.x, c.y
                    );
                }
            }
        }
    }
    s + "0\nENDSEC\n0\nEOF\n"
}

fn win_ansi(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| match c {
            ' '..='~' => c as u8,
            '⌀' | 'Ø' => 0xD8,
            '°' => 0xB0,
            '×' => 0xD7,
            '±' => 0xB1,
            _ => b'?',
        })
        .collect()
}

/// Single-page vector PDF (base-14 Helvetica, no font embedding needed).
pub fn to_pdf(r: &DrawingResult, d: &Drawing) -> Vec<u8> {
    use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};
    let k = 72.0 / 25.4;
    let (w, h) = ((r.size.x * k) as f32, (r.size.y * k) as f32);
    let (cat, tree, page, font, content_id) = (Ref::new(1), Ref::new(2), Ref::new(3), Ref::new(4), Ref::new(5));
    let mut pdf = Pdf::new();
    pdf.catalog(cat).pages(tree);
    pdf.pages(tree).kids([page]).count(1);
    let mut pg = pdf.page(page);
    pg.media_box(Rect::new(0.0, 0.0, w, h));
    pg.parent(tree);
    pg.contents(content_id);
    pg.resources().fonts().pair(Name(b"F1"), font);
    pg.finish();
    pdf.type1_font(font).base_font(Name(b"Helvetica")).encoding_predefined(Name(b"WinAnsiEncoding"));
    let mut c = Content::new();
    let pt = |p: DVec2| ((p.x * k) as f32, (p.y * k) as f32);
    for p in primitives(r, d) {
        match p {
            Prim::Polyline { pts, style } => {
                let (_, lw, dash) = style_svg(style);
                c.set_line_width((lw * k) as f32);
                let dashes: Vec<f32> = dash.split_whitespace().filter_map(|x| x.parse::<f64>().ok()).map(|x| (x * k) as f32).collect();
                c.set_dash_pattern(dashes, 0.0);
                if let Some(first) = pts.first() {
                    let (x, y) = pt(*first);
                    c.move_to(x, y);
                    for q in &pts[1..] {
                        let (x, y) = pt(*q);
                        c.line_to(x, y);
                    }
                    c.stroke();
                }
            }
            Prim::Circle { c: ctr, r: rad, style } => {
                let (_, lw, _) = style_svg(style);
                c.set_line_width((lw * k) as f32);
                c.set_dash_pattern([], 0.0);
                let n = 48;
                for i in 0..=n {
                    let q = ctr + DVec2::from_angle(std::f64::consts::TAU * i as f64 / n as f64) * rad;
                    let (x, y) = pt(q);
                    if i == 0 {
                        c.move_to(x, y);
                    } else {
                        c.line_to(x, y);
                    }
                }
                c.stroke();
            }
            Prim::Text { pos, height, angle_deg, text, anchor } => {
                let size = (height * k * FONT_EM) as f32;
                let width = (text_width(&text, height) * k) as f32;
                let shift = match anchor {
                    Anchor::Start => 0.0,
                    Anchor::Middle => -width / 2.0,
                    Anchor::End => -width,
                };
                let (x, y) = pt(pos);
                let (s, co) = (angle_deg.to_radians().sin() as f32, angle_deg.to_radians().cos() as f32);
                c.begin_text();
                c.set_font(Name(b"F1"), size);
                c.set_text_matrix([co, s, -s, co, x + co * shift, y + s * shift]);
                c.show(Str(&win_ansi(&text)));
                c.end_text();
            }
            Prim::Arrow { tip, dir, size } => {
                let [a, b, e] = arrow_pts(tip, dir, size);
                let (ax, ay) = pt(a);
                let (bx, by) = pt(b);
                let (ex, ey) = pt(e);
                c.move_to(ax, ay);
                c.line_to(bx, by);
                c.line_to(ex, ey);
                c.close_path();
                c.fill_nonzero();
            }
            Prim::Fill { pts } => {
                for (i, q) in pts.iter().enumerate() {
                    let (x, y) = pt(*q);
                    if i == 0 {
                        c.move_to(x, y);
                    } else {
                        c.line_to(x, y);
                    }
                }
                if !pts.is_empty() {
                    c.close_path();
                    c.fill_nonzero();
                }
            }
        }
    }
    let data = c.finish();
    pdf.stream(content_id, &data);
    pdf.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_bases_are_right_handed() {
        for f in [CubeFace::PosX, CubeFace::NegX, CubeFace::PosY, CubeFace::NegY, CubeFace::PosZ, CubeFace::NegZ] {
            let (n, x, up) = ViewOrientation::Face(f).basis();
            assert!((x.cross(up) - n).length() < 1e-12, "{f:?}");
        }
        let (n, x, up) = ViewOrientation::Face(CubeFace::NegY).basis();
        assert_eq!((n, x, up), (DVec3::NEG_Y, DVec3::X, DVec3::Z), "front view");
        let (n, _, up) = ViewOrientation::Iso(IsoCorner { x: 1, y: -1, z: 1 }).basis();
        assert!(up.z > 0.0 && (n.length() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn values_and_scales() {
        assert_eq!(fmt_value(40.0), "40");
        assert_eq!(fmt_value(12.5), "12.5");
        assert_eq!(fmt_value(1.23456), "1.23");
        assert_eq!(scale_text(2.0), "2:1");
        assert_eq!(scale_text(0.5), "1:2");
    }
}
