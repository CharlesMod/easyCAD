//! easycad-sync: Google Drive sync for easyCAD.
//!
//! - [`auth`]: OAuth2 (PKCE + loopback redirect), refresh token in the OS keychain.
//! - [`drive::DriveClient`]: Drive v3 as a [`RemoteStore`]; [`memory::MemoryRemote`] fakes it.
//! - [`engine::Engine`]: the local-first three-way sync of the [`config`] roots.
//! - [`background::SyncHandle`]: the engine on a thread, for the GUI.
//!
//! Design notes are in `TODO.md` next to this crate.

pub mod auth;
pub mod background;
pub mod config;
pub mod drive;
pub mod engine;
pub mod error;
pub mod events;
pub mod local;
pub mod memory;
pub mod plan;
pub mod remote;
pub mod state;

pub use auth::{Authenticator, ClientConfig, KeychainStore, TokenProvider, TokenStore};
pub use background::{BackgroundOptions, SyncHandle};
pub use config::{RootConfig, SyncConfig};
pub use drive::DriveClient;
pub use engine::Engine;
pub use error::{AuthError, RemoteError, SyncError};
pub use events::{ConflictInfo, EventSink, SyncEvent, SyncReport, SyncStatus};
pub use memory::MemoryRemote;
pub use remote::{ChangeSet, FileMeta, RemoteFile, RemoteStore, UploadRequest};
