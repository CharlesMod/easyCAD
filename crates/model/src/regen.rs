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
use crate::select::{resolve_edges, resolve_faces};
use crate::sketch::{segment_point, Sketch, SolveReport};

/// Geometry of a body plus its lazily computed display mesh.
pub struct BodyGeom {
    pub shape: Shape,
    mesh: OnceLock<Result<Arc<Mesh>, String>>,
}

impl BodyGeom {
    pub fn new(shape: Shape) -> Arc<Self> {
        Arc::new(BodyGeom { shape, mesh: OnceLock::new() })
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
        self.geom
            .mesh
            .get_or_init(|| self.geom.shape.tessellate(MESH_LIN_DEFL, MESH_ANG_DEFL_DEG).map(Arc::new).map_err(|e| e.0))
            .clone()
    }
}

#[derive(Clone)]
struct ToolOp {
    target: BodyId,
    op: BoolOp,
    tool: Arc<Shape>,
}

#[derive(Clone, Default)]
struct ModelState {
    bodies: Vec<Body>,
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
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FeatureReport {
    pub feature: FeatureId,
    pub status: FeatureStatus,
    /// Fillet/chamfer details (edges applied / reduced / failed).
    #[serde(skip)]
    pub dressup: Option<DressUpReport>,
    pub sketch: Option<SolveReport>,
    /// Served from the regeneration cache.
    pub cached: bool,
}

#[derive(Clone)]
pub struct RegenResult {
    pub bodies: Vec<Body>,
    pub reports: Vec<FeatureReport>,
    pub recomputed: usize,
    pub reused: usize,
}

impl RegenResult {
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
        }
    }
}

#[derive(Debug)]
struct Fail(String);

impl<E: std::fmt::Display> From<E> for Fail {
    fn from(e: E) -> Self {
        Fail(e.to_string())
    }
}

type R<T> = Result<T, Fail>;

fn fail<T>(m: impl Into<String>) -> R<T> {
    Err(Fail(m.into()))
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
        let (vals, param_err) = match doc.params.evaluate() {
            Ok(v) => (v, None),
            Err(e) => (Values::new(), Some(e.to_string())),
        };
        let mut key = 0x6361_7365_776f_726bu64; // "casework"
        let mut state: Arc<ModelState> = Arc::new(ModelState::default());
        let mut reports = Vec::new();
        let (mut recomputed, mut reused) = (0, 0);
        for f in &doc.features {
            let mut h = DefaultHasher::new();
            key.hash(&mut h);
            fingerprint(f, doc, &vals).hash(&mut h);
            param_err.hash(&mut h);
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
            let (next, report) = if f.suppressed {
                ((*state).clone(), FeatureReport { feature: f.id, status: FeatureStatus::Suppressed, dressup: None, sketch: None, cached: false })
            } else {
                let mut ctx = Ctx { doc, vals: &vals, blobs, lib, regen: self, report: None, sketch_report: None, warning: None };
                let res = ctx.exec(f, &state);
                let (dressup, sketch, warning) = (ctx.report.take(), ctx.sketch_report.take(), ctx.warning.take());
                match res {
                    Ok(s) => (
                        s,
                        FeatureReport {
                            feature: f.id,
                            status: warning.map(FeatureStatus::Warning).unwrap_or(FeatureStatus::Ok),
                            dressup,
                            sketch,
                            cached: false,
                        },
                    ),
                    Err(Fail(e)) => {
                        let e = match &param_err {
                            Some(pe) => format!("{e} (parameters: {pe})"),
                            None => e,
                        };
                        ((*state).clone(), FeatureReport { feature: f.id, status: FeatureStatus::Error(e), dressup, sketch, cached: false })
                    }
                }
            };
            let step: Step = Arc::new((next, report));
            self.insert(key, step.clone());
            state = Arc::new(step.0.clone());
            reports.push(step.1.clone());
        }
        RegenResult { bodies: state.bodies.clone(), reports, recomputed, reused }
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
    blobs: &'a BlobStore,
    lib: &'a Library,
    regen: &'a mut Regenerator,
    report: Option<DressUpReport>,
    sketch_report: Option<SolveReport>,
    warning: Option<String>,
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
        e.eval(self.vals).map_err(|err| Fail(format!("`{}`: {err}", e.0)))
    }

    fn target(&self, st: &ModelState, id: Option<BodyId>) -> R<usize> {
        match id {
            Some(id) => st.bodies.iter().position(|b| b.id == id).ok_or_else(|| Fail(format!("body {id:?} does not exist"))),
            None => st.bodies.iter().rposition(|b| !b.reference).ok_or_else(|| Fail("there is no solid body yet".into())),
        }
    }

    fn sketch(&mut self, id: SketchId) -> R<Sketch> {
        let mut sk = self.doc.sketch(id).cloned().ok_or_else(|| Fail(format!("sketch {id:?} does not exist")))?;
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
            ProfileSel::Indices(ix) => ix.iter().map(|i| all.get(*i).cloned().ok_or_else(|| Fail(format!("profile {i} does not exist")))).collect::<R<_>>()?,
            ProfileSel::Containing(pts) => {
                let mut v = Vec::new();
                for p in pts {
                    let hit = all.iter().find(|pr| profile_contains(pr, *p)).ok_or_else(|| Fail(format!("no profile contains {p:?}")))?;
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
        st.tools.entry(fid).or_default().push(ToolOp { target: tid, op, tool });
        Ok(())
    }

    fn exec(&mut self, f: &Feature, prev: &ModelState) -> R<ModelState> {
        let mut st = prev.clone();
        let fid = f.id;
        match &f.kind {
            FeatureKind::Extrude { sketch, profiles, distance, symmetric, reverse, draft_deg, op, target } => {
                let (plane, prs) = self.profiles(*sketch, profiles)?;
                let d = self.ev(distance)?;
                let draft = self.ev(draft_deg)?;
                let dir = if *reverse { -plane.normal } else { plane.normal };
                let mut tools = Vec::new();
                for pr in &prs {
                    tools.push(profile_face(&plane, pr)?.extrude_ext(&frame(&plane), dir, d, *symmetric, draft)?);
                }
                let tool = fuse_many(&tools)?;
                self.apply_op(&mut st, fid, &f.name, *op, *target, tool)?;
            }
            FeatureKind::Revolve { sketch, profiles, axis, angle_deg, op, target } => {
                let (plane, prs) = self.profiles(*sketch, profiles)?;
                let a = self.ev(angle_deg)?;
                let mut tools = Vec::new();
                for pr in &prs {
                    tools.push(profile_face(&plane, pr)?.revolve(axis.origin, axis.dir, a)?);
                }
                let tool = fuse_many(&tools)?;
                self.apply_op(&mut st, fid, &f.name, *op, *target, tool)?;
            }
            FeatureKind::Fillet { body, edges, radius, auto_reduce } | FeatureKind::Chamfer { body, edges, distance: radius, auto_reduce } => {
                let i = self.target(&st, *body)?;
                let r = self.ev(radius)?;
                let shape = st.bodies[i].shape().clone();
                let ids = resolve_edges(&shape, edges)?;
                let req: Vec<(u32, f64)> = ids.iter().map(|e| (*e, r)).collect();
                let opts = dressup::DressUpOptions { auto_reduce: *auto_reduce, ..Default::default() };
                let (out, rep) = if matches!(f.kind, FeatureKind::Fillet { .. }) {
                    dressup::fillet(&shape, &req, &opts)?
                } else {
                    dressup::chamfer(&shape, &req, &opts)?
                };
                let applied = rep.applied_count();
                let summary = rep.summary();
                let all = rep.all_applied();
                self.report = Some(rep);
                if applied == 0 {
                    return fail(summary);
                }
                if !all {
                    self.warning = Some(summary);
                }
                st.bodies[i].geom = BodyGeom::new(out);
            }
            FeatureKind::Shell { body, open_faces, thickness, outward } => {
                let i = self.target(&st, *body)?;
                let t = self.ev(thickness)?;
                let shape = st.bodies[i].shape().clone();
                let faces = resolve_faces(&shape, open_faces)?;
                st.bodies[i].geom = BodyGeom::new(shape.shell(&faces, t, *outward)?);
            }
            FeatureKind::Draft { body, faces, neutral, pull, angle_deg } => {
                let i = self.target(&st, *body)?;
                let a = self.ev(angle_deg)?;
                let shape = st.bodies[i].shape().clone();
                let ids = resolve_faces(&shape, faces)?;
                st.bodies[i].geom = BodyGeom::new(shape.draft(&ids, *pull, a, &frame(neutral))?);
            }
            FeatureKind::Rib { sketch, thickness, height, draft_deg, flip, body } => {
                let sk = self.sketch(*sketch)?;
                let (t, h, dr) = (self.ev(thickness)?, self.ev(height)?, self.ev(draft_deg)?);
                let chains = sk.profiles().open;
                if chains.is_empty() {
                    return fail("rib sketch has no open chain");
                }
                let dir = if *flip { -sk.plane.normal } else { sk.plane.normal };
                let tool = rib_tool(&sk.plane, &chains, t, h, dr, dir)?;
                self.apply_op(&mut st, fid, &f.name, BoolOp::Join, *body, tool)?;
            }
            FeatureKind::ScrewBoss { body, plane, points, od, hole_dia, height, hole_depth, base_fillet, draft_deg } => {
                let spec = BossSpec {
                    od: self.ev(od)?,
                    hole: self.ev(hole_dia)?,
                    height: self.ev(height)?,
                    hole_depth: hole_depth.as_ref().map(|e| self.ev(e)).transpose()?,
                    fillet: self.ev(base_fillet)?,
                    draft: self.ev(draft_deg)?,
                };
                self.bosses(&mut st, fid, *body, plane, points, &spec)?;
            }
            FeatureKind::StandoffsFromPcb { body, source, plane, od, hole_dia, height, base_fillet, draft_deg } => {
                let pts = self.pcb_points(&st, source, plane)?;
                if pts.is_empty() {
                    return fail("no mounting holes found in the PCB within the diameter range");
                }
                let spec = BossSpec {
                    od: self.ev(od)?,
                    hole: self.ev(hole_dia)?,
                    height: self.ev(height)?,
                    hole_depth: None,
                    fillet: self.ev(base_fillet)?,
                    draft: self.ev(draft_deg)?,
                };
                self.bosses(&mut st, fid, *body, plane, &pts, &spec)?;
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
                self.replicate(&mut st, fid, &f.name, source, &xfs)?;
            }
            FeatureKind::Mirror { source, plane } => {
                self.replicate(&mut st, fid, &f.name, source, &[Xf::Mirror(plane.origin, plane.normal)])?;
            }
            FeatureKind::Split { body, plane, new_name } => {
                let i = self.target(&st, *body)?;
                let (below, above) = st.bodies[i].shape().split(plane.origin, plane.normal)?;
                let (Some(below), Some(above)) = (below, above) else {
                    return fail("the split plane does not cut the body");
                };
                st.bodies[i].geom = BodyGeom::new(below);
                st.bodies.push(Body { id: BodyId::of(fid), name: new_name.clone(), reference: false, part: None, geom: BodyGeom::new(above) });
            }
            FeatureKind::ImportStep { import, reference, translate } => {
                let imp = self.doc.import(*import).ok_or_else(|| Fail(format!("import {import:?} missing")))?;
                let mut s = (*self.step_shape(&imp.blob)?).clone();
                if *translate != DVec3::ZERO {
                    s = s.translated(*translate)?;
                }
                if *reference {
                    st.bodies.push(Body { id: BodyId::of(fid), name: imp.name.clone(), reference: true, part: None, geom: BodyGeom::new(s) });
                } else {
                    for (k, solid) in s.solids()?.into_iter().enumerate() {
                        st.bodies.push(Body {
                            id: BodyId { feature: fid, index: k as u32 },
                            name: format!("{} {}", imp.name, k + 1),
                            reference: false,
                            part: None,
                            geom: BodyGeom::new(solid),
                        });
                    }
                }
            }
            FeatureKind::InsertPart { part, placements, flip, offset, auto_hole, target } => {
                let inst = self.part(part)?;
                let off = self.ev(offset)?;
                let ti = self.target(&st, *target).ok();
                let mut holes = Vec::new();
                let hole_spec = match auto_hole {
                    Some(ah) => {
                        let mut spec = inst.hole(ah.choice)?;
                        if let Some(d) = &ah.depth {
                            spec.depth = Some(self.ev(d)?);
                        }
                        Some(spec)
                    }
                    None => None,
                };
                for (k, pl) in placements.iter().enumerate() {
                    let (origin, mut axis) = self.placement(&st, ti, pl)?;
                    if *flip {
                        axis = -axis;
                    }
                    let placed = place(&inst.shape, origin + axis * off, axis, 0.0)?;
                    st.bodies.push(Body {
                        id: BodyId { feature: fid, index: k as u32 },
                        name: format!("{} {} #{}", part.name, part.size, k + 1),
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
                    self.apply_op(&mut st, fid, &f.name, BoolOp::Cut, Some(tid), fuse_many(&holes)?)?;
                }
            }
        }
        Ok(st)
    }

    fn step_shape(&mut self, blob: &str) -> R<Arc<Shape>> {
        if let Some(s) = self.regen.imports.get(blob) {
            return Ok(s.clone());
        }
        let data = self.blobs.get(blob).ok_or_else(|| Fail(format!("blob {blob} is missing")))?;
        let s = Arc::new(Shape::read_step_str(&String::from_utf8_lossy(&data))?);
        self.regen.imports.insert(blob.to_string(), s.clone());
        Ok(s)
    }

    fn dxf(&mut self, blob: &str) -> R<Arc<DxfDrawing>> {
        if let Some(d) = self.regen.dxfs.get(blob) {
            return Ok(d.clone());
        }
        let data = self.blobs.get(blob).ok_or_else(|| Fail(format!("blob {blob} is missing")))?;
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
            target.map(|i| st.bodies[i].shape()).ok_or_else(|| Fail("placement needs a target body".into()))
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
                let imp = self.doc.import(*import).ok_or_else(|| Fail("DXF import missing".into()))?;
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
                        let imp = self.doc.import(*import).ok_or_else(|| Fail("STEP import missing".into()))?;
                        (*self.step_shape(&imp.blob.clone())?).clone()
                    }
                };
                let faces = shape.faces()?;
                let n = plane.normal.normalize();
                let board = faces
                    .iter()
                    .filter(|f| f.kind == SurfaceKind::Plane && f.normal.dot(n).abs() > 0.999)
                    .max_by(|a, b| a.area.total_cmp(&b.area))
                    .ok_or_else(|| Fail("no board face parallel to the boss plane".into()))?;
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
        let n = plane.normal.normalize();
        let r_top = b.od / 2.0 - b.height * b.draft.to_radians().tan();
        if r_top <= b.hole / 2.0 {
            return fail("draft makes the boss top narrower than the hole");
        }
        let mut bosses = Vec::new();
        let mut holes = Vec::new();
        for p in points {
            let base = plane.to_world(*p);
            bosses.push(if b.draft.abs() > 1e-9 {
                Shape::cone(base, n, b.od / 2.0, r_top, b.height)?
            } else {
                Shape::cylinder(base, n, b.od / 2.0, b.height)?
            });
            let depth = b.hole_depth.unwrap_or(b.height);
            holes.push(Shape::cylinder(base + n * (b.height - depth), n, b.hole / 2.0, depth + 1.0)?);
        }
        let i = self.target(st, body)?;
        let tid = st.bodies[i].id;
        self.apply_op(st, fid, "", BoolOp::Join, Some(tid), fuse_many(&bosses)?)?;
        if b.fillet > 0.0 {
            let i = self.target(st, Some(tid))?;
            let shape = st.bodies[i].shape().clone();
            let edges = shape.edges()?;
            let req: Vec<(u32, f64)> = edges
                .iter()
                .filter(|e| match e.circle {
                    Some((c, _, r)) => (r - b.od / 2.0).abs() < 1e-3 && points.iter().any(|p| plane.to_world(*p).distance(c) < 1e-3),
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
        self.apply_op(st, fid, "", BoolOp::Cut, Some(tid), fuse_many(&holes)?)?;
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
                let tool = fuse_many(&all[1..])?;
                st.bodies[i].geom = BodyGeom::new(fused);
                st.tools.entry(fid).or_default().push(ToolOp { target: *b, op: BoolOp::Join, tool: Arc::new(tool) });
            }
            PatternSource::Features(fs) => {
                // Group transformed tools per (target, op) and apply each group once.
                let mut groups: Vec<(BodyId, BoolOp, Vec<Shape>)> = Vec::new();
                for f in fs {
                    let ops = st.tools.get(f).cloned().ok_or_else(|| Fail(format!("feature {f:?} has no tool bodies to pattern (only extrude, revolve, rib, boss, hole and pattern features can be patterned)")))?;
                    for op in ops {
                        let gi = match groups.iter().position(|g| g.0 == op.target && g.1 == op.op && op.op != BoolOp::NewBody) {
                            Some(g) => g,
                            None => {
                                groups.push((op.target, op.op, Vec::new()));
                                groups.len() - 1
                            }
                        };
                        for x in xfs {
                            groups[gi].2.push(x.apply(&op.tool)?);
                        }
                    }
                }
                for (target, op, tools) in groups {
                    if op == BoolOp::NewBody {
                        for t in tools {
                            self.apply_op(st, fid, name, BoolOp::NewBody, None, t)?;
                        }
                    } else {
                        let tool = fuse_many(&tools)?;
                        self.apply_op(st, fid, name, op, Some(target), tool)?;
                    }
                }
            }
        }
        Ok(())
    }
}

struct BossSpec {
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
