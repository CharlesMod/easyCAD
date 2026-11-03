//! Autosave and crash recovery. Every few minutes (Settings → Autosave) a dirty
//! document is copied to `<data dir>/easycad/recovery/<doc id>/` (a normal `.ecad`
//! folder plus `info.json`), off the UI thread. Saving or closing cleanly removes
//! the copy; closing with unsaved changes writes a last one. On the next launch,
//! copies whose process is gone are offered for restore.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use easycad_model::document::Document;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Info {
    pub name: String,
    pub doc_id: String,
    /// Where the document was opened from / saved to, if anywhere.
    pub original: Option<PathBuf>,
    /// Seconds since the Unix epoch.
    pub saved_at: u64,
    pub revision: u64,
    pub pid: u32,
}

/// A recovery copy found on disk.
#[derive(Clone, Debug)]
pub struct Entry {
    pub dir: PathBuf,
    pub info: Info,
}

impl Entry {
    pub fn age_text(&self) -> String {
        let s = now().saturating_sub(self.info.saved_at);
        match s {
            0..=59 => "just now".into(),
            60..=3599 => format!("{} min ago", s / 60),
            3600..=86399 => format!("{} h ago", s / 3600),
            _ => format!("{} days ago", s / 86400),
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `$XDG_DATA_HOME/easycad/recovery` (or the platform equivalent).
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let h = PathBuf::from(h);
                if cfg!(target_os = "macos") {
                    h.join("Library/Application Support")
                } else {
                    h.join(".local/share")
                }
            })
        })?;
    Some(base.join("easycad").join("recovery"))
}

fn entry_dir(doc_id: &str) -> Option<PathBuf> {
    // doc ids are UUIDs; keep the name filesystem-safe whatever they are
    let safe: String = doc_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    Some(dir()?.join(safe))
}

/// Write a recovery copy (blocking). The copy is written next to the old one and
/// swapped in, so a crash mid-write leaves the previous copy intact.
pub fn write(doc: &Document, original: Option<&Path>) -> Result<PathBuf, String> {
    let d = entry_dir(&doc.data.doc_id).ok_or("no data directory")?;
    let tmp = d.with_extension("tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    // blobs (imported STEP files) are content-addressed: link the previous copy's
    // instead of writing them again; save_to_dir writes only the missing ones
    let (old, new) = (d.join("doc.ecad").join("blobs"), tmp.join("doc.ecad").join("blobs"));
    if let Ok(rd) = std::fs::read_dir(&old) {
        let wanted: std::collections::HashSet<String> = doc.blob_refs().into_iter().collect();
        let _ = std::fs::create_dir_all(&new);
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if wanted.contains(&name) && std::fs::hard_link(e.path(), new.join(&name)).is_err() {
                let _ = std::fs::copy(e.path(), new.join(&name));
            }
        }
    }
    doc.save_to_dir(&tmp.join("doc.ecad"))
        .map_err(|e| e.to_string())?;
    let info = Info {
        name: doc.data.name.clone(),
        doc_id: doc.data.doc_id.clone(),
        original: original.map(Path::to_path_buf),
        saved_at: now(),
        revision: doc.data.revision,
        pid: std::process::id(),
    };
    std::fs::write(
        tmp.join("info.json"),
        serde_json::to_string_pretty(&info).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&d);
    std::fs::rename(&tmp, &d).map_err(|e| e.to_string())?;
    Ok(d)
}

pub fn remove(doc_id: &str) {
    if let Some(d) = entry_dir(doc_id) {
        let _ = std::fs::remove_dir_all(&d);
    }
}

pub fn discard(e: &Entry) {
    let _ = std::fs::remove_dir_all(&e.dir);
}

fn alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    if cfg!(target_os = "linux") {
        return Path::new(&format!("/proc/{pid}")).exists();
    }
    // elsewhere: assume another running instance owns it only while it keeps
    // writing (handled by the caller's age check)
    false
}

/// Recovery copies left by a process that is gone (crashed, or closed unsaved).
pub fn orphans() -> Vec<Entry> {
    let Some(root) = dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut v: Vec<Entry> = rd
        .flatten()
        .filter(|e| e.path().extension().is_none())
        .filter_map(|e| {
            let dir = e.path();
            let info: Info =
                serde_json::from_str(&std::fs::read_to_string(dir.join("info.json")).ok()?).ok()?;
            dir.join("doc.ecad").is_dir().then_some(Entry { dir, info })
        })
        .filter(|e| !alive(e.info.pid))
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.info.saved_at));
    v
}

pub fn load(e: &Entry) -> Result<Document, String> {
    Document::load_from_dir(&e.dir.join("doc.ecad")).map_err(|e| e.to_string())
}

/// Periodic autosave state (lives in the app).
#[derive(Default)]
pub struct Autosaver {
    /// Revision of the last copy written (or of the document when it was clean).
    pub saved_rev: Option<u64>,
    pub last_at: f64,
    job: Option<Receiver<Result<PathBuf, String>>>,
    pub last_error: Option<String>,
    pub last_path: Option<PathBuf>,
}

impl Autosaver {
    /// Call every frame. `interval_s` 0 = off. Returns true when a write finished.
    pub fn tick(
        &mut self,
        now: f64,
        interval_s: f64,
        doc: &Document,
        dirty: bool,
        original: Option<&Path>,
    ) -> bool {
        let mut finished = false;
        if let Some(rx) = &self.job {
            if let Ok(r) = rx.try_recv() {
                match r {
                    Ok(p) => {
                        self.last_path = Some(p);
                        self.last_error = None;
                    }
                    Err(e) => self.last_error = Some(e),
                }
                self.job = None;
                finished = true;
            }
        }
        if interval_s <= 0.0 || self.job.is_some() || !dirty {
            return finished;
        }
        if self.saved_rev == Some(doc.data.revision) || now - self.last_at < interval_s {
            return finished;
        }
        self.last_at = now;
        self.saved_rev = Some(doc.data.revision);
        let mut copy = Document::new(&doc.data.name);
        copy.data = doc.data.clone();
        copy.blobs = doc.blobs.clone();
        let original = original.map(Path::to_path_buf);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(write(&copy, original.as_deref()));
        });
        self.job = Some(rx);
        finished
    }

    /// Wait for a running write (exit path).
    pub fn flush(&mut self) {
        if let Some(rx) = self.job.take() {
            let _ = rx.recv();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_find_restore_discard() {
        let base = std::env::temp_dir().join(format!("cw-recovery-{}", std::process::id()));
        std::env::set_var("XDG_DATA_HOME", &base);
        let mut doc = Document::new("box");
        doc.set_param("wall", "2.5").unwrap();
        let d = write(&doc, Some(Path::new("/tmp/box.ecad"))).unwrap();
        assert!(d.join("doc.ecad").is_dir());
        // our own copy is not an orphan
        assert!(orphans().iter().all(|e| e.info.doc_id != doc.data.doc_id));
        // pretend it was left by a dead process
        let mut info: Info =
            serde_json::from_str(&std::fs::read_to_string(d.join("info.json")).unwrap()).unwrap();
        info.pid = u32::MAX - 7;
        std::fs::write(d.join("info.json"), serde_json::to_string(&info).unwrap()).unwrap();
        let o = orphans();
        let e = o.iter().find(|e| e.info.doc_id == doc.data.doc_id).expect("orphan");
        assert_eq!(e.info.original.as_deref(), Some(Path::new("/tmp/box.ecad")));
        let back = load(e).unwrap();
        assert_eq!(back.data.params.get("wall").map(|p| p.expr.0.clone()), Some("2.5".into()));
        discard(e);
        assert!(orphans().is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
