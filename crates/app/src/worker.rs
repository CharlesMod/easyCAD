//! Background regeneration. The UI thread never regenerates: it sends the full
//! document state to this worker, which keeps the per-prefix regeneration cache,
//! tessellates every body, builds the picking BVH and sends the result back.
//! Only the latest request is processed (older queued ones are dropped).

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use easycad_model::assembly::{
    AssemblyCache, AssemblyResult, ComponentId, MateSolve, OccPath, OccurrenceId, Transform,
};
use easycad_model::document::{BlobStore, DocData};
use easycad_model::feature::{BodyId, FeatureId};
use easycad_model::positions::PositionId;
use easycad_model::library::Library;
use easycad_model::occt::{BBox, EdgeInfo, FaceInfo, Mesh, Shape};
use easycad_model::pick::{BodyPicker, EdgeTolerance, Hit, HitKind, Picker};
use easycad_model::regen::{Body, MeshQuality, RegenResult, Regenerator};
use glam::{DAffine3, DVec3, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// The committed document (possibly rolled back).
    Model,
    /// A feature dialog's live preview.
    Preview,
    /// A drawing sheet (HLR views + annotations) for the Drawing screen.
    Drawing,
    /// The Claude bridge's own model: the whole committed document, whatever the
    /// user's viewport shows (dialog rollback, section, active component).
    Agent,
}

pub struct Request {
    pub id: u64,
    pub purpose: Purpose,
    pub data: DocData,
    pub blobs: BlobStore,
    pub library: Library,
    /// Also return the bodies after the first `n` features, for each `n` here
    /// (served from the cache; the fillet dialog maps report edges with them).
    pub prefixes: Vec<usize>,
    /// For `Purpose::Drawing`: the (draft) drawing to compute.
    pub drawing: Option<easycad_model::drawing::Drawing>,
    /// Tessellation level (`Preview` while dragging).
    pub quality: MeshQuality,
    /// Section plane (origin, normal, in-plane x) to cut every solid with (joint dialog).
    pub section: Option<(glam::DVec3, glam::DVec3, glam::DVec3)>,
    /// Assemblies: the occurrence being edited (`[]` = the root). Its component's
    /// bodies are `Output::bodies` and its frame is the view frame.
    pub active: OccPath,
    /// Assemblies: explode amount 0..1 (display only).
    pub explode: f32,
    /// Section view: (point, normal) in the view frame; bodies keep what is behind
    /// the plane and get cap faces.
    pub cut: Option<(DVec3, DVec3)>,
    /// Show the model in this position (animation state; display only).
    pub pose: Option<PositionId>,
}

/// One solid's cut at the section plane, as triangles in plane coordinates.
pub struct SectionCut {
    pub body: BodyId,
    pub tris: Vec<[glam::DVec2; 3]>,
}

#[derive(Clone)]
pub struct MeshedBody {
    pub body: Body,
    pub mesh: Option<Arc<Mesh>>,
    pub error: Option<String>,
    /// Topology of the shape (indices match the mesh's face / edge ids).
    /// Per-vertex colours when the body carries an appearance (coloured STEP).
    pub colors: Option<Arc<Vec<[f32; 3]>>>,
    pub faces: Vec<FaceInfo>,
    pub edges: Vec<EdgeInfo>,
}

/// A body placed by another occurrence (assemblies): drawn and picked in its
/// component frame through `xf`.
pub struct CtxBody {
    /// Unique per (occurrence path, body): the selection / pick key.
    pub key: BodyId,
    pub path: OccPath,
    pub component: ComponentId,
    /// In the component's frame (`mb.body.id` is the component's own body id).
    pub mb: MeshedBody,
    pub picker: Option<Arc<BodyPicker>>,
    /// Component frame -> view frame, explode included.
    pub xf: DAffine3,
    /// Component frame -> view frame without explode.
    pub place: DAffine3,
}

/// Assembly side of an output.
pub struct AsmView {
    pub active: OccPath,
    pub active_comp: ComponentId,
    /// View frame -> world (the active occurrence's placement).
    pub frame: Transform,
    pub result: Arc<AssemblyResult>,
    /// Mates solved before regenerating (the data in `Output::data` is solved).
    pub solve: Option<MateSolve>,
    pub explode: f32,
}

pub struct Output {
    pub id: u64,
    pub purpose: Purpose,
    pub data: DocData,
    pub result: RegenResult,
    pub bodies: Vec<MeshedBody>,
    pub picker: Arc<Picker>,
    pub bbox: Option<BBox>,
    pub prefix_bodies: HashMap<usize, Vec<Body>>,
    pub drawing: Option<Arc<easycad_model::drawing::DrawingResult>>,
    pub sections: Vec<SectionCut>,
    pub quality: MeshQuality,
    pub elapsed: Duration,
    /// Assemblies: bodies of the other occurrences.
    pub ctx: Vec<CtxBody>,
    pub asm: Option<AsmView>,
    /// Context bodies can be picked (the root is active).
    pub ctx_pickable: bool,
    /// Section view caps: (mesh, mesh frame -> view frame).
    pub caps: Vec<(Arc<Mesh>, DAffine3)>,
    /// Section view: the whole (uncut) mesh of each body the plane cut, by body
    /// (or occurrence) key, in its own mesh frame. The viewport draws it as a
    /// wireframe, so the cut-away half stays visible as context.
    pub cut_away: HashMap<BodyId, Arc<Mesh>>,
    /// The position the bodies are shown in (`None` = as modeled).
    pub pose: Option<PositionId>,
    /// The position the request asked for (even when it failed).
    pub pose_requested: Option<PositionId>,
    /// Why the requested position could not be applied (bodies shown as modeled).
    pub pose_error: Option<String>,
}

impl Output {
    pub fn mesh(&self, id: BodyId) -> Option<&Arc<Mesh>> {
        self.meshed(id).and_then(|b| b.mesh.as_ref())
    }
    pub fn body(&self, id: BodyId) -> Option<&Body> {
        self.meshed(id).map(|b| &b.body)
    }
    pub fn shape(&self, id: BodyId) -> Option<&Shape> {
        self.body(id).map(|b| b.shape())
    }
    /// A body of the active component, or a placed context body by its key.
    pub fn meshed(&self, id: BodyId) -> Option<&MeshedBody> {
        self.bodies
            .iter()
            .find(|b| b.body.id == id)
            .or_else(|| self.ctx_body(id).map(|c| &c.mb))
    }
    pub fn ctx_body(&self, key: BodyId) -> Option<&CtxBody> {
        if !is_ctx_key(key) {
            return None;
        }
        self.ctx.iter().find(|c| c.key == key)
    }
    /// Mesh frame -> view frame of a body (identity for the active component's).
    pub fn xf(&self, id: BodyId) -> DAffine3 {
        self.ctx_body(id).map(|c| c.xf).unwrap_or(DAffine3::IDENTITY)
    }
    /// Is this a body of the component being edited (not a placed context body)?
    pub fn is_active_body(&self, id: BodyId) -> bool {
        !is_ctx_key(id) && self.bodies.iter().any(|b| b.body.id == id)
    }
    /// Every face / edge along the ray over the active bodies and (when pickable)
    /// the context bodies, nearest first. `skip` filters context bodies out.
    pub fn pick_all(&self, origin: Vec3, dir: Vec3, tol: EdgeTolerance, skip: &dyn Fn(&CtxBody) -> bool) -> Vec<Hit> {
        let mut hits = self.picker.pick_all(origin, dir, tol);
        if self.ctx_pickable {
            let mut tmp = Vec::new();
            for c in &self.ctx {
                let Some(bp) = &c.picker else { continue };
                if skip(c) {
                    continue;
                }
                let inv = c.xf.inverse();
                let o = inv.transform_point3(origin.as_dvec3()).as_vec3();
                let d = inv.transform_vector3(dir.as_dvec3()).as_vec3();
                tmp.clear();
                bp.pick(o, d, tol, &mut tmp);
                for mut h in tmp.drain(..) {
                    h.body = c.key;
                    h.point = c.xf.transform_point3(h.point.as_dvec3()).as_vec3();
                    hits.push(h);
                }
            }
            hits.sort_by(|a, b| a.t.total_cmp(&b.t));
            let mut out: Vec<Hit> = Vec::new();
            for h in hits {
                let dup = out
                    .iter()
                    .rev()
                    .take_while(|o| h.t - o.t < 1e-3 + 1e-6 * h.t)
                    .any(|o| o.body == h.body && o.kind == h.kind);
                if !dup {
                    out.push(h);
                }
            }
            out.sort_by(|a, b| {
                let ta = (a.t * 1e3).round();
                let tb = (b.t * 1e3).round();
                ta.total_cmp(&tb).then_with(|| {
                    matches!(b.kind, HitKind::Edge(_)).cmp(&matches!(a.kind, HitKind::Edge(_)))
                })
            });
            return out;
        }
        hits
    }
    pub fn triangles(&self) -> usize {
        self.bodies
            .iter()
            .chain(self.ctx.iter().map(|c| &c.mb))
            .filter_map(|b| b.mesh.as_ref())
            .map(|m| m.indices.len())
            .sum()
    }
}

/// Handle owned by the UI.
pub struct Worker {
    tx: Option<Sender<Request>>,
    rx: Receiver<Output>,
    thread: Option<JoinHandle<()>>,
    next_id: u64,
    /// Id of the newest request sent, per purpose.
    pub pending: HashMap<PurposeKey, u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PurposeKey(u8);

fn pkey(p: Purpose) -> PurposeKey {
    PurposeKey(match p {
        Purpose::Model => 0,
        Purpose::Preview => 1,
        Purpose::Drawing => 2,
        Purpose::Agent => 3,
    })
}

impl Worker {
    /// Start the worker. `wake` is called after each result (e.g. request a repaint).
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let (tx, req_rx) = channel::<Request>();
        let (out_tx, rx) = channel::<Output>();
        let thread = std::thread::Builder::new()
            .name("easycad-regen".into())
            .spawn(move || run(req_rx, out_tx, wake))
            .expect("spawn regen worker");
        Worker {
            tx: Some(tx),
            rx,
            thread: Some(thread),
            next_id: 1,
            pending: HashMap::new(),
        }
    }

    pub fn submit(
        &mut self,
        purpose: Purpose,
        data: DocData,
        blobs: BlobStore,
        library: Library,
        prefixes: Vec<usize>,
    ) -> u64 {
        self.submit_full(Request {
            id: 0,
            purpose,
            data,
            blobs,
            library,
            prefixes,
            drawing: None,
            quality: MeshQuality::Display,
            section: None,
            active: Vec::new(),
            explode: 0.0,
            cut: None,
            pose: None,
        })
    }

    pub fn submit_full(&mut self, mut r: Request) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        r.id = id;
        self.pending.insert(pkey(r.purpose), id);
        if let Some(tx) = &self.tx {
            let _ = tx.send(r);
        }
        id
    }

    /// Results that arrived since the last poll (oldest first).
    pub fn poll(&mut self) -> Vec<Output> {
        let mut v = Vec::new();
        while let Ok(o) = self.rx.try_recv() {
            if self.pending.get(&pkey(o.purpose)) == Some(&o.id) {
                self.pending.remove(&pkey(o.purpose));
            }
            v.push(o);
        }
        v
    }

    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn busy_with(&self, p: Purpose) -> bool {
        self.pending.contains_key(&pkey(p))
    }
    pub fn cancel(&mut self, p: Purpose) {
        self.pending.remove(&pkey(p));
    }

    /// Block until nothing is pending (tests); returns false on timeout.
    pub fn wait_idle(&mut self, timeout: Duration, sink: &mut Vec<Output>) -> bool {
        let start = Instant::now();
        while self.busy() {
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(o) => {
                    if self.pending.get(&pkey(o.purpose)) == Some(&o.id) {
                        self.pending.remove(&pkey(o.purpose));
                    }
                    sink.push(o);
                }
                Err(_) if start.elapsed() > timeout => return false,
                Err(_) => {}
            }
        }
        true
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Context body keys: high bit of the feature id set (real feature ids are small).
pub fn is_ctx_key(k: BodyId) -> bool {
    k.feature.0 & 0x8000_0000 != 0
}

/// Stable key of (occurrence path, body).
pub fn ctx_key(path: &[OccurrenceId], body: BodyId) -> BodyId {
    // FNV-1a over the path
    let mut h: u32 = 0x811c_9dc5;
    for o in path {
        for b in o.0.to_le_bytes() {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    BodyId {
        feature: FeatureId(0x8000_0000 | (h & 0x7fff_ffff)),
        index: (body.feature.0 << 8) ^ (body.index & 0xff),
    }
}

type VertexColors = Arc<Vec<[f32; 3]>>;

/// A component body's mesh with what its instances share: picker, topology, bounds.
type MeshEntry = (Arc<Mesh>, Arc<BodyPicker>, Arc<(Vec<FaceInfo>, Vec<EdgeInfo>)>, BBox);

/// Worker-side caches for assemblies.
#[derive(Default)]
struct AsmCaches {
    asm: AssemblyCache,
    /// Per mesh (by pointer): picker, topology and mesh bounds, shared by instances.
    meshes: HashMap<usize, MeshEntry>,
    used: std::collections::HashSet<usize>,
    /// Per-vertex colours by mesh (pointer; the mesh is held so the address stays
    /// unique): the same `Arc` every regeneration, so the viewport's image cache
    /// and the GPU upload see an unchanged body as unchanged.
    colors: HashMap<usize, (Arc<Mesh>, Option<VertexColors>)>,
}

impl AsmCaches {
    fn colors(&mut self, body: &easycad_model::regen::Body, mesh: &Arc<Mesh>, q: MeshQuality) -> Option<Arc<Vec<[f32; 3]>>> {
        body.appearance()?;
        let k = Arc::as_ptr(mesh) as usize;
        self.colors
            .entry(k)
            .or_insert_with(|| (mesh.clone(), body.mesh_colors(q).ok().map(Arc::new)))
            .1
            .clone()
    }
}

fn run(rx: Receiver<Request>, tx: Sender<Output>, wake: impl Fn()) {
    let mut regen = Regenerator::default();
    let mut dcache = easycad_model::drawing::DrawingCache::default();
    let mut acache = AsmCaches::default();
    while let Ok(mut req) = rx.recv() {
        // keep only the newest request of each purpose, model first
        let mut latest: Vec<Request> = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(r) => {
                    if r.purpose == req.purpose {
                        req = r;
                    } else {
                        latest.retain(|l| l.purpose != r.purpose);
                        latest.push(r);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        latest.push(req);
        latest.sort_by_key(|r| r.purpose as u8);
        for r in latest {
            let out = process(&mut regen, &mut dcache, &mut acache, r);
            if tx.send(out).is_err() {
                return;
            }
            wake();
        }
    }
}

fn process(
    regen: &mut Regenerator,
    dcache: &mut easycad_model::drawing::DrawingCache,
    acache: &mut AsmCaches,
    mut r: Request,
) -> Output {
    // no kernel mutex: the engine's concurrency contract allows parallel threads
    let t0 = Instant::now();
    // colours of meshes nothing else holds any more
    acache.colors.retain(|_, (m, _)| Arc::strong_count(m) > 1);
    let asm_mode = !r.data.occurrences.is_empty();
    let mut asm_out: Option<(AssemblyResult, Option<MateSolve>, ComponentId, OccPath)> = None;
    let result = if asm_mode {
        let solve = if r.data.mates.is_empty() {
            None
        } else {
            let (moves, solve) = easycad_model::assembly::solve_mates(&r.data);
            for (id, t) in moves {
                if let Some(o) = r.data.occurrence_mut(id) {
                    o.transform = t;
                }
            }
            Some(solve)
        };
        let ar = easycad_model::assembly::regenerate_assembly(
            &r.data,
            &r.blobs,
            &r.library,
            regen,
            &mut acache.asm,
        );
        let (comp, active) = match r.data.component_at(&r.active) {
            Ok(c) => (c, r.active.clone()),
            Err(_) => (ComponentId::ROOT, Vec::new()),
        };
        let res = ar
            .component(comp)
            .map(|c| c.regen.clone())
            .unwrap_or_else(|| regen.regenerate(&r.data, &r.blobs, &r.library));
        asm_out = Some((ar, solve, comp, active));
        res
    } else {
        regen.regenerate(&r.data, &r.blobs, &r.library)
    };
    // position (animation state): move root bodies and occurrences, display only
    let mut result = result;
    let mut pose_error = None;
    let pose_id = r.pose.filter(|_| r.purpose != Purpose::Drawing);
    if let Some(pid) = pose_id {
        let root_active = asm_out.as_ref().map_or(true, |a| a.2 == ComponentId::ROOT);
        let applied = r.data.pose(pid).and_then(|pose| {
            if root_active {
                result.bodies = pose.apply(&result.bodies)?;
            }
            if let Some(a) = asm_out.as_mut() {
                a.0 = pose.apply_assembly(&a.0)?;
            }
            Ok(())
        });
        if let Err(e) = applied {
            pose_error = Some(e);
        }
    }
    let drawing = r.drawing.as_ref().map(|d| {
        // balloons and BOM tables from the document data (GUI_NEEDS #11)
        let flat;
        let ar = match &asm_out {
            Some(a) => &a.0,
            None => {
                flat = AssemblyResult::flat(result.clone());
                &flat
            }
        };
        let bom = r.data.bom(ar);
        let tree = r.data.bom_tree(ar);
        let src = easycad_model::drawing::DrawingSources {
            regen: &result,
            bom: &bom,
            assembly: asm_out.as_ref().map(|a| &a.0),
            bom_tree: Some(&tree),
        };
        Arc::new(easycad_model::drawing::compute_with(&r.data, &src, d, dcache))
    });
    // face provenance is lazy: fill it here, off the UI thread (pick-stack labels)
    for b in &result.bodies {
        let _ = result.face_origins.of(b.id);
    }
    let mut prefix_bodies = HashMap::new();
    let active_comp = asm_out.as_ref().map(|a| a.2).unwrap_or(ComponentId::ROOT);
    for n in std::mem::take(&mut r.prefixes) {
        if active_comp == ComponentId::ROOT {
            let mut d = r.data.clone();
            d.features.truncate(n);
            let res = regen.regenerate(&d, &r.blobs, &r.library);
            prefix_bodies.insert(n, res.bodies);
        } else if let Some(mut d) = r.data.component_view(active_comp) {
            // the component's regenerator still holds its in-context inputs
            d.features.truncate(n);
            let res = acache.asm.regenerator(active_comp).regenerate(&d, &r.blobs, &r.library);
            prefix_bodies.insert(n, res.bodies);
        }
    }
    let bodies: Vec<MeshedBody> = result
        .bodies
        .iter()
        .map(|b| {
            let faces = b.shape().faces().unwrap_or_default();
            let edges = b.shape().edges().unwrap_or_default();
            match b.mesh_quality(r.quality) {
                Ok(m) => MeshedBody {
                    colors: acache.colors(b, &m, r.quality),
                    body: b.clone(),
                    mesh: Some(m),
                    error: None,
                    faces,
                    edges,
                },
                Err(e) => MeshedBody {
                    body: b.clone(),
                    mesh: None,
                    error: Some(e),
                    colors: None,
                    faces,
                    edges,
                },
            }
        })
        .collect();
    let mut sections = Vec::new();
    if let Some((o, n, x)) = r.section {
        let y = n.cross(x);
        for b in bodies.iter().filter(|b| !b.body.reference) {
            let Ok(face) = b.body.shape().section(o, n) else {
                continue;
            };
            let Ok(m) = face.tessellate(0.02, 5.0) else {
                continue;
            };
            let p2 = |i: u32| {
                let p = m.positions[i as usize];
                let v = glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64) - o;
                glam::DVec2::new(v.dot(x), v.dot(y))
            };
            let tris = m
                .indices
                .iter()
                .map(|t| [p2(t[0]), p2(t[1]), p2(t[2])])
                .collect();
            sections.push(SectionCut {
                body: b.body.id,
                tris,
            });
        }
    }
    let picker = if r.purpose == Purpose::Drawing {
        Arc::new(Picker::new(std::iter::empty()))
    } else {
        Arc::new(Picker::new(
            bodies
                .iter()
                .filter_map(|b| b.mesh.as_ref().map(|m| (b.body.id, &**m))),
        ))
    };
    let mut bbox: Option<BBox> = None;
    let mut grow = |p: DVec3| {
        bbox = Some(match bbox {
            None => BBox { min: p, max: p },
            Some(bb) => BBox {
                min: bb.min.min(p),
                max: bb.max.max(p),
            },
        });
    };
    for b in &bodies {
        if let Some(m) = &b.mesh {
            for p in &m.positions {
                grow(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64));
            }
        }
    }
    let (mut ctx, mut asm, mut ctx_pickable) = (Vec::new(), None, false);
    if let Some((ar, solve, comp, active)) = asm_out {
        let frame = r.data.world_transform(&active).unwrap_or(Transform::IDENTITY);
        if r.purpose != Purpose::Drawing {
            ctx = context_bodies(&r, &ar, &active, &frame, acache);
        }
        for c in &ctx {
            if let Some(bb) = acache
                .meshes
                .get(&c.mb.mesh.as_ref().map(|m| Arc::as_ptr(m) as usize).unwrap_or(0))
                .map(|e| e.3)
            {
                for i in 0..8 {
                    let p = DVec3::new(
                        if i & 1 == 0 { bb.min.x } else { bb.max.x },
                        if i & 2 == 0 { bb.min.y } else { bb.max.y },
                        if i & 4 == 0 { bb.min.z } else { bb.max.z },
                    );
                    grow(c.xf.transform_point3(p));
                }
            }
        }
        ctx_pickable = active.is_empty();
        asm = Some(AsmView {
            active,
            active_comp: comp,
            frame,
            result: Arc::new(ar),
            solve,
            explode: r.explode,
        });
    }
    let mut caps = Vec::new();
    let mut cut_away = HashMap::new();
    let (mut bodies, mut picker) = (bodies, picker);
    let mut ctx = ctx;
    if let Some((o, n)) = r.cut {
        // section view (GUI_NEEDS #9): keep what is behind the plane, add the caps
        for b in &mut bodies {
            if let Some((mb, cap)) = cut_body(&b.body, o, n, r.quality) {
                if let Some(whole) = b.mesh.clone().filter(|_| !kept_whole(&mb, b)) {
                    cut_away.insert(b.body.id, whole);
                }
                *b = mb;
                if let Some(c) = cap {
                    // a hair towards the viewer: the kept half has its own face there
                    caps.push((c, DAffine3::from_translation(n * 0.05)));
                }
            }
        }
        picker = Arc::new(Picker::new(
            bodies.iter().filter_map(|b| b.mesh.as_ref().map(|m| (b.body.id, &**m))),
        ));
        for c in &mut ctx {
            let inv = c.xf.inverse();
            let (lo, ln) = (inv.transform_point3(o), inv.transform_vector3(n));
            if let Some((mb, cap)) = cut_body(&c.mb.body, lo, ln, r.quality) {
                if let Some(whole) = c.mb.mesh.clone().filter(|_| !kept_whole(&mb, &c.mb)) {
                    cut_away.insert(c.key, whole);
                }
                c.picker = mb.mesh.as_ref().map(|m| Arc::new(BodyPicker::new(mb.body.id, m)));
                c.mb = mb;
                if let Some(cm) = cap {
                    caps.push((cm, DAffine3::from_translation(n * 0.05) * c.xf));
                }
            }
        }
    }
    Output {
        id: r.id,
        purpose: r.purpose,
        data: r.data,
        result,
        bodies,
        picker,
        bbox,
        prefix_bodies,
        drawing,
        sections,
        quality: r.quality,
        elapsed: t0.elapsed(),
        ctx,
        asm,
        ctx_pickable,
        caps,
        cut_away,
        pose: pose_id.filter(|_| pose_error.is_none()),
        pose_requested: pose_id,
        pose_error,
    }
}

/// The plane missed the body on its front: all of it is kept, nothing is cut away.
pub fn kept_whole(cut: &MeshedBody, whole: &MeshedBody) -> bool {
    cut.mesh.is_some() && cut.faces.len() == whole.faces.len() && cut.edges.len() == whole.edges.len()
        && match (cut.body.shape().volume(), whole.body.shape().volume()) {
            (Ok(a), Ok(b)) => (a - b).abs() <= 1e-6 * b.abs().max(1.0),
            _ => false,
        }
}

/// One body cut at a plane: the kept half (meshed, with its topology) and the cap.
pub fn cut_body(b: &Body, o: DVec3, n: DVec3, q: MeshQuality) -> Option<(MeshedBody, Option<Arc<Mesh>>)> {
    let sv = b.section_view(o, n, q).ok()?;
    let kept = match sv.kept {
        Some(k) => k,
        // all of it is in front of the plane: nothing left to show
        None => {
            return Some((
                MeshedBody { body: b.clone(), mesh: None, error: None, colors: None, faces: Vec::new(), edges: Vec::new() },
                None,
            ))
        }
    };
    let mesh = kept.mesh_quality(q).ok();
    Some((
        MeshedBody {
            faces: kept.shape().faces().unwrap_or_default(),
            edges: kept.shape().edges().unwrap_or_default(),
            mesh,
            error: None,
            colors: None,
            body: kept,
        },
        // references (a PCB) are cut but get no cap: they draw translucent
        if b.reference { None } else { sv.cap_mesh },
    ))
}

/// The other occurrences' bodies, placed in the view frame (with the explode).
fn context_bodies(
    r: &Request,
    ar: &AssemblyResult,
    active: &OccPath,
    frame: &Transform,
    cache: &mut AsmCaches,
) -> Vec<CtxBody> {
    let to_view = frame.inverse().affine();
    cache.used.clear();
    let inst: Vec<_> = ar
        .instances
        .iter()
        .filter(|i| i.visible && i.path != *active)
        .collect();
    // automatic explode: top-level groups spread apart along z by their height
    // order, and away from the centre in xy; stored `explode` offsets scale too
    let mut auto: HashMap<OccurrenceId, DVec3> = HashMap::new();
    let t = r.explode.clamp(0.0, 1.0) as f64;
    let mut prepared = Vec::new();
    for i in &inst {
        let Ok(m) = i.body.mesh_quality(r.quality) else {
            continue;
        };
        let k = Arc::as_ptr(&m) as usize;
        if let std::collections::hash_map::Entry::Vacant(slot) = cache.meshes.entry(k) {
            let faces = i.body.shape().faces().unwrap_or_default();
            let edges = i.body.shape().edges().unwrap_or_default();
            let mut bb: Option<BBox> = None;
            for p in &m.positions {
                let p = DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
                bb = Some(match bb {
                    None => BBox { min: p, max: p },
                    Some(b) => BBox { min: b.min.min(p), max: b.max.max(p) },
                });
            }
            let bb = bb.unwrap_or(BBox { min: DVec3::ZERO, max: DVec3::ZERO });
            slot.insert((m.clone(), Arc::new(BodyPicker::new(i.body.id, &m)), Arc::new((faces, edges)), bb));
        }
        cache.used.insert(k);
        prepared.push((i, m, k));
    }
    if t > 0.0 && active.is_empty() {
        let mut groups: HashMap<OccurrenceId, BBox> = HashMap::new();
        let mut all: Option<BBox> = None;
        for (i, _, k) in &prepared {
            let Some(top) = i.path.first() else { continue };
            let bb = cache.meshes[k].3;
            let a = i.transform.affine();
            for c in 0..8 {
                let p = a.transform_point3(DVec3::new(
                    if c & 1 == 0 { bb.min.x } else { bb.max.x },
                    if c & 2 == 0 { bb.min.y } else { bb.max.y },
                    if c & 4 == 0 { bb.min.z } else { bb.max.z },
                ));
                for b in [groups.entry(*top).or_insert(BBox { min: p, max: p }), all.get_or_insert(BBox { min: p, max: p })] {
                    b.min = b.min.min(p);
                    b.max = b.max.max(p);
                }
            }
        }
        if let Some(all) = all {
            let mut order: Vec<(OccurrenceId, BBox)> = groups.into_iter().collect();
            order.sort_by(|a, b| a.1.center().z.total_cmp(&b.1.center().z).then(a.0.cmp(&b.0)));
            let n = order.len() as f64;
            let h = all.size().z.max(10.0);
            for (rank, (id, bb)) in order.iter().enumerate() {
                let c = bb.center() - all.center();
                let z = (rank as f64 - (n - 1.0) / 2.0) * h * 0.9;
                auto.insert(*id, DVec3::new(c.x * 0.8, c.y * 0.8, z) * t);
            }
        }
    }
    let mut out = Vec::new();
    for (i, m, k) in prepared {
        let colors = cache.colors(&i.body, &m, r.quality);
        let (_, picker, topo, _) = &cache.meshes[&k];
        let place = to_view * i.transform.affine();
        let stored = i.display.translation - i.transform.translation;
        let off = stored * t + i.path.first().and_then(|o| auto.get(o)).copied().unwrap_or(DVec3::ZERO);
        let xf = to_view * DAffine3::from_translation(off) * i.transform.affine();
        out.push(CtxBody {
            key: ctx_key(&i.path, i.body.id),
            path: i.path.clone(),
            component: i.component,
            mb: MeshedBody {
                body: i.body.clone(),
                mesh: Some(m),
                error: None,
                colors,
                faces: topo.0.clone(),
                edges: topo.1.clone(),
            },
            picker: Some(picker.clone()),
            xf,
            place,
        });
    }
    let used = std::mem::take(&mut cache.used);
    cache.meshes.retain(|k, _| used.contains(k));
    cache.used = used;
    out
}
