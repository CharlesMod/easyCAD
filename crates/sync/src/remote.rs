//! The storage abstraction the sync engine runs against.
//!
//! [`crate::drive::DriveClient`] implements it for Google Drive and
//! [`crate::memory::MemoryRemote`] implements it in memory for tests.
//! Files are addressed by `(root, path)` where `root` is a sync root name
//! (a subfolder of `easyCAD/`) and `path` is a `/`-separated relative path.

use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::RemoteError;

/// Metadata we attach to every remote file (Drive `appProperties`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    /// Device that last uploaded this file.
    pub device: Option<String>,
    /// `doc_id` from a `doc.json`, when the file is one.
    pub doc_id: Option<String>,
    /// `revision` from a `doc.json`, when the file is one.
    pub revision: Option<i64>,
}

/// A file as the remote currently has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFile {
    pub id: String,
    pub root: String,
    pub path: String,
    pub size: u64,
    /// Drive `md5Checksum` (lowercase hex).
    pub md5: Option<String>,
    /// Drive `headRevisionId`.
    pub head_revision_id: Option<String>,
    /// Drive `version` (monotonic per file).
    pub version: Option<i64>,
    pub modified_time: Option<DateTime<Utc>>,
    pub meta: FileMeta,
}

/// Everything a store needs to upload one local file.
#[derive(Debug, Clone)]
pub struct UploadRequest {
    pub root: String,
    pub path: String,
    pub local_path: std::path::PathBuf,
    /// `Some` updates that remote file in place (keeps its id and revision history).
    pub existing_id: Option<String>,
    pub size: u64,
    pub meta: FileMeta,
}

/// One entry of the remote change feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    pub id: String,
    /// Gone for good, or in the trash.
    pub removed: bool,
    /// The file's version after the change, when the feed says.
    pub version: Option<i64>,
    pub folder: bool,
}

/// Result of polling the remote change feed.
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    /// Number of changed (or removed) files visible to the app.
    pub changed: usize,
    /// Token to use for the next poll.
    pub new_token: String,
    /// The changes themselves (the engine skips the ones it made).
    pub changes: Vec<Change>,
}

/// Progress callback: `(bytes_done, bytes_total)`.
pub type ProgressFn<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

#[async_trait]
pub trait RemoteStore: Send + Sync {
    /// Identity of the remote folder backing `root`, creating it if needed.
    /// If this changes between syncs (different account, folder trashed and
    /// recreated), the engine discards its last-synced state for that root
    /// rather than interpreting the difference as deletions.
    async fn root_id(&self, root: &str) -> Result<String, RemoteError>;

    /// All live (non-trashed) files under `root`, across all pages.
    async fn list(&self, root: &str) -> Result<Vec<RemoteFile>, RemoteError>;

    /// Current metadata of a file, or `None` if it is gone or trashed.
    async fn get(&self, id: &str) -> Result<Option<RemoteFile>, RemoteError>;

    /// Download file content to `dest` (the engine passes a temp path and
    /// renames it into place after verifying checksums).
    async fn download(
        &self,
        file: &RemoteFile,
        dest: &Path,
        progress: ProgressFn<'_>,
    ) -> Result<(), RemoteError>;

    /// Size above which the engine uses `begin_resumable`/`resume_upload`.
    fn resumable_threshold(&self) -> u64 {
        5 * 1024 * 1024
    }

    /// Single-request upload (Drive multipart) for small files.
    async fn upload(&self, req: &UploadRequest) -> Result<RemoteFile, RemoteError>;

    /// Start a resumable upload; returns a session handle that the engine
    /// persists so an interrupted upload can continue after a restart.
    async fn begin_resumable(&self, req: &UploadRequest) -> Result<String, RemoteError>;

    /// Send (the rest of) the file for a resumable session. Asks the remote
    /// how much it already has and continues from there. Returns
    /// [`RemoteError::SessionExpired`] if the session must be restarted.
    async fn resume_upload(
        &self,
        session: &str,
        req: &UploadRequest,
        progress: ProgressFn<'_>,
    ) -> Result<RemoteFile, RemoteError>;

    /// Server-side copy (no data transfer); used to dedupe identical content.
    async fn copy(
        &self,
        src_id: &str,
        root: &str,
        path: &str,
        meta: &FileMeta,
    ) -> Result<RemoteFile, RemoteError>;

    /// Move to trash (recoverable for 30 days on Drive), never hard-delete.
    async fn trash(&self, id: &str) -> Result<(), RemoteError>;

    /// Token marking "now" in the change feed.
    async fn start_page_token(&self) -> Result<String, RemoteError>;

    /// Changes since `token`.
    async fn changes_since(&self, token: &str) -> Result<ChangeSet, RemoteError>;
}
