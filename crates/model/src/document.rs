//! The document: parameters, sketches, features and imports (the serializable
//! [`DocData`]), plus undo/redo snapshots, content-addressed blobs and the
//! regeneration cache.
//!
//! On disk a document is a **folder** `<name>.cwk/`:
//! ```text
//! Enclosure.cwk/
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
use crate::regen::{RegenResult, Regenerator};
use crate::sketch::Sketch;

pub const FORMAT_VERSION: u32 = 1;
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
    pub next_id: u32,
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
            next_id: 1,
        }
    }

    fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn sketch(&self, id: SketchId) -> Option<&Sketch> {
        self.sketches.iter().find(|s| s.id == id).map(|s| &s.sketch)
    }
    pub fn sketch_mut(&mut self, id: SketchId) -> Option<&mut Sketch> {
        self.sketches.iter_mut().find(|s| s.id == id).map(|s| &mut s.sketch)
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
        self.sketches.push(SketchEntry { id, sketch });
        id
    }

    pub fn add_feature(&mut self, name: &str, kind: FeatureKind) -> FeatureId {
        let id = FeatureId(self.alloc());
        self.features.push(Feature { id, name: name.into(), suppressed: false, kind });
        id
    }

    /// Hashes of every blob the document references.
    pub fn blob_refs(&self) -> Vec<String> {
        let mut v: Vec<String> = self.imports.iter().map(|i| i.import.blob.clone()).collect();
        for f in &self.features {
            if let FeatureKind::InsertPart { part: PartRef { blob: Some(b), .. }, .. } = &f.kind {
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
        let d: DocData = serde_json::from_str(s)?;
        if d.format > FORMAT_VERSION {
            return Err(DocError::Format(d.format));
        }
        Ok(d)
    }
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
    regen: Regenerator,
    last: Option<RegenResult>,
}

impl Document {
    pub fn new(name: &str) -> Self {
        Self::with_data(DocData::new(name, &default_device()), BlobStore::default())
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
        }
    }

    fn touch(&mut self) {
        self.data.revision += 1;
        self.data.modified_by_device = self.device_id.clone();
        self.last = None;
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
    pub fn regenerate(&mut self) -> &RegenResult {
        if self.last.is_none() {
            self.last = Some(self.regen.regenerate(&self.data, &self.blobs, &self.library));
        }
        self.last.as_ref().unwrap()
    }

    pub fn regenerator(&mut self) -> &mut Regenerator {
        &mut self.regen
    }

    /// Bill of materials from `InsertPart` features (suppressed ones excluded).
    pub fn bom(&self) -> Vec<BomLine> {
        let mut lines: Vec<BomLine> = Vec::new();
        for f in self.data.features.iter().filter(|f| !f.suppressed) {
            if let FeatureKind::InsertPart { part, placements, .. } = &f.kind {
                match lines.iter_mut().find(|l| l.source == part.source && l.part == part.id && l.size == part.size) {
                    Some(l) => l.qty += placements.len(),
                    None => lines.push(BomLine {
                        source: part.source.clone(),
                        part: part.id.clone(),
                        name: part.name.clone(),
                        size: part.size.clone(),
                        qty: placements.len(),
                        vendor: part.vendor.clone(),
                        part_number: part.part_number.clone(),
                    }),
                }
            }
        }
        lines.sort_by(|a, b| (&a.name, &a.size).cmp(&(&b.name, &b.size)));
        lines
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
    std::env::var("CASEWORK_DEVICE_ID")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "local".into())
}
