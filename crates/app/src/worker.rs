//! Background regeneration. The UI thread never regenerates: it sends the full
//! document state to this worker, which keeps the per-prefix regeneration cache,
//! tessellates every body, builds the picking BVH and sends the result back.
//! Only the latest request is processed (older queued ones are dropped).

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use casework_model::document::{BlobStore, DocData};
use casework_model::feature::BodyId;
use casework_model::library::Library;
use casework_model::occt::{BBox, EdgeInfo, FaceInfo, Mesh, Shape};
use casework_model::pick::Picker;
use casework_model::regen::{Body, MeshQuality, RegenResult, Regenerator};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// The committed document (possibly rolled back).
    Model,
    /// A feature dialog's live preview.
    Preview,
    /// A drawing sheet (HLR views + annotations) for the Drawing screen.
    Drawing,
}

/// Serializes kernel-heavy work between the regen worker and background export
/// threads (a crash was seen with several kernels running concurrently in tests).
pub static KERNEL: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn kernel_lock() -> std::sync::MutexGuard<'static, ()> {
    KERNEL.lock().unwrap_or_else(|e| e.into_inner())
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
    pub drawing: Option<casework_model::drawing::Drawing>,
    /// Tessellation level (`Preview` while dragging).
    pub quality: MeshQuality,
    /// Section plane (origin, normal, in-plane x) to cut every solid with (joint dialog).
    pub section: Option<(glam::DVec3, glam::DVec3, glam::DVec3)>,
}

/// One solid's cut at the section plane, as triangles in plane coordinates.
pub struct SectionCut {
    pub body: BodyId,
    pub tris: Vec<[glam::DVec2; 3]>,
}

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

pub struct Output {
    pub id: u64,
    pub purpose: Purpose,
    pub data: DocData,
    pub result: RegenResult,
    pub bodies: Vec<MeshedBody>,
    pub picker: Arc<Picker>,
    pub bbox: Option<BBox>,
    pub prefix_bodies: HashMap<usize, Vec<Body>>,
    pub drawing: Option<Arc<casework_model::drawing::DrawingResult>>,
    pub sections: Vec<SectionCut>,
    pub quality: MeshQuality,
    pub elapsed: Duration,
}

impl Output {
    pub fn mesh(&self, id: BodyId) -> Option<&Arc<Mesh>> {
        self.bodies
            .iter()
            .find(|b| b.body.id == id)
            .and_then(|b| b.mesh.as_ref())
    }
    pub fn body(&self, id: BodyId) -> Option<&Body> {
        self.bodies.iter().map(|b| &b.body).find(|b| b.id == id)
    }
    pub fn shape(&self, id: BodyId) -> Option<&Shape> {
        self.body(id).map(|b| b.shape())
    }
    pub fn meshed(&self, id: BodyId) -> Option<&MeshedBody> {
        self.bodies.iter().find(|b| b.body.id == id)
    }
    pub fn triangles(&self) -> usize {
        self.bodies
            .iter()
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
    })
}

impl Worker {
    /// Start the worker. `wake` is called after each result (e.g. request a repaint).
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let (tx, req_rx) = channel::<Request>();
        let (out_tx, rx) = channel::<Output>();
        let thread = std::thread::Builder::new()
            .name("casework-regen".into())
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

fn run(rx: Receiver<Request>, tx: Sender<Output>, wake: impl Fn()) {
    let mut regen = Regenerator::default();
    let mut dcache = casework_model::drawing::DrawingCache::default();
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
            let out = process(&mut regen, &mut dcache, r);
            if tx.send(out).is_err() {
                return;
            }
            wake();
        }
    }
}

fn process(
    regen: &mut Regenerator,
    dcache: &mut casework_model::drawing::DrawingCache,
    r: Request,
) -> Output {
    let _kernel = kernel_lock();
    let t0 = Instant::now();
    let result = regen.regenerate(&r.data, &r.blobs, &r.library);
    let drawing = r.drawing.as_ref().map(|d| {
        // BOM for balloons, from the document data alone
        let mut tmp = casework_model::document::Document::new(&r.data.name);
        tmp.data = r.data.clone();
        let bom = tmp.bom();
        Arc::new(casework_model::drawing::compute(
            &r.data, &result, &bom, d, dcache,
        ))
    });
    let mut prefix_bodies = HashMap::new();
    for n in r.prefixes {
        let mut d = r.data.clone();
        d.features.truncate(n);
        let res = regen.regenerate(&d, &r.blobs, &r.library);
        prefix_bodies.insert(n, res.bodies);
    }
    let bodies: Vec<MeshedBody> = result
        .bodies
        .iter()
        .map(|b| {
            let faces = b.shape().faces().unwrap_or_default();
            let edges = b.shape().edges().unwrap_or_default();
            let colors = b
                .appearance()
                .and_then(|_| b.mesh_colors(r.quality).ok())
                .map(Arc::new);
            match b.mesh_quality(r.quality) {
                Ok(m) => MeshedBody {
                    body: b.clone(),
                    mesh: Some(m),
                    error: None,
                    colors,
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
    for b in &bodies {
        if let Some(m) = &b.mesh {
            for p in &m.positions {
                let p = glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
                bbox = Some(match bbox {
                    None => BBox { min: p, max: p },
                    Some(bb) => BBox {
                        min: bb.min.min(p),
                        max: bb.max.max(p),
                    },
                });
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
    }
}
