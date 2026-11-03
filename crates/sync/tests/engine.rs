//! The sync engine against the in-memory remote: no network. Two engines on
//! one `MemoryRemote` stand for two devices.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use easycad_sync::{
    BackgroundOptions, Engine, EventSink, MemoryRemote, RootConfig, SyncConfig, SyncEvent, SyncHandle,
};

const ROOT: &str = "Documents";

struct Device {
    dir: PathBuf,
    engine: Engine,
}

fn config(dir: &Path, device: &str) -> SyncConfig {
    let mut c = SyncConfig::new(vec![RootConfig::new(ROOT, dir.join("docs"))], dir.join("state.json"));
    c.device = device.into();
    c
}

impl Device {
    fn new(remote: &MemoryRemote, dir: &Path, name: &str) -> Device {
        let engine = Engine::new(Arc::new(remote.clone()), config(dir, name), EventSink::null());
        Device { dir: dir.to_path_buf(), engine }
    }
    fn docs(&self) -> PathBuf {
        self.dir.join("docs")
    }
    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.docs().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.docs().join(rel)).ok()
    }
    /// Relative paths of synced files (not the local trash).
    fn files(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.docs()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                let rel = p.strip_prefix(self.docs()).unwrap().to_string_lossy().replace('\\', "/");
                if rel.starts_with(".cwsync") {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(rel);
                }
            }
        }
        out.sort();
        out
    }
    async fn sync(&mut self) -> easycad_sync::SyncReport {
        self.engine.sync_once().await.expect("sync")
    }
}

fn sha(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// A document folder: doc.json plus one blob. Returns the blob's path.
fn write_doc(d: &Device, name: &str, json: &str, blob: &[u8]) -> String {
    let b = format!("{name}.ecad/blobs/{}", sha(blob));
    d.write(&b, blob);
    d.write(&format!("{name}.ecad/doc.json"), json.as_bytes());
    b
}

#[tokio::test]
async fn new_local_files_upload_and_a_second_pass_is_a_noop() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, t.path(), "a");
    let blob = write_doc(&a, "Box", r#"{"doc_id":"d1","revision":3}"#, b"step data");
    let r = a.sync().await;
    assert_eq!(r.uploaded, 2);
    assert_eq!(remote.paths(ROOT), vec![blob, "Box.ecad/doc.json".to_string()]);
    let doc = remote.file(ROOT, "Box.ecad/doc.json").unwrap();
    assert_eq!(doc.meta.doc_id.as_deref(), Some("d1"));
    assert_eq!(doc.meta.revision, Some(3));
    assert_eq!(doc.meta.device.as_deref(), Some("a"));
    assert!(a.sync().await.is_noop());
}

#[tokio::test]
async fn new_remote_files_download() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    remote.put(ROOT, "notes/readme.txt", b"hello", "other");
    let mut a = Device::new(&remote, t.path(), "a");
    let r = a.sync().await;
    assert_eq!(r.downloaded, 1);
    assert_eq!(a.read("notes/readme.txt").unwrap(), b"hello");
    assert!(a.sync().await.is_noop());
}

#[tokio::test]
async fn edits_flow_between_two_devices() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, &t.path().join("a"), "a");
    let mut b = Device::new(&remote, &t.path().join("b"), "b");
    write_doc(&a, "Box", r#"{"v":1}"#, b"blob");
    a.sync().await;
    b.sync().await;
    assert_eq!(b.read("Box.ecad/doc.json").unwrap(), br#"{"v":1}"#);
    b.write("Box.ecad/doc.json", br#"{"v":2}"#);
    b.sync().await;
    let r = a.sync().await;
    assert_eq!(r.downloaded, 1);
    assert!(r.conflicts.is_empty());
    assert_eq!(a.read("Box.ecad/doc.json").unwrap(), br#"{"v":2}"#);
}

#[tokio::test]
async fn both_edited_keeps_both_versions() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, &t.path().join("a"), "a");
    let mut b = Device::new(&remote, &t.path().join("b"), "b");
    let blob = write_doc(&a, "Box", r#"{"doc_id":"d1","v":1}"#, b"blob");
    a.sync().await;
    b.sync().await;
    a.write("Box.ecad/doc.json", br#"{"doc_id":"d1","v":"from a"}"#);
    b.write("Box.ecad/doc.json", br#"{"doc_id":"d1","v":"from b"}"#);
    a.sync().await;
    let r = b.sync().await;
    assert_eq!(r.conflicts.len(), 1);
    let c = &r.conflicts[0];
    assert_eq!(c.path, "Box.ecad");
    assert!(c.conflict_copy.starts_with("Box (conflict a "), "{}", c.conflict_copy);
    assert!(c.conflict_copy.ends_with(".ecad"));
    assert_eq!(c.doc_id.as_deref(), Some("d1"));
    // b's version stays (and is pushed), a's is in the copy with its blob
    assert_eq!(b.read("Box.ecad/doc.json").unwrap(), br#"{"doc_id":"d1","v":"from b"}"#);
    assert_eq!(
        b.read(&format!("{}/doc.json", c.conflict_copy)).unwrap(),
        br#"{"doc_id":"d1","v":"from a"}"#
    );
    let copy_blob = blob.replacen("Box.ecad", &c.conflict_copy, 1);
    assert_eq!(b.read(&copy_blob).unwrap(), b"blob");
    assert_eq!(remote.content(ROOT, "Box.ecad/doc.json").unwrap(), br#"{"doc_id":"d1","v":"from b"}"#);
    // the copy reached the remote as a new doc, its blob by server-side copy
    assert!(remote.paths(ROOT).contains(&format!("{}/doc.json", c.conflict_copy)));
    assert!(remote.stats().copies >= 1);
    // and a ends up with both
    a.sync().await;
    assert_eq!(a.files(), b.files());
    assert!(a.sync().await.is_noop());
    assert!(b.sync().await.is_noop());
}

#[tokio::test]
async fn plain_file_conflict_gets_a_named_copy() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, t.path(), "a");
    a.write("notes.txt", b"one");
    a.sync().await;
    a.write("notes.txt", b"mine");
    remote.put(ROOT, "notes.txt", b"theirs", "laptop");
    let r = a.sync().await;
    assert_eq!(r.conflicts.len(), 1);
    let copy = &r.conflicts[0].conflict_copy;
    assert!(copy.starts_with("notes (conflict laptop ") && copy.ends_with(".txt"), "{copy}");
    assert_eq!(a.read(copy).unwrap(), b"theirs");
    assert_eq!(remote.content(ROOT, "notes.txt").unwrap(), b"mine");
}

#[tokio::test]
async fn deletes_propagate_when_the_other_side_is_unchanged() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, &t.path().join("a"), "a");
    let mut b = Device::new(&remote, &t.path().join("b"), "b");
    a.write("x.txt", b"x");
    a.write("y.txt", b"y");
    a.sync().await;
    b.sync().await;
    // a deletes x: remote trashes it, b moves it to its local trash
    std::fs::remove_file(a.docs().join("x.txt")).unwrap();
    assert_eq!(a.sync().await.deleted_remote, 1);
    assert_eq!(b.sync().await.deleted_local, 1);
    assert_eq!(b.files(), ["y.txt"]);
    assert!(b.docs().join(".cwsync-trash").is_dir());
    // a deletes y while b edits it: the edit wins and comes back to a
    std::fs::remove_file(a.docs().join("y.txt")).unwrap();
    b.write("y.txt", b"edited");
    b.sync().await;
    let r = a.sync().await;
    assert_eq!(r.deleted_remote, 0);
    assert_eq!(a.read("y.txt").unwrap(), b"edited");
}

#[tokio::test]
async fn identical_blobs_are_copied_server_side() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, t.path(), "a");
    let big = vec![7u8; 100_000];
    write_doc(&a, "One", "{}", &big);
    a.sync().await;
    remote.reset_stats();
    write_doc(&a, "Two", r#"{"two":1}"#, &big);
    let r = a.sync().await;
    let s = remote.stats();
    assert_eq!(s.copies, 1, "{s:?}");
    assert_eq!(s.uploads, 1, "only Two's doc.json: {s:?}");
    assert!(s.upload_bytes < 100, "{s:?}");
    assert_eq!(r.copied_remote, 1);
}

#[tokio::test]
async fn rename_is_delete_plus_cheap_add() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, t.path(), "a");
    let blob = write_doc(&a, "Old", "{}", &[1u8; 50_000]);
    a.sync().await;
    remote.reset_stats();
    std::fs::rename(a.docs().join("Old.ecad"), a.docs().join("New.ecad")).unwrap();
    a.sync().await;
    let paths = remote.paths(ROOT);
    assert!(paths.iter().all(|p| p.starts_with("New.ecad/")), "{paths:?}");
    assert!(paths.contains(&blob.replace("Old.ecad", "New.ecad")));
    let s = remote.stats();
    assert_eq!((s.copies, s.trashes, s.upload_bytes), (2, 2, 0), "{s:?}");
}

#[tokio::test]
async fn interrupted_upload_resumes_after_a_restart() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new().with_resumable_threshold(1024, 256);
    let data: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 251) as u8).collect();
    {
        let mut a = Device::new(&remote, t.path(), "a");
        a.write("big.step", &data);
        remote.interrupt_upload_at(1024);
        let e = a.engine.sync_once().await.unwrap_err();
        assert!(e.is_offline(), "{e}");
        assert!(remote.paths(ROOT).is_empty());
    }
    // a fresh engine (the app restarted) continues the same session
    let mut a = Device::new(&remote, t.path(), "a");
    a.sync().await;
    assert_eq!(remote.content(ROOT, "big.step").unwrap(), data);
    let s = remote.stats();
    assert_eq!(s.sessions_started, 1, "{s:?}");
    assert_eq!(s.upload_bytes, 4096, "nothing sent twice: {s:?}");
    assert!(a.engine.state().roots[ROOT].pending.is_empty());
}

#[tokio::test]
async fn offline_then_reconnect() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, &t.path().join("a"), "a");
    let mut b = Device::new(&remote, &t.path().join("b"), "b");
    a.write("shared.txt", b"v1");
    a.sync().await;
    b.sync().await;
    remote.set_offline(true);
    a.write("from_a.txt", b"a");
    b.write("from_b.txt", b"b");
    let e = a.engine.sync_once().await.unwrap_err();
    assert!(e.is_offline());
    assert!(b.engine.sync_once().await.unwrap_err().is_offline());
    remote.set_offline(false);
    a.sync().await;
    b.sync().await;
    a.sync().await;
    assert_eq!(a.files(), ["from_a.txt", "from_b.txt", "shared.txt"]);
    assert_eq!(a.files(), b.files());
}

#[tokio::test]
async fn safety_stops() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, t.path(), "a");
    for i in 0..12 {
        a.write(&format!("f{i}.txt"), format!("{i}").as_bytes());
    }
    a.sync().await;
    // an emptied root does not empty the Drive
    for i in 0..12 {
        std::fs::remove_file(a.docs().join(format!("f{i}.txt"))).unwrap();
    }
    let r = a.engine.sync_once().await.unwrap();
    assert!(r.errors[0].message.contains("safety stop"), "{r:?}");
    assert_eq!(remote.paths(ROOT).len(), 12);
    // a root that vanished (unmounted) is not treated as deleted
    std::fs::remove_dir_all(a.docs()).unwrap();
    let r = a.engine.sync_once().await.unwrap();
    assert_eq!(r.errors.len(), 1, "{r:?}");
    assert!(r.errors[0].message.contains("missing"));
    assert_eq!(remote.paths(ROOT).len(), 12);
}

#[tokio::test]
async fn a_recreated_drive_folder_resets_the_base_instead_of_deleting() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, t.path(), "a");
    a.write("keep.txt", b"k");
    a.sync().await;
    remote.recreate_root(ROOT);
    let r = a.sync().await;
    assert_eq!(r.deleted_local, 0);
    assert_eq!(a.read("keep.txt").unwrap(), b"k");
    assert_eq!(remote.paths(ROOT), ["keep.txt"]);
}

#[tokio::test]
async fn a_lost_state_db_is_safe() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, t.path(), "a");
    a.write("a.txt", b"same");
    a.sync().await;
    std::fs::remove_file(t.path().join("state.json")).unwrap();
    remote.reset_stats();
    let mut a = Device::new(&remote, t.path(), "a");
    let r = a.sync().await;
    assert!(r.is_noop(), "{r:?}");
    assert_eq!(remote.stats().uploads, 0);
}

#[test]
fn background_handle_syncs_and_stops() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let dir = t.path().to_path_buf();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(dir.join("docs/a.txt"), b"a").unwrap();
    let (sink, rx) = EventSink::channel();
    let engine = Engine::new(Arc::new(remote.clone()), config(&dir, "a"), sink.clone());
    let opts = BackgroundOptions { watch: true, debounce: Duration::from_millis(100), ..Default::default() };
    let h = SyncHandle::spawn(engine, sink, opts).unwrap();
    let wait_completed = |rx: &std::sync::mpsc::Receiver<SyncEvent>| loop {
        match rx.recv_timeout(Duration::from_secs(10)).expect("an event") {
            SyncEvent::Completed(r) => return r,
            _ => continue,
        }
    };
    assert_eq!(wait_completed(&rx).uploaded, 1);
    // the watcher picks up a new file
    std::fs::write(dir.join("docs/b.txt"), b"b").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while remote.paths(ROOT).len() < 2 && std::time::Instant::now() < deadline {
        let _ = rx.recv_timeout(Duration::from_millis(200));
    }
    assert_eq!(remote.paths(ROOT), ["a.txt", "b.txt"]);
    h.sync_now();
    wait_completed(&rx);
    h.stop();
}

#[tokio::test]
async fn our_own_uploads_are_not_remote_news() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    let mut a = Device::new(&remote, t.path(), "a");
    write_doc(&a, "Box", "{}", b"blob");
    a.write("old.txt", b"x");
    a.sync().await;
    std::fs::remove_file(a.docs().join("old.txt")).unwrap();
    a.sync().await;
    assert!(!a.engine.remote_changed().await.unwrap(), "only our own uploads and trash happened");
    remote.put(ROOT, "from_b.txt", b"b", "b");
    assert!(a.engine.remote_changed().await.unwrap(), "another device's upload is news");
    assert!(!a.engine.remote_changed().await.unwrap(), "and only once");
}

#[tokio::test]
async fn downloads_are_noted_as_our_own_writes() {
    let t = tempfile::tempdir().unwrap();
    let remote = MemoryRemote::new();
    remote.put(ROOT, "sub/x.txt", b"x", "b");
    let mut a = Device::new(&remote, t.path(), "a");
    a.sync().await;
    let w = a.engine.written();
    let w = w.lock().unwrap();
    assert!(w.contains_key(&a.docs().join("sub/x.txt")));
    assert!(w.contains_key(&a.docs().join("sub")), "the folder it created too");
}
