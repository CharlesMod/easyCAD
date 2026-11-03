//! The state database: per root and path, what both sides looked like at the
//! last successful sync (the "base" of the three-way plan), a stat cache so
//! big files are not rehashed, pending resumable sessions, the root folder id
//! and the change-feed token. One JSON file, written atomically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::SyncError;

/// One path as of the last sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub remote_id: String,
    pub remote_md5: Option<String>,
    #[serde(default)]
    pub remote_rev: Option<String>,
    #[serde(default)]
    pub remote_version: Option<i64>,
    pub local_md5: String,
    pub local_size: u64,
    pub local_mtime_ns: i128,
}

/// Hash of a local file as of a given size and mtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    pub size: u64,
    pub mtime_ns: i128,
    pub md5: String,
}

/// A resumable upload that has not finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub session: String,
    pub size: u64,
    pub md5: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootState {
    #[serde(default)]
    pub remote_root_id: Option<String>,
    #[serde(default)]
    pub entries: BTreeMap<String, Entry>,
    #[serde(default)]
    pub stats: BTreeMap<String, Stat>,
    #[serde(default)]
    pub pending: BTreeMap<String, Pending>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDb {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub roots: BTreeMap<String, RootState>,
    /// Drive `changes` token, shared by all roots.
    #[serde(default)]
    pub page_token: Option<String>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl StateDb {
    /// Load, or start empty. A corrupt file is set aside as `<name>.corrupt`
    /// and sync starts over from "both sides as they are", which is safe:
    /// equal content counts as synced and nothing is deleted without a base.
    pub fn load(path: &Path) -> StateDb {
        let mut db = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("sync state {} is unreadable ({e}); starting over", path.display());
                let _ = std::fs::rename(path, path.with_extension("json.corrupt"));
                StateDb::default()
            }),
            Err(_) => StateDb::default(),
        };
        db.version = 1;
        db.path = Some(path.to_path_buf());
        db
    }

    /// In memory only (tests).
    pub fn ephemeral() -> StateDb {
        StateDb { version: 1, ..Default::default() }
    }

    pub fn root(&mut self, name: &str) -> &mut RootState {
        self.roots.entry(name.to_string()).or_default()
    }

    /// Write to `<path>.tmp` and rename over the old file.
    pub fn save(&self) -> Result<(), SyncError> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_vec_pretty(self).map_err(|e| SyncError::State(e.to_string()))?;
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&text)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_survives_corruption() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        let mut db = StateDb::load(&p);
        db.root("Documents").remote_root_id = Some("r1".into());
        db.page_token = Some("7".into());
        db.save().unwrap();
        let back = StateDb::load(&p);
        assert_eq!(back.roots["Documents"].remote_root_id.as_deref(), Some("r1"));
        std::fs::write(&p, "{ nope").unwrap();
        let fresh = StateDb::load(&p);
        assert!(fresh.roots.is_empty());
        assert!(p.with_extension("json.corrupt").exists());
    }
}
