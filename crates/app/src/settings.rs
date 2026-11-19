//! Per-user settings (navigation, units, mesh quality, autosave, default and
//! last-used process, recent files), stored as JSON in the user's config
//! directory. Local file only.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// Process profile for new documents (else the last one used).
    #[serde(default)]
    pub default_profile: Option<String>,
    #[serde(default)]
    pub nav: NavPreset,
    #[serde(default)]
    pub mesh: MeshPref,
    /// Minutes between recovery copies; 0 = off.
    #[serde(default = "two")]
    pub autosave_min: u32,
    /// Restart into a newly installed build, keeping the session (see reload.rs).
    #[serde(default = "yes")]
    pub auto_reload: bool,
}

fn yes() -> bool {
    true
}

fn two() -> u32 {
    2
}

/// Mouse navigation presets (wheel zooms in all of them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NavPreset {
    /// Shift+middle orbits, middle pans (Fusion 360 style).
    #[default]
    easyCAD,
    /// Middle orbits, Ctrl+middle pans.
    SolidWorks,
    /// Right orbits, middle pans.
    Onshape,
    /// Middle orbits, Shift+middle pans.
    Blender,
}

impl NavPreset {
    pub const ALL: [NavPreset; 4] = [
        NavPreset::easyCAD,
        NavPreset::SolidWorks,
        NavPreset::Onshape,
        NavPreset::Blender,
    ];
    pub fn label(self) -> &'static str {
        match self {
            NavPreset::easyCAD => "Fusion 360 / easyCAD",
            NavPreset::SolidWorks => "SolidWorks",
            NavPreset::Onshape => "Onshape",
            NavPreset::Blender => "Blender",
        }
    }
    /// (orbit, pan) hints for the status bar and the cheat sheet.
    pub fn hints(self) -> (&'static str, &'static str) {
        match self {
            NavPreset::easyCAD => ("Shift+MMB", "MMB"),
            NavPreset::SolidWorks => ("MMB", "Ctrl+MMB"),
            NavPreset::Onshape => ("RMB", "MMB"),
            NavPreset::Blender => ("MMB", "Shift+MMB"),
        }
    }
    /// What a drag with these buttons / modifiers does.
    pub fn classify(self, middle: bool, right: bool, shift: bool, ctrl: bool) -> Option<Nav> {
        match self {
            NavPreset::easyCAD if middle => Some(if shift { Nav::Orbit } else { Nav::Pan }),
            NavPreset::SolidWorks if middle => Some(if ctrl { Nav::Pan } else { Nav::Orbit }),
            NavPreset::Onshape if right => Some(Nav::Orbit),
            NavPreset::Onshape if middle => Some(Nav::Pan),
            NavPreset::Blender if middle => Some(if shift { Nav::Pan } else { Nav::Orbit }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Orbit,
    Pan,
}

/// Viewport tessellation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeshPref {
    Coarse,
    #[default]
    Normal,
    Fine,
}

impl MeshPref {
    pub const ALL: [MeshPref; 3] = [MeshPref::Coarse, MeshPref::Normal, MeshPref::Fine];
    pub fn label(self) -> &'static str {
        match self {
            MeshPref::Coarse => "Coarse (fastest)",
            MeshPref::Normal => "Normal",
            MeshPref::Fine => "Fine (export quality)",
        }
    }
    pub fn quality(self) -> easycad_model::regen::MeshQuality {
        use easycad_model::regen::MeshQuality as Q;
        match self {
            MeshPref::Coarse => Q::Preview,
            MeshPref::Normal => Q::Display,
            MeshPref::Fine => Q::Final,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        // every field defaults the way serde fills a missing one
        serde_json::from_str("{}").expect("settings defaults")
    }
}

/// The folder holding `settings.json` (also the Google client file).
pub fn dir() -> Option<std::path::PathBuf> {
    path().and_then(|p| p.parent().map(std::path::Path::to_path_buf))
}

fn path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("easycad").join("settings.json"))
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
            let text = serde_json::to_string_pretty(self).unwrap_or_default();
            // unchanged settings are not rewritten (each write is also a sync pass)
            if std::fs::read_to_string(&p).ok().as_deref() != Some(text.as_str()) {
                let _ = std::fs::write(p, text);
            }
        }
    }
}
