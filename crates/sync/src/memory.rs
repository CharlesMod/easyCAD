//! An in-memory [`RemoteStore`] for tests: files by id, an offline switch,
//! a mid-upload interruption, transfer statistics, and helpers to edit the
//! "remote" behind the engine's back. Clone it to share one store between
//! two engines (two devices).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use md5::{Digest, Md5};

use crate::error::RemoteError;
use crate::remote::{ChangeSet, FileMeta, ProgressFn, RemoteFile, RemoteStore, UploadRequest};

/// What crossed the "network".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub uploads: usize,
    pub upload_bytes: u64,
    pub downloads: usize,
    pub copies: usize,
    pub trashes: usize,
    pub sessions_started: usize,
}

struct Session {
    req: UploadRequest,
    received: Vec<u8>,
}

#[derive(Default)]
struct Inner {
    files: HashMap<String, (RemoteFile, Vec<u8>)>,
    trashed: HashMap<String, (RemoteFile, Vec<u8>)>,
    roots: HashMap<String, String>,
    sessions: HashMap<String, Session>,
    next: u64,
    offline: bool,
    /// Fail a resumable upload (as offline) once this many bytes arrived.
    interrupt_at: Option<u64>,
    stats: Stats,
    change: u64,
    log: Vec<crate::remote::Change>,
}

#[derive(Clone)]
pub struct MemoryRemote {
    inner: Arc<Mutex<Inner>>,
    threshold: u64,
    chunk: usize,
}

impl Default for MemoryRemote {
    fn default() -> Self {
        MemoryRemote::new()
    }
}

fn md5_hex(b: &[u8]) -> String {
    hex::encode(Md5::digest(b))
}

impl MemoryRemote {
    pub fn new() -> MemoryRemote {
        MemoryRemote { inner: Arc::default(), threshold: 5 * 1024 * 1024, chunk: 64 * 1024 }
    }

    /// Use resumable uploads above `bytes` (tests use small files).
    pub fn with_resumable_threshold(mut self, bytes: u64, chunk: usize) -> Self {
        self.threshold = bytes;
        self.chunk = chunk.max(1);
        self
    }

    pub fn set_offline(&self, off: bool) {
        self.inner.lock().unwrap().offline = off;
    }

    /// The next resumable upload stops (offline) after `bytes` bytes.
    pub fn interrupt_upload_at(&self, bytes: u64) {
        self.inner.lock().unwrap().interrupt_at = Some(bytes);
    }

    pub fn stats(&self) -> Stats {
        self.inner.lock().unwrap().stats.clone()
    }

    pub fn reset_stats(&self) {
        self.inner.lock().unwrap().stats = Stats::default();
    }

    /// Paths of live files in `root`, sorted.
    pub fn paths(&self, root: &str) -> Vec<String> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<String> = g.files.values().filter(|(f, _)| f.root == root).map(|(f, _)| f.path.clone()).collect();
        v.sort();
        v
    }

    pub fn content(&self, root: &str, path: &str) -> Option<Vec<u8>> {
        let g = self.inner.lock().unwrap();
        g.files.values().find(|(f, _)| f.root == root && f.path == path).map(|(_, b)| b.clone())
    }

    pub fn file(&self, root: &str, path: &str) -> Option<RemoteFile> {
        let g = self.inner.lock().unwrap();
        g.files.values().find(|(f, _)| f.root == root && f.path == path).map(|(f, _)| f.clone())
    }

    /// Write a file as another client would (new, or in place).
    pub fn put(&self, root: &str, path: &str, bytes: &[u8], device: &str) {
        let mut g = self.inner.lock().unwrap();
        let existing = g.files.values().find(|(f, _)| f.root == root && f.path == path).map(|(f, _)| f.id.clone());
        let meta = FileMeta { device: Some(device.into()), ..Default::default() };
        store(&mut g, existing, root, path, bytes.to_vec(), meta);
    }

    /// Trash a file as another client would.
    pub fn remove(&self, root: &str, path: &str) {
        let mut g = self.inner.lock().unwrap();
        if let Some(id) = g.files.values().find(|(f, _)| f.root == root && f.path == path).map(|(f, _)| f.id.clone()) {
            let f = g.files.remove(&id).unwrap();
            g.trashed.insert(id.clone(), f);
            g.change += 1;
            g.log.push(crate::remote::Change { id, removed: true, version: None, folder: false });
        }
    }

    /// Forget a root's folder (as if it was deleted and recreated): its files go
    /// and it gets a new id.
    pub fn recreate_root(&self, root: &str) {
        let mut g = self.inner.lock().unwrap();
        g.roots.remove(root);
        let gone: Vec<String> = g.files.values().filter(|(f, _)| f.root == root).map(|(f, _)| f.id.clone()).collect();
        g.files.retain(|_, (f, _)| f.root != root);
        for id in gone {
            g.change += 1;
            g.log.push(crate::remote::Change { id, removed: true, version: None, folder: false });
        }
    }

    fn check(&self) -> Result<std::sync::MutexGuard<'_, Inner>, RemoteError> {
        let g = self.inner.lock().unwrap();
        if g.offline {
            return Err(RemoteError::Offline("memory remote is offline".into()));
        }
        Ok(g)
    }
}

fn store(g: &mut Inner, id: Option<String>, root: &str, path: &str, bytes: Vec<u8>, meta: FileMeta) -> RemoteFile {
    let id = id.unwrap_or_else(|| {
        g.next += 1;
        format!("f{}", g.next)
    });
    let version = g.files.get(&id).and_then(|(f, _)| f.version).unwrap_or(0) + 1;
    g.change += 1;
    let f = RemoteFile {
        id: id.clone(),
        root: root.into(),
        path: path.into(),
        size: bytes.len() as u64,
        md5: Some(md5_hex(&bytes)),
        head_revision_id: Some(format!("r{}", g.change)),
        version: Some(version),
        modified_time: Some(chrono::Utc::now()),
        meta,
    };
    g.files.insert(id.clone(), (f.clone(), bytes));
    g.log.push(crate::remote::Change { id, removed: false, version: Some(version), folder: false });
    f
}

#[async_trait]
impl RemoteStore for MemoryRemote {
    async fn root_id(&self, root: &str) -> Result<String, RemoteError> {
        let mut g = self.check()?;
        if let Some(id) = g.roots.get(root) {
            return Ok(id.clone());
        }
        g.next += 1;
        let id = format!("root{}", g.next);
        g.roots.insert(root.into(), id.clone());
        Ok(id)
    }

    async fn list(&self, root: &str) -> Result<Vec<RemoteFile>, RemoteError> {
        let g = self.check()?;
        Ok(g.files.values().filter(|(f, _)| f.root == root).map(|(f, _)| f.clone()).collect())
    }

    async fn get(&self, id: &str) -> Result<Option<RemoteFile>, RemoteError> {
        let g = self.check()?;
        Ok(g.files.get(id).map(|(f, _)| f.clone()))
    }

    async fn download(&self, file: &RemoteFile, dest: &Path, progress: ProgressFn<'_>) -> Result<(), RemoteError> {
        let bytes = {
            let mut g = self.check()?;
            g.stats.downloads += 1;
            g.files.get(&file.id).map(|(_, b)| b.clone()).ok_or(RemoteError::NotFound)?
        };
        std::fs::write(dest, &bytes)?;
        progress(bytes.len() as u64, bytes.len() as u64);
        Ok(())
    }

    fn resumable_threshold(&self) -> u64 {
        self.threshold
    }

    async fn upload(&self, req: &UploadRequest) -> Result<RemoteFile, RemoteError> {
        let bytes = std::fs::read(&req.local_path)?;
        let mut g = self.check()?;
        if let Some(id) = &req.existing_id {
            if !g.files.contains_key(id) {
                return Err(RemoteError::NotFound);
            }
        }
        g.stats.uploads += 1;
        g.stats.upload_bytes += bytes.len() as u64;
        Ok(store(&mut g, req.existing_id.clone(), &req.root, &req.path, bytes, req.meta.clone()))
    }

    async fn begin_resumable(&self, req: &UploadRequest) -> Result<String, RemoteError> {
        let mut g = self.check()?;
        g.next += 1;
        g.stats.sessions_started += 1;
        let id = format!("session{}", g.next);
        g.sessions.insert(id.clone(), Session { req: req.clone(), received: Vec::new() });
        Ok(id)
    }

    async fn resume_upload(
        &self,
        session: &str,
        req: &UploadRequest,
        progress: ProgressFn<'_>,
    ) -> Result<RemoteFile, RemoteError> {
        let bytes = std::fs::read(&req.local_path)?;
        let mut g = self.check()?;
        let interrupt = g.interrupt_at;
        let s = g.sessions.get_mut(session).ok_or(RemoteError::SessionExpired)?;
        if bytes.len() as u64 != s.req.size {
            return Err(RemoteError::SessionExpired);
        }
        let mut sent = 0u64;
        while s.received.len() < bytes.len() {
            let start = s.received.len();
            let end = (start + self.chunk).min(bytes.len());
            s.received.extend_from_slice(&bytes[start..end]);
            sent += (end - start) as u64;
            progress(s.received.len() as u64, bytes.len() as u64);
            if interrupt.is_some_and(|at| s.received.len() as u64 >= at) && s.received.len() < bytes.len() {
                g.interrupt_at = None;
                g.stats.upload_bytes += sent;
                return Err(RemoteError::Offline("connection dropped mid-upload".into()));
            }
        }
        let s = g.sessions.remove(session).unwrap();
        g.stats.uploads += 1;
        g.stats.upload_bytes += sent;
        Ok(store(&mut g, s.req.existing_id.clone(), &s.req.root, &s.req.path, s.received, s.req.meta.clone()))
    }

    async fn copy(&self, src_id: &str, root: &str, path: &str, meta: &FileMeta) -> Result<RemoteFile, RemoteError> {
        let mut g = self.check()?;
        let bytes = g.files.get(src_id).map(|(_, b)| b.clone()).ok_or(RemoteError::NotFound)?;
        g.stats.copies += 1;
        Ok(store(&mut g, None, root, path, bytes, meta.clone()))
    }

    async fn trash(&self, id: &str) -> Result<(), RemoteError> {
        let mut g = self.check()?;
        if let Some(f) = g.files.remove(id) {
            g.trashed.insert(id.into(), f);
            g.stats.trashes += 1;
            g.change += 1;
            g.log.push(crate::remote::Change { id: id.into(), removed: true, version: None, folder: false });
        }
        Ok(())
    }

    async fn start_page_token(&self) -> Result<String, RemoteError> {
        Ok(self.check()?.log.len().to_string())
    }

    async fn changes_since(&self, token: &str) -> Result<ChangeSet, RemoteError> {
        let g = self.check()?;
        let from: u64 = token.parse().unwrap_or(0);
        // the log has one entry per change, so a token is an index into it
        let changes: Vec<_> = g.log.iter().skip(from as usize).cloned().collect();
        Ok(ChangeSet { changed: changes.len(), new_token: g.log.len().to_string(), changes })
    }
}
