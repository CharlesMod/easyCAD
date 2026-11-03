//! casework-sync: Google Drive sync for Casework. WORK IN PROGRESS, see TODO.md.
//!
//! This crate is deliberately not a member of the root workspace (it has its
//! own `[workspace]` table) so the default Casework build stays offline-only.

pub mod error;
pub mod events;
pub mod remote;

pub use error::{AuthError, RemoteError, SyncError};
pub use events::{ConflictInfo, EventSink, SyncEvent, SyncReport, SyncStatus};
pub use remote::{ChangeSet, FileMeta, RemoteFile, RemoteStore, UploadRequest};
