//! The three-way plan: local now vs the last-synced base vs remote now.
//!
//! Rules (see TODO.md): equal content is synced whatever the base says; a
//! side changes only relative to the base; a change on one side flows to the
//! other; a change on both sides keeps both (conflict copy of the remote
//! version, local pushed); a delete flows only when the other side is
//! unchanged; blobs never conflict, and a blob delete flows only when the
//! receiving side's `doc.json` is unchanged or absent.

use std::collections::{BTreeMap, BTreeSet};

use crate::local::{blob_doc, doc_json_dir, LocalFile};
use crate::remote::RemoteFile;
use crate::state::Entry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Equal on both sides: just record the base.
    Record { path: String, local: LocalFile, remote: RemoteFile },
    /// Gone on both sides: drop the base.
    Forget { path: String },
    /// Push local. `existing` is the remote file to update in place; it must
    /// still look like this when the upload starts.
    Upload { path: String, local: LocalFile, existing: Option<RemoteFile> },
    /// Pull remote. `local` is what the local file looked like when planned (if any).
    Download { path: String, remote: RemoteFile, local: Option<LocalFile> },
    DeleteLocal { path: String, local: LocalFile },
    TrashRemote { path: String, remote: RemoteFile },
    /// Write the remote version of a file to a conflict copy (an `Upload` of
    /// the local version follows in a later phase).
    ConflictFile { path: String, remote: RemoteFile },
    /// Same for a whole document: the remote `doc.json` and its blobs go to a
    /// conflict copy of the `.ecad` folder.
    ConflictDoc { doc: String, doc_json: RemoteFile },
}

impl Action {
    pub fn path(&self) -> &str {
        match self {
            Action::Record { path, .. }
            | Action::Forget { path }
            | Action::Upload { path, .. }
            | Action::Download { path, .. }
            | Action::DeleteLocal { path, .. }
            | Action::TrashRemote { path, .. }
            | Action::ConflictFile { path, .. } => path,
            Action::ConflictDoc { doc, .. } => doc,
        }
    }

    /// Execution order: conflict copies, blobs, other files, `doc.json`, deletes.
    pub fn phase(&self) -> u8 {
        match self {
            Action::Record { .. } | Action::Forget { .. } | Action::ConflictFile { .. } | Action::ConflictDoc { .. } => 0,
            Action::DeleteLocal { .. } | Action::TrashRemote { .. } => 4,
            Action::Upload { path, .. } | Action::Download { path, .. } => {
                if blob_doc(path).is_some() {
                    1
                } else if doc_json_dir(path).is_some() {
                    3
                } else {
                    2
                }
            }
        }
    }

    /// Whether this moves data or changes a side (not just the base).
    pub fn is_transfer(&self) -> bool {
        !matches!(self, Action::Record { .. } | Action::Forget { .. })
    }
}

fn remote_changed(b: &Entry, r: &RemoteFile) -> bool {
    b.remote_id != r.id || b.remote_md5 != r.md5
}

/// Pick one remote file per path: the one the base knows, else the newest.
pub fn index_remote(files: Vec<RemoteFile>, base: &BTreeMap<String, Entry>) -> BTreeMap<String, RemoteFile> {
    let mut out: BTreeMap<String, RemoteFile> = BTreeMap::new();
    for f in files {
        let known = |x: &RemoteFile| base.get(&x.path).is_some_and(|b| b.remote_id == x.id);
        let replace = match out.get(&f.path) {
            None => true,
            Some(cur) if known(cur) => false,
            Some(_) if known(&f) => true,
            Some(cur) => f.modified_time > cur.modified_time,
        };
        if replace {
            out.insert(f.path.clone(), f);
        }
    }
    out
}

pub fn plan(
    local: &BTreeMap<String, LocalFile>,
    base: &BTreeMap<String, Entry>,
    remote: &BTreeMap<String, RemoteFile>,
) -> Vec<Action> {
    // doc.json change per side, for the blob-delete rule
    let doc_changed_local = |doc: &str| {
        let p = format!("{doc}/doc.json");
        match (local.get(&p), base.get(&p)) {
            (Some(l), Some(b)) => l.md5 != b.local_md5,
            (Some(_), None) => true,
            (None, _) => false,
        }
    };
    let doc_changed_remote = |doc: &str| {
        let p = format!("{doc}/doc.json");
        match (remote.get(&p), base.get(&p)) {
            (Some(r), Some(b)) => remote_changed(b, r),
            (Some(_), None) => true,
            (None, _) => false,
        }
    };

    let paths: BTreeSet<&String> = local.keys().chain(base.keys()).chain(remote.keys()).collect();
    let mut out = Vec::new();
    for p in paths {
        let path = p.clone();
        let (l, b, r) = (local.get(p), base.get(p), remote.get(p));
        let blob = blob_doc(p).map(|(doc, _)| doc);
        match (l, r) {
            (Some(l), Some(r)) if r.md5.as_deref() == Some(l.md5.as_str()) => {
                let current = b.is_some_and(|b| {
                    b.remote_id == r.id && b.remote_md5 == r.md5 && b.local_md5 == l.md5 && b.local_mtime_ns == l.mtime_ns
                });
                if !current {
                    out.push(Action::Record { path, local: l.clone(), remote: r.clone() });
                }
            }
            (Some(l), Some(r)) => {
                let lc = b.is_none_or(|b| b.local_md5 != l.md5);
                let rc = b.is_none_or(|b| remote_changed(b, r));
                if blob.is_some() {
                    // content-addressed: whichever side changed is the repair
                    if rc && !lc {
                        out.push(Action::Download { path, remote: r.clone(), local: Some(l.clone()) });
                    } else {
                        out.push(Action::Upload { path, local: l.clone(), existing: Some(r.clone()) });
                    }
                    continue;
                }
                match (lc, rc) {
                    (true, false) => out.push(Action::Upload { path, local: l.clone(), existing: Some(r.clone()) }),
                    (false, true) => out.push(Action::Download { path, remote: r.clone(), local: Some(l.clone()) }),
                    _ => {
                        match doc_json_dir(p) {
                            Some(doc) => out.push(Action::ConflictDoc { doc: doc.into(), doc_json: r.clone() }),
                            None => out.push(Action::ConflictFile { path: path.clone(), remote: r.clone() }),
                        }
                        out.push(Action::Upload { path, local: l.clone(), existing: Some(r.clone()) });
                    }
                }
            }
            (Some(l), None) => match b {
                None => out.push(Action::Upload { path, local: l.clone(), existing: None }),
                Some(b) if b.local_md5 == l.md5 => {
                    // deleted remotely, unchanged here
                    if blob.is_some_and(doc_changed_local) {
                        out.push(Action::Upload { path, local: l.clone(), existing: None });
                    } else {
                        out.push(Action::DeleteLocal { path, local: l.clone() });
                    }
                }
                // modified here, deleted there: the modification wins
                Some(_) => out.push(Action::Upload { path, local: l.clone(), existing: None }),
            },
            (None, Some(r)) => match b {
                None => out.push(Action::Download { path, remote: r.clone(), local: None }),
                Some(b) if !remote_changed(b, r) => {
                    if blob.is_some_and(doc_changed_remote) {
                        out.push(Action::Download { path, remote: r.clone(), local: None });
                    } else {
                        out.push(Action::TrashRemote { path, remote: r.clone() });
                    }
                }
                Some(_) => out.push(Action::Download { path, remote: r.clone(), local: None }),
            },
            (None, None) => out.push(Action::Forget { path }),
        }
    }
    out.sort_by_key(Action::phase);
    out
}

/// `dir/Name (conflict dev 2026-11-03 101500).ecad`, or `stem (…).ext` for a file.
pub fn conflict_name(path: &str, device: &str, when: chrono::DateTime<chrono::Local>) -> String {
    let (dir, name) = match path.rsplit_once('/') {
        Some((d, n)) => (Some(d), n),
        None => (None, path),
    };
    let tag = format!("(conflict {device} {})", when.format("%Y-%m-%d %H%M%S"));
    let new = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} {tag}.{ext}"),
        _ => format!("{name} {tag}"),
    };
    match dir {
        Some(d) => format!("{d}/{new}"),
        None => new,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lf(md5: &str) -> LocalFile {
        LocalFile { size: 1, mtime_ns: 1, md5: md5.into() }
    }
    fn rf(path: &str, id: &str, md5: &str) -> RemoteFile {
        RemoteFile {
            id: id.into(),
            root: "R".into(),
            path: path.into(),
            size: 1,
            md5: Some(md5.into()),
            head_revision_id: None,
            version: None,
            modified_time: None,
            meta: Default::default(),
        }
    }
    fn base(id: &str, l: &str, r: &str) -> Entry {
        Entry {
            remote_id: id.into(),
            remote_md5: Some(r.into()),
            remote_rev: None,
            remote_version: None,
            local_md5: l.into(),
            local_size: 1,
            local_mtime_ns: 1,
        }
    }

    #[test]
    fn three_way_cases() {
        let mut local = BTreeMap::new();
        let mut b = BTreeMap::new();
        let mut remote = BTreeMap::new();
        local.insert("new_local".to_string(), lf("a"));
        remote.insert("new_remote".to_string(), rf("new_remote", "1", "b"));
        local.insert("same".to_string(), lf("c"));
        remote.insert("same".to_string(), rf("same", "2", "c"));
        local.insert("del_remote".to_string(), lf("d"));
        b.insert("del_remote".to_string(), base("3", "d", "d"));
        b.insert("del_local".to_string(), base("4", "e", "e"));
        remote.insert("del_local".to_string(), rf("del_local", "4", "e"));
        local.insert("both".to_string(), lf("f2"));
        b.insert("both".to_string(), base("5", "f", "f"));
        remote.insert("both".to_string(), rf("both", "5", "f3"));
        local.insert("mod_vs_del".to_string(), lf("g2"));
        b.insert("mod_vs_del".to_string(), base("6", "g", "g"));
        let acts = plan(&local, &b, &remote);
        let kind = |p: &str| {
            acts.iter()
                .filter(|a| a.path() == p)
                .map(|a| format!("{a:?}").split([' ', '{']).next().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(kind("new_local"), ["Upload"]);
        assert_eq!(kind("new_remote"), ["Download"]);
        assert_eq!(kind("same"), ["Record"]);
        assert_eq!(kind("del_remote"), ["DeleteLocal"]);
        assert_eq!(kind("del_local"), ["TrashRemote"]);
        assert_eq!(kind("both"), ["ConflictFile", "Upload"]);
        assert_eq!(kind("mod_vs_del"), ["Upload"]);
    }

    #[test]
    fn blob_delete_waits_for_an_unchanged_doc() {
        let blob = format!("A.ecad/blobs/{}", "0".repeat(64));
        let mut local = BTreeMap::new();
        let mut b = BTreeMap::new();
        let remote = BTreeMap::new();
        // remote deleted the blob (and the doc), local edited doc.json
        local.insert(blob.clone(), lf("x"));
        b.insert(blob.clone(), base("1", "x", "x"));
        local.insert("A.ecad/doc.json".to_string(), lf("d2"));
        b.insert("A.ecad/doc.json".to_string(), base("2", "d", "d"));
        let acts = plan(&local, &b, &remote);
        assert!(acts.iter().any(|a| matches!(a, Action::Upload { path, .. } if *path == blob)));
        assert!(!acts.iter().any(|a| matches!(a, Action::DeleteLocal { .. })));
    }

    #[test]
    fn conflict_names() {
        let t = chrono::TimeZone::with_ymd_and_hms(&chrono::Local, 2026, 11, 3, 10, 15, 0).unwrap();
        assert_eq!(conflict_name("a/Box.ecad", "lap", t), "a/Box (conflict lap 2026-11-03 101500).ecad");
        assert_eq!(conflict_name("notes", "lap", t), "notes (conflict lap 2026-11-03 101500)");
    }
}
