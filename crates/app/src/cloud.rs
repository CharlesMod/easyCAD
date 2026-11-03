//! Google Drive sync in the app: sign-in state, the background sync service
//! (`easycad-sync`), and what the status bar and Settings show. The sync
//! roots are the easyCAD Documents folder, the parts library folder and
//! `settings.json`; everything goes to `easyCAD/` in the user's My Drive.
//!
//! Inert until [`Cloud::start`] (the binary calls it; tests never do), so
//! headless tests never touch the keychain, the network or ~/Documents.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use easycad_sync::auth::{Authenticator, ClientConfig, KeychainStore};
use easycad_sync::config::RootConfig;
use easycad_sync::{BackgroundOptions, ConflictInfo, DriveClient, Engine, EventSink, SyncConfig, SyncEvent, SyncHandle, SyncStatus};

/// What the status bar shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Indicator {
    /// Sync is not available on this computer (no Google client file).
    Hidden,
    Starting,
    SignedOut,
    SigningIn,
    Syncing { index: usize, total: usize },
    Synced,
    Offline,
    Paused,
    SignInNeeded,
    Conflict,
}

impl Indicator {
    pub fn label(&self) -> String {
        match self {
            Indicator::Hidden => String::new(),
            Indicator::Starting => "Drive…".into(),
            Indicator::SignedOut => "Sign in to sync".into(),
            Indicator::SigningIn => "Signing in…".into(),
            Indicator::Syncing { index, total } if *total > 0 => format!("Syncing {index}/{total}"),
            Indicator::Syncing { .. } => "Syncing…".into(),
            Indicator::Synced => "Synced".into(),
            Indicator::Offline => "Offline".into(),
            Indicator::Paused => "Sync paused".into(),
            Indicator::SignInNeeded => "Sign-in needed".into(),
            Indicator::Conflict => "Sync conflict".into(),
        }
    }
}

/// What a frame should tell the user (as toasts).
pub enum Notice {
    Info(String),
    Warn(String),
}

enum Ctl {
    SignedIn,
    SignedOut,
    SignInFailed(String),
    Account(String),
    KeychainError(String),
}

pub struct Cloud {
    /// `None`: sync unavailable (the reason is in `unavailable`).
    auth: Option<Arc<Authenticator>>,
    pub unavailable: Option<String>,
    rt: Option<tokio::runtime::Runtime>,
    ctl_tx: Sender<Ctl>,
    ctl_rx: Receiver<Ctl>,
    handle: Option<SyncHandle>,
    events: Option<Receiver<SyncEvent>>,
    waker: Arc<dyn Fn() + Send + Sync>,
    pub signed_in: Option<bool>,
    pub signing_in: bool,
    pub account: Option<String>,
    pub status: Option<SyncStatus>,
    pub progress: Option<(usize, usize, String)>,
    pub needs_sign_in: Option<String>,
    pub conflicts: Vec<ConflictInfo>,
    pub last_error: Option<String>,
    /// The settings folder, synced as the Settings root (only `settings.json`).
    settings_dir: Option<PathBuf>,
}

impl Default for Cloud {
    fn default() -> Self {
        Cloud::disabled()
    }
}

/// `~/Documents/easyCAD`: where new documents are saved by default.
pub fn documents_dir() -> PathBuf {
    easycad_sync::config::documents_dir()
}

impl Cloud {
    /// No sync (tests, or before `start`).
    pub fn disabled() -> Cloud {
        let (ctl_tx, ctl_rx) = channel();
        Cloud {
            auth: None,
            unavailable: None,
            rt: None,
            ctl_tx,
            ctl_rx,
            handle: None,
            events: None,
            waker: Arc::new(|| {}),
            signed_in: None,
            signing_in: false,
            account: None,
            status: None,
            progress: None,
            needs_sign_in: None,
            conflicts: Vec::new(),
            last_error: None,
            settings_dir: None,
        }
    }

    /// Look for the Google client, check the keychain for a stored sign-in
    /// and, when there is one, start syncing. `EASYCAD_SYNC=off` skips it all.
    pub fn start(ctx: &egui::Context, settings_dir: Option<PathBuf>) -> Cloud {
        let mut c = Cloud::disabled();
        let ctx = ctx.clone();
        // a burst of sync events (one per transferred chunk) becomes at most
        // 10 frames/s: repeated requests keep the earliest pending deadline
        c.waker = Arc::new(move || ctx.request_repaint_after(std::time::Duration::from_millis(100)));
        c.settings_dir = settings_dir;
        if std::env::var("EASYCAD_SYNC").is_ok_and(|v| v == "off") {
            c.unavailable = Some("Sync is turned off (EASYCAD_SYNC=off).".into());
            return c;
        }
        let client = match ClientConfig::discover(None) {
            Ok(cl) => cl,
            Err(e) => {
                c.unavailable = Some(e.to_string());
                return c;
            }
        };
        let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                c.unavailable = Some(e.to_string());
                return c;
            }
        };
        let auth = Arc::new(Authenticator::new(client, Arc::new(KeychainStore::default_entry())));
        let (tx, a, w) = (c.ctl_tx.clone(), auth.clone(), c.waker.clone());
        rt.spawn(async move {
            let _ = tx.send(match a.is_signed_in().await {
                Ok(true) => Ctl::SignedIn,
                Ok(false) => Ctl::SignedOut,
                Err(e) => Ctl::KeychainError(e.to_string()),
            });
            w();
        });
        c.auth = Some(auth);
        c.rt = Some(rt);
        c
    }

    pub fn available(&self) -> bool {
        self.auth.is_some()
    }

    pub fn sign_in(&mut self) {
        let (Some(rt), Some(auth)) = (&self.rt, &self.auth) else { return };
        self.signing_in = true;
        self.last_error = None;
        let (tx, a, w) = (self.ctl_tx.clone(), auth.clone(), self.waker.clone());
        rt.spawn(async move {
            let _ = tx.send(match a.sign_in().await {
                Ok(()) => Ctl::SignedIn,
                Err(e) => Ctl::SignInFailed(e.to_string()),
            });
            w();
        });
    }

    pub fn sign_out(&mut self) {
        let (Some(rt), Some(auth)) = (&self.rt, &self.auth) else { return };
        // dropping the handle stops the thread without blocking the GUI
        self.handle = None;
        self.events = None;
        self.status = None;
        self.progress = None;
        self.account = None;
        self.conflicts.clear();
        let (tx, a, w) = (self.ctl_tx.clone(), auth.clone(), self.waker.clone());
        rt.spawn(async move {
            // the token is deleted here even when the revoke fails
            if let Err(e) = a.sign_out().await {
                eprintln!("easycad sync: sign-out: {e}");
            }
            let _ = tx.send(Ctl::SignedOut);
            w();
        });
    }

    pub fn sync_now(&self) {
        if let Some(h) = &self.handle {
            h.sync_now();
        }
    }

    /// The roots: Documents, Parts Library, Settings (just `settings.json`).
    fn config(&self) -> SyncConfig {
        let mut cfg = SyncConfig::default_roots();
        if let Some(dir) = &self.settings_dir {
            for r in cfg.roots.iter_mut().filter(|r| r.name == "Settings") {
                *r = RootConfig::new("Settings", dir).include("settings.json");
            }
        }
        cfg
    }

    fn start_engine(&mut self) {
        let Some(auth) = self.auth.clone() else { return };
        if let Some(h) = &self.handle {
            h.resume();
            return;
        }
        let (sink, rx) = EventSink::channel();
        let w = self.waker.clone();
        let sink = sink.with_waker(move || w());
        let drive = Arc::new(DriveClient::new(auth.clone()));
        let engine = Engine::new(drive.clone(), self.config(), sink.clone());
        match SyncHandle::spawn(engine, sink, BackgroundOptions::default()) {
            Ok(h) => {
                self.handle = Some(h);
                self.events = Some(rx);
                self.status = Some(SyncStatus::Syncing);
            }
            Err(e) => self.last_error = Some(format!("cannot start sync: {e}")),
        }
        if let Some(rt) = &self.rt {
            let (tx, w) = (self.ctl_tx.clone(), self.waker.clone());
            rt.spawn(async move {
                if let Ok(a) = drive.account().await {
                    if let Some(who) = a.email.or(a.name) {
                        let _ = tx.send(Ctl::Account(who));
                        w();
                    }
                }
            });
        }
    }

    /// Drain sign-in results and sync events; returns what to toast.
    pub fn poll(&mut self, open_doc: Option<&std::path::Path>) -> Vec<Notice> {
        let mut out = Vec::new();
        while let Ok(m) = self.ctl_rx.try_recv() {
            match m {
                Ctl::SignedIn => {
                    let fresh = self.signing_in;
                    self.signing_in = false;
                    self.signed_in = Some(true);
                    self.needs_sign_in = None;
                    self.start_engine();
                    if fresh {
                        out.push(Notice::Info("Signed in to Google Drive: syncing easyCAD/".into()));
                    }
                }
                Ctl::SignedOut => {
                    self.signed_in = Some(false);
                    self.signing_in = false;
                }
                Ctl::SignInFailed(e) => {
                    self.signing_in = false;
                    self.last_error = Some(e.clone());
                    out.push(Notice::Warn(format!("Google sign-in failed: {e}")));
                }
                Ctl::Account(a) => self.account = Some(a),
                Ctl::KeychainError(e) => {
                    self.signed_in = Some(false);
                    self.last_error = Some(format!("keychain: {e}"));
                }
            }
        }
        let Some(rx) = &self.events else { return out };
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        for ev in events {
            match ev {
                SyncEvent::Status(s) => {
                    if !matches!(s, SyncStatus::Syncing) {
                        self.progress = None;
                    }
                    self.status = Some(s);
                }
                SyncEvent::Progress(p) => self.progress = Some((p.index, p.total, p.path)),
                // settings.json: this computer's copy wins quietly
                SyncEvent::Conflict(c) if c.root == "Settings" => {}
                SyncEvent::Conflict(c) => {
                    let name = |p: &std::path::Path| {
                        p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                    };
                    let mine = open_doc.is_some_and(|d| d == c.local_abs);
                    let from = c.remote_device.clone().unwrap_or_else(|| "another device".into());
                    out.push(Notice::Warn(if mine {
                        format!(
                            "This document was also changed on {from}. Yours is kept; theirs was saved as {}",
                            name(&c.conflict_copy_abs)
                        )
                    } else {
                        format!(
                            "Sync conflict: {} changed here and on {from}. Both kept (theirs as {})",
                            name(&c.local_abs),
                            name(&c.conflict_copy_abs)
                        )
                    }));
                    self.conflicts.push(c);
                }
                SyncEvent::AuthRequired { message } => {
                    self.needs_sign_in = Some(message);
                    self.signed_in = Some(false);
                    out.push(Notice::Warn("Google Drive sign-in expired: sign in again in Settings".into()));
                }
                SyncEvent::Error(e) => {
                    let at = e.path.map(|p| format!("{p}: ")).unwrap_or_default();
                    self.last_error = Some(format!("{at}{}", e.message));
                }
                SyncEvent::Completed(r) => {
                    if r.errors.is_empty() {
                        self.last_error = None;
                    }
                }
            }
        }
        out
    }

    pub fn indicator(&self) -> Indicator {
        if !self.available() {
            return Indicator::Hidden;
        }
        if self.signing_in {
            return Indicator::SigningIn;
        }
        if self.needs_sign_in.is_some() {
            return Indicator::SignInNeeded;
        }
        match (self.signed_in, &self.status) {
            (None, _) => Indicator::Starting,
            (Some(false), _) => Indicator::SignedOut,
            (Some(true), Some(SyncStatus::Syncing)) => {
                let (index, total) = self.progress.as_ref().map_or((0, 0), |p| (p.0, p.1));
                Indicator::Syncing { index, total }
            }
            (Some(true), Some(SyncStatus::Offline { .. })) => Indicator::Offline,
            (Some(true), Some(SyncStatus::Paused)) => Indicator::Paused,
            (Some(true), Some(SyncStatus::AuthRequired)) => Indicator::SignInNeeded,
            (Some(true), _) if !self.conflicts.is_empty() => Indicator::Conflict,
            (Some(true), Some(SyncStatus::Idle { .. })) => Indicator::Synced,
            (Some(true), _) => Indicator::Syncing { index: 0, total: 0 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use easycad_sync::auth::MemoryStore;

    fn conflict(root: &str, local: &str) -> SyncEvent {
        SyncEvent::Conflict(ConflictInfo {
            root: root.into(),
            path: "Box.ecad".into(),
            conflict_copy: "Box (conflict lap 2026-11-03 101500).ecad".into(),
            local_abs: PathBuf::from(local),
            conflict_copy_abs: PathBuf::from("/d/Box (conflict lap 2026-11-03 101500).ecad"),
            remote_device: Some("lap".into()),
            remote_modified: None,
            doc_id: None,
        })
    }

    #[test]
    fn disabled_cloud_is_hidden() {
        let c = Cloud::disabled();
        assert_eq!(c.indicator(), Indicator::Hidden);
        assert!(!c.available());
    }

    #[test]
    fn events_drive_the_indicator_and_notices() {
        let mut c = Cloud::disabled();
        c.auth = Some(Arc::new(Authenticator::new(ClientConfig::new("id", "s"), Arc::new(MemoryStore::default()))));
        assert_eq!(c.indicator(), Indicator::Starting);
        c.ctl_tx.send(Ctl::SignedOut).unwrap();
        assert!(c.poll(None).is_empty());
        assert_eq!(c.indicator(), Indicator::SignedOut);
        // as if signed in with the engine running
        let (sink, rx) = EventSink::channel();
        c.events = Some(rx);
        c.signed_in = Some(true);
        sink.emit(SyncEvent::Status(SyncStatus::Syncing));
        sink.emit(SyncEvent::Progress(easycad_sync::events::Progress {
            root: "Documents".into(),
            path: "Box.ecad/doc.json".into(),
            kind: easycad_sync::events::TransferKind::Upload,
            index: 2,
            total: 5,
            bytes_done: 0,
            bytes_total: 10,
        }));
        c.poll(None);
        assert_eq!(c.indicator(), Indicator::Syncing { index: 2, total: 5 });
        sink.emit(SyncEvent::Status(SyncStatus::Idle { last_success: None }));
        c.poll(None);
        assert_eq!(c.indicator(), Indicator::Synced);
        sink.emit(SyncEvent::Status(SyncStatus::Offline { retry_in_secs: 15 }));
        c.poll(None);
        assert_eq!(c.indicator(), Indicator::Offline);
        sink.emit(conflict("Settings", "/c/settings.json"));
        sink.emit(conflict("Documents", "/d/Box.ecad"));
        sink.emit(SyncEvent::Status(SyncStatus::Idle { last_success: None }));
        let n = c.poll(Some(std::path::Path::new("/d/Box.ecad")));
        assert_eq!(n.len(), 1, "settings conflicts stay quiet");
        assert!(matches!(&n[0], Notice::Warn(t) if t.contains("This document was also changed on lap")));
        assert_eq!(c.indicator(), Indicator::Conflict);
        sink.emit(SyncEvent::AuthRequired { message: "expired".into() });
        c.poll(None);
        assert_eq!(c.indicator(), Indicator::SignInNeeded);
    }
}
