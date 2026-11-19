//! Live reload: when a newer build of this binary is installed at the path it
//! was started from, the app saves its session, `exec`s the new binary with
//! `--resume <file>` and the new process restores the session (same PID, same
//! window). See docs/DEV-RELOAD.md.
//!
//! Three pieces, each testable without a window:
//! * [`Watcher`]: notices a replaced, completely written executable.
//! * [`idle`]: the gate that keeps a reload from interrupting an interaction.
//! * [`Snapshot`]: the session as JSON plus a copy of the document.
//!
//! The `EasyCadApp` glue (taking and applying a snapshot, the per-frame tick) is
//! at the bottom.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use easycad_model::document::Document;
use serde::{Deserialize, Serialize};

/// Git short sha (a trailing `+` = built with uncommitted changes).
pub const GIT_SHA: &str = env!("EASYCAD_GIT_SHA");

/// Seconds since the Unix epoch when this binary was built.
pub fn build_time() -> u64 {
    env!("EASYCAD_BUILD_TIME").parse().unwrap_or(0)
}

/// "0.1.0 (abc123def, built 2026-11-19 14:03 UTC)".
pub fn build_label() -> String {
    format!("{} ({}, built {})", env!("CARGO_PKG_VERSION"), GIT_SHA, utc_text(build_time()))
}

/// Short form for toasts: "abc123def".
pub fn build_short() -> String {
    GIT_SHA.to_string()
}

fn utc_text(secs: u64) -> String {
    // civil-from-days (Howard Hinnant), no time crate needed
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

// ---------------------------------------------------------------- watcher

/// What identifies one file on disk: replacing it (rename over it) changes the
/// inode, copying over it changes size or mtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i128,
}

#[cfg(unix)]
pub fn stamp(p: &Path) -> Option<(Stamp, bool)> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let m = std::fs::metadata(p).ok()?;
    if !m.is_file() {
        return None;
    }
    let st = Stamp { ino: m.ino(), size: m.size(), mtime_ns: m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128 };
    Some((st, m.permissions().mode() & 0o111 != 0))
}

#[cfg(not(unix))]
pub fn stamp(p: &Path) -> Option<(Stamp, bool)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as i128;
    Some((Stamp { ino: 0, size: m.len(), mtime_ns: t }, true))
}

/// The path this process was started from, resolved at startup and with the
/// kernel's " (deleted)" suffix removed (Linux reports it once replaced).
pub fn exe_path() -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?;
    let s = p.to_string_lossy();
    Some(match s.strip_suffix(" (deleted)") {
        Some(t) => PathBuf::from(t),
        None => p,
    })
}

/// Pure polling state: feed it the current stamp of the watched path at each poll.
#[derive(Debug)]
pub struct Watcher {
    pub path: PathBuf,
    running: Option<Stamp>,
    /// A changed file seen at the previous poll, and when it was first seen so.
    candidate: Option<(Stamp, Instant)>,
    /// How long a changed file must stay unchanged to count as completely written.
    pub settle: Duration,
}

impl Watcher {
    pub fn new(path: PathBuf) -> Watcher {
        let running = stamp(&path).map(|s| s.0);
        Watcher { path, running, candidate: None, settle: Duration::from_millis(300) }
    }

    /// True once a different, executable file has stayed unchanged for `settle`.
    pub fn poll(&mut self, now: Instant) -> bool {
        let cur = stamp(&self.path);
        self.step(cur, now)
    }

    fn step(&mut self, cur: Option<(Stamp, bool)>, now: Instant) -> bool {
        let Some((st, exec)) = cur else {
            // mid-replace (the path is briefly absent with some installers)
            self.candidate = None;
            return false;
        };
        if Some(st) == self.running || !exec || st.size == 0 {
            self.candidate = None;
            return false;
        }
        match self.candidate {
            Some((c, since)) if c == st => now.duration_since(since) >= self.settle,
            _ => {
                self.candidate = Some((st, now));
                false
            }
        }
    }
}

/// Starts the polling thread (1 s). The flag turns true when a new build is
/// ready; `wake` is called then so the UI notices without a timer.
pub fn spawn_watcher(path: PathBuf, wake: impl Fn() + Send + 'static) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = flag.clone();
    let _ = std::thread::Builder::new().name("easycad-reload-watch".into()).spawn(move || {
        let mut w = Watcher::new(path);
        loop {
            std::thread::sleep(Duration::from_millis(if w.candidate.is_some() { 150 } else { 1000 }));
            if w.poll(Instant::now()) {
                f.store(true, Ordering::SeqCst);
                wake();
                return;
            }
        }
    });
    flag
}

// ---------------------------------------------------------------- idle gate

/// What the idle gate looks at (filled from the app each frame).
#[derive(Clone, Copy, Debug, Default)]
pub struct Busy {
    pub pointer_down: bool,
    pub dragging: bool,
    pub typing: bool,
    /// A dialog, prompt, wizard or other modal is open.
    pub modal: bool,
    /// A sketch (or similar mode) holds state the snapshot cannot carry.
    pub unsnapshottable: bool,
    /// A pending debounced edit or a background job that would be lost.
    pub pending_edit: bool,
}

/// True when it is safe to restart now.
pub fn idle(b: &Busy) -> bool {
    !(b.pointer_down || b.dragging || b.typing || b.modal || b.unsnapshottable || b.pending_edit)
}

/// Why not (for the status hint); `None` = idle.
pub fn busy_reason(b: &Busy) -> Option<&'static str> {
    if b.pointer_down || b.dragging {
        Some("a drag is in progress")
    } else if b.typing {
        Some("a text field is being edited")
    } else if b.modal {
        Some("a dialog is open")
    } else if b.unsnapshottable {
        Some("a sketch is being edited")
    } else if b.pending_edit {
        Some("an edit is pending")
    } else {
        None
    }
}

// ---------------------------------------------------------------- snapshot

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraSnap {
    pub target: [f32; 3],
    pub distance: f32,
    pub rot: [f32; 4],
    pub fov_y: f32,
    pub ortho: bool,
    pub scene_center: [f32; 3],
    pub scene_radius: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModeSnap {
    Model,
    /// A committed sketch being edited (uncommitted draft state is not carried:
    /// reload waits until the sketch is closed, see [`Busy::unsnapshottable`]).
    Sketch(u32),
    Parts,
    Drawing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowSnap {
    pub pos: Option<[f32; 2]>,
    pub size: [f32; 2],
    pub maximized: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    /// Sha of the build that wrote it.
    pub from_build: String,
    pub pid: u32,
    /// The document's own file/folder, if it has one.
    pub doc_path: Option<PathBuf>,
    /// Unsaved edits at the time (the saved baseline is then "one revision behind").
    pub dirty: bool,
    /// Never-saved document that holds work.
    pub unnamed_work: bool,
    pub camera: CameraSnap,
    /// 0 shaded, 1 shaded+edges, 2 wireframe.
    pub display: u8,
    pub mode: ModeSnap,
    pub browser_open: bool,
    pub show_build_plate: bool,
    pub hidden: Vec<easycad_model::feature::BodyId>,
    pub window: Option<WindowSnap>,
    /// Undo history lives inside the document type and is not carried over.
    pub undo_lost: bool,
}

pub const VERSION: u32 = 1;

/// `$XDG_DATA_HOME/easycad/reload`.
pub fn dir() -> Option<PathBuf> {
    crate::recovery::dir().and_then(|d| d.parent().map(|p| p.join("reload")))
}

fn json_path(root: &Path, pid: u32) -> PathBuf {
    root.join(format!("{pid}.json"))
}
fn doc_dir(root: &Path, pid: u32) -> PathBuf {
    root.join(format!("{pid}.doc.ecad"))
}

/// Write the snapshot and the document copy under `root` (atomically: the JSON
/// appears last, after the document is complete). Returns the JSON path.
pub fn write_session(root: &Path, snap: &Snapshot, doc: &Document) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let dd = doc_dir(root, snap.pid);
    let _ = std::fs::remove_dir_all(&dd);
    doc.save_to_dir(&dd).map_err(|e| e.to_string())?;
    let jp = json_path(root, snap.pid);
    let tmp = jp.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(snap).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &jp).map_err(|e| e.to_string())?;
    Ok(jp)
}

/// Read a session written by [`write_session`].
pub fn read_session(json: &Path) -> Result<(Snapshot, Document), String> {
    let snap: Snapshot = serde_json::from_str(&std::fs::read_to_string(json).map_err(|e| e.to_string())?)
        .map_err(|e| format!("bad reload file: {e}"))?;
    if snap.version != VERSION {
        return Err(format!("reload file version {} (expected {VERSION})", snap.version));
    }
    let root = json.parent().ok_or("no parent")?;
    let doc = Document::load_from_dir(&doc_dir(root, snap.pid)).map_err(|e| e.to_string())?;
    Ok((snap, doc))
}

/// Remove a session's files (after a successful restore).
pub fn clear_session(json: &Path) {
    if let (Some(root), Some(stem)) = (json.parent(), json.file_stem().and_then(|s| s.to_str())) {
        let _ = std::fs::remove_dir_all(root.join(format!("{stem}.doc.ecad")));
    }
    let _ = std::fs::remove_file(json);
}

/// `--resume <file>` from the command line.
pub fn resume_arg() -> Option<PathBuf> {
    let mut a = std::env::args().skip(1);
    (a.next().as_deref() == Some("--resume")).then(|| a.next().map(PathBuf::from)).flatten()
}

/// Replace this process with `exe --resume <json>` (Unix `execv`: same PID).
/// Only returns on failure.
#[cfg(unix)]
pub fn exec_new(exe: &Path, json: &Path) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe).arg("--resume").arg(json).exec()
}

#[cfg(not(unix))]
pub fn exec_new(exe: &Path, json: &Path) -> std::io::Error {
    match std::process::Command::new(exe).arg("--resume").arg(json).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => e,
    }
}

// ---------------------------------------------------------------- app glue

/// Reload state kept in the app.
#[derive(Default)]
pub struct State {
    pub flag: Option<Arc<AtomicBool>>,
    pub exe: Option<PathBuf>,
    /// Told the user a build is waiting (once).
    pub told: bool,
    /// Session file this process was resumed from (cleared after restore).
    pub resumed_from: Option<PathBuf>,
}

fn ident(n: f32) -> f32 {
    if n.is_finite() { n } else { 0.0 }
}

impl crate::app::EasyCadApp {
    /// Start watching the binary this process was started from.
    pub fn start_reload_watch(&mut self, ctx: &egui::Context) {
        if std::env::var_os("EASYCAD_NO_AUTORELOAD").is_some_and(|v| v != "0" && !v.is_empty()) {
            return;
        }
        let Some(exe) = exe_path() else { return };
        let c = ctx.clone();
        self.reload.flag = Some(spawn_watcher(exe.clone(), move || c.request_repaint()));
        self.reload.exe = Some(exe);
    }

    fn reload_busy(&self, ctx: &egui::Context) -> Busy {
        Busy {
            pointer_down: ctx.input(|i| i.pointer.any_down()),
            dragging: self.dragging,
            typing: self.typing || ctx.text_edit_focused(),
            modal: self.dialog.is_some()
                || self.path_prompt.is_some()
                || self.new_doc_prompt.is_some()
                || self.wizard.is_some()
                || self.export.is_some()
                || self.calibrate.is_some()
                || !self.recovery_offer.is_empty(),
            unsnapshottable: matches!(self.mode, crate::app::Mode::Sketch(_) | crate::app::Mode::Drawing(_)),
            pending_edit: self.pending_profile.is_some()
                || self.params.pending.is_some()
                || self.params.adding.is_some()
                || self.preview_dirty.is_some()
                || self.export_job.is_some(),
        }
    }

    /// Per frame: when a new build is ready and the app is idle, restart into it.
    pub fn reload_tick(&mut self, ctx: &egui::Context) {
        let Some(flag) = &self.reload.flag else { return };
        if !flag.load(Ordering::SeqCst) || !self.settings.auto_reload {
            return;
        }
        let busy = self.reload_busy(ctx);
        if let Some(why) = busy_reason(&busy) {
            if !self.reload.told {
                self.reload.told = true;
                self.toast(format!("New build installed; it applies when idle ({why})"));
            }
            ctx.request_repaint_after(Duration::from_millis(400));
            return;
        }
        if let Err(e) = self.reload_now(ctx) {
            // never loop on failure: stop watching and say why
            self.warn(format!("Auto-reload failed: {e}"));
            self.reload.flag = None;
        }
    }

    /// Snapshot and exec. Returns only on failure.
    pub fn reload_now(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let exe = self.reload.exe.clone().ok_or("no executable path")?;
        let snap = self.take_snapshot(ctx);
        // safety net: a normal recovery copy too (offered if the restore fails)
        if snap.dirty || snap.unnamed_work {
            let _ = crate::recovery::write(&self.doc, self.path.as_deref());
        }
        let root = dir().ok_or("no data directory")?;
        let json = write_session(&root, &snap, &self.doc)?;
        self.autosave.flush();
        let e = exec_new(&exe, &json);
        clear_session(&json);
        Err(format!("could not start {}: {e}", exe.display()))
    }

    pub fn take_snapshot(&self, ctx: &egui::Context) -> Snapshot {
        let c = &self.camera;
        let window = ctx.input(|i| {
            let v = i.viewport();
            v.inner_rect.map(|r| WindowSnap {
                pos: v.outer_rect.map(|o| [o.min.x, o.min.y]),
                size: [r.width(), r.height()],
                maximized: v.maximized.unwrap_or(false),
            })
        });
        Snapshot {
            version: VERSION,
            from_build: build_short(),
            pid: std::process::id(),
            doc_path: self.path.clone(),
            dirty: self.is_dirty(),
            unnamed_work: self.path.is_none() && !crate::ui_welcome::is_empty(self),
            camera: CameraSnap {
                target: c.target.to_array(),
                distance: c.distance,
                rot: c.rot.to_array(),
                fov_y: c.fov_y,
                ortho: c.ortho,
                scene_center: c.scene_center.to_array(),
                scene_radius: c.scene_radius,
            },
            display: match self.display {
                crate::viewport::gpu::DisplayMode::Shaded => 0,
                crate::viewport::gpu::DisplayMode::ShadedEdges => 1,
                crate::viewport::gpu::DisplayMode::Wireframe => 2,
            },
            mode: match &self.mode {
                crate::app::Mode::Parts(_) => ModeSnap::Parts,
                crate::app::Mode::Sketch(s) => s.id.map(|i| ModeSnap::Sketch(i.0)).unwrap_or(ModeSnap::Model),
                _ => ModeSnap::Model,
            },
            browser_open: self.browser_open,
            show_build_plate: self.show_build_plate,
            hidden: self.hidden.iter().copied().collect(),
            window,
            undo_lost: true,
        }
    }

    /// Apply a restored session to a freshly built app (whose document is the
    /// restored one).
    pub fn apply_snapshot(&mut self, snap: &Snapshot) {
        let rev = self.doc.data.revision;
        self.path = snap.doc_path.clone();
        self.saved_revision = if snap.dirty || snap.unnamed_work { rev.wrapping_sub(1) } else { rev };
        let c = &snap.camera;
        self.camera.target = glam::Vec3::from_array(c.target);
        self.camera.distance = ident(c.distance).max(0.001);
        let q = glam::Quat::from_array(c.rot);
        self.camera.rot = if q.is_finite() && q.length_squared() > 0.5 { q.normalize() } else { self.camera.rot };
        self.camera.fov_y = c.fov_y;
        self.camera.ortho = c.ortho;
        self.camera.scene_center = glam::Vec3::from_array(c.scene_center);
        self.camera.scene_radius = c.scene_radius;
        self.fit_pending = false;
        self.display = match snap.display {
            0 => crate::viewport::gpu::DisplayMode::Shaded,
            2 => crate::viewport::gpu::DisplayMode::Wireframe,
            _ => crate::viewport::gpu::DisplayMode::ShadedEdges,
        };
        self.browser_open = snap.browser_open;
        self.show_build_plate = snap.show_build_plate;
        self.hidden = snap.hidden.iter().copied().collect();
        if snap.mode == ModeSnap::Parts {
            self.mode = crate::app::Mode::Parts(Box::new(crate::ui_parts::PartsUi::new(&self.doc)));
        }
        let note = format!(
            "Updated to {} (was {}) - session restored{}",
            build_short(),
            snap.from_build,
            if snap.undo_lost { "; undo history was reset" } else { "" }
        );
        self.toast(note);
    }
}

/// Recovery copies owned by this PID (a reload keeps the PID) become orphans, so
/// the normal recovery offer lists them. Used when a restore fails.
pub fn orphan_own_recovery_copies() {
    let Some(root) = crate::recovery::dir() else { return };
    for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let p = e.path().join("info.json");
        if let Ok(t) = std::fs::read_to_string(&p) {
            if let Ok(mut i) = serde_json::from_str::<crate::recovery::Info>(&t) {
                if i.pid == std::process::id() {
                    i.pid = 0;
                    let _ = std::fs::write(p, serde_json::to_string_pretty(&i).unwrap_or_default());
                }
            }
        }
    }
}

/// Window options from a snapshot (applied before the window exists).
pub fn apply_window(opts: &mut eframe::NativeOptions, w: &WindowSnap) {
    let mut v = std::mem::take(&mut opts.viewport);
    v = v.with_inner_size(w.size);
    if let Some(p) = w.pos {
        v = v.with_position(p);
    }
    if w.maximized {
        v = v.with_maximized(true);
    }
    opts.viewport = v;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cw-reload-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn write_exe(p: &Path, body: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(p, body).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn watcher_sees_replaced_complete_executable() {
        let d = tmp("watch");
        let bin = d.join("easycad");
        write_exe(&bin, b"old build");
        let mut w = Watcher::new(bin.clone());
        let t0 = Instant::now();
        assert!(!w.poll(t0), "unchanged file is not a new build");
        // atomic install: write beside, rename over (new inode)
        let new = d.join(".easycad.new");
        write_exe(&new, b"new build, longer");
        std::fs::rename(&new, &bin).unwrap();
        assert!(!w.poll(t0), "first sight only starts the settle timer");
        assert!(!w.poll(t0 + Duration::from_millis(100)), "not settled yet");
        assert!(w.poll(t0 + Duration::from_millis(400)), "settled: ready");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn watcher_waits_while_file_still_growing_or_not_executable() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("grow");
        let bin = d.join("easycad");
        write_exe(&bin, b"old");
        let mut w = Watcher::new(bin.clone());
        let t0 = Instant::now();
        std::fs::write(&bin, b"partial").unwrap();
        assert!(!w.poll(t0));
        std::fs::write(&bin, b"partial and more").unwrap(); // size changed: timer restarts
        assert!(!w.poll(t0 + Duration::from_millis(500)));
        assert!(w.poll(t0 + Duration::from_millis(900)));
        // not executable yet: never ready
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&bin, b"third version!!").unwrap();
        let mut w = Watcher::new(d.join("missing"));
        assert!(!w.poll(t0));
        assert!(!w.poll(t0 + Duration::from_secs(5)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn idle_gate() {
        assert!(idle(&Busy::default()));
        assert!(busy_reason(&Busy::default()).is_none());
        for b in [
            Busy { pointer_down: true, ..Default::default() },
            Busy { dragging: true, ..Default::default() },
            Busy { typing: true, ..Default::default() },
            Busy { modal: true, ..Default::default() },
            Busy { unsnapshottable: true, ..Default::default() },
            Busy { pending_edit: true, ..Default::default() },
        ] {
            assert!(!idle(&b), "{b:?}");
            assert!(busy_reason(&b).is_some());
        }
    }

    #[test]
    fn utc_text_known_dates() {
        assert_eq!(utc_text(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_text(1_700_000_000), "2023-11-14 22:13 UTC");
    }

    #[test]
    fn session_round_trip_keeps_unsaved_edits() {
        let d = tmp("session");
        let mut doc = Document::new("bracket");
        doc.set_param("wall", "3.25").unwrap();
        let snap = Snapshot {
            version: VERSION,
            from_build: "abc".into(),
            pid: 4242,
            doc_path: Some(PathBuf::from("/tmp/bracket.ecad")),
            dirty: true,
            unnamed_work: false,
            camera: CameraSnap {
                target: [1.0, 2.0, 3.0],
                distance: 123.0,
                rot: [0.0, 0.0, 0.0, 1.0],
                fov_y: 0.5,
                ortho: true,
                scene_center: [0.0; 3],
                scene_radius: 50.0,
            },
            display: 2,
            mode: ModeSnap::Parts,
            browser_open: false,
            show_build_plate: true,
            hidden: vec![],
            window: Some(WindowSnap { pos: Some([10.0, 20.0]), size: [1200.0, 800.0], maximized: false }),
            undo_lost: true,
        };
        let json = write_session(&d, &snap, &doc).unwrap();
        let (back, bdoc) = read_session(&json).unwrap();
        assert_eq!(back, snap);
        assert_eq!(bdoc.data.params.get("wall").map(|p| p.expr.0.clone()), Some("3.25".into()));
        assert_eq!(bdoc.data.doc_id, doc.data.doc_id);
        clear_session(&json);
        assert!(!json.exists() && !doc_dir(&d, 4242).exists());
        // a wrong version is refused, not guessed at
        let mut bad = snap.clone();
        bad.version = 99;
        let json = write_session(&d, &bad, &doc).unwrap();
        assert!(read_session(&json).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
