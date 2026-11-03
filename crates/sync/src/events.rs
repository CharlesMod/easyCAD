//! Events the GUI consumes. Delivered on a `std::sync::mpsc` channel so a
//! non-async GUI loop can drain them with `try_recv()` each frame.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use chrono::{DateTime, Utc};

/// Overall state of the sync service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatus {
    Idle { last_success: Option<DateTime<Utc>> },
    Syncing,
    /// Network unavailable; local work continues and sync resumes automatically.
    Offline { retry_in_secs: u64 },
    Paused,
    /// Waiting for the user to sign in again (see [`SyncEvent::AuthRequired`]).
    AuthRequired,
    Stopped,
}

/// What a progress event is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Upload,
    Download,
    Copy,
    DeleteLocal,
    DeleteRemote,
    ConflictCopy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub root: String,
    pub path: String,
    pub kind: TransferKind,
    /// 1-based index of this action in the current plan.
    pub index: usize,
    pub total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Emitted when both sides changed the same file. Both versions are kept:
/// the local version stays at `path` (and is pushed to the remote), the
/// remote version is written to `conflict_copy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictInfo {
    pub root: String,
    /// The conflicted path relative to the root: a `.cwk` folder for
    /// documents, otherwise the file.
    pub path: String,
    /// Relative path of the copy holding the remote version.
    pub conflict_copy: String,
    /// Absolute paths, for convenience.
    pub local_abs: PathBuf,
    pub conflict_copy_abs: PathBuf,
    pub remote_device: Option<String>,
    pub remote_modified: Option<DateTime<Utc>>,
    pub doc_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorInfo {
    pub root: Option<String>,
    pub path: Option<String>,
    pub message: String,
}

/// Per-pass summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub uploaded: usize,
    pub copied_remote: usize,
    pub downloaded: usize,
    pub deleted_local: usize,
    pub deleted_remote: usize,
    pub conflicts: Vec<ConflictInfo>,
    pub errors: Vec<ErrorInfo>,
    /// Actions skipped because a file changed while syncing; they are
    /// picked up by the next pass.
    pub deferred: usize,
}

impl SyncReport {
    pub fn is_noop(&self) -> bool {
        self.uploaded + self.copied_remote + self.downloaded + self.deleted_local
            + self.deleted_remote
            + self.conflicts.len()
            == 0
    }
    pub(crate) fn merge(&mut self, o: SyncReport) {
        self.uploaded += o.uploaded;
        self.copied_remote += o.copied_remote;
        self.downloaded += o.downloaded;
        self.deleted_local += o.deleted_local;
        self.deleted_remote += o.deleted_remote;
        self.conflicts.extend(o.conflicts);
        self.errors.extend(o.errors);
        self.deferred += o.deferred;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEvent {
    Status(SyncStatus),
    Progress(Progress),
    Conflict(ConflictInfo),
    /// The stored Google token is missing or was revoked/expired. Show a
    /// "Sign in to Google" prompt, call `Authenticator::sign_in`, then
    /// `SyncHandle::resume()`.
    AuthRequired { message: String },
    Error(ErrorInfo),
    /// A pass finished (possibly with per-file errors listed in the report).
    Completed(SyncReport),
}

type Waker = Arc<dyn Fn() + Send + Sync>;

/// Sending half of the event channel, with an optional waker (e.g. egui's
/// `ctx.request_repaint()`) invoked after each event.
#[derive(Clone)]
pub struct EventSink {
    tx: Option<mpsc::Sender<SyncEvent>>,
    waker: Option<Waker>,
}

impl EventSink {
    /// A new sink and the receiver the GUI drains.
    pub fn channel() -> (EventSink, mpsc::Receiver<SyncEvent>) {
        let (tx, rx) = mpsc::channel();
        (EventSink { tx: Some(tx), waker: None }, rx)
    }

    /// A sink that drops everything.
    pub fn null() -> EventSink {
        EventSink { tx: None, waker: None }
    }

    pub fn with_waker(mut self, waker: impl Fn() + Send + Sync + 'static) -> Self {
        self.waker = Some(Arc::new(waker));
        self
    }

    pub fn emit(&self, ev: SyncEvent) {
        if let Some(tx) = &self.tx {
            // A dropped receiver just means nobody is listening.
            let _ = tx.send(ev);
        }
        if let Some(w) = &self.waker {
            w();
        }
    }
}

impl std::fmt::Debug for EventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSink").field("connected", &self.tx.is_some()).finish()
    }
}
