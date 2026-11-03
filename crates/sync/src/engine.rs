//! The sync engine: one `sync_once()` pass scans each root, lists the remote,
//! plans three-way and runs the plan in phases, re-checking each side just
//! before it changes it. Local-first: nothing local is lost, a delete is a
//! move to `.cwsync-trash` locally and to the Drive trash remotely.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::config::{abs_path, PathFilter, RootConfig, SyncConfig};
use crate::error::{RemoteError, SyncError};
use crate::events::{ConflictInfo, ErrorInfo, EventSink, Progress, SyncEvent, SyncReport, SyncStatus, TransferKind};
use crate::local::{self, blob_doc, doc_json_dir, LocalFile};
use crate::plan::{self, Action};
use crate::remote::{FileMeta, RemoteFile, RemoteStore, UploadRequest};
use crate::state::{Entry, Pending, RootState, StateDb};

pub struct Engine {
    remote: Arc<dyn RemoteStore>,
    cfg: SyncConfig,
    state: StateDb,
    events: EventSink,
}

/// Why an action did not run this pass.
enum Skip {
    /// The file changed while syncing; the next pass picks it up.
    Deferred,
    Failed(RemoteError),
}

impl From<RemoteError> for Skip {
    fn from(e: RemoteError) -> Self {
        Skip::Failed(e)
    }
}

impl From<std::io::Error> for Skip {
    fn from(e: std::io::Error) -> Self {
        Skip::Failed(RemoteError::Io(e))
    }
}

/// One root's pass in progress.
struct Pass<'a> {
    root: &'a RootConfig,
    st: RootState,
    /// remote files by path, kept current as the pass uploads
    remote: BTreeMap<String, RemoteFile>,
    report: SyncReport,
    /// docs (`X.ecad`) whose blobs failed: their `doc.json` waits
    failed_docs: HashSet<String>,
}

fn entry(l: &LocalFile, r: &RemoteFile) -> Entry {
    Entry {
        remote_id: r.id.clone(),
        remote_md5: r.md5.clone(),
        remote_rev: r.head_revision_id.clone(),
        remote_version: r.version,
        local_md5: l.md5.clone(),
        local_size: l.size,
        local_mtime_ns: l.mtime_ns,
    }
}

fn now_local(p: &Path) -> Option<LocalFile> {
    let (size, mtime_ns) = local::stat(p)?;
    Some(LocalFile { size, mtime_ns, md5: local::md5_file(p).ok()? })
}

/// Whether `p` still has the size and mtime it was planned with.
fn unchanged(p: &Path, l: &LocalFile) -> bool {
    local::stat(p) == Some((l.size, l.mtime_ns))
}

impl Engine {
    pub fn new(remote: Arc<dyn RemoteStore>, cfg: SyncConfig, events: EventSink) -> Engine {
        let state = StateDb::load(&cfg.state_path);
        Engine { remote, cfg, state, events }
    }

    pub fn config(&self) -> &SyncConfig {
        &self.cfg
    }

    pub fn state(&self) -> &StateDb {
        &self.state
    }

    /// Whether the remote change feed has anything new since the last pass
    /// (true when there is no token yet).
    pub async fn remote_changed(&mut self) -> Result<bool, SyncError> {
        let Some(t) = self.state.page_token.clone() else { return Ok(true) };
        let cs = self.remote.changes_since(&t).await?;
        self.state.page_token = Some(cs.new_token);
        self.state.save()?;
        Ok(cs.changed > 0)
    }

    /// One full pass over every root. Offline and auth errors end the pass
    /// (state so far is saved); other errors are per root or per file and
    /// land in the report.
    pub async fn sync_once(&mut self) -> Result<SyncReport, SyncError> {
        self.events.emit(SyncEvent::Status(SyncStatus::Syncing));
        // taken first, so changes made by others during the pass show up next poll
        let token = self.remote.start_page_token().await?;
        let mut report = SyncReport::default();
        for root in self.cfg.roots.clone() {
            match self.sync_root(&root).await {
                Ok(r) => report.merge(r),
                Err(e) if e.is_offline() || e.is_auth() => {
                    self.state.save()?;
                    return Err(e);
                }
                Err(e) => {
                    let info = ErrorInfo { root: Some(root.name.clone()), path: None, message: e.to_string() };
                    self.events.emit(SyncEvent::Error(info.clone()));
                    report.errors.push(info);
                }
            }
        }
        self.state.page_token = Some(token);
        self.state.save()?;
        self.events.emit(SyncEvent::Completed(report.clone()));
        self.events.emit(SyncEvent::Status(SyncStatus::Idle { last_success: Some(chrono::Utc::now()) }));
        Ok(report)
    }

    async fn sync_root(&mut self, root: &RootConfig) -> Result<SyncReport, SyncError> {
        let mut total = SyncReport::default();
        // a second round uploads the conflict copies the first one wrote
        for _ in 0..2 {
            let r = self.root_round(root).await?;
            let again = !r.conflicts.is_empty();
            total.merge(r);
            if !again {
                break;
            }
        }
        Ok(total)
    }

    async fn root_round(&mut self, root: &RootConfig) -> Result<SyncReport, SyncError> {
        let rid = self.remote.root_id(&root.name).await?;
        let mut st = self.state.roots.remove(&root.name).unwrap_or_default();
        if st.remote_root_id.as_deref() != Some(rid.as_str()) {
            if st.remote_root_id.is_some() {
                tracing::warn!("sync root {} has a new Drive folder; starting its base over", root.name);
            }
            st.entries.clear();
            st.pending.clear();
            st.remote_root_id = Some(rid);
        }
        if !root.local.is_dir() {
            if !st.entries.is_empty() {
                self.state.roots.insert(root.name.clone(), st);
                return Err(SyncError::RootMissing(root.local.clone()));
            }
            std::fs::create_dir_all(&root.local)?;
        }
        let filter = PathFilter::new(root);
        let scanned = local::scan(&root.local, &filter, &mut st.stats);
        let localf = match scanned {
            Ok(l) => l,
            Err(e) => {
                self.state.roots.insert(root.name.clone(), st);
                return Err(e.into());
            }
        };
        let listed = match self.remote.list(&root.name).await {
            Ok(l) => l,
            Err(e) => {
                self.state.roots.insert(root.name.clone(), st);
                return Err(e.into());
            }
        };
        let remote = plan::index_remote(listed.into_iter().filter(|f| filter.accepts(&f.path)).collect(), &st.entries);
        let actions = plan::plan(&localf, &st.entries, &remote);

        let del_remote = actions.iter().filter(|a| matches!(a, Action::TrashRemote { .. })).count();
        let del_local = actions.iter().filter(|a| matches!(a, Action::DeleteLocal { .. })).count();
        let max = self.cfg.max_blind_deletes;
        let stop = if localf.is_empty() && del_remote > max {
            Some(format!("{} is empty and {del_remote} files would be trashed on Drive", root.local.display()))
        } else if remote.is_empty() && del_local > max {
            Some(format!("easyCAD/{} on Drive is empty and {del_local} local files would be removed", root.name))
        } else {
            None
        };
        if let Some(why) = stop {
            self.state.roots.insert(root.name.clone(), st);
            return Err(SyncError::SafetyStop(why));
        }

        let mut pass = Pass { root, st, remote, report: SyncReport::default(), failed_docs: HashSet::new() };
        let total = actions.iter().filter(|a| a.is_transfer()).count();
        let mut index = 0;
        let mut result = Ok(());
        for a in actions {
            if a.is_transfer() {
                index += 1;
            }
            let path = a.path().to_string();
            match self.run(&mut pass, a, index, total).await {
                Ok(()) => {}
                Err(Skip::Deferred) => pass.report.deferred += 1,
                Err(Skip::Failed(e)) if e.is_fatal_for_pass() => {
                    result = Err(SyncError::Remote(e));
                    break;
                }
                Err(Skip::Failed(e)) => {
                    if let Some((doc, _)) = blob_doc(&path) {
                        pass.failed_docs.insert(doc.to_string());
                    }
                    let info = ErrorInfo { root: Some(root.name.clone()), path: Some(path), message: e.to_string() };
                    self.events.emit(SyncEvent::Error(info.clone()));
                    pass.report.errors.push(info);
                }
            }
            if index % 20 == 0 {
                self.state.roots.insert(root.name.clone(), pass.st.clone());
                self.state.save()?;
            }
        }
        self.state.roots.insert(root.name.clone(), pass.st);
        self.state.save()?;
        result.map(|()| pass.report)
    }

    /// The start of an action (byte progress comes from the transfer itself).
    fn progress(&self, root: &str, path: &str, kind: TransferKind, index: usize, total: usize, all: u64) {
        self.events.emit(SyncEvent::Progress(Progress {
            root: root.into(),
            path: path.into(),
            kind,
            index,
            total,
            bytes_done: 0,
            bytes_total: all,
        }));
    }

    async fn run(&mut self, pass: &mut Pass<'_>, a: Action, index: usize, total: usize) -> Result<(), Skip> {
        let root = pass.root;
        match a {
            Action::Record { path, local, remote } => {
                pass.st.entries.insert(path, entry(&local, &remote));
                Ok(())
            }
            Action::Forget { path } => {
                pass.st.entries.remove(&path);
                Ok(())
            }
            Action::Upload { path, local, existing } => {
                self.progress(&root.name, &path, TransferKind::Upload, index, total, local.size);
                self.upload(pass, &path, &local, existing, index, total).await
            }
            Action::Download { path, remote, local } => {
                if doc_json_dir(&path).is_some_and(|d| pass.failed_docs.contains(d)) {
                    return Err(Skip::Deferred);
                }
                self.progress(&root.name, &path, TransferKind::Download, index, total, remote.size);
                let abs = abs_path(&root.local, &path);
                match &local {
                    Some(l) if !unchanged(&abs, l) => return Err(Skip::Deferred),
                    None if abs.exists() => return Err(Skip::Deferred),
                    _ => {}
                }
                self.fetch(&remote, &abs, &path, index, total).await?;
                let now = now_local(&abs).ok_or(Skip::Deferred)?;
                pass.st.entries.insert(path, entry(&now, &remote));
                pass.report.downloaded += 1;
                Ok(())
            }
            Action::DeleteLocal { path, local } => {
                self.progress(&root.name, &path, TransferKind::DeleteLocal, index, total, 0);
                if !unchanged(&abs_path(&root.local, &path), &local) {
                    return Err(Skip::Deferred);
                }
                local::trash_local(&root.local, &path)?;
                pass.st.entries.remove(&path);
                pass.report.deleted_local += 1;
                Ok(())
            }
            Action::TrashRemote { path, remote } => {
                self.progress(&root.name, &path, TransferKind::DeleteRemote, index, total, 0);
                match self.remote.get(&remote.id).await? {
                    Some(cur) if cur.md5 != remote.md5 => return Err(Skip::Deferred),
                    _ => {}
                }
                self.remote.trash(&remote.id).await?;
                pass.st.entries.remove(&path);
                pass.remote.remove(&path);
                pass.report.deleted_remote += 1;
                Ok(())
            }
            Action::ConflictFile { path, remote } => {
                self.progress(&root.name, &path, TransferKind::ConflictCopy, index, total, remote.size);
                let copy = plan::conflict_name(&path, remote.meta.device.as_deref().unwrap_or("remote"), chrono::Local::now());
                let copy_abs = abs_path(&root.local, &copy);
                self.fetch(&remote, &copy_abs, &path, index, total).await?;
                self.conflict(pass, &path, &copy, &remote);
                Ok(())
            }
            Action::ConflictDoc { doc, doc_json } => {
                self.progress(&root.name, &doc, TransferKind::ConflictCopy, index, total, doc_json.size);
                let copy = plan::conflict_name(&doc, doc_json.meta.device.as_deref().unwrap_or("remote"), chrono::Local::now());
                let prefix = format!("{doc}/");
                let parts: Vec<RemoteFile> =
                    pass.remote.values().filter(|f| f.path.starts_with(&prefix)).cloned().collect();
                for f in parts {
                    let rest = &f.path[prefix.len()..];
                    let dest = abs_path(&root.local, &format!("{copy}/{rest}"));
                    // a blob we already have locally is copied, not downloaded
                    let same_here = abs_path(&root.local, &f.path);
                    let have = blob_doc(&f.path).is_some()
                        && pass.st.stats.get(&f.path).is_some_and(|s| Some(&s.md5) == f.md5.as_ref());
                    if have {
                        if let Some(d) = dest.parent() {
                            std::fs::create_dir_all(d)?;
                        }
                        std::fs::copy(&same_here, &dest)?;
                    } else {
                        self.fetch(&f, &dest, &f.path, index, total).await?;
                    }
                }
                self.conflict(pass, &doc, &copy, &doc_json);
                Ok(())
            }
        }
    }

    fn conflict(&self, pass: &mut Pass<'_>, path: &str, copy: &str, remote: &RemoteFile) {
        let info = ConflictInfo {
            root: pass.root.name.clone(),
            path: path.into(),
            conflict_copy: copy.into(),
            local_abs: abs_path(&pass.root.local, path),
            conflict_copy_abs: abs_path(&pass.root.local, copy),
            remote_device: remote.meta.device.clone(),
            remote_modified: remote.modified_time,
            doc_id: remote.meta.doc_id.clone(),
        };
        self.events.emit(SyncEvent::Conflict(info.clone()));
        pass.report.conflicts.push(info);
    }

    /// Download to a temp file beside `dest`, verify md5 (and the sha256 a blob
    /// is named by), then rename into place.
    async fn fetch(&self, r: &RemoteFile, dest: &Path, path: &str, index: usize, total: usize) -> Result<(), Skip> {
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = local::temp_path(dest);
        let ev = self.events.clone();
        let (root, p) = (r.root.clone(), path.to_string());
        let prog = move |done: u64, all: u64| {
            ev.emit(SyncEvent::Progress(Progress {
                root: root.clone(),
                path: p.clone(),
                kind: TransferKind::Download,
                index,
                total,
                bytes_done: done,
                bytes_total: all,
            }))
        };
        let got = self.remote.download(r, &tmp, &prog).await;
        let check = got.map_err(Skip::from).and_then(|()| {
            if let Some(want) = &r.md5 {
                if &local::md5_file(&tmp)? != want {
                    return Err(Skip::Failed(RemoteError::Other(format!("{path}: downloaded file has the wrong md5"))));
                }
            }
            if let Some((_, sha)) = blob_doc(path) {
                if !local::sha256_file(&tmp)?.eq_ignore_ascii_case(sha) {
                    return Err(Skip::Failed(RemoteError::Other(format!("{path}: blob does not match its name"))));
                }
            }
            Ok(())
        });
        if let Err(e) = check {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        std::fs::rename(&tmp, dest)?;
        Ok(())
    }

    async fn upload(
        &mut self,
        pass: &mut Pass<'_>,
        path: &str,
        l: &LocalFile,
        existing: Option<RemoteFile>,
        index: usize,
        total: usize,
    ) -> Result<(), Skip> {
        let root = pass.root;
        if doc_json_dir(path).is_some_and(|d| pass.failed_docs.contains(d)) {
            return Err(Skip::Deferred);
        }
        let abs = abs_path(&root.local, path);
        if !unchanged(&abs, l) {
            return Err(Skip::Deferred);
        }
        if let Some(e) = &existing {
            match self.remote.get(&e.id).await? {
                Some(cur) if cur.md5 == e.md5 => {}
                _ => return Err(Skip::Deferred),
            }
        }
        let meta = file_meta(&self.cfg.device, path, &abs);
        let req = UploadRequest {
            root: root.name.clone(),
            path: path.into(),
            local_path: abs.clone(),
            existing_id: existing.as_ref().map(|e| e.id.clone()),
            size: l.size,
            meta: meta.clone(),
        };
        let twin = (existing.is_none() && l.size > 0)
            .then(|| pass.remote.values().find(|f| f.md5.as_deref() == Some(l.md5.as_str())).cloned())
            .flatten();
        let got = if let Some(src) = twin {
            let f = self.remote.copy(&src.id, &root.name, path, &meta).await?;
            pass.report.copied_remote += 1;
            f
        } else if l.size > self.remote.resumable_threshold() {
            let f = self.resumable(pass, &req, l, index, total).await?;
            pass.report.uploaded += 1;
            f
        } else {
            let f = self.remote.upload(&req).await?;
            pass.report.uploaded += 1;
            f
        };
        pass.st.entries.insert(path.into(), entry(l, &got));
        pass.remote.insert(path.into(), got);
        Ok(())
    }

    async fn resumable(
        &mut self,
        pass: &mut Pass<'_>,
        req: &UploadRequest,
        l: &LocalFile,
        index: usize,
        total: usize,
    ) -> Result<RemoteFile, Skip> {
        let ev = self.events.clone();
        let (root, p) = (req.root.clone(), req.path.clone());
        let prog = move |done: u64, all: u64| {
            ev.emit(SyncEvent::Progress(Progress {
                root: root.clone(),
                path: p.clone(),
                kind: TransferKind::Upload,
                index,
                total,
                bytes_done: done,
                bytes_total: all,
            }))
        };
        for _ in 0..2 {
            let session = match pass.st.pending.get(&req.path) {
                Some(s) if s.size == l.size && s.md5 == l.md5 => s.session.clone(),
                _ => {
                    let s = self.remote.begin_resumable(req).await?;
                    pass.st.pending.insert(req.path.clone(), Pending { session: s.clone(), size: l.size, md5: l.md5.clone() });
                    // persisted at once so a crash or restart resumes it
                    self.state.roots.insert(pass.root.name.clone(), pass.st.clone());
                    self.state.save().map_err(|e| Skip::Failed(RemoteError::Other(e.to_string())))?;
                    s
                }
            };
            match self.remote.resume_upload(&session, req, &prog).await {
                Ok(f) => {
                    pass.st.pending.remove(&req.path);
                    return Ok(f);
                }
                Err(RemoteError::SessionExpired) => {
                    pass.st.pending.remove(&req.path);
                }
                Err(e) => return Err(Skip::Failed(e)),
            }
        }
        Err(Skip::Failed(RemoteError::SessionExpired))
    }
}

/// `cw_dev`, plus `cw_doc`/`cw_rev` read from a `doc.json`.
fn file_meta(device: &str, path: &str, abs: &Path) -> FileMeta {
    let mut m = FileMeta { device: Some(device.into()), ..Default::default() };
    if doc_json_dir(path).is_some() {
        if let Some(v) = std::fs::read(abs).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) {
            m.doc_id = v["doc_id"].as_str().map(String::from);
            m.revision = v["revision"].as_i64();
        }
    }
    m
}
