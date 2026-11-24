//! The document: parameters, sketches, features and imports (the serializable
//! [`DocData`]), plus undo/redo snapshots, content-addressed blobs and the
//! regeneration cache.
//!
//! On disk a document is a **folder** `<name>.ecad/`:
//! ```text
//! Enclosure.ecad/
//!   doc.json            deterministic, pretty-printed DocData
//!   blobs/<sha256>      imported STEP/DXF files and embedded library STEP parts
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::feature::*;
use crate::library::{Library, PartInfo};
use crate::params::{Expr, ExprError, Params};
use crate::process::{BuildOrientation, ProcessProfile};
use crate::regen::{RegenResult, Regenerator};
use crate::sketch::Sketch;

/// 2: assemblies (`components`, `occurrences`, `mates`). Format-1 files load as a
/// single root component (the new fields default to empty).
pub const FORMAT_VERSION: u32 = 2;
pub const DOC_FILE: &str = "doc.json";
pub const BLOB_DIR: &str = "blobs";

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("parameter: {0}")]
    Param(#[from] ExprError),
    #[error("unsupported document format {0}")]
    Format(u32),
    #[error("blob {0} is referenced but missing")]
    MissingBlob(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportKind {
    Step,
    Dxf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Import {
    pub name: String,
    pub kind: ImportKind,
    /// sha256 (hex) of the file content in the blob store.
    pub blob: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchEntry {
    pub id: SketchId,
    pub sketch: Sketch,
    /// User override of the sketch's visibility in the 3D view. `None` = the
    /// default: shown until a feature consumes the sketch, then hidden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportEntry {
    pub id: ImportId,
    pub import: Import,
}

/// Everything that is saved and snapshotted for undo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocData {
    pub format: u32,
    /// Stable UUID of the document across devices.
    pub doc_id: String,
    /// Monotonically increasing; bumped on every change (including undo/redo).
    pub revision: u64,
    pub modified_by_device: String,
    pub name: String,
    pub params: Params,
    pub sketches: Vec<SketchEntry>,
    pub features: Vec<Feature>,
    pub imports: Vec<ImportEntry>,
    /// Free-form metadata (title block fields: author, part number, ...).
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
    /// Process profiles available in this document (built-ins + user presets).
    #[serde(default = "ProcessProfile::builtins")]
    pub processes: Vec<ProcessProfile>,
    /// Name of the profile fits and process stages are resolved against.
    #[serde(default = "default_profile")]
    pub active_profile: String,
    /// Document default build orientation.
    #[serde(default)]
    pub build_orientation: BuildOrientation,
    /// Per-body build orientation overrides.
    #[serde(default)]
    pub body_orientations: Vec<BodyOrientation>,
    /// Per-hole teardrop on/off overrides.
    #[serde(default)]
    pub teardrop_overrides: Vec<TeardropOverride>,
    /// 2D drawings.
    #[serde(default)]
    pub drawings: Vec<crate::drawing::Drawing>,
    /// Component definitions other than the root (the root is this document's own
    /// params/sketches/features/imports). Sorted by id.
    #[serde(default)]
    pub components: Vec<crate::assembly::Component>,
    /// Occurrences placed directly in the root component.
    #[serde(default)]
    pub occurrences: Vec<crate::assembly::Occurrence>,
    /// Mates between occurrences (paths from the root).
    #[serde(default)]
    pub mates: Vec<crate::assembly::Mate>,
    /// History rollback marker: only the first `n` root features are computed
    /// (the rest report `rolled_back`); new features are inserted at the marker,
    /// which moves past them. `None` = the end of the history.
    #[serde(default)]
    pub rollback: Option<usize>,
    /// Construction features the user hid in the 3D view (default: all shown).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_datums: Vec<FeatureId>,
    /// Show the origin planes/axes/point in the 3D view (default off; shown while picking).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub origin_visible: bool,
    pub next_id: u32,
    /// Saved alternative histories (each a full snapshot with its own `branches`
    /// emptied). The live history is `branch`; see `branch.rs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<crate::branch::Branch>,
    /// Name of the branch the live history is.
    #[serde(default = "crate::branch::main_name", skip_serializing_if = "crate::branch::is_main")]
    pub branch: String,
    /// Stable id of the live branch (0 for the original).
    #[serde(default, skip_serializing_if = "crate::branch::is_zero")]
    pub branch_id: u32,
    /// The branch this one was started from, and the feature count it shared with it then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_parent: Option<String>,
    #[serde(default, skip_serializing_if = "crate::branch::is_zero_usize")]
    pub branch_split: usize,
}

impl DocData {
    pub fn new(name: &str, device: &str) -> Self {
        DocData {
            format: FORMAT_VERSION,
            doc_id: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            modified_by_device: device.into(),
            name: name.into(),
            params: Params::default(),
            sketches: Vec::new(),
            features: Vec::new(),
            imports: Vec::new(),
            meta: BTreeMap::new(),
            processes: ProcessProfile::builtins(),
            active_profile: default_profile(),
            build_orientation: BuildOrientation::default(),
            body_orientations: Vec::new(),
            teardrop_overrides: Vec::new(),
            drawings: Vec::new(),
            components: Vec::new(),
            occurrences: Vec::new(),
            mates: Vec::new(),
            rollback: None,
            hidden_datums: Vec::new(),
            origin_visible: false,
            next_id: 1,
            branches: Vec::new(),
            branch: crate::branch::main_name(),
            branch_id: 0,
            branch_parent: None,
            branch_split: 0,
        }
    }

    pub(crate) fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The active process profile (falls back to nominal if the name is unknown).
    pub fn profile(&self) -> ProcessProfile {
        self.profile_named(&self.active_profile).unwrap_or_else(ProcessProfile::molded_nominal)
    }
    /// Add the active profile's default parameters that the document does not
    /// already have (never overwrites). Returns how many were added.
    pub fn seed_profile_params(&mut self) -> usize {
        let defaults = self.profile().params;
        let mut n = 0;
        for d in defaults {
            if self.params.get(&d.name).is_some() || !crate::params::valid_name(&d.name) {
                continue;
            }
            if self.params.set(&d.name, d.expr.as_str()).is_ok() {
                if let Some(p) = self.params.list.iter_mut().find(|p| p.name == d.name) {
                    p.comment = d.comment;
                }
                n += 1;
            }
        }
        n
    }

    pub fn profile_named(&self, name: &str) -> Option<ProcessProfile> {
        self.processes.iter().find(|p| p.name == name).cloned()
    }
    /// Parameter values under the active profile (what features see).
    pub fn values(&self) -> Result<crate::params::Values, ExprError> {
        let mut seed = crate::params::Values::new();
        self.profile().inject(&mut seed);
        self.params.evaluate_seeded(seed)
    }

    /// Build orientation of a body (override or document default).
    pub fn orientation_of(&self, body: BodyId) -> BuildOrientation {
        self.body_orientations.iter().find(|o| o.body == body).map(|o| o.orientation).unwrap_or(self.build_orientation)
    }

    pub fn drawing(&self, id: crate::drawing::DrawingId) -> Option<&crate::drawing::Drawing> {
        self.drawings.iter().find(|d| d.id == id)
    }
    pub fn drawing_mut(&mut self, id: crate::drawing::DrawingId) -> Option<&mut crate::drawing::Drawing> {
        self.drawings.iter_mut().find(|d| d.id == id)
    }

    pub fn sketch(&self, id: SketchId) -> Option<&Sketch> {
        self.sketches.iter().find(|s| s.id == id).map(|s| &s.sketch)
    }
    pub fn sketch_mut(&mut self, id: SketchId) -> Option<&mut Sketch> {
        self.sketches.iter_mut().find(|s| s.id == id).map(|s| &mut s.sketch)
    }
    /// True when some feature builds from this sketch.
    pub fn sketch_consumed(&self, id: SketchId) -> bool {
        self.features.iter().any(|f| f.kind.sketch() == Some(id))
    }

    /// Index of the first feature that builds from this sketch.
    pub fn first_consumer(&self, id: SketchId) -> Option<usize> {
        self.features.iter().position(|f| f.kind.sketch() == Some(id))
    }

    /// The rollback marker that puts the history head right after sketch `id`:
    /// the sketch's first consuming feature (and all later ones) is rolled
    /// back, the sketch itself stays live. `None` (roll forward) when nothing
    /// consumes it, since it then sits after every feature.
    pub fn rollback_after_sketch(&self, id: SketchId) -> Option<usize> {
        self.first_consumer(id)
    }

    /// Whether the sketch is drawn in the 3D view: the user's choice if any,
    /// else visible until a feature consumes it.
    pub fn sketch_visible(&self, id: SketchId) -> bool {
        match self.sketches.iter().find(|s| s.id == id) {
            // consumed means by a feature that is still live (not rolled back)
            Some(e) => e.visible.unwrap_or_else(|| !self.first_consumer(id).is_some_and(|i| i < self.head_pos())),
            None => false,
        }
    }

    /// Set (or with `None` reset) the visibility override.
    pub fn set_sketch_visible(&mut self, id: SketchId, v: Option<bool>) {
        if let Some(e) = self.sketches.iter_mut().find(|s| s.id == id) {
            e.visible = v;
        }
    }

    /// Whether a construction plane/axis/point is drawn (default: yes).
    pub fn datum_visible(&self, id: FeatureId) -> bool {
        !self.hidden_datums.contains(&id)
    }

    pub fn set_datum_visible(&mut self, id: FeatureId, visible: bool) {
        self.hidden_datums.retain(|f| *f != id);
        if !visible {
            self.hidden_datums.push(id);
            self.hidden_datums.sort();
        }
    }

    /// Update the cached `plane` of every sketch that follows a construction plane
    /// from a fresh regeneration, so editors and drawings use the current plane.
    /// Sketches whose datum did not resolve keep their stored plane. Returns whether
    /// anything changed. (A cache update: not an undoable edit, no revision bump.)
    pub fn refresh_sketch_planes(&mut self, res: &RegenResult) -> bool {
        let mut changed = false;
        for e in &mut self.sketches {
            if let Some(src) = e.sketch.plane_src {
                if let Some((_, crate::datum::DatumGeom::Plane(p))) = res.datums.iter().find(|(f, _)| *f == src) {
                    if e.sketch.plane != *p {
                        e.sketch.plane = *p;
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Write back the geometry regen re-linked projections to (points, entities,
    /// constraints; the plane is `refresh_sketch_planes`' job), so the stored
    /// sketch, the editor and regen agree. Returns whether anything changed. (A
    /// cache update like `refresh_sketch_planes`: no undo step, no revision bump.)
    pub fn apply_relinked(&mut self, res: &RegenResult) -> bool {
        let mut changed = false;
        for (id, sk) in res.reports.iter().flat_map(|r| &r.relinked) {
            if let Some(e) = self.sketches.iter_mut().find(|e| e.id == *id) {
                if e.sketch.points != sk.points || e.sketch.entities != sk.entities || e.sketch.constraints != sk.constraints
                    || e.sketch.proj_srcs != sk.proj_srcs
                {
                    e.sketch.points = sk.points.clone();
                    e.sketch.entities = sk.entities.clone();
                    e.sketch.constraints = sk.constraints.clone();
                    e.sketch.proj_srcs = sk.proj_srcs.clone();
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn import(&self, id: ImportId) -> Option<&Import> {
        self.imports.iter().find(|s| s.id == id).map(|s| &s.import)
    }
    pub fn feature(&self, id: FeatureId) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id)
    }
    pub fn feature_mut(&mut self, id: FeatureId) -> Option<&mut Feature> {
        self.features.iter_mut().find(|f| f.id == id)
    }

    pub fn add_sketch(&mut self, sketch: Sketch) -> SketchId {
        let id = SketchId(self.alloc());
        self.sketches.push(SketchEntry { id, sketch, visible: None });
        id
    }

    /// Append a root feature, or insert it at the rollback marker (which then
    /// moves past it).
    pub fn add_feature(&mut self, name: &str, kind: FeatureKind) -> FeatureId {
        let id = FeatureId(self.alloc());
        let f = Feature { id, name: name.into(), suppressed: false, kind };
        match self.rollback {
            Some(r) if r < self.features.len() => {
                self.features.insert(r, f);
                self.rollback = Some(r + 1);
            }
            _ => {
                self.features.push(f);
                self.rollback = None;
            }
        }
        id
    }

    /// Hashes of every blob the document references.
    pub fn blob_refs(&self) -> Vec<String> {
        let mut v: Vec<String> = self.imports.iter().map(|i| i.import.blob.clone()).collect();
        let comp_imports = self.components.iter().flat_map(|c| c.imports.iter());
        v.extend(comp_imports.map(|i| i.import.blob.clone()));
        let feats = self.features.iter().chain(self.components.iter().flat_map(|c| c.features.iter()));
        for f in feats {
            if let FeatureKind::InsertPart { part: PartRef { blob: Some(b), .. }, .. } = &f.kind {
                v.push(b.clone());
            }
        }
        for c in &self.components {
            if let crate::assembly::ComponentSource::Step { blob, .. } = &c.source {
                v.push(blob.clone());
            }
            if let Some(PartRef { blob: Some(b), .. }) = &c.purchased {
                v.push(b.clone());
            }
        }
        v.sort();
        v.dedup();
        v
    }

    /// Deterministic pretty JSON (struct fields in declaration order, maps sorted).
    pub fn to_json(&self) -> Result<String, DocError> {
        // Round-trip through Value so any map ends up key-sorted.
        let v = serde_json::to_value(self)?;
        Ok(serde_json::to_string_pretty(&v)? + "\n")
    }

    pub fn from_json(s: &str) -> Result<Self, DocError> {
        let mut d: DocData = serde_json::from_str(s)?;
        if d.format > FORMAT_VERSION {
            return Err(DocError::Format(d.format));
        }
        // Migration 1 -> 2: a flat document is the root component with no children;
        // the assembly fields defaulted to empty above.
        d.format = FORMAT_VERSION;
        d.components.sort_by_key(|c| c.id);
        Ok(d)
    }
}

fn default_profile() -> String {
    ProcessProfile::molded_nominal().name
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BodyOrientation {
    pub body: BodyId,
    pub orientation: BuildOrientation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TeardropOverride {
    pub body: BodyId,
    /// The hole's cylindrical face.
    pub face: crate::select::FaceSelector,
    pub enabled: bool,
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Content-addressed binary store (sha256 → bytes).
#[derive(Clone, Debug, Default)]
pub struct BlobStore {
    blobs: BTreeMap<String, Arc<Vec<u8>>>,
}

impl BlobStore {
    pub fn put(&mut self, data: Vec<u8>) -> String {
        let h = sha256_hex(&data);
        self.blobs.entry(h.clone()).or_insert_with(|| Arc::new(data));
        h
    }
    pub fn get(&self, hash: &str) -> Option<Arc<Vec<u8>>> {
        self.blobs.get(hash).cloned()
    }
    pub fn contains(&self, hash: &str) -> bool {
        self.blobs.contains_key(hash)
    }
    pub fn hashes(&self) -> impl Iterator<Item = &String> {
        self.blobs.keys()
    }
}

#[derive(Clone, Debug)]
struct Snapshot {
    label: String,
    data: DocData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Stl,
    ThreeMf,
    Step,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportOptions {
    /// Process profile override (e.g. FDM for STL, molded for STEP).
    pub profile: Option<String>,
    /// Rotate each body by its build orientation and drop it onto z = 0.
    pub orient_to_plate: bool,
    /// STEP only: include reference bodies (PCBs, library parts).
    pub include_reference: bool,
}

/// One line of a bill of materials.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BomLine {
    pub source: String,
    pub part: String,
    pub name: String,
    pub size: String,
    pub qty: usize,
    pub vendor: Option<String>,
    pub part_number: Option<String>,
}

/// An open document.
pub struct Document {
    pub data: DocData,
    pub blobs: BlobStore,
    pub library: Library,
    pub device_id: String,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    pub max_undo: usize,
    pub(crate) regen: Regenerator,
    last: Option<RegenResult>,
    drawing_cache: crate::drawing::DrawingCache,
    pub(crate) asm: crate::assembly::AssemblyCache,
    pub(crate) asm_last: Option<crate::assembly::AssemblyResult>,
}

impl Document {
    pub fn new(name: &str) -> Self {
        let mut data = DocData::new(name, &default_device());
        data.seed_profile_params();
        Self::with_data(data, BlobStore::default())
    }

    /// A new document under `profile` (falls back to the default if unknown),
    /// starting with that profile's default parameters.
    pub fn new_with_profile(name: &str, profile: &str) -> Self {
        let mut d = Self::new(name);
        if d.data.profile_named(profile).is_some() && d.data.active_profile != profile {
            d.data.active_profile = profile.to_string();
            d.data.params = Params::default();
            d.data.seed_profile_params();
        }
        d
    }

    /// Add the active profile's default parameters this document lacks (undoable;
    /// never overwrites). Returns how many were added.
    pub fn apply_profile_defaults(&mut self) -> usize {
        let mut n = 0;
        let _ = self.edit("Apply profile defaults", |d| {
            n = d.seed_profile_params();
            Ok::<_, DocError>(())
        });
        n
    }

    fn with_data(data: DocData, blobs: BlobStore) -> Self {
        Document {
            device_id: default_device(),
            data,
            blobs,
            library: Library::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            max_undo: 500,
            regen: Regenerator::default(),
            last: None,
            drawing_cache: crate::drawing::DrawingCache::default(),
            asm: Default::default(),
            asm_last: None,
        }
    }

    fn touch(&mut self) {
        self.data.revision += 1;
        self.data.modified_by_device = self.device_id.clone();
        self.last = None;
        self.asm_last = None;
    }

    /// Apply an undoable change. If `f` fails, the document is left unchanged.
    pub fn edit<T, E>(&mut self, label: &str, f: impl FnOnce(&mut DocData) -> Result<T, E>) -> Result<T, E> {
        let before = self.data.clone();
        match f(&mut self.data) {
            Ok(v) => {
                self.undo.push(Snapshot { label: label.into(), data: before });
                if self.undo.len() > self.max_undo {
                    self.undo.remove(0);
                }
                self.redo.clear();
                self.touch();
                Ok(v)
            }
            Err(e) => {
                self.data = before;
                Err(e)
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }
    /// The document state an undo would restore (for previews; nothing changes).
    pub fn peek_undo(&self) -> Option<&DocData> {
        self.undo.last().map(|s| &s.data)
    }
    /// The document state a redo would restore.
    pub fn peek_redo(&self) -> Option<&DocData> {
        self.redo.last().map(|s| &s.data)
    }

    /// Undo the last edit; returns its label.
    pub fn undo(&mut self) -> Option<String> {
        let s = self.undo.pop()?;
        let rev = self.data.revision;
        let cur = std::mem::replace(&mut self.data, s.data);
        self.redo.push(Snapshot { label: s.label.clone(), data: cur });
        self.data.revision = rev;
        self.touch();
        Some(s.label)
    }

    pub fn redo(&mut self) -> Option<String> {
        let s = self.redo.pop()?;
        let rev = self.data.revision;
        let cur = std::mem::replace(&mut self.data, s.data);
        self.undo.push(Snapshot { label: s.label.clone(), data: cur });
        self.data.revision = rev;
        self.touch();
        Some(s.label)
    }

    // ---------------------------------------------------------------- edits

    pub fn set_param(&mut self, name: &str, expr: impl Into<Expr>) -> Result<(), DocError> {
        let expr = expr.into();
        self.edit(&format!("Set {name}"), |d| {
            d.params.set(name, expr)?;
            d.params.evaluate()?;
            Ok::<_, DocError>(())
        })
    }

    pub fn add_sketch(&mut self, sketch: Sketch) -> SketchId {
        let label = format!("Add sketch {}", sketch.name);
        self.edit(&label, |d| Ok::<_, DocError>(d.add_sketch(sketch))).unwrap()
    }

    pub fn add_feature(&mut self, name: &str, kind: FeatureKind) -> FeatureId {
        let label = format!("{} {name}", kind.label());
        self.edit(&label, |d| Ok::<_, DocError>(d.add_feature(name, kind))).unwrap()
    }

    /// Put the history head right after sketch `id` (one undo step).
    pub fn roll_back_to_sketch(&mut self, id: SketchId) {
        let at = self.data.rollback_after_sketch(id);
        self.set_rollback(at);
    }

    /// Move the history rollback marker (undoable): `Some(n)` computes only the
    /// first `n` root features, `None` (or `n` ≥ the feature count) rolls forward.
    pub fn set_rollback(&mut self, at: Option<usize>) {
        let at = at.filter(|n| *n < self.data.features.len());
        if at == self.data.rollback {
            return;
        }
        let label = match at {
            Some(n) => format!("Roll back to {n}"),
            None => "Roll forward".to_string(),
        };
        self.edit(&label, |d| {
            d.rollback = at;
            Ok::<_, DocError>(())
        })
        .unwrap()
    }

    /// Import a STEP file (stored as a blob) and add an `ImportStep` feature.
    pub fn import_step(&mut self, name: &str, data: Vec<u8>, reference: bool) -> (ImportId, FeatureId) {
        let size = data.len() as u64;
        let blob = self.blobs.put(data);
        self.edit(&format!("Import {name}"), |d| {
            let id = ImportId(d.alloc());
            d.imports.push(ImportEntry { id, import: Import { name: name.into(), kind: ImportKind::Step, blob, size } });
            let f = d.add_feature(name, FeatureKind::ImportStep { import: id, reference, translate: glam::DVec3::ZERO });
            Ok::<_, DocError>((id, f))
        })
        .unwrap()
    }

    pub fn import_step_file(&mut self, path: &Path, reference: bool) -> Result<(ImportId, FeatureId), DocError> {
        let data = std::fs::read(path)?;
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "STEP".into());
        Ok(self.import_step(&name, data, reference))
    }

    /// Store a DXF as an import (no feature; use it from sketches or standoffs).
    pub fn import_dxf(&mut self, name: &str, data: Vec<u8>) -> ImportId {
        let size = data.len() as u64;
        let blob = self.blobs.put(data);
        self.edit(&format!("Import {name}"), |d| {
            let id = ImportId(d.alloc());
            d.imports.push(ImportEntry { id, import: Import { name: name.into(), kind: ImportKind::Dxf, blob, size } });
            Ok::<_, DocError>(id)
        })
        .unwrap()
    }

    /// Parsed DXF of an import.
    pub fn dxf(&self, id: ImportId) -> Result<crate::dxf::DxfDrawing, DocError> {
        let imp = self.data.import(id).ok_or_else(|| DocError::Other("no such import".into()))?;
        let b = self.blobs.get(&imp.blob).ok_or_else(|| DocError::MissingBlob(imp.blob.clone()))?;
        crate::dxf::DxfDrawing::parse(&String::from_utf8_lossy(&b)).map_err(|e| DocError::Other(e.to_string()))
    }

    /// Build a `PartRef` for a library part, embedding local STEP files as blobs.
    pub fn part_ref(&mut self, info: &PartInfo, size: &str) -> Result<PartRef, DocError> {
        let src = self.library.source(&info.source).cloned().ok_or_else(|| DocError::Other(format!("unknown part source {}", info.source)))?;
        let blob = match src.step_file(&info.id) {
            Some(p) => Some(self.blobs.put(std::fs::read(p)?)),
            None => None,
        };
        let inst = src.build(&info.id, size).map_err(|e| DocError::Other(e.to_string()))?;
        Ok(PartRef {
            source: info.source.clone(),
            id: info.id.clone(),
            size: size.into(),
            name: info.name.clone(),
            part_number: info.part_number.clone(),
            vendor: info.vendor.clone(),
            blob,
            thread: inst.mate.thread.clone(),
        })
    }

    /// Switch the active process profile (undoable); regeneration re-resolves fits.
    pub fn set_active_profile(&mut self, name: &str) -> Result<(), DocError> {
        if self.data.profile_named(name).is_none() {
            return Err(DocError::Other(format!("no process profile named `{name}`")));
        }
        self.edit(&format!("Process: {name}"), |d| {
            d.active_profile = name.into();
            Ok::<_, DocError>(())
        })
    }

    /// Add a process profile, or replace the one with the same name in place (its
    /// position in the list is kept).
    pub fn add_profile(&mut self, p: ProcessProfile) {
        let label = format!("Add process {}", p.name);
        self.update_profile(&label, p)
    }

    /// Replace (in place) or add a process profile, with an undo label.
    pub fn update_profile(&mut self, label: &str, p: ProcessProfile) {
        self.edit(label, |d| {
            match d.processes.iter_mut().find(|q| q.name == p.name) {
                Some(q) => *q = p,
                None => d.processes.push(p),
            }
            Ok::<_, DocError>(())
        })
        .unwrap()
    }

    /// Set the build orientation of one body, or the document default (`None`).
    pub fn set_build_orientation(&mut self, body: Option<BodyId>, o: BuildOrientation) {
        self.edit("Build orientation", |d| {
            match body {
                None => d.build_orientation = o,
                Some(b) => {
                    d.body_orientations.retain(|x| x.body != b);
                    d.body_orientations.push(BodyOrientation { body: b, orientation: o });
                }
            }
            Ok::<_, DocError>(())
        })
        .unwrap()
    }

    /// Regenerate as if `profile` were active (shares the cache; the document is
    /// not modified).
    pub fn regenerate_with_profile(&mut self, profile: &str) -> Result<RegenResult, DocError> {
        if self.data.profile_named(profile).is_none() {
            return Err(DocError::Other(format!("no process profile named `{profile}`")));
        }
        let mut d = self.data.clone();
        d.active_profile = profile.into();
        Ok(self.regen.regenerate(&d, &self.blobs, &self.library))
    }

    /// Export solids (STL/3MF) or all bodies (STEP), optionally under another
    /// process profile and re-oriented so each body's plate face is −Z. Covers the
    /// whole assembly (`Scope::Assembly`: the root's bodies and every visible
    /// occurrence, placed); a flat document exports its bodies as before.
    pub fn export(&mut self, format: ExportFormat, path: &Path, opts: &ExportOptions) -> Result<(), DocError> {
        self.export_scoped(format, path, opts, &crate::assembly::Scope::Assembly)
    }

    /// Placed bodies of a scope (regenerated under `profile` if given; components
    /// with their own `process` keep it).
    pub fn scope_bodies(&mut self, scope: &crate::assembly::Scope, profile: Option<&str>) -> Result<Vec<crate::assembly::ScopeItem>, DocError> {
        use crate::assembly::{AssemblyResult, Scope};
        if let Some(p) = profile {
            if self.data.profile_named(p).is_none() {
                return Err(DocError::Other(format!("no process profile named `{p}`")));
            }
        }
        let flat = self.data.occurrences.is_empty() && matches!(scope, Scope::Root | Scope::Assembly);
        let r = match (profile, flat) {
            (None, true) => AssemblyResult::flat(self.regenerate().clone()),
            (Some(p), true) => AssemblyResult::flat(self.regenerate_with_profile(p)?),
            (None, false) => self.regenerate_assembly().clone(),
            (Some(p), false) => {
                let mut d = self.data.clone();
                d.active_profile = p.into();
                crate::assembly::regenerate_assembly(&d, &self.blobs, &self.library, &mut self.regen, &mut self.asm)
            }
        };
        crate::assembly::scope_items(&self.data, &r, scope, false).map_err(DocError::Other)
    }

    /// [`Document::export`] of one scope. Instances are named "Occ:1 / Sub:2 /
    /// Body". With `orient_to_plate`, each instance is printed from its component
    /// frame: root bodies use their own build orientation, other components the
    /// document default.
    pub fn export_scoped(&mut self, format: ExportFormat, path: &Path, opts: &ExportOptions, scope: &crate::assembly::Scope) -> Result<(), DocError> {
        let items = self.scope_bodies(scope, opts.profile.as_deref())?;
        let mut bodies: Vec<crate::regen::Body> = Vec::new();
        for it in items {
            let keep = match format {
                ExportFormat::Step => opts.include_reference || !it.body.reference,
                _ => !it.body.reference,
            };
            if !keep {
                continue;
            }
            let mut b = it.body.clone();
            if !it.path.is_empty() {
                let mut names: Vec<String> = it.path.iter().filter_map(|o| self.data.occurrence(*o).map(|o| o.name.clone())).collect();
                names.push(b.name.clone());
                b.name = names.join(" / ");
            }
            if opts.orient_to_plate {
                let o = if it.component == crate::assembly::ComponentId::ROOT { self.data.orientation_of(b.id) } else { self.data.build_orientation };
                let (axis, angle) = o.quat().to_axis_angle();
                let mut s = if angle.abs() > 1e-12 { b.shape().rotated(glam::DVec3::ZERO, axis, angle.to_degrees()) } else { Ok(b.shape().clone()) }
                    .map_err(|e| DocError::Other(e.to_string()))?;
                let bb = s.bbox().map_err(|e| DocError::Other(e.to_string()))?;
                s = s.translated(glam::DVec3::new(0.0, 0.0, -bb.min.z)).map_err(|e| DocError::Other(e.to_string()))?;
                b.geom = crate::regen::BodyGeom::new(s);
            } else if !it.transform.is_identity() {
                b.geom = crate::regen::BodyGeom::new(it.placed_shape().map_err(|e| DocError::Other(e.to_string()))?);
            }
            bodies.push(b);
        }
        let refs: Vec<&crate::regen::Body> = bodies.iter().collect();
        let r = match format {
            ExportFormat::Stl => crate::export::write_stl(&refs, path),
            ExportFormat::ThreeMf => crate::export::write_3mf(&refs, path),
            ExportFormat::Step => crate::export::write_step(&refs, path),
        };
        r.map_err(|e| DocError::Other(e.to_string()))
    }

    /// Add an empty drawing (undoable).
    pub fn add_drawing(&mut self, name: &str, sheet: crate::drawing::Sheet) -> crate::drawing::DrawingId {
        self.edit(&format!("Add drawing {name}"), |d| {
            let id = crate::drawing::DrawingId(d.alloc());
            d.drawings.push(crate::drawing::Drawing::new(id, name, sheet));
            Ok::<_, DocError>(id)
        })
        .unwrap()
    }

    /// Undoable change to one drawing.
    pub fn edit_drawing<T>(&mut self, id: crate::drawing::DrawingId, label: &str, f: impl FnOnce(&mut crate::drawing::Drawing) -> T) -> Result<T, DocError> {
        self.edit(label, |d| d.drawing_mut(id).map(f).ok_or_else(|| DocError::Other("no such drawing".into())))
    }

    /// Regenerate and compute a drawing (views, annotations, title block).
    pub fn drawing_result(&mut self, id: crate::drawing::DrawingId) -> Result<crate::drawing::DrawingResult, DocError> {
        let drawing = self.data.drawing(id).cloned().ok_or_else(|| DocError::Other("no such drawing".into()))?;
        let regen = self.regenerate().clone();
        let bom = self.bom();
        use crate::drawing::AnnotationKind as K;
        let asm_needed = !self.data.occurrences.is_empty()
            || drawing.views.iter().any(|v| v.scope != crate::assembly::Scope::Root)
            || drawing.annotations.iter().any(|a| matches!(a.kind, K::ItemBalloon { .. } | K::BomTable { .. }));
        let asm = if asm_needed { Some(self.regenerate_assembly().clone()) } else { None };
        let tree = asm.as_ref().map(|a| self.data.bom_tree(a));
        let src = crate::drawing::DrawingSources { regen: &regen, bom: &bom, assembly: asm.as_ref(), bom_tree: tree.as_ref() };
        Ok(crate::drawing::compute_with(&self.data, &src, &drawing, &mut self.drawing_cache))
    }

    /// Export a drawing as SVG, DXF or PDF (by extension of `path`).
    pub fn export_drawing(&mut self, id: crate::drawing::DrawingId, path: &Path) -> Result<(), DocError> {
        let r = self.drawing_result(id)?;
        let d = self.data.drawing(id).unwrap();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let bytes = match ext.as_str() {
            "svg" => crate::drawing::to_svg(&r, d).into_bytes(),
            "dxf" => crate::drawing::to_dxf(&r, d).into_bytes(),
            "pdf" => crate::drawing::to_pdf(&r, d),
            other => return Err(DocError::Other(format!("unknown drawing format `{other}`"))),
        };
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// Accept the best candidate of an `Unresolved` feature: replaces `old` by `new`
    /// inside the feature (undoable). Returns false if the selector was not found.
    pub fn accept_candidate(&mut self, feature: FeatureId, old: &crate::select::Selector, new: &crate::select::Selector) -> bool {
        self.edit("Re-pick reference", |d| {
            let f = d.feature_mut(feature).ok_or(())?;
            if crate::select::replace_selector(f, old, new) > 0 {
                Ok(())
            } else {
                Err(())
            }
        })
        .is_ok()
    }

    // ---------------------------------------------------------------- regen

    /// Regenerate (cached per feature prefix). Cheap when nothing changed.
    /// The root component's bodies only; placed components are in
    /// [`Document::regenerate_assembly`]. A root with in-context references
    /// regenerates through the assembly.
    pub fn regenerate(&mut self) -> &RegenResult {
        if self.last.is_none() {
            let in_context = self.data.features.iter().any(|f| matches!(f.kind, FeatureKind::ContextRef { .. }));
            let r = if in_context {
                self.regenerate_assembly().component(crate::assembly::ComponentId::ROOT).map(|c| c.regen.clone()).unwrap()
            } else {
                self.regen.regenerate(&self.data, &self.blobs, &self.library)
            };
            self.last = Some(r);
        }
        self.last.as_ref().unwrap()
    }

    pub fn regenerator(&mut self) -> &mut Regenerator {
        &mut self.regen
    }

    /// Bill of materials of purchased parts: one line per (part, size), counting
    /// every placed part body (so patterned/mirrored parts are included) in every
    /// occurrence of the assembly, plus purchased components. See [`DocData::bom`].
    pub fn bom(&mut self) -> Vec<BomLine> {
        if self.data.occurrences.is_empty() {
            let r = crate::assembly::AssemblyResult::flat(self.regenerate().clone());
            return self.data.bom(&r);
        }
        let r = self.regenerate_assembly().clone();
        self.data.bom(&r)
    }

    // ---------------------------------------------------------------- persistence

    pub fn blob_refs(&self) -> Vec<String> {
        self.data.blob_refs()
    }

    /// Write `dir/doc.json` and any missing `dir/blobs/<sha256>`.
    pub fn save_to_dir(&self, dir: &Path) -> Result<(), DocError> {
        let bdir = dir.join(BLOB_DIR);
        std::fs::create_dir_all(&bdir)?;
        for h in self.blob_refs() {
            let p = bdir.join(&h);
            if !p.exists() {
                let data = self.blobs.get(&h).ok_or_else(|| DocError::MissingBlob(h.clone()))?;
                let tmp = bdir.join(format!("{h}.tmp"));
                std::fs::write(&tmp, &*data)?;
                std::fs::rename(&tmp, &p)?;
            }
        }
        let tmp = dir.join("doc.json.tmp");
        std::fs::write(&tmp, self.data.to_json()?)?;
        std::fs::rename(&tmp, dir.join(DOC_FILE))?;
        Ok(())
    }

    pub fn load_from_dir(dir: &Path) -> Result<Document, DocError> {
        let data = DocData::from_json(&std::fs::read_to_string(dir.join(DOC_FILE))?)?;
        let mut blobs = BlobStore::default();
        for h in data.blob_refs() {
            let bytes = std::fs::read(dir.join(BLOB_DIR).join(&h)).map_err(|_| DocError::MissingBlob(h.clone()))?;
            if sha256_hex(&bytes) != h {
                return Err(DocError::Other(format!("blob {h} is corrupt")));
            }
            blobs.put(bytes);
        }
        Ok(Self::with_data(data, blobs))
    }
}

fn default_device() -> String {
    std::env::var("EASYCAD_DEVICE_ID")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "local".into())
}
