//! Regeneration: replay the feature list into bodies, caching the model state after
//! every feature under a chained hash of everything that feature depends on. Undo,
//! redo and re-applying an earlier edit therefore hit the cache for every unchanged
//! prefix and only recompute from the first changed feature.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, OnceLock};

use glam::{DVec2, DVec3};
use occt::{dressup, DressUpReport, Edge, Frame, Mesh, Shape, SurfaceKind};
use serde::Serialize;

use crate::document::{BlobStore, DocData};
use crate::dxf::DxfDrawing;
use crate::feature::*;
use crate::geom::{Plane, Profile, Segment2};
use crate::library::{local_mate, place, Library, PartInfo, PartInstance};
use crate::params::{Expr, Values};
use crate::process::{Fit, FitClass, ProcessProfile, TeardropStyle};
use crate::select::{resolve_edges, resolve_faces, SelectError, Selector};
use crate::sketch::{segment_point, Sketch, SolveReport};

/// Geometry of a body plus its lazily computed display mesh.
pub struct BodyGeom {
    pub shape: Shape,
    meshes: [OnceLock<Result<Arc<Mesh>, String>>; 3],
    /// Names/colours of sub-parts (imported STEP assemblies).
    pub appearance: Option<Appearance>,
}

/// A named, optionally coloured sub-part of a body (e.g. one component of a PCB).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SubPart {
    pub name: String,
    /// sRGB 0..1.
    pub color: Option<[f32; 3]>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Appearance {
    pub parts: Vec<SubPart>,
    /// Owning part per face (face index of the body's shape).
    pub face_part: Vec<u32>,
}

/// Colour used for faces without one.
pub const DEFAULT_COLOR: [f32; 3] = [0.72, 0.74, 0.78];

impl BodyGeom {
    pub fn new(shape: Shape) -> Arc<Self> {
        Arc::new(BodyGeom { shape, meshes: Default::default(), appearance: None })
    }
    pub fn with_appearance(shape: Shape, appearance: Appearance) -> Arc<Self> {
        Arc::new(BodyGeom { shape, meshes: Default::default(), appearance: Some(appearance) })
    }
}

/// Tessellation levels. `Preview` is for interactive dragging, `Display` the
/// default viewport mesh, `Final` print/export quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum MeshQuality {
    Preview,
    Display,
    Final,
}

impl MeshQuality {
    /// (linear deflection mm, angular deflection deg).
    pub fn deflection(self) -> (f64, f64) {
        match self {
            MeshQuality::Preview => (0.25, 25.0),
            MeshQuality::Display => (MESH_LIN_DEFL, MESH_ANG_DEFL_DEG),
            MeshQuality::Final => (0.02, 8.0),
        }
    }
    fn slot(self) -> usize {
        self as usize
    }
}

/// A library part placed by an `InsertPart` feature.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PartUse {
    pub feature: FeatureId,
    pub part: PartRef,
}

#[derive(Clone)]
pub struct Body {
    pub id: BodyId,
    pub name: String,
    /// Reference bodies (PCBs, library parts) are never booleaned or printed.
    pub reference: bool,
    pub part: Option<PartUse>,
    pub geom: Arc<BodyGeom>,
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body").field("id", &self.id).field("name", &self.name).field("reference", &self.reference).finish()
    }
}

/// Default display tessellation.
pub const MESH_LIN_DEFL: f64 = 0.05;
pub const MESH_ANG_DEFL_DEG: f64 = 12.0;

impl Body {
    pub fn shape(&self) -> &Shape {
        &self.geom.shape
    }
    /// Display mesh (computed once per shape, shared by every cached state).
    pub fn mesh(&self) -> Result<Arc<Mesh>, String> {
        self.mesh_quality(MeshQuality::Display)
    }
    /// Sub-part names and colours (imported assemblies), if any.
    pub fn appearance(&self) -> Option<&Appearance> {
        self.geom.appearance.as_ref()
    }
    /// Colour per face index, if the body has an appearance.
    pub fn face_colors(&self) -> Option<Vec<[f32; 3]>> {
        let a = self.appearance()?;
        Some(a.face_part.iter().map(|p| a.parts.get(*p as usize).and_then(|x| x.color).unwrap_or(DEFAULT_COLOR)).collect())
    }
    /// Per-vertex colours for a mesh of this body (`DEFAULT_COLOR` where unknown).
    pub fn mesh_colors(&self, q: MeshQuality) -> Result<Vec<[f32; 3]>, String> {
        let m = self.mesh_quality(q)?;
        let mut out = vec![DEFAULT_COLOR; m.positions.len()];
        if let Some(fc) = self.face_colors() {
            for (t, f) in m.indices.iter().zip(&m.tri_face) {
                let c = fc.get(*f as usize).copied().unwrap_or(DEFAULT_COLOR);
                for v in t {
                    out[*v as usize] = c;
                }
            }
        }
        Ok(out)
    }

    /// Mesh at a quality level (computed once per shape and level, shared by every
    /// cached state that holds this geometry).
    pub fn mesh_quality(&self, q: MeshQuality) -> Result<Arc<Mesh>, String> {
        let (l, a) = q.deflection();
        self.geom.meshes[q.slot()].get_or_init(|| self.geom.shape.tessellate(l, a).map(Arc::new).map_err(|e| e.0)).clone()
    }
}

/// A body cut open by a plane, for display ("Section view", "Slice body at sketch
/// plane"). GUI_NEEDS #9.
#[derive(Clone)]
pub struct SectionView {
    /// What stays on the back side of the plane (against its normal), as a body
    /// with the original id and name; the whole body when the plane misses it on
    /// the front, `None` when nothing is behind the plane.
    pub kept: Option<Body>,
    /// The cut faces (planar regions in the plane); `None` when the plane misses
    /// the body.
    pub cap: Option<Shape>,
    /// Triangles of `cap` (normals face the viewer, i.e. along the plane normal).
    pub cap_mesh: Option<Arc<Mesh>>,
    /// Boundary polylines of the cap (from the cap mesh's edges).
    pub outline: Vec<Vec<DVec3>>,
}

impl Body {
    /// Cut this body by the plane through `origin` with normal `normal` (body
    /// frame); see [`SectionView`]. `q` sets the cap's mesh density.
    pub fn section_view(&self, origin: DVec3, normal: DVec3, q: MeshQuality) -> Result<SectionView, String> {
        let n = normal.normalize_or_zero();
        if n == DVec3::ZERO {
            return Err("section: the plane normal is zero".into());
        }
        let shape = self.shape();
        let (below, above) = shape.split(origin, n).map_err(|e| e.0)?;
        let kept = match (below, above) {
            (Some(b), _) => Some(Body { geom: BodyGeom::new(b), ..self.clone() }),
            (None, None) => Some(self.clone()),
            (None, Some(_)) => None,
        };
        let cap = shape.section(origin, n).ok().filter(|c| c.area().unwrap_or(0.0) > 1e-9);
        let (cap_mesh, outline) = match &cap {
            Some(c) => {
                let (l, a) = q.deflection();
                let mut m = c.tessellate(l, a).map_err(|e| e.0)?;
                // face the viewer: flip triangles whose normal points away
                let p3 = |i: u32| {
                    let v = m.positions[i as usize];
                    DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
                };
                let flip = m.indices.first().map(|t| (p3(t[1]) - p3(t[0])).cross(p3(t[2]) - p3(t[0])).dot(n) < 0.0).unwrap_or(false);
                if flip {
                    for t in m.indices.iter_mut() {
                        t.swap(1, 2);
                    }
                }
                let nf = [n.x as f32, n.y as f32, n.z as f32];
                for v in m.normals.iter_mut() {
                    *v = nf;
                }
                let outline = m.edges.iter().map(|e| e.points.iter().map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect()).collect();
                (Some(Arc::new(m)), outline)
            }
            None => (None, Vec::new()),
        };
        Ok(SectionView { kept, cap, cap_mesh, outline })
    }
}

#[derive(Clone)]
struct ToolOp {
    target: BodyId,
    op: BoolOp,
}

#[derive(Clone, Default)]
struct ModelState {
    bodies: Vec<Body>,
    /// Face provenance per body geometry.
    faces: crate::provenance::FaceTables,
    tools: BTreeMap<FeatureId, Vec<ToolOp>>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum FeatureStatus {
    Ok,
    Suppressed,
    /// Applied, with caveats (e.g. some fillet edges skipped).
    Warning(String),
    /// Not applied; the model continues from the previous state.
    Error(String),
    /// Not applied: a stored face/edge reference no longer matches confidently.
    /// `best_candidate` can be accepted with `Document::accept_candidate`.
    Unresolved(Box<Unresolved>),
}

/// A stored reference that no longer matches confidently.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Unresolved {
    pub selector: Selector,
    pub best_candidate: Option<Selector>,
    /// Mismatch of the best candidate (≤ 1 would have been accepted).
    pub score: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FeatureReport {
    pub feature: FeatureId,
    pub status: FeatureStatus,
    /// Fillet/chamfer details (edges applied / reduced / failed).
    #[serde(skip)]
    pub dressup: Option<DressUpReport>,
    pub sketch: Option<SolveReport>,
    /// Parting-joint details (checks, warnings, O-ring / snap-fit numbers).
    pub joint: Option<crate::joint::JointReport>,
    /// Served from the regeneration cache.
    pub cached: bool,
    /// Time spent computing this feature (0 when cached).
    #[serde(default)]
    pub elapsed_ms: f64,
    /// Fillet/chamfer: for each `dressup.outcomes[k].request`, the edge set
    /// (index into `FeatureKind::edge_sets()`) and the selector within that set
    /// that produced the edge.
    #[serde(default)]
    pub edge_requests: Vec<EdgeRequest>,
    /// Fillet/chamfer: input-shape edges left out because they match the
    /// feature's `skipped` list.
    #[serde(default)]
    pub skipped_edges: Vec<u32>,
    /// The feature is after the document's rollback marker (not computed).
    #[serde(default)]
    pub rolled_back: bool,
}

/// Where a fillet/chamfer request edge came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct EdgeRequest {
    /// Index into `FeatureKind::edge_sets()`.
    pub set: usize,
    /// Index into that set's `edges`.
    pub selector: usize,
}

impl FeatureReport {
    pub fn new(feature: FeatureId, status: FeatureStatus) -> Self {
        FeatureReport {
            feature,
            status,
            dressup: None,
            sketch: None,
            joint: None,
            cached: false,
            elapsed_ms: 0.0,
            edge_requests: Vec::new(),
            skipped_edges: Vec::new(),
            rolled_back: false,
        }
    }
}

#[derive(Clone)]
pub struct RegenResult {
    pub bodies: Vec<Body>,
    pub reports: Vec<FeatureReport>,
    pub recomputed: usize,
    pub reused: usize,
    /// What the final process stage (active profile) did.
    pub process: ProcessReport,
    /// Per body: the feature and role that made each face (see [`RegenResult::face_origin`]).
    pub face_origins: crate::provenance::FaceOrigins,
}

/// A cylindrical hole considered by the teardrop stage.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HoleRef {
    pub body: BodyId,
    /// Face index on the body *before* the process stage.
    pub face: u32,
    pub center: DVec3,
    pub axis: DVec3,
    pub diameter: f64,
    pub length: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProcessReport {
    pub profile: String,
    pub teardropped: Vec<HoleRef>,
    pub skipped: Vec<(HoleRef, String)>,
    pub warnings: Vec<String>,
}

impl RegenResult {
    /// Which feature made face `face` of `body`, and in what role (GUI_NEEDS #8).
    /// Computed on first use (lazily, then cached in the result).
    pub fn face_origin(&self, body: BodyId, face: u32) -> Option<crate::provenance::FaceOrigin> {
        self.face_origins.face(body, face)
    }
    pub fn body(&self, name: &str) -> Option<&Body> {
        self.bodies.iter().find(|b| b.name == name)
    }
    pub fn body_by_id(&self, id: BodyId) -> Option<&Body> {
        self.bodies.iter().find(|b| b.id == id)
    }
    pub fn solids(&self) -> impl Iterator<Item = &Body> {
        self.bodies.iter().filter(|b| !b.reference)
    }
    pub fn report(&self, f: FeatureId) -> Option<&FeatureReport> {
        self.reports.iter().find(|r| r.feature == f)
    }
    pub fn errors(&self) -> Vec<(FeatureId, String)> {
        self.reports
            .iter()
            .filter_map(|r| match &r.status {
                FeatureStatus::Error(e) => Some((r.feature, e.clone())),
                _ => None,
            })
            .collect()
    }
}

type Step = Arc<(ModelState, FeatureReport)>;

/// Holds the per-prefix cache. Keep one per open document.
pub struct Regenerator {
    cache: HashMap<u64, Step>,
    lru: VecDeque<u64>,
    pub capacity: usize,
    imports: HashMap<String, Arc<Shape>>,
    dxfs: HashMap<String, Arc<DxfDrawing>>,
    parts: HashMap<String, Arc<PartInstance>>,
    stages: HashMap<u64, (Vec<Body>, ProcessReport, crate::provenance::FaceOrigins)>,
    xcaf: HashMap<String, Arc<(Shape, Vec<occt::StepPart>)>>,
    /// In-context bodies per source occurrence path, already in this component's
    /// frame (set by the assembly regeneration before `regenerate`).
    pub(crate) context: HashMap<Vec<crate::assembly::OccurrenceId>, Arc<Vec<ContextBody>>>,
    /// Identity of each context entry (source result + relative placement).
    pub(crate) context_keys: HashMap<Vec<crate::assembly::OccurrenceId>, u64>,
    /// Why a context entry is unavailable (e.g. a dependency cycle).
    pub(crate) context_errors: HashMap<Vec<crate::assembly::OccurrenceId>, String>,
}

/// A body of another occurrence seen from the component being regenerated.
#[derive(Clone)]
pub(crate) struct ContextBody {
    pub name: String,
    pub reference: bool,
    pub shape: Shape,
}

impl Default for Regenerator {
    fn default() -> Self {
        Regenerator {
            cache: HashMap::new(),
            lru: VecDeque::new(),
            capacity: 512,
            imports: HashMap::new(),
            dxfs: HashMap::new(),
            parts: HashMap::new(),
            stages: HashMap::new(),
            xcaf: HashMap::new(),
            context: HashMap::new(),
            context_keys: HashMap::new(),
            context_errors: HashMap::new(),
        }
    }
}

#[derive(Debug)]
struct Fail {
    msg: String,
    unresolved: Option<Box<Unresolved>>,
}

impl Fail {
    fn msg(m: impl Into<String>) -> Self {
        Fail { msg: m.into(), unresolved: None }
    }
}

macro_rules! fail_from {
    ($($t:ty),*) => {$(
        impl From<$t> for Fail {
            fn from(e: $t) -> Self {
                Fail::msg(e.to_string())
            }
        }
    )*};
}
fail_from!(occt::Error, crate::sketch::SketchError, crate::library::LibraryError, crate::dxf::DxfError, crate::params::ExprError);

impl From<SelectError> for Fail {
    fn from(e: SelectError) -> Self {
        let msg = e.to_string();
        match e {
            SelectError::Unresolved { selector, best, score, .. } => {
                Fail { msg, unresolved: Some(Box::new(Unresolved { selector: *selector, best_candidate: best.map(|b| *b), score })) }
            }
            SelectError::Kernel(_) => Fail::msg(msg),
        }
    }
}

type R<T> = Result<T, Fail>;

fn fail<T>(m: impl Into<String>) -> R<T> {
    Err(Fail::msg(m))
}

fn collect_idents(v: &serde_json::Value, out: &mut BTreeSet<String>) {
    match v {
        serde_json::Value::String(s) => {
            let mut cur = String::new();
            for ch in s.chars().chain(std::iter::once(' ')) {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    cur.push(ch);
                } else {
                    if cur.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
                        out.insert(std::mem::take(&mut cur));
                    }
                    cur.clear();
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_idents(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| collect_idents(x, out)),
        _ => {}
    }
}

/// Everything a feature's result depends on besides the previous state.
fn fingerprint(f: &Feature, doc: &DocData, vals: &Values) -> u64 {
    let mut h = DefaultHasher::new();
    let mut deps = vec![serde_json::to_value(f).unwrap_or_default()];
    let sketch = match &f.kind {
        FeatureKind::Extrude { sketch, .. } | FeatureKind::Revolve { sketch, .. } | FeatureKind::Rib { sketch, .. } => Some(*sketch),
        _ => None,
    };
    if let Some(s) = sketch.and_then(|s| doc.sketch(s)) {
        deps.push(serde_json::to_value(s).unwrap_or_default());
    }
    let import = match &f.kind {
        FeatureKind::ImportStep { import, .. } => Some(*import),
        FeatureKind::StandoffsFromPcb { source: PcbSource::Dxf { import, .. } | PcbSource::Step { import, .. }, .. } => {
            Some(*import)
        }
        _ => None,
    };
    if let Some(i) = import.and_then(|i| doc.import(i)) {
        deps.push(serde_json::to_value(i).unwrap_or_default());
    }
    let mut idents = BTreeSet::new();
    for d in &deps {
        d.to_string().hash(&mut h);
        collect_idents(d, &mut idents);
    }
    for id in idents {
        if let Some(v) = vals.get(&id) {
            id.hash(&mut h);
            v.to_bits().hash(&mut h);
        }
    }
    h.finish()
}

impl Regenerator {
    pub fn clear(&mut self) {
        self.cache.clear();
        self.lru.clear();
    }

    pub fn cached_states(&self) -> usize {
        self.cache.len()
    }

    pub fn regenerate(&mut self, doc: &DocData, blobs: &BlobStore, lib: &Library) -> RegenResult {
        let profile = doc.profile();
        let mut seed = Values::new();
        profile.inject(&mut seed);
        let (vals, param_err) = match doc.params.evaluate_seeded(seed.clone()) {
            Ok(v) => (v, None),
            Err(e) => (seed, Some(e.to_string())),
        };
        // Fit resolution depends on the profile's numbers: key every state on them.
        let mut key = {
            let mut h = DefaultHasher::new();
            0x6361_7365_776f_726bu64.hash(&mut h); // "easycad"
            serde_json::to_string(&(&profile.fits, &profile.hole_comp, profile.xy_comp)).unwrap_or_default().hash(&mut h);
            h.finish()
        };
        let mut state: Arc<ModelState> = Arc::new(ModelState::default());
        let mut reports = Vec::new();
        let (mut recomputed, mut reused) = (0, 0);
        let live = doc.rollback.map_or(doc.features.len(), |r| r.min(doc.features.len()));
        let prof = std::env::var("CW_PROF").is_ok();
        let (mut t_key, mut t_prov) = (0.0, 0.0);
        for f in &doc.features[..live] {
            let tk = std::time::Instant::now();
            let mut h = DefaultHasher::new();
            key.hash(&mut h);
            fingerprint(f, doc, &vals).hash(&mut h);
            t_key += tk.elapsed().as_secs_f64() * 1000.0;
            param_err.hash(&mut h);
            if let FeatureKind::ContextRef { source, root, .. } = &f.kind {
                let k = crate::assembly::ctx_key(*root, source);
                self.context_keys.get(&k).hash(&mut h);
                self.context_errors.get(&k).hash(&mut h);
            }
            key = h.finish();
            if let Some(hit) = self.cache.get(&key).cloned() {
                state = Arc::new(hit.0.clone());
                let mut r = hit.1.clone();
                r.cached = true;
                reports.push(r);
                reused += 1;
                continue;
            }
            recomputed += 1;
            let t0 = std::time::Instant::now();
            let (next, report) = if f.suppressed {
                ((*state).clone(), FeatureReport::new(f.id, FeatureStatus::Suppressed))
            } else {
                let mut ctx = Ctx {
                    doc,
                    vals: &vals,
                    profile: &profile,
                    blobs,
                    lib,
                    regen: self,
                    report: None,
                    sketch_report: None,
                    warning: None,
                    joint: None,
                    xf: Vec::new(),
                    target_default: None,
                    edge_requests: Vec::new(),
                    skipped_edges: Vec::new(),
                };
                let res = ctx.exec(f, &state).map(|mut s| {
                    let tp = std::time::Instant::now();
                    s.faces = crate::provenance::update(&state.faces, &s.bodies, Some(f), doc);
                    t_prov += tp.elapsed().as_secs_f64() * 1000.0;
                    s
                });
                let (dressup, sketch, warning, joint) = (ctx.report.take(), ctx.sketch_report.take(), ctx.warning.take(), ctx.joint.take());
                let (edge_requests, skipped_edges) = (std::mem::take(&mut ctx.edge_requests), std::mem::take(&mut ctx.skipped_edges));
                match res {
                    Ok(s) => (
                        s,
                        FeatureReport {
                            dressup,
                            sketch,
                            joint,
                            edge_requests,
                            skipped_edges,
                            ..FeatureReport::new(f.id, warning.map(FeatureStatus::Warning).unwrap_or(FeatureStatus::Ok))
                        },
                    ),
                    Err(Fail { msg, unresolved }) => {
                        let status = match unresolved {
                            Some(u) => FeatureStatus::Unresolved(u),
                            None => FeatureStatus::Error(match &param_err {
                                Some(pe) => format!("{msg} (parameters: {pe})"),
                                None => msg,
                            }),
                        };
                        ((*state).clone(), FeatureReport { dressup, sketch, joint, edge_requests, skipped_edges, ..FeatureReport::new(f.id, status) })
                    }
                }
            };
            let mut report = report;
            report.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let step: Step = Arc::new((next, report));
            self.insert(key, step.clone());
            state = Arc::new(step.0.clone());
            reports.push(step.1.clone());
        }
        for f in &doc.features[live..] {
            reports.push(FeatureReport { rolled_back: true, ..FeatureReport::new(f.id, FeatureStatus::Suppressed) });
        }
        // Final process stage (teardrops), cached on the final model key plus the
        // profile's process settings, orientations and overrides.
        let stage_key = {
            let mut h = DefaultHasher::new();
            key.hash(&mut h);
            serde_json::to_string(&(&profile.teardrop, &doc.build_orientation, &doc.body_orientations, &doc.teardrop_overrides))
                .unwrap_or_default()
                .hash(&mut h);
            h.finish()
        };
        let ts = std::time::Instant::now();
        let (bodies, process, face_origins) = match self.stages.get(&stage_key) {
            Some(hit) => hit.clone(),
            None => {
                let (b, p) = process_stage(&state.bodies, &profile, doc);
                let fo = crate::provenance::FaceOrigins(crate::provenance::update(&state.faces, &b, None, doc));
                let r = (b, p, fo);
                if self.stages.len() > self.capacity {
                    self.stages.clear();
                }
                self.stages.insert(stage_key, r.clone());
                r
            }
        };
        if prof {
            eprintln!("  regen keys {t_key:.1} ms · provenance {t_prov:.1} ms · process stage {:.1} ms", ts.elapsed().as_secs_f64() * 1000.0);
        }
        RegenResult { bodies, reports, recomputed, reused, process, face_origins }
    }

    fn insert(&mut self, key: u64, s: Step) {
        if self.cache.insert(key, s).is_none() {
            self.lru.push_back(key);
        }
        while self.lru.len() > self.capacity {
            if let Some(k) = self.lru.pop_front() {
                self.cache.remove(&k);
            }
        }
    }
}

struct Ctx<'a> {
    doc: &'a DocData,
    vals: &'a Values,
    profile: &'a ProcessProfile,
    blobs: &'a BlobStore,
    lib: &'a Library,
    regen: &'a mut Regenerator,
    report: Option<DressUpReport>,
    sketch_report: Option<SolveReport>,
    warning: Option<String>,
    joint: Option<crate::joint::JointReport>,
    /// Pattern transform stack (applied innermost first).
    xf: Vec<Xf>,
    /// Target used when a re-executed feature has `None` as its target.
    target_default: Option<BodyId>,
    edge_requests: Vec<EdgeRequest>,
    skipped_edges: Vec<u32>,
}

fn frame(p: &Plane) -> Frame {
    Frame { origin: p.origin, x_dir: p.x_dir, normal: p.normal }
}

fn seg3(p: &Plane, s: &Segment2) -> Edge {
    match *s {
        Segment2::Line { a, b } => Edge::Line(p.to_world(a), p.to_world(b)),
        Segment2::Arc { start, end, .. } => Edge::Arc3(p.to_world(start), p.to_world(segment_point(s, 0.5)), p.to_world(end)),
        Segment2::Circle { center, radius } => Edge::Circle { center: p.to_world(center), normal: p.normal, radius },
    }
}

/// Planar face for a profile placed on a plane.
pub fn profile_face(plane: &Plane, pr: &Profile) -> Result<Shape, occt::Error> {
    let outer: Vec<Edge> = pr.outer.iter().map(|s| seg3(plane, s)).collect();
    let holes: Vec<Vec<Edge>> = pr.holes.iter().map(|h| h.iter().map(|s| seg3(plane, s)).collect()).collect();
    Shape::face(&frame(plane), &outer, &holes)
}

#[derive(Clone, Copy)]
enum Xf {
    Translate(DVec3),
    Rotate(DVec3, DVec3, f64),
    Mirror(DVec3, DVec3),
}

impl Xf {
    fn point(&self, p: DVec3) -> DVec3 {
        match *self {
            Xf::Translate(v) => p + v,
            Xf::Rotate(o, d, a) => o + glam::DQuat::from_axis_angle(d.normalize(), a.to_radians()) * (p - o),
            Xf::Mirror(o, n) => {
                let n = n.normalize();
                p - n * 2.0 * (p - o).dot(n)
            }
        }
    }
    fn dir(&self, v: DVec3) -> DVec3 {
        match *self {
            Xf::Translate(_) => v,
            Xf::Rotate(_, d, a) => glam::DQuat::from_axis_angle(d.normalize(), a.to_radians()) * v,
            Xf::Mirror(_, n) => {
                let n = n.normalize();
                v - n * 2.0 * v.dot(n)
            }
        }
    }
    fn apply(&self, s: &Shape) -> Result<Shape, occt::Error> {
        match *self {
            Xf::Translate(v) => s.translated(v),
            Xf::Rotate(o, d, a) => s.rotated(o, d, a),
            Xf::Mirror(o, n) => s.mirrored(o, n),
        }
    }
}

fn fuse_many(shapes: &[Shape]) -> R<Shape> {
    match shapes.len() {
        0 => fail("nothing to combine"),
        1 => Ok(shapes[0].clone()),
        _ => Ok(Shape::fuse_all(&shapes.iter().collect::<Vec<_>>())?),
    }
}

fn tidy(s: Shape) -> Shape {
    match s.unify() {
        Ok(u) if u.is_valid() => u,
        _ => s,
    }
}

impl Ctx<'_> {
    fn ev(&self, e: &Expr) -> R<f64> {
        e.eval(self.vals).map_err(|err| Fail::msg(format!("`{}`: {err}", e.0)))
    }

    /// Apply the pattern transform stack to a tool shape.
    fn tx(&self, s: Shape) -> R<Shape> {
        let mut s = s;
        for x in self.xf.iter().rev() {
            s = x.apply(&s)?;
        }
        Ok(s)
    }
    fn tx_point(&self, p: DVec3) -> DVec3 {
        self.xf.iter().rev().fold(p, |p, x| x.point(p))
    }
    fn tx_dir(&self, v: DVec3) -> DVec3 {
        self.xf.iter().rev().fold(v, |v, x| x.dir(v))
    }

    /// Evaluate with a default fit role unless the expression carries its own.
    fn ev_fit(&self, e: &Expr, default: Fit) -> R<f64> {
        e.eval_or(self.vals, default).map_err(|err| Fail::msg(format!("`{}`: {err}", e.0)))
    }

    fn target(&self, st: &ModelState, id: Option<BodyId>) -> R<usize> {
        match id {
            Some(id) => st.bodies.iter().position(|b| b.id == id).ok_or_else(|| Fail::msg(format!("body {id:?} does not exist"))),
            None => match self.target_default {
                Some(id) => st.bodies.iter().position(|b| b.id == id).ok_or_else(|| Fail::msg(format!("body {id:?} does not exist"))),
                None => st.bodies.iter().rposition(|b| !b.reference).ok_or_else(|| Fail::msg("there is no solid body yet")),
            },
        }
    }

    fn sketch(&mut self, id: SketchId) -> R<Sketch> {
        let mut sk = self.doc.sketch(id).cloned().ok_or_else(|| Fail::msg(format!("sketch {id:?} does not exist")))?;
        if !sk.constraints.is_empty() {
            let rep = sk.solve(self.vals)?;
            let ok = rep.converged;
            self.sketch_report = Some(rep);
            if !ok {
                return fail(format!("sketch `{}` does not solve (conflicting constraints)", sk.name));
            }
        }
        Ok(sk)
    }

    fn profiles(&mut self, id: SketchId, sel: &ProfileSel) -> R<(Plane, Vec<Profile>)> {
        let sk = self.sketch(id)?;
        let all = sk.profiles().profiles;
        if all.is_empty() {
            return fail(format!("sketch `{}` has no closed profile", sk.name));
        }
        let picked = match sel {
            ProfileSel::All => all,
            ProfileSel::Indices(ix) => ix.iter().map(|i| all.get(*i).cloned().ok_or_else(|| Fail::msg(format!("profile {i} does not exist")))).collect::<R<_>>()?,
            ProfileSel::Containing(pts) => {
                let mut v = Vec::new();
                for p in pts {
                    let hit = all.iter().find(|pr| profile_contains(pr, *p)).ok_or_else(|| Fail::msg(format!("no profile contains {p:?}")))?;
                    if !v.contains(hit) {
                        v.push(hit.clone());
                    }
                }
                v
            }
        };
        Ok((sk.plane, picked))
    }

    fn apply_op(&self, st: &mut ModelState, fid: FeatureId, name: &str, op: BoolOp, target: Option<BodyId>, tool: Shape) -> R<()> {
        let tool = Arc::new(tool);
        let tid = match op {
            BoolOp::NewBody => {
                let id = BodyId { feature: fid, index: st.bodies.iter().filter(|b| b.id.feature == fid).count() as u32 };
                st.bodies.push(Body { id, name: name.to_string(), reference: false, part: None, geom: BodyGeom::new((*tool).clone()) });
                id
            }
            _ => {
                let i = self.target(st, target)?;
                let cur = st.bodies[i].shape();
                let r = match op {
                    BoolOp::Join => tidy(cur.fuse(&tool)?),
                    BoolOp::Cut => tidy(cur.cut(&tool)?),
                    BoolOp::Intersect => tidy(cur.common(&tool)?),
                    BoolOp::NewBody => unreachable!(),
                };
                if r.count(occt::ShapeKind::Solid) == 0 {
                    return fail("the operation removed the whole body");
                }
                st.bodies[i].geom = BodyGeom::new(r);
                st.bodies[i].id
            }
        };
        st.tools.entry(fid).or_default().push(ToolOp { target: tid, op });
        Ok(())
    }

    fn exec(&mut self, f: &Feature, prev: &ModelState) -> R<ModelState> {
        let mut st = prev.clone();
        self.exec_kind(&f.kind, &f.name, f.id, &mut st)?;
        Ok(st)
    }

    /// Execute a feature recipe into `st`. Patterns call this again for their source
    /// features with a transform pushed on `self.xf`.
    fn exec_kind(&mut self, kind: &FeatureKind, name: &str, fid: FeatureId, st: &mut ModelState) -> R<()> {
        match kind {
            FeatureKind::Extrude { sketch, profiles, distance, symmetric, reverse, draft_deg, op, target } => {
                let (plane, prs) = self.profiles(*sketch, profiles)?;
                let d = self.ev(distance)?;
                let draft = self.ev(draft_deg)?;
                let dir = if *reverse { -plane.normal } else { plane.normal };
                let mut tools = Vec::new();
                for pr in &prs {
                    tools.push(profile_face(&plane, pr)?.extrude_ext(&frame(&plane), dir, d, *symmetric, draft)?);
                }
                let tool = self.tx(fuse_many(&tools)?)?;
                self.apply_op(st, fid, name, *op, *target, tool)?;
            }
            FeatureKind::Revolve { sketch, profiles, axis, angle_deg, op, target } => {
                let (plane, prs) = self.profiles(*sketch, profiles)?;
                let a = self.ev(angle_deg)?;
                let mut tools = Vec::new();
                for pr in &prs {
                    tools.push(profile_face(&plane, pr)?.revolve(axis.origin, axis.dir, a)?);
                }
                let tool = self.tx(fuse_many(&tools)?)?;
                self.apply_op(st, fid, name, *op, *target, tool)?;
            }
            FeatureKind::Fillet { body, skipped, .. } | FeatureKind::Chamfer { body, skipped, .. } => {
                let is_fillet = matches!(kind, FeatureKind::Fillet { .. });
                let i = self.target(st, *body)?;
                let shape = st.bodies[i].shape().clone();
                // skipped edges that no longer exist need no skipping
                let mut skip: Vec<u32> = skipped.iter().filter_map(|sel| sel.resolve(&shape).ok()).flatten().collect();
                skip.sort_unstable();
                skip.dedup();
                let mut req: Vec<(u32, f64, bool)> = Vec::new();
                let mut map: Vec<EdgeRequest> = Vec::new();
                let mut skipped_hit: Vec<u32> = Vec::new();
                for (si, set) in kind.edge_sets().iter().enumerate() {
                    let r = self.ev(&set.size)?;
                    for (ki, sel) in set.edges.iter().enumerate() {
                        for e in resolve_edges(&shape, std::slice::from_ref(sel))? {
                            if skip.contains(&e) {
                                if !skipped_hit.contains(&e) {
                                    skipped_hit.push(e);
                                }
                                continue;
                            }
                            // an edge in two sets: the later set wins
                            match req.iter().position(|q| q.0 == e) {
                                Some(k) => {
                                    req[k] = (e, r, set.auto_reduce);
                                    map[k] = EdgeRequest { set: si, selector: ki };
                                }
                                None => {
                                    req.push((e, r, set.auto_reduce));
                                    map.push(EdgeRequest { set: si, selector: ki });
                                }
                            }
                        }
                    }
                }
                self.skipped_edges = skipped_hit.clone();
                if req.is_empty() {
                    return fail(if skipped_hit.is_empty() { "no edges to dress" } else { "every edge is in the skipped list" });
                }
                let opts = dressup::DressUpOptions::default();
                let dk = if is_fillet { dressup::DressUpKind::Fillet } else { dressup::DressUpKind::Chamfer };
                let (out, rep) = dressup::dress(dk, &shape, &req, &opts)?;
                let applied = rep.applied_count();
                let mut summary = rep.summary();
                if !skipped_hit.is_empty() {
                    summary += &format!(" · {} skipped", skipped_hit.len());
                }
                let all = rep.all_applied();
                self.report = Some(rep);
                self.edge_requests = map;
                if applied == 0 {
                    return fail(summary);
                }
                let mut warn = Vec::new();
                if !all {
                    warn.push(summary);
                }
                if matches!(kind, FeatureKind::Fillet { setback: true, .. }) {
                    // TODO: setback vertex blends (OCCT's MakeFillet has no setback corner
                    // option; would need a custom corner patch).
                    warn.push("setback corners are not supported yet; corners use the kernel's rolling-ball blend".into());
                }
                if !warn.is_empty() {
                    self.warning = Some(warn.join("; "));
                }
                st.bodies[i].geom = BodyGeom::new(out);
            }
            FeatureKind::Shell { body, open_faces, thickness, outward } => {
                let i = self.target(st, *body)?;
                let t = self.ev(thickness)?;
                let shape = st.bodies[i].shape().clone();
                let faces = resolve_faces(&shape, open_faces)?;
                st.bodies[i].geom = BodyGeom::new(shape.shell(&faces, t, *outward)?);
            }
            FeatureKind::Draft { body, faces, neutral, pull, angle_deg, offset } => {
                let i = self.target(st, *body)?;
                let a = self.ev(angle_deg)?;
                let neutral = &neutral.offset(self.ev(offset)?);
                let shape = st.bodies[i].shape().clone();
                let ids = resolve_faces(&shape, faces)?;
                st.bodies[i].geom = BodyGeom::new(shape.draft(&ids, *pull, a, &frame(neutral))?);
            }
            FeatureKind::Rib { sketch, thickness, height, draft_deg, flip, body, extent } => {
                let sk = self.sketch(*sketch)?;
                let (t, dr) = (self.ev(thickness)?, self.ev(draft_deg)?);
                let chains = sk.profiles().open;
                if chains.is_empty() {
                    return fail("rib sketch has no open chain");
                }
                let dir = if *flip { -sk.plane.normal } else { sk.plane.normal };
                let tool = match extent {
                    RibExtent::Height => self.tx(rib_tool(&sk.plane, &chains, t, self.ev(height)?, dr, dir)?)?,
                    RibExtent::ToBody => {
                        // grow far, then keep only the part between the sketch plane
                        // and the first body surface along the direction
                        let i = self.target(st, *body)?;
                        let bshape = st.bodies[i].shape().clone();
                        let bb = bshape.bbox()?;
                        let tall = bb.size().length() * 2.0 + 10.0;
                        let long = self.tx(rib_tool(&sk.plane, &chains, t, tall, 0.0, dir)?)?;
                        let d = self.tx_dir(dir).normalize();
                        let start = self.tx_point(sk.plane.origin).dot(d);
                        let span = |s: &Shape| -> R<(f64, f64)> {
                            let b = s.bbox()?;
                            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
                            for k in 0..8 {
                                let p = DVec3::new(
                                    if k & 1 == 0 { b.min.x } else { b.max.x },
                                    if k & 2 == 0 { b.min.y } else { b.max.y },
                                    if k & 4 == 0 { b.min.z } else { b.max.z },
                                );
                                lo = lo.min(p.dot(d));
                                hi = hi.max(p.dot(d));
                            }
                            Ok((lo, hi))
                        };
                        let pieces = long.cut(&bshape)?;
                        let mut keep = Vec::new();
                        for sol in pieces.solids()? {
                            let (lo, hi) = span(&sol)?;
                            if (lo - start).abs() < 1e-3 {
                                if hi > start + tall - 1e-3 {
                                    return fail("the rib never meets the body along its direction (flip it?)");
                                }
                                keep.push(sol);
                            }
                        }
                        if keep.is_empty() {
                            return fail("the rib starts inside the body; move the sketch plane into free space");
                        }
                        if dr.abs() < 1e-9 {
                            fuse_many(&keep)?
                        } else {
                            // drafted: rebuild only as tall as needed (plus overshoot), trim again
                            let reach = keep.iter().map(|k| span(k).map(|(_, hi)| hi - start)).collect::<R<Vec<_>>>()?.into_iter().fold(0.0, f64::max);
                            let h2 = reach + (0.05 * reach).max(1.0);
                            let drafted = self.tx(rib_tool(&sk.plane, &chains, t, h2, dr, dir)?)?;
                            let pieces = drafted.cut(&bshape)?;
                            let mut keep2 = Vec::new();
                            for sol in pieces.solids()? {
                                if (span(&sol)?.0 - start).abs() < 1e-3 {
                                    keep2.push(sol);
                                }
                            }
                            if keep2.is_empty() {
                                return fail("the drafted rib vanished before reaching the body (reduce the draft)");
                            }
                            fuse_many(&keep2)?
                        }
                    }
                };
                self.apply_op(st, fid, name, BoolOp::Join, *body, tool)?;
            }
            FeatureKind::ScrewBoss { body, plane, points, od, hole_dia, height, hole_depth, base_fillet, draft_deg, offset } => {
                let plane = &plane.offset(self.ev(offset)?);
                let spec = BossSpec {
                    gussets: None,
                    lid: None,
                    od: self.ev(od)?,
                    hole: self.ev_fit(hole_dia, Fit::hole(FitClass::Snug))?,
                    height: self.ev(height)?,
                    hole_depth: hole_depth.as_ref().map(|e| self.ev(e)).transpose()?,
                    fillet: self.ev(base_fillet)?,
                    draft: self.ev(draft_deg)?,
                };
                self.bosses(st, fid, *body, plane, points, &spec)?;
            }
            FeatureKind::StandoffsFromPcb { body, source, plane, od, hole_dia, height, base_fillet, draft_deg, offset, exclude, gussets, lid_holes } => {
                let plane = &plane.offset(self.ev(offset)?);
                let tp = std::time::Instant::now();
                let mut pts = self.pcb_points(st, source, plane)?;
                if std::env::var("CW_PROF").is_ok() {
                    eprintln!("  standoffs pcb points: {:.1} ms", tp.elapsed().as_secs_f64() * 1000.0);
                }
                if pts.is_empty() {
                    return fail("no mounting holes found in the PCB within the diameter range");
                }
                pts.retain(|p| !exclude.iter().any(|x| x.distance(*p) < 0.5));
                if pts.is_empty() {
                    return fail("every mounting hole is excluded");
                }
                let h = self.ev(height)?;
                let spec = BossSpec {
                    gussets: match gussets {
                        Some(g) => Some((self.ev(&g.thickness)?, g.height.as_ref().map(|e| self.ev(e)).transpose()?.unwrap_or(0.6 * h), self.ev(&g.max_length)?)),
                        None => None,
                    },
                    lid: match lid_holes {
                        Some(l) => Some((
                            l.body,
                            self.ev_fit(&l.hole_dia, Fit::hole(FitClass::Snug))?,
                            self.ev_fit(&l.cbore_dia, Fit::hole(FitClass::Snug))?,
                            self.ev(&l.cbore_depth)?,
                        )),
                        None => None,
                    },
                    od: self.ev(od)?,
                    hole: self.ev_fit(hole_dia, Fit::hole(FitClass::Snug))?,
                    height: self.ev(height)?,
                    hole_depth: None,
                    fillet: self.ev(base_fillet)?,
                    draft: self.ev(draft_deg)?,
                };
                self.bosses(st, fid, *body, plane, &pts, &spec)?;
            }
            FeatureKind::Pattern { source, kind } => {
                let xfs: Vec<Xf> = match kind {
                    PatternKind::Linear { dir, spacing, count } => {
                        let (s, n) = (self.ev(spacing)?, self.ev(count)?.round() as i64);
                        let d = dir.normalize_or_zero();
                        (1..n.max(1)).map(|i| Xf::Translate(d * s * i as f64)).collect()
                    }
                    PatternKind::Circular { axis, count, total_deg } => {
                        let (n, tot) = (self.ev(count)?.round() as u32, self.ev(total_deg)?);
                        let step = occt::circular_step(n, tot);
                        (1..n.max(1)).map(|i| Xf::Rotate(axis.origin, axis.dir, step * i as f64)).collect()
                    }
                };
                self.replicate(st, fid, name, source, &xfs)?;
            }
            FeatureKind::Mirror { source, plane } => {
                self.replicate(st, fid, name, source, &[Xf::Mirror(plane.origin, plane.normal)])?;
            }
            FeatureKind::Split { body, plane, new_name, offset } => {
                let i = self.target(st, *body)?;
                let plane = &plane.offset(self.ev(offset)?);
                let (below, above) = st.bodies[i].shape().split(plane.origin, plane.normal)?;
                let (Some(below), Some(above)) = (below, above) else {
                    return fail("the split plane does not cut the body");
                };
                st.bodies[i].geom = BodyGeom::new(below);
                st.bodies.push(Body { id: BodyId::of(fid), name: new_name.clone(), reference: false, part: None, geom: BodyGeom::new(above) });
            }
            FeatureKind::PartingJoint { body, plane, offset, kind, rim, upper_name, class, closing_radius } => {
                let i = self.target(st, *body)?;
                let pl = plane.offset(self.ev(offset)?);
                let rim = match rim {
                    Some(r) => Some((self.ev(&r.width)?, self.ev(&r.height)?)),
                    None => None,
                };
                let shape = st.bodies[i].shape().clone();
                let closing = match closing_radius {
                    Some(e) => self.ev(e)?,
                    None => 12.0,
                };
                let inp = crate::joint::JointInput { body: &shape, plane: pl, kind, rim, class: *class, closing, profile: self.profile, vals: self.vals };
                let (lo, up, rep) = crate::joint::build(&inp).map_err(Fail::msg)?;
                if !rep.warnings.is_empty() {
                    self.warning = Some(rep.warnings.join("; "));
                }
                self.joint = Some(rep);
                st.bodies[i].geom = BodyGeom::new(lo);
                st.bodies.push(Body { id: BodyId::of(fid), name: upper_name.clone(), reference: false, part: None, geom: BodyGeom::new(up) });
            }
            FeatureKind::ImportStep { import, reference, translate } => {
                let imp = self.doc.import(*import).ok_or_else(|| Fail::msg(format!("import {import:?} missing")))?;
                let xc = self.step_xcaf(&imp.blob)?;
                let (mut s, parts) = (xc.0.clone(), xc.1.clone());
                if *translate != DVec3::ZERO {
                    s = s.translated(*translate)?;
                }
                let sub: Vec<SubPart> = parts.iter().map(|p| SubPart { name: p.name.clone(), color: p.color }).collect();
                if *reference {
                    // the whole assembly is one locked reference body with named,
                    // coloured sub-parts
                    let face_part = s.face_owners()?;
                    let app = Appearance { parts: sub, face_part };
                    st.bodies.push(Body { id: BodyId::of(fid), name: imp.name.clone(), reference: true, part: None, geom: BodyGeom::with_appearance(s, app) });
                } else {
                    for (k, solid) in s.solids()?.into_iter().enumerate() {
                        let sp = sub.get(k).cloned().unwrap_or(SubPart { name: format!("{} {}", imp.name, k + 1), color: None });
                        let nf = solid.count(occt::ShapeKind::Face) as usize;
                        st.bodies.push(Body {
                            id: BodyId { feature: fid, index: k as u32 },
                            name: sp.name.clone(),
                            reference: false,
                            part: None,
                            geom: BodyGeom::with_appearance(solid, Appearance { parts: vec![sp], face_part: vec![0; nf] }),
                        });
                    }
                }
            }
            FeatureKind::InsertPart { part, placements, flip, offset, auto_hole, target } => {
                let inst = self.part(part)?;
                let off = self.ev(offset)?;
                let ti = self.target(st, *target).ok();
                let mut holes = Vec::new();
                let hole_spec = match auto_hole {
                    Some(ah) => {
                        let mut spec = inst.hole(ah.choice)?;
                        if let Some(d) = &ah.depth {
                            spec.depth = Some(self.ev(d)?);
                        }
                        let fit = ah.fit.unwrap_or(Fit::hole(FitClass::Snug));
                        spec.diameter = self.profile.resolve(spec.diameter, fit);
                        if let Some((d, h)) = spec.counterbore {
                            spec.counterbore = Some((self.profile.resolve(d, fit), h));
                        }
                        if let Some((d, a)) = spec.countersink {
                            spec.countersink = Some((self.profile.resolve(d, fit), a));
                        }
                        Some(spec)
                    }
                    None => None,
                };
                for (k, pl) in placements.iter().enumerate() {
                    let (origin, axis) = self.placement(st, ti, pl)?;
                    let (origin, mut axis) = (self.tx_point(origin), self.tx_dir(axis));
                    if *flip {
                        axis = -axis;
                    }
                    let placed = place(&inst.shape, origin + axis * off, axis, 0.0)?;
                    let index = st.bodies.iter().filter(|b| b.id.feature == fid).count() as u32;
                    let _ = k;
                    st.bodies.push(Body {
                        id: BodyId { feature: fid, index },
                        name: format!("{} {} #{}", part.name, part.size, index + 1),
                        reference: true,
                        part: Some(PartUse { feature: fid, part: part.clone() }),
                        geom: BodyGeom::new(placed),
                    });
                    if let Some(spec) = &hole_spec {
                        let through = match ti {
                            Some(i) => st.bodies[i].shape().bbox()?.size().length() * 2.0 + 10.0,
                            None => 1000.0,
                        };
                        holes.push(place(&spec.tool(through)?, origin, axis, 0.0)?);
                    }
                }
                if !holes.is_empty() {
                    let Some(i) = ti else { return fail("auto-hole needs a target body") };
                    let tid = st.bodies[i].id;
                    self.apply_op(st, fid, name, BoolOp::Cut, Some(tid), fuse_many(&holes)?)?;
                }
            }
            FeatureKind::ContextRef { source, body, op, target, offset, reference, root } => {
                let key = crate::assembly::ctx_key(*root, source);
                let Some(bodies) = self.regen.context.get(&key).cloned() else {
                    return fail(self.regen.context_errors.get(&key).cloned().unwrap_or_else(|| {
                        format!("in-context source {source:?} is not available (regenerate the assembly)")
                    }));
                };
                let cb = match body {
                    Some(n) => bodies.iter().find(|b| &b.name == n),
                    None => bodies.iter().find(|b| !b.reference),
                }
                .ok_or_else(|| Fail::msg(format!("in-context source has no body {}", body.as_deref().unwrap_or("(solid)"))))?;
                let d = self.ev(offset)?;
                let shape = if d.abs() > 1e-9 { cb.shape.offset_solid(d)? } else { cb.shape.clone() };
                let tool = self.tx(shape)?;
                if *op == BoolOp::NewBody && *reference {
                    let index = st.bodies.iter().filter(|b| b.id.feature == fid).count() as u32;
                    st.bodies.push(Body { id: BodyId { feature: fid, index }, name: name.to_string(), reference: true, part: None, geom: BodyGeom::new(tool) });
                } else {
                    self.apply_op(st, fid, name, *op, *target, tool)?;
                }
            }
        }
        Ok(())
    }

    fn step_shape(&mut self, blob: &str) -> R<Arc<Shape>> {
        if let Some(s) = self.regen.imports.get(blob) {
            return Ok(s.clone());
        }
        let data = self.blobs.get(blob).ok_or_else(|| Fail::msg(format!("blob {blob} is missing")))?;
        let s = Arc::new(Shape::read_step_str(&String::from_utf8_lossy(&data))?);
        self.regen.imports.insert(blob.to_string(), s.clone());
        Ok(s)
    }

    fn step_xcaf(&mut self, blob: &str) -> R<Arc<(Shape, Vec<occt::StepPart>)>> {
        if let Some(s) = self.regen.xcaf.get(blob) {
            return Ok(s.clone());
        }
        let data = self.blobs.get(blob).ok_or_else(|| Fail::msg(format!("blob {blob} is missing")))?;
        let s = Arc::new(Shape::read_step_xcaf(&String::from_utf8_lossy(&data))?);
        self.regen.xcaf.insert(blob.to_string(), s.clone());
        Ok(s)
    }

    fn dxf(&mut self, blob: &str) -> R<Arc<DxfDrawing>> {
        if let Some(d) = self.regen.dxfs.get(blob) {
            return Ok(d.clone());
        }
        let data = self.blobs.get(blob).ok_or_else(|| Fail::msg(format!("blob {blob} is missing")))?;
        let d = Arc::new(DxfDrawing::parse(&String::from_utf8_lossy(&data))?);
        self.regen.dxfs.insert(blob.to_string(), d.clone());
        Ok(d)
    }

    fn part(&mut self, r: &PartRef) -> R<Arc<PartInstance>> {
        let key = format!("{}\u{1}{}\u{1}{}\u{1}{}", r.source, r.id, r.size, r.blob.clone().unwrap_or_default());
        if let Some(p) = self.regen.parts.get(&key) {
            return Ok(p.clone());
        }
        let inst = if let Some(blob) = &r.blob {
            let shape = (*self.step_shape(blob)?).clone();
            let entry = crate::library::LocalEntry {
                id: r.id.clone(),
                file: String::new(),
                name: r.name.clone(),
                vendor: r.vendor.clone(),
                part_number: r.part_number.clone(),
                tags: Vec::new(),
                url: None,
                category: crate::library::Category::Other,
                thread: r.thread.clone(),
                head: None,
                depth_below: 0.0,
            };
            PartInstance {
                info: PartInfo {
                    source: r.source.clone(),
                    id: r.id.clone(),
                    name: r.name.clone(),
                    category: crate::library::Category::Other,
                    standard: None,
                    system: None,
                    tags: Vec::new(),
                    vendor: r.vendor.clone(),
                    part_number: r.part_number.clone(),
                    url: None,
                    notes: Vec::new(),
                },
                size: r.size.clone(),
                shape,
                mate: local_mate(&entry),
            }
        } else {
            self.lib.build(&r.source, &r.id, &r.size)?
        };
        let inst = Arc::new(inst);
        self.regen.parts.insert(key, inst.clone());
        Ok(inst)
    }

    fn placement(&self, st: &ModelState, target: Option<usize>, pl: &PlacementRef) -> R<(DVec3, DVec3)> {
        let shape = || -> R<&Shape> {
            target.map(|i| st.bodies[i].shape()).ok_or_else(|| Fail::msg("placement needs a target body"))
        };
        Ok(match pl {
            PlacementRef::Explicit { origin, axis } => (*origin, axis.normalize_or_zero()),
            PlacementRef::CircularEdge { edge } => {
                let s = shape()?;
                let ids = edge.resolve(s)?;
                let edges = s.edges()?;
                let e = &edges[ids[0] as usize];
                let Some((c, ax, _)) = e.circle else { return fail("placement edge is not circular") };
                let faces = s.faces()?;
                let n = e
                    .faces
                    .iter()
                    .map(|fi| &faces[*fi as usize])
                    .find(|f| f.kind == SurfaceKind::Plane && f.normal.dot(ax).abs() > 0.999)
                    .map(|f| f.normal)
                    .unwrap_or(ax);
                (c, n.normalize())
            }
            PlacementRef::PointOnFace { face, point } => {
                let s = shape()?;
                let fi = face.resolve(s)?[0];
                let f = &s.faces()?[fi as usize];
                let n = f.normal.normalize();
                let o = if f.kind == SurfaceKind::Plane { *point - n * (*point - f.centroid).dot(n) } else { *point };
                (o, n)
            }
        })
    }

    fn pcb_points(&mut self, st: &ModelState, source: &PcbSource, plane: &Plane) -> R<Vec<DVec2>> {
        match source {
            PcbSource::Dxf { import, layers, min_dia, max_dia, offset } => {
                let imp = self.doc.import(*import).ok_or_else(|| Fail::msg("DXF import missing"))?;
                let d = self.dxf(&imp.blob.clone())?;
                let ls: Vec<&str> = layers.iter().map(|s| s.as_str()).collect();
                Ok(d.circles(&ls)
                    .into_iter()
                    .filter(|(_, r)| 2.0 * r >= *min_dia - 1e-9 && 2.0 * r <= *max_dia + 1e-9)
                    .map(|(c, _)| c + *offset)
                    .collect())
            }
            PcbSource::Step { import, min_dia, max_dia } => {
                // Prefer the placed reference body of this import, else the raw import.
                let placed = st.bodies.iter().find(|b| {
                    self.doc.features.iter().any(|f| f.id == b.id.feature && matches!(f.kind, FeatureKind::ImportStep { import: i, .. } if i == *import))
                });
                let shape = match placed {
                    Some(b) => b.shape().clone(),
                    None => {
                        let imp = self.doc.import(*import).ok_or_else(|| Fail::msg("STEP import missing"))?;
                        (*self.step_shape(&imp.blob.clone())?).clone()
                    }
                };
                let faces = shape.faces()?;
                let n = plane.normal.normalize();
                let board = faces
                    .iter()
                    .filter(|f| f.kind == SurfaceKind::Plane && f.normal.dot(n).abs() > 0.999)
                    .max_by(|a, b| a.area.total_cmp(&b.area))
                    .ok_or_else(|| Fail::msg("no board face parallel to the boss plane"))?;
                let edges = shape.edges()?;
                let mut pts: Vec<DVec2> = Vec::new();
                for ei in shape.face_edges(board.index)? {
                    if let Some((c, ax, r)) = edges[ei as usize].circle {
                        if ax.dot(n).abs() > 0.999 && 2.0 * r >= *min_dia - 1e-9 && 2.0 * r <= *max_dia + 1e-9 {
                            let p = plane.to_local(c);
                            if !pts.iter().any(|q| q.distance(p) < 1e-3) {
                                pts.push(p);
                            }
                        }
                    }
                }
                Ok(pts)
            }
        }
    }

    fn bosses(&mut self, st: &mut ModelState, fid: FeatureId, body: Option<BodyId>, plane: &Plane, points: &[DVec2], b: &BossSpec) -> R<()> {
        if points.is_empty() {
            return fail("no boss points");
        }
        if b.hole >= b.od {
            return fail("hole diameter must be smaller than the boss diameter");
        }
        let n = self.tx_dir(plane.normal).normalize();
        let bases: Vec<DVec3> = points.iter().map(|p| self.tx_point(plane.to_world(*p))).collect();
        let r_top = b.od / 2.0 - b.height * b.draft.to_radians().tan();
        if r_top <= b.hole / 2.0 {
            return fail("draft makes the boss top narrower than the hole");
        }
        let pre = self.target(st, body).map(|i| st.bodies[i].shape().clone());
        let mut bosses = Vec::new();
        let mut holes = Vec::new();
        let mut notes = Vec::new();
        if let (Some((t, hg, reach)), Ok(pre)) = (b.gussets, &pre) {
            let u = self.tx_dir(plane.x_dir).normalize();
            let v = n.cross(u);
            let r = b.od / 2.0;
            let hg = hg.clamp(0.1, b.height);
            let mut missing = 0;
            for &base in &bases {
                // probe the four in-plane directions for the nearest wall
                let mut best: Option<(f64, DVec3)> = None;
                for dir in [u, -u, v, -v] {
                    if reach <= r + 0.2 {
                        break;
                    }
                    let probe = oriented_box(base, dir, n, (r, reach), t / 2.0, (0.1 * hg, 0.9 * hg))?;
                    let Ok(hit) = pre.common(&probe) else { continue };
                    let d = hit.edges().unwrap_or_default().iter().flat_map(|e| [e.start, e.end]).map(|p| (p - base).dot(dir)).fold(f64::INFINITY, f64::min);
                    if d.is_finite() && d > r + 0.2 && best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, dir));
                    }
                }
                match best {
                    Some((d, dir)) => bosses.push(oriented_box(base, dir, n, (0.0, d + 0.3), t / 2.0, (0.0, hg))?),
                    None => missing += 1,
                }
            }
            if missing > 0 {
                notes.push(format!("gussets: no wall within {reach:.1} mm of {missing} standoff(s)"));
            }
        }
        for &base in &bases {
            bosses.push(if b.draft.abs() > 1e-9 {
                Shape::cone(base, n, b.od / 2.0, r_top, b.height)?
            } else {
                Shape::cylinder(base, n, b.od / 2.0, b.height)?
            });
            let depth = b.hole_depth.unwrap_or(b.height);
            holes.push(Shape::cylinder(base + n * (b.height - depth), n, b.hole / 2.0, depth + 1.0)?);
        }
        let prof = std::env::var("CW_PROF").is_ok();
        let t0 = std::time::Instant::now();
        let lap = |w: &str| {
            if prof {
                eprintln!("  bosses {w}: {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
            }
        };
        let i = self.target(st, body)?;
        let tid = st.bodies[i].id;
        // the join is not unified or checked: the hole cut below does both
        let joined = st.bodies[i].shape().fuse(&fuse_many(&bosses)?)?;
        st.bodies[i].geom = BodyGeom::new(joined);
        st.tools.entry(fid).or_default().push(ToolOp { target: tid, op: BoolOp::Join });
        lap("join");
        if b.fillet > 0.0 {
            let i = self.target(st, Some(tid))?;
            let shape = st.bodies[i].shape().clone();
            let edges = shape.edges()?;
            let req: Vec<(u32, f64)> = edges
                .iter()
                .filter(|e| match e.circle {
                    Some((c, _, r)) => (r - b.od / 2.0).abs() < 1e-3 && bases.iter().any(|p| p.distance(c) < 1e-3),
                    None => false,
                })
                .map(|e| (e.index, b.fillet))
                .collect();
            if !req.is_empty() {
                let (out, rep) = dressup::fillet(&shape, &req, &Default::default())?;
                if !rep.all_applied() {
                    self.warning = Some(format!("base fillet: {}", rep.summary()));
                }
                self.report = Some(rep);
                st.bodies[i].geom = BodyGeom::new(out);
            } else {
                self.warning = Some("base fillet: no boss base edges found".into());
            }
        }
        lap("fillet");
        self.apply_op(st, fid, "", BoolOp::Cut, Some(tid), fuse_many(&holes)?)?;
        lap("holes");
        if let Some((lid, hole, cbd, cbh)) = b.lid {
            let li = self.target(st, lid.or(Some(tid)))?;
            let lid_shape = st.bodies[li].shape().clone();
            let reach = lid_shape.bbox()?.size().length() + 10.0;
            let start = b.height + 0.3;
            let mut cuts = Vec::new();
            let mut missing = 0;
            for &base in &bases {
                let probe = Shape::cylinder(base + n * start, n, hole / 2.0, reach)?;
                let top = lid_shape.common(&probe).ok().and_then(|h| h.edges().ok()).unwrap_or_default().iter().flat_map(|e| [e.start, e.end]).map(|p| (p - base).dot(n)).fold(f64::NEG_INFINITY, f64::max);
                if !top.is_finite() || top <= start {
                    missing += 1;
                    continue;
                }
                cuts.push(Shape::cylinder(base + n * start, n, hole / 2.0, top - start + 1.0)?);
                if cbd > hole && cbh > 0.0 {
                    cuts.push(Shape::cylinder(base + n * (top - cbh), n, cbd / 2.0, cbh + 1.0)?);
                }
            }
            if !cuts.is_empty() {
                let lid_id = st.bodies[li].id;
                self.apply_op(st, fid, "", BoolOp::Cut, Some(lid_id), fuse_many(&cuts)?)?;
            }
            if missing > 0 {
                notes.push(format!("lid holes: no material above {missing} standoff(s)"));
            }
        }
        if !notes.is_empty() {
            let w = notes.join("; ");
            self.warning = Some(match self.warning.take() {
                Some(x) => format!("{x}; {w}"),
                None => w,
            });
        }
        Ok(())
    }

    fn replicate(&mut self, st: &mut ModelState, fid: FeatureId, name: &str, source: &PatternSource, xfs: &[Xf]) -> R<()> {
        if xfs.is_empty() {
            return fail("pattern needs at least two instances");
        }
        match source {
            PatternSource::Body(b) => {
                let i = self.target(st, Some(*b))?;
                let orig = st.bodies[i].shape().clone();
                let mut all = vec![orig.clone()];
                for x in xfs {
                    all.push(x.apply(&orig)?);
                }
                let fused = tidy(fuse_many(&all)?);
                st.bodies[i].geom = BodyGeom::new(fused);
                st.tools.entry(fid).or_default().push(ToolOp { target: *b, op: BoolOp::Join });
            }
            PatternSource::Features(fs) => {
                // Re-run each source feature's recipe at every transformed placement,
                // so bosses keep their fillets, parts are copied, fits and process
                // stages apply as for the original.
                for f in fs {
                    let src = self.doc.feature(*f).cloned().ok_or_else(|| Fail::msg(format!("feature {f:?} does not exist")))?;
                    if !matches!(
                        src.kind,
                        FeatureKind::Extrude { .. }
                            | FeatureKind::Revolve { .. }
                            | FeatureKind::Rib { .. }
                            | FeatureKind::ScrewBoss { .. }
                            | FeatureKind::StandoffsFromPcb { .. }
                            | FeatureKind::InsertPart { .. }
                            | FeatureKind::Pattern { .. }
                            | FeatureKind::Mirror { .. }
                    ) {
                        return fail(format!("`{}` ({}) cannot be patterned; pattern the body instead", src.name, src.kind.label()));
                    }
                    if src.suppressed {
                        continue;
                    }
                    let target = st.tools.get(f).and_then(|ops| ops.iter().find(|o| o.op != BoolOp::NewBody)).map(|o| o.target);
                    for x in xfs {
                        let saved = self.target_default;
                        self.target_default = target.or(saved);
                        self.xf.push(*x);
                        let r = self.exec_kind(&src.kind, name, fid, st);
                        self.xf.pop();
                        self.target_default = saved;
                        r.map_err(|e| Fail::msg(format!("pattern instance of `{}`: {}", src.name, e.msg)))?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// A box in the frame (x = `dir`, z = `n`): x in `xs`, y in ±`half_y`, z in `zs`.
fn oriented_box(base: DVec3, dir: DVec3, n: DVec3, xs: (f64, f64), half_y: f64, zs: (f64, f64)) -> R<Shape> {
    let b = Shape::make_box(DVec3::new(xs.0, -half_y, zs.0), DVec3::new(xs.1 - xs.0, 2.0 * half_y, zs.1 - zs.0))?;
    let q = glam::DQuat::from_mat3(&glam::DMat3::from_cols(dir, n.cross(dir), n));
    Ok(crate::assembly::Transform::new(q, base).apply(&b)?)
}

struct BossSpec {
    /// (thickness, height, max length) of gussets to the nearest wall.
    gussets: Option<(f64, f64, f64)>,
    /// (lid body, hole, counterbore diameter, counterbore depth).
    lid: Option<(Option<BodyId>, f64, f64, f64)>,
    od: f64,
    hole: f64,
    height: f64,
    hole_depth: Option<f64>,
    fillet: f64,
    draft: f64,
}

fn profile_contains(pr: &Profile, p: DVec2) -> bool {
    use crate::sketch::{point_in_polygon, polygonize};
    point_in_polygon(p, &polygonize(&pr.outer)) && !pr.holes.iter().any(|h| point_in_polygon(p, &polygonize(h)))
}

/// Thicken open chains into a rib solid: one slab per segment plus round joints.
fn rib_tool(plane: &Plane, chains: &[Vec<Segment2>], t: f64, h: f64, draft: f64, dir: DVec3) -> R<Shape> {
    let fr = frame(plane);
    let mut pieces = Vec::new();
    let half = t / 2.0;
    for chain in chains {
        for (k, s) in chain.iter().enumerate() {
            let pr = match *s {
                Segment2::Line { a, b } => {
                    let n = (b - a).normalize_or_zero().perp() * half;
                    Profile {
                        outer: vec![
                            Segment2::Line { a: a - n, b: b - n },
                            Segment2::Line { a: b - n, b: b + n },
                            Segment2::Line { a: b + n, b: a + n },
                            Segment2::Line { a: a + n, b: a - n },
                        ],
                        holes: vec![],
                    }
                }
                Segment2::Arc { center, start, end, ccw } => {
                    let r = (start - center).length();
                    if r <= half {
                        return fail("rib arc radius must exceed half the thickness");
                    }
                    let (u0, u1) = ((start - center).normalize(), (end - center).normalize());
                    let (o0, o1, i0, i1) = (center + u0 * (r + half), center + u1 * (r + half), center + u0 * (r - half), center + u1 * (r - half));
                    Profile {
                        outer: vec![
                            Segment2::Arc { center, start: o0, end: o1, ccw },
                            Segment2::Line { a: o1, b: i1 },
                            Segment2::Arc { center, start: i1, end: i0, ccw: !ccw },
                            Segment2::Line { a: i0, b: o0 },
                        ],
                        holes: vec![],
                    }
                }
                Segment2::Circle { .. } => continue,
            };
            pieces.push(profile_face(plane, &pr)?.extrude_ext(&fr, dir, h, false, draft)?);
            // round joint at the start of every segment after the first
            if k > 0 {
                let p = crate::sketch::segment_point(s, 0.0);
                let disk = Profile { outer: vec![Segment2::Circle { center: p, radius: half }], holes: vec![] };
                pieces.push(profile_face(plane, &disk)?.extrude_ext(&fr, dir, h, false, draft)?);
            }
        }
    }
    fuse_many(&pieces)
}

// ---------------------------------------------------------------------------
// Process stage

fn process_stage(bodies: &[Body], profile: &ProcessProfile, doc: &DocData) -> (Vec<Body>, ProcessReport) {
    let mut rep = ProcessReport { profile: profile.name.clone(), ..Default::default() };
    let td = profile.teardrop;
    let any_override = doc.teardrop_overrides.iter().any(|o| o.enabled);
    if !td.enabled && !any_override {
        return (bodies.to_vec(), rep);
    }
    // bodies are independent (each scan reads every face): one thread per body
    let per: Vec<(Body, ProcessReport)> = std::thread::scope(|sc| {
        let jobs: Vec<_> = bodies
            .iter()
            .map(|b| {
                sc.spawn(move || {
                    let mut rep = ProcessReport::default();
                    if b.reference {
                        return (b.clone(), rep);
                    }
                    let body = match teardrop_body(b, profile, doc, &mut rep) {
                        Ok(Some(s)) => {
                            let mut nb = b.clone();
                            nb.geom = BodyGeom::new(s);
                            nb
                        }
                        Ok(None) => b.clone(),
                        Err(e) => {
                            rep.warnings.push(format!("{}: teardrop stage failed ({e}); body left unchanged", b.name));
                            b.clone()
                        }
                    };
                    (body, rep)
                })
            })
            .collect();
        jobs.into_iter().zip(bodies).map(|(j, b)| j.join().unwrap_or_else(|_| (b.clone(), ProcessReport { warnings: vec![format!("{}: teardrop stage panicked; body left unchanged", b.name)], ..Default::default() }))).collect()
    });
    let mut out = Vec::with_capacity(bodies.len());
    for (b, r) in per {
        out.push(b);
        rep.teardropped.extend(r.teardropped);
        rep.skipped.extend(r.skipped);
        rep.warnings.extend(r.warnings);
    }
    (out, rep)
}

fn teardrop_body(b: &Body, profile: &ProcessProfile, doc: &DocData, rep: &mut ProcessReport) -> Result<Option<Shape>, occt::Error> {
    let td = profile.teardrop;
    let shape = b.shape();
    let up = doc.orientation_of(b.id).up_in_model().normalize();
    let faces = shape.faces()?;
    let edges = shape.edges()?;
    // per-hole overrides for this body
    let mut overrides: HashMap<u32, bool> = HashMap::new();
    for o in doc.teardrop_overrides.iter().filter(|o| o.body == b.id) {
        if let Ok(ix) = o.face.resolve(shape) {
            for i in ix {
                overrides.insert(i, o.enabled);
            }
        }
    }
    let mut cutters = Vec::new();
    for f in faces.iter().filter(|f| f.kind == occt::SurfaceKind::Cylinder) {
        let Some((o, a)) = f.axis else { continue };
        let a = a.normalize();
        let r = f.radius;
        let radial = f.point - (o + a * (f.point - o).dot(a));
        if f.normal.dot(radial) >= 0.0 {
            continue; // convex: a boss or pin, not a hole
        }
        // axial extent from the face's boundary edges
        let (mut t0, mut t1) = (f64::INFINITY, f64::NEG_INFINITY);
        for ei in shape.face_edges(f.index)? {
            let e = &edges[ei as usize];
            for p in [e.start, e.end, e.midpoint] {
                let t = (p - o).dot(a);
                t0 = t0.min(t);
                t1 = t1.max(t);
            }
        }
        let len = t1 - t0;
        let hole = HoleRef { body: b.id, face: f.index, center: o + a * (t0 + t1) / 2.0, axis: a, diameter: 2.0 * r, length: len };
        let forced = overrides.get(&f.index).copied();
        if forced == Some(false) {
            rep.skipped.push((hole, "disabled by override".into()));
            continue;
        }
        if forced.is_none() && !td.enabled {
            continue;
        }
        if len.is_nan() || len <= 1e-6 {
            rep.skipped.push((hole, "zero-length hole".into()));
            continue;
        }
        if forced.is_none() && 2.0 * r < td.min_diameter {
            rep.skipped.push((hole, format!("diameter {:.2} below the {:.2} mm minimum", 2.0 * r, td.min_diameter)));
            continue;
        }
        let from_horizontal = a.dot(up).abs().clamp(0.0, 1.0).asin().to_degrees();
        if forced.is_none() && from_horizontal > td.angle_threshold_deg {
            rep.skipped.push((hole, format!("axis {from_horizontal:.0}° from horizontal (threshold {:.0}°)", td.angle_threshold_deg)));
            continue;
        }
        let full = 2.0 * std::f64::consts::PI * r * len;
        if (f.area - full).abs() > 0.03 * full {
            rep.skipped.push((hole, "not a full round hole (partial cylinder)".into()));
            continue;
        }
        let u = (up - a * up.dot(a)).normalize_or_zero();
        if u == DVec3::ZERO {
            rep.skipped.push((hole, "axis parallel to build direction".into()));
            continue;
        }
        let s = a.cross(u);
        let c0 = o + a * t0;
        let p2 = |x: f64, y: f64| c0 + s * x + u * y;
        let k = r * std::f64::consts::FRAC_1_SQRT_2;
        let apex = r * std::f64::consts::SQRT_2;
        let mut pts = vec![p2(0.0, 0.0), p2(k, k)];
        match td.style {
            TeardropStyle::Pointed45 => pts.push(p2(0.0, apex)),
            TeardropStyle::Flat(fr) => {
                let h = fr.clamp(std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::SQRT_2) * r;
                let half = apex - h;
                if half <= 1e-9 {
                    pts.push(p2(0.0, apex));
                } else if h <= k + 1e-9 {
                    rep.skipped.push((hole, "flat height at or below the tangent points (no cap)".into()));
                    continue;
                } else {
                    pts.push(p2(half, h));
                    pts.push(p2(-half, h));
                }
            }
        }
        pts.push(p2(-k, k));
        let edges3: Vec<Edge> = (0..pts.len()).map(|i| Edge::Line(pts[i], pts[(i + 1) % pts.len()])).collect();
        let face = Shape::face(&Frame { origin: c0, x_dir: s, normal: a }, &edges3, &[])?;
        cutters.push(face.extrude(a * len)?);
        rep.teardropped.push(hole);
    }
    if cutters.is_empty() {
        return Ok(None);
    }
    let refs: Vec<&Shape> = cutters.iter().collect();
    let tool = if refs.len() == 1 { cutters[0].clone() } else { Shape::fuse_all(&refs)? };
    let r = tidy(shape.cut(&tool)?);
    if !r.is_valid() {
        return Err(occt::Error("teardrop cut produced an invalid solid".into()));
    }
    Ok(Some(r))
}
