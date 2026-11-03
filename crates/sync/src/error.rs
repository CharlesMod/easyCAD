//! Error types. Nothing in this crate panics on auth, network or remote
//! failures; everything is reported through these types and `SyncEvent`s.

use std::path::PathBuf;

/// Errors from the OAuth2 layer.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// No refresh token is stored: the user has never signed in, or signed out.
    #[error("not signed in")]
    NotSignedIn,
    /// The stored refresh token was rejected (revoked, expired, or the
    /// client was changed). The token has been deleted; the user must sign in again.
    #[error("sign-in required: {0}")]
    ReauthRequired(String),
    /// Transport failure (offline, DNS, TLS, timeout) or a transient 5xx.
    /// The stored token is kept; retry later.
    #[error("network error: {0}")]
    Network(String),
    /// The authorization server answered with an error we cannot recover from automatically.
    #[error("OAuth error: {0}")]
    OAuth(String),
    /// The user denied consent in the browser.
    #[error("access denied by user: {0}")]
    Denied(String),
    /// The browser flow did not complete in time.
    #[error("sign-in timed out")]
    Timeout,
    /// OS keychain could not be read or written.
    #[error("keychain error: {0}")]
    Keychain(String),
    /// Missing or malformed client credentials.
    #[error("client configuration error: {0}")]
    Config(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl AuthError {
    /// True when the only fix is an interactive sign-in.
    pub fn needs_sign_in(&self) -> bool {
        matches!(self, AuthError::NotSignedIn | AuthError::ReauthRequired(_))
    }
}

/// Errors from a [`crate::RemoteStore`].
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// Interactive sign-in is required before the store can be used again.
    #[error("authentication required: {0}")]
    AuthRequired(String),
    /// Network unreachable / connection dropped / retries exhausted on transient errors.
    #[error("offline: {0}")]
    Offline(String),
    #[error("not found")]
    NotFound,
    /// A resumable upload session is no longer valid; start a new one.
    #[error("upload session expired")]
    SessionExpired,
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl RemoteError {
    /// Errors that abort a whole sync pass (as opposed to failing one file).
    pub fn is_fatal_for_pass(&self) -> bool {
        matches!(self, RemoteError::AuthRequired(_) | RemoteError::Offline(_))
    }
}

impl From<AuthError> for RemoteError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::NotSignedIn | AuthError::ReauthRequired(_) => {
                RemoteError::AuthRequired(e.to_string())
            }
            AuthError::Network(m) => RemoteError::Offline(m),
            other => RemoteError::Other(other.to_string()),
        }
    }
}

/// Errors from the sync engine.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Remote(#[from] RemoteError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("state database error: {0}")]
    State(String),
    /// A sync root folder that previously held synced files is missing
    /// (unmounted drive?). Nothing is deleted remotely.
    #[error("sync root {0} is missing; refusing to treat it as deleted")]
    RootMissing(PathBuf),
    /// The plan would delete many files at once on one side because the
    /// other side looks empty. Sync for that root is stopped instead.
    #[error("safety stop: {0}")]
    SafetyStop(String),
}

impl SyncError {
    pub fn is_offline(&self) -> bool {
        matches!(self, SyncError::Remote(RemoteError::Offline(_)))
    }
    pub fn is_auth(&self) -> bool {
        matches!(self, SyncError::Remote(RemoteError::AuthRequired(_)))
    }
}
