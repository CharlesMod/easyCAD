//! Small per-user settings (last-used process profile), stored as JSON in the
//! user's config directory. Local file only.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub last_profile: Option<String>,
    /// Display lengths in inches.
    #[serde(default)]
    pub inch: bool,
    /// Folder of the last file chosen in a file dialog.
    #[serde(default)]
    pub last_dir: Option<std::path::PathBuf>,
    /// Recently opened or saved documents, newest first.
    #[serde(default)]
    pub recent: Vec<std::path::PathBuf>,
}

fn path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("casework").join("settings.json"))
}

impl Settings {
    /// Put a document at the top of the recent list (max 8).
    pub fn push_recent(p: &std::path::Path) {
        let mut s = Settings::load();
        s.recent.retain(|q| q != p);
        s.recent.insert(0, p.to_path_buf());
        s.recent.truncate(8);
        s.save();
    }
    pub fn load() -> Settings {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) {
        if let Some(p) = path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
        }
    }
}
