//! Background sync for a non-async GUI: a thread running its own tokio
//! runtime, a file watcher on every root (debounced), a poll of the Drive
//! change feed, and a periodic full pass. Offline backs off up to 5 minutes;
//! auth-required pauses until [`SyncHandle::resume`].

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::engine::Engine;
use crate::error::SyncError;
use crate::events::{ErrorInfo, EventSink, SyncEvent, SyncStatus};

#[derive(Debug, Clone)]
pub struct BackgroundOptions {
    /// Quiet time after the last local change before a pass.
    pub debounce: Duration,
    /// How often to ask Drive whether anything changed.
    pub poll: Duration,
    /// A full pass at least this often anyway.
    pub full_pass: Duration,
    pub first_backoff: Duration,
    pub max_backoff: Duration,
    /// Watch the roots for local changes.
    pub watch: bool,
}

impl Default for BackgroundOptions {
    fn default() -> Self {
        BackgroundOptions {
            debounce: Duration::from_secs(2),
            poll: Duration::from_secs(60),
            full_pass: Duration::from_secs(15 * 60),
            first_backoff: Duration::from_secs(15),
            max_backoff: Duration::from_secs(300),
            watch: true,
        }
    }
}

#[derive(Debug)]
enum Cmd {
    SyncNow,
    Pause,
    Resume,
    Stop,
    LocalChange(Vec<std::path::PathBuf>),
}

/// Controls the background thread. Dropping it stops the thread (without
/// waiting); [`SyncHandle::stop`] waits for the current file to finish.
pub struct SyncHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SyncHandle {
    pub fn spawn(engine: Engine, events: EventSink, opts: BackgroundOptions) -> std::io::Result<SyncHandle> {
        let (tx, rx) = mpsc::unbounded_channel();
        let watch_tx = tx.clone();
        let thread = std::thread::Builder::new().name("easycad-sync".into()).spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    events.emit(SyncEvent::Error(ErrorInfo { root: None, path: None, message: e.to_string() }));
                    return;
                }
            };
            rt.block_on(run(engine, events, opts, rx, watch_tx));
        })?;
        Ok(SyncHandle { tx, thread: Some(thread) })
    }

    pub fn sync_now(&self) {
        let _ = self.tx.send(Cmd::SyncNow);
    }
    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }
    /// Continue after a pause or a sign-in, with a pass right away.
    pub fn resume(&self) {
        let _ = self.tx.send(Cmd::Resume);
    }
    pub fn stop(mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for SyncHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
    }
}

fn is_noise(p: &std::path::Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
        n.starts_with(".cwsync") || n.ends_with(".tmp") || n.ends_with(".part") || n.starts_with(".~lock.")
    }) || p.components().any(|c| c.as_os_str() == ".cwsync-trash")
}

async fn run(
    mut engine: Engine,
    events: EventSink,
    opts: BackgroundOptions,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    watch_tx: mpsc::UnboundedSender<Cmd>,
) {
    let _watcher = if opts.watch { watch(&engine, watch_tx) } else { None };
    let written = engine.written();
    let far = Duration::from_secs(365 * 24 * 3600);
    let mut paused = false;
    let mut auth_wait = false;
    let mut dirty: Option<Instant> = None;
    let mut next_full = Instant::now();
    let mut next_poll = Instant::now() + opts.poll;
    let mut backoff = opts.first_backoff;
    loop {
        let wake = if paused || auth_wait {
            Instant::now() + far
        } else {
            let d = dirty.map_or(next_full, |t| (t + opts.debounce).min(next_full));
            d.min(next_poll)
        };
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None | Some(Cmd::Stop) => break,
                Some(Cmd::SyncNow) => next_full = Instant::now(),
                Some(Cmd::Pause) => {
                    paused = true;
                    events.emit(SyncEvent::Status(SyncStatus::Paused));
                }
                Some(Cmd::Resume) => {
                    paused = false;
                    auth_wait = false;
                    backoff = opts.first_backoff;
                    next_full = Instant::now();
                }
                Some(Cmd::LocalChange(paths)) => {
                    // our own downloads and moves are not local edits
                    let w = written.lock().unwrap();
                    let fresh = |p: &std::path::Path| w.get(p).is_some_and(|t| t.elapsed().as_secs() < 10);
                    if paths.is_empty() || paths.iter().any(|p| !fresh(p)) {
                        dirty = Some(Instant::now());
                    }
                }
            },
            _ = tokio::time::sleep_until(wake) => {}
        }
        if paused || auth_wait {
            continue;
        }
        let now = Instant::now();
        let mut due = now >= next_full || dirty.is_some_and(|t| now >= t + opts.debounce);
        let mut failed: Option<SyncError> = None;
        if !due && now >= next_poll {
            match engine.remote_changed().await {
                Ok(changed) => {
                    due = changed;
                    next_poll = now + opts.poll;
                }
                Err(e) => failed = Some(e),
            }
        }
        if due && failed.is_none() {
            dirty = None;
            match engine.sync_once().await {
                Ok(_) => {
                    backoff = opts.first_backoff;
                    next_full = Instant::now() + opts.full_pass;
                    next_poll = Instant::now() + opts.poll;
                }
                Err(e) => failed = Some(e),
            }
        }
        if let Some(e) = failed {
            if e.is_auth() {
                auth_wait = true;
                events.emit(SyncEvent::AuthRequired { message: e.to_string() });
                events.emit(SyncEvent::Status(SyncStatus::AuthRequired));
            } else {
                if e.is_offline() {
                    events.emit(SyncEvent::Status(SyncStatus::Offline { retry_in_secs: backoff.as_secs() }));
                } else {
                    events.emit(SyncEvent::Error(ErrorInfo { root: None, path: None, message: e.to_string() }));
                    events.emit(SyncEvent::Status(SyncStatus::Idle { last_success: None }));
                }
                next_full = Instant::now() + backoff;
                next_poll = next_full + opts.poll;
                backoff = (backoff * 2).min(opts.max_backoff);
            }
        }
    }
    events.emit(SyncEvent::Status(SyncStatus::Stopped));
}

fn watch(engine: &Engine, tx: mpsc::UnboundedSender<Cmd>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if ev.kind.is_access() {
                return;
            }
            let paths: Vec<std::path::PathBuf> = ev.paths.into_iter().filter(|p| !is_noise(p)).collect();
            if !paths.is_empty() {
                let _ = tx.send(Cmd::LocalChange(paths));
            }
        }
    })
    .map_err(|e| tracing::warn!("file watcher unavailable ({e}); relying on polling"))
    .ok()?;
    for r in &engine.config().roots {
        let _ = std::fs::create_dir_all(&r.local);
        if let Err(e) = w.watch(&r.local, notify::RecursiveMode::Recursive) {
            tracing::warn!("cannot watch {}: {e}", r.local.display());
        }
    }
    Some(w)
}
