//! The OAuth client credentials: the JSON file the Google console downloads
//! for a Desktop app client (top-level key `installed`).
//!
//! Desktop "secrets" are not secret in the OAuth sense (Google says so), but
//! the file still never goes into logs, the repository or a sync root.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::AuthError;

pub const GOOGLE_AUTH_URI: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
pub const GOOGLE_REVOKE_URI: &str = "https://oauth2.googleapis.com/revoke";

/// The only scope asked for: files this app created or opened. It needs no
/// Google app verification and cannot read anything else in the user's Drive.
pub const DRIVE_FILE_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";

#[derive(Clone, PartialEq, Eq)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: String,
    pub auth_uri: String,
    pub token_uri: String,
    pub revoke_uri: String,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("client_id", &"<redacted>")
            .field("client_secret", &"<redacted>")
            .field("token_uri", &self.token_uri)
            .finish()
    }
}

#[derive(Deserialize)]
struct FileJson {
    installed: Option<Inner>,
    web: Option<Inner>,
}

#[derive(Deserialize)]
struct Inner {
    client_id: String,
    #[serde(default)]
    client_secret: String,
    auth_uri: Option<String>,
    token_uri: Option<String>,
}

impl ClientConfig {
    pub fn new(client_id: &str, client_secret: &str) -> ClientConfig {
        ClientConfig {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            auth_uri: GOOGLE_AUTH_URI.into(),
            token_uri: GOOGLE_TOKEN_URI.into(),
            revoke_uri: GOOGLE_REVOKE_URI.into(),
        }
    }

    /// Parse the console's `client_secret_<id>.apps.googleusercontent.com.json`.
    pub fn from_json(text: &str) -> Result<ClientConfig, AuthError> {
        let f: FileJson = serde_json::from_str(text)
            .map_err(|e| AuthError::Config(format!("not a Google OAuth client file ({e})")))?;
        let i = f
            .installed
            .or(f.web)
            .ok_or_else(|| AuthError::Config("the client file has no `installed` section".into()))?;
        if i.client_id.trim().is_empty() {
            return Err(AuthError::Config("the client file has an empty client_id".into()));
        }
        let mut c = ClientConfig::new(i.client_id.trim(), i.client_secret.trim());
        if let Some(u) = i.auth_uri {
            // the console still writes the v1 endpoint; v2 is the same flow
            if u != "https://accounts.google.com/o/oauth2/auth" {
                c.auth_uri = u;
            }
        }
        if let Some(u) = i.token_uri {
            c.token_uri = u;
        }
        Ok(c)
    }

    pub fn from_file(path: &Path) -> Result<ClientConfig, AuthError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| AuthError::Config(format!("cannot read {}: {e}", path.display())))?;
        ClientConfig::from_json(&text)
    }

    /// Lookup order: `explicit` (a `--client-file`), env `EASYCAD_GOOGLE_CLIENT_FILE`,
    /// `<config>/easycad/google_client.json`, then env `EASYCAD_GOOGLE_CLIENT_ID` +
    /// `EASYCAD_GOOGLE_CLIENT_SECRET`.
    pub fn discover(explicit: Option<&Path>) -> Result<ClientConfig, AuthError> {
        if let Some(p) = explicit {
            return ClientConfig::from_file(p);
        }
        if let Some(p) = std::env::var_os("EASYCAD_GOOGLE_CLIENT_FILE") {
            return ClientConfig::from_file(Path::new(&p));
        }
        let installed = installed_path();
        if installed.is_file() {
            return ClientConfig::from_file(&installed);
        }
        match (std::env::var("EASYCAD_GOOGLE_CLIENT_ID"), std::env::var("EASYCAD_GOOGLE_CLIENT_SECRET")) {
            (Ok(id), Ok(secret)) if !id.is_empty() => Ok(ClientConfig::new(&id, &secret)),
            _ => Err(AuthError::Config(format!(
                "no Google OAuth client: put the console's Desktop-app JSON at {}",
                installed.display()
            ))),
        }
    }
}

/// `<config>/easycad/google_client.json`.
pub fn installed_path() -> PathBuf {
    crate::config::config_dir().join("google_client.json")
}

/// Copy a client file to [`installed_path`] (mode 0600) so it is found next time.
pub fn install(from: &Path) -> Result<PathBuf, AuthError> {
    let to = installed_path();
    if from.canonicalize().ok() == to.canonicalize().ok() {
        return Ok(to);
    }
    ClientConfig::from_file(from)?;
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::copy(from, &to)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_console_file() {
        let c = ClientConfig::from_json(
            r#"{"installed":{"client_id":"abc.apps.googleusercontent.com","project_id":"p",
            "auth_uri":"https://accounts.google.com/o/oauth2/auth","token_uri":"https://oauth2.googleapis.com/token",
            "client_secret":"s3","redirect_uris":["http://localhost"]}}"#,
        )
        .unwrap();
        assert_eq!(c.client_id, "abc.apps.googleusercontent.com");
        assert_eq!(c.auth_uri, GOOGLE_AUTH_URI);
        assert!(!format!("{c:?}").contains("s3"));
        assert!(ClientConfig::from_json("{}").is_err());
        assert!(ClientConfig::from_json("nope").is_err());
    }
}
