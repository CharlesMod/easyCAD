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

use glam::{DQuat, DVec2, DVec3};
use occt::{CurveKind, EdgeInfo, HlrKind};
use serde::{Deserialize, Serialize};

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
        });
        id
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
#[derive(Clone, Copy, Debug)]
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

type LocalEdge = (Vec<DVec2>, bool, HlrKindS, Option<(BodyId, u32)>);

/// HLR output of one body in one orientation (local coordinates), with sources.
struct HlrBody {
    _geom: Arc<BodyGeom>,
    edges: Vec<(Vec<DVec2>, bool, HlrKindS, Option<u32>)>,
    poly: bool,
}

/// Caches hidden-line results per (body geometry, orientation, algorithm).
#[derive(Default)]
pub struct DrawingCache {
    hlr: HashMap<(usize, [u64; 6], bool), Arc<HlrBody>>,
}

impl DrawingCache {
    pub fn clear(&mut self) {
        self.hlr.clear();
    }
    pub fn len(&self) -> usize {
        self.hlr.len()
    }
    pub fn is_empty(&self) -> bool {
        self.hlr.is_empty()
    }
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

struct Ctx<'a> {
    data: &'a DocData,
    regen: &'a RegenResult,
    frames: HashMap<ViewId, (ViewFrame, Vec<BodyId>, &'a View)>,
    view_sources: HashMap<ViewId, Vec<(BodyId, u32)>>,
}

impl Ctx<'_> {
    fn frame(&self, v: ViewId) -> Result<(ViewFrame, &Vec<BodyId>), String> {
        self.frames.get(&v).map(|(f, b, _)| (*f, b)).ok_or_else(|| format!("view {v:?} does not exist"))
    }

    fn resolve(&self, view: ViewId, r: &DimRef) -> Result<Resolved, String> {
        let (f, bodies) = self.frame(view)?;
        let body_id = match r {
            DimRef::Edge { body, .. } | DimRef::Vertex { body, .. } => *body,
        };
        if !bodies.contains(&body_id) {
            return Err("the referenced body is not shown in this view".into());
        }
        let body = self.regen.body_by_id(body_id).ok_or("the referenced body no longer exists")?;
        let shape = body.shape();
        match r {
            DimRef::Edge { edge, .. } => {
                let ids = edge.resolve(shape).map_err(|e| format!("reference lost: {e}"))?;
                let infos = shape.edges().map_err(|e| e.0)?;
                let e = &infos[ids[0] as usize];
                let shown = self.view_sources.get(&view).is_some_and(|s| s.contains(&(body_id, e.index)));
                let face_on_circle = matches!(e.circle, Some((_, ax, _)) if ax.normalize().dot(f.n).abs() > 0.999);
                if !shown && !face_on_circle {
                    return Err(if e.circle.is_some() {
                        "the circle is not seen face-on in this view".into()
                    } else {
                        "the referenced edge is not drawn in this view".into()
                    });
                }
                Ok(match (e.kind, e.circle) {
                    (CurveKind::Line, _) => Resolved::Line { a: f.sheet(e.start), b: f.sheet(e.end) },
                    (CurveKind::Circle, Some((c, ax, r))) => {
                        Resolved::Circle { c: f.sheet(c), r: r * f.scale, face_on: ax.normalize().dot(f.n).abs() > 0.999 }
                    }
                    _ => Resolved::Other { mid: f.sheet(e.midpoint) },
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
                    .map(|(p, _)| Resolved::Point(f.sheet(p)))
                    .ok_or_else(|| "the referenced vertex no longer exists".into())
            }
        }
    }
}

const TEXT_H: f64 = 3.5;
const ARROW: f64 = 2.5;

fn line(a: DVec2, b: DVec2, style: LineStyle) -> Prim {
    Prim::Polyline { pts: vec![a, b], style }
}

fn text(pos: DVec2, s: &str, angle: f64, anchor: Anchor) -> Prim {
    Prim::Text { pos, height: TEXT_H, angle_deg: angle, text: s.into(), anchor }
}

fn arrow(tip: DVec2, dir: DVec2) -> Prim {
    Prim::Arrow { tip, dir: dir.normalize_or_zero(), size: ARROW }
}

/// Compute a drawing against a regenerated model.
pub fn compute(data: &DocData, regen: &RegenResult, bom: &[BomLine], drawing: &Drawing, cache: &mut DrawingCache) -> DrawingResult {
    let vals = data.values().unwrap_or_default();
    let size = drawing.sheet.size_mm();
    let mut views = Vec::new();
    let mut ctx = Ctx { data, regen, frames: HashMap::new(), view_sources: HashMap::new() };
    for v in &drawing.views {
        let (n, x, up) = v.orientation.basis();
        let bodies: Vec<BodyId> = if v.bodies.is_empty() { regen.solids().map(|b| b.id).collect() } else { v.bodies.clone() };
        let mut local: Vec<LocalEdge> = Vec::new();
        let mut err = None;
        let mut poly = false;
        for bid in &bodies {
            let Some(b) = regen.body_by_id(*bid) else {
                err = Some(format!("body {bid:?} no longer exists"));
                continue;
            };
            match hlr_body(cache, b, n, x, v.exact) {
                Ok(h) => {
                    poly |= h.poly;
                    for (pts, vis, kind, src) in &h.edges {
                        local.push((pts.clone(), *vis, *kind, src.map(|s| (*bid, s))));
                    }
                }
                Err(e) => err = Some(e),
            }
        }
        let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for (pts, ..) in &local {
            for p in pts {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        let center = if lo.x.is_finite() { (lo + hi) / 2.0 } else { DVec2::ZERO };
        let frame = ViewFrame { n, x, up, rot: v.rotation_deg.to_radians(), scale: v.scale, position: v.position, center };
        let edges: Vec<ViewEdge> = local
            .into_iter()
            .map(|(pts, visible, kind, source)| ViewEdge { points: pts.iter().map(|p| frame.local_to_sheet(*p)).collect(), visible, kind, source })
            .collect();
        let (mut smin, mut smax) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for e in edges.iter().filter(|e| e.visible || v.hidden_lines) {
            for p in &e.points {
                smin = smin.min(*p);
                smax = smax.max(*p);
            }
        }
        ctx.view_sources.insert(v.id, edges.iter().filter_map(|e| e.source).collect());
        ctx.frames.insert(v.id, (frame, bodies, v));
        views.push(ViewResult { id: v.id, edges, min: smin, max: smax, poly_fallback: poly, error: err });
    }

    let first_scale = drawing.views.first().map(|v| v.scale).unwrap_or(1.0);
    let annotations = drawing.annotations.iter().map(|a| annotate(&ctx, a, &vals, bom, first_scale)).collect();

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
    DrawingResult { size, views, annotations, frame, title }
}

fn annotate(ctx: &Ctx, a: &Annotation, vals: &Values, bom: &[BomLine], scale0: f64) -> AnnotationResult {
    let mut res = AnnotationResult { id: a.id, status: AnnotationStatus::Ok, value: None, text: String::new(), prims: Vec::new() };
    if let Err(e) = annotate_inner(ctx, a, vals, bom, scale0, &mut res) {
        res.status = AnnotationStatus::Dangling(e);
        res.prims.clear();
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
    res
}

fn annotate_inner(ctx: &Ctx, a: &Annotation, vals: &Values, bom: &[BomLine], scale0: f64, res: &mut AnnotationResult) -> Result<(), String> {
    use AnnotationKind as K;
    let push_text = |res: &mut AnnotationResult, pos: DVec2, s: String, angle: f64| {
        res.prims.push(text(pos, &s, angle, Anchor::Middle));
        res.text = s;
    };
    match &a.kind {
        K::Linear { view, a: ra, b: rb, kind, offset } => {
            let (f, _) = ctx.frame(*view)?;
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
        }
        K::Ordinate { view, origin, target, axis, offset } => {
            let (f, _) = ctx.frame(*view)?;
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
        }
        K::Radius { view, circle, diameter, leader_deg } => {
            let (f, _) = ctx.frame(*view)?;
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
        }
        K::Chamfer { view, edge, offset } => {
            let (f, _) = ctx.frame(*view)?;
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
            let (f, bodies) = ctx.frame(*view)?;
            let _ = bodies;
            let b = ctx.regen.body_by_id(*body).ok_or("the ballooned body no longer exists")?;
            let pu = b.part.as_ref().ok_or("the ballooned body is not a library part")?;
            let row = bom
                .iter()
                .position(|l| l.source == pu.part.source && l.part == pu.part.id && l.size == pu.part.size)
                .ok_or("the part is not in the BOM")?;
            let anchor = f.sheet(b.shape().bbox().map_err(|e| e.0)?.center());
            let r = 4.0;
            let d = (*position - anchor).normalize_or_zero();
            res.prims.push(line(anchor, *position - d * r, LineStyle::Thin));
            res.prims.push(Prim::Circle { c: *position, r, style: LineStyle::Thin });
            res.prims.push(Prim::Circle { c: anchor, r: 0.5, style: LineStyle::Thin });
            res.value = Some((row + 1) as f64);
            push_text(res, *position - DVec2::new(0.0, TEXT_H * 0.35), (row + 1).to_string(), 0.0);
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
            let (f, _) = ctx.frame(*view)?;
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
                    "<text x=\"{:.3}\" y=\"{:.3}\" font-family=\"sans-serif\" font-size=\"{height}\" text-anchor=\"{a}\" transform=\"rotate({:.3} {:.3} {:.3})\">{}</text>\n",
                    pos.x,
                    h - pos.y,
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
                let size = (height * k * 1.4) as f32;
                let width = 0.5 * size * text.chars().count() as f32;
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
