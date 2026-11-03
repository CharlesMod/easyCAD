//! What to sync: the sync roots (a local folder each, mapped to a subfolder of
//! `easyCAD/` in My Drive), the device name stamped on uploads, and where the
//! state database lives.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

/// One local folder kept in step with `easyCAD/<name>/` on Drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootConfig {
    /// Drive subfolder name, also the key in the state database.
    pub name: String,
    pub local: PathBuf,
    /// When not empty, only paths matching one of these globs sync (the
    /// settings root uses it to sync `settings.json` and nothing else).
    pub include: Vec<String>,
    /// Extra globs to leave alone, on top of the built-in temp-file list.
    pub ignore: Vec<String>,
}

impl RootConfig {
    pub fn new(name: &str, local: impl Into<PathBuf>) -> RootConfig {
        RootConfig { name: name.into(), local: local.into(), include: Vec::new(), ignore: Vec::new() }
    }
    pub fn include(mut self, glob: &str) -> RootConfig {
        self.include.push(glob.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub roots: Vec<RootConfig>,
    /// Stamped on uploads (`cw_dev`) and used in conflict-copy names.
    pub device: String,
    /// The JSON state database.
    pub state_path: PathBuf,
    /// A pass that would delete more than this many files on one side while
    /// the other side is empty stops instead.
    pub max_blind_deletes: usize,
}

impl SyncConfig {
    pub fn new(roots: Vec<RootConfig>, state_path: impl Into<PathBuf>) -> SyncConfig {
        SyncConfig { roots, device: device_name(), state_path: state_path.into(), max_blind_deletes: 10 }
    }

    /// The default roots: Documents (`~/Documents/easyCAD`), Parts Library
    /// (`<data>/easycad/library`) and Settings (`<config>/easycad`, only
    /// `settings.json`), with the state in `<data>/easycad/sync/state.json`.
    pub fn default_roots() -> SyncConfig {
        SyncConfig::new(
            vec![
                RootConfig::new("Documents", documents_dir()),
                RootConfig::new("Parts Library", library_dir()),
                RootConfig::new("Settings", config_dir()).include("settings.json"),
            ],
            data_dir().join("sync").join("state.json"),
        )
    }
}

/// `$XDG_CONFIG_HOME/easycad` (else the platform config dir): the app's
/// settings and the Google client file live here.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("easycad")
}

/// `<data dir>/easycad`: the parts library and the sync state.
pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("easycad")
}

/// `~/Documents/easyCAD`, where new documents are saved by default.
pub fn documents_dir() -> PathBuf {
    dirs::document_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Documents")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("easyCAD")
}

/// The local STEP parts library folder (a `library.json` and its STEP files).
pub fn library_dir() -> PathBuf {
    data_dir().join("library")
}

/// The host name, else "device".
pub fn device_name() -> String {
    let from_env = std::env::var("HOSTNAME").ok().or_else(|| std::env::var("COMPUTERNAME").ok());
    let from_file = || std::fs::read_to_string("/etc/hostname").ok();
    from_env
        .or_else(from_file)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "device".into())
}

/// Names the engine never syncs: its own temp files and editors' and OSes' droppings.
pub const BUILTIN_IGNORES: &[&str] =
    &[".cwsync-*", "*.tmp", "*.step.tmp", "*.part", "~$*", ".~lock.*", ".DS_Store", "Thumbs.db"];

/// Which relative paths of a root take part in sync.
#[derive(Debug, Clone)]
pub struct PathFilter {
    names: GlobSet,
    paths: GlobSet,
    include: Option<GlobSet>,
}

impl PathFilter {
    pub fn new(root: &RootConfig) -> PathFilter {
        let set = |globs: &mut dyn Iterator<Item = &str>| {
            let mut b = GlobSetBuilder::new();
            for g in globs {
                if let Ok(g) = Glob::new(g) {
                    b.add(g);
                }
            }
            b.build().unwrap_or_else(|_| GlobSet::empty())
        };
        let names = set(&mut BUILTIN_IGNORES.iter().copied().chain(root.ignore.iter().map(String::as_str)));
        let paths = set(&mut root.ignore.iter().map(String::as_str));
        let include = (!root.include.is_empty()).then(|| set(&mut root.include.iter().map(String::as_str)));
        PathFilter { names, paths, include }
    }

    /// `rel` is `/`-separated. A path is skipped when any component matches
    /// an ignore glob, or the whole path does, or it misses the include list.
    pub fn accepts(&self, rel: &str) -> bool {
        if rel.split('/').any(|c| self.names.is_match(c)) || self.paths.is_match(rel) {
            return false;
        }
        self.include.as_ref().is_none_or(|inc| inc.is_match(rel))
    }

    /// For the directory walk: whether a folder can hold accepted files.
    pub fn enters(&self, rel_dir: &str) -> bool {
        let last = rel_dir.rsplit('/').next().unwrap_or(rel_dir);
        !self.names.is_match(last) && !self.paths.is_match(rel_dir)
    }
}

/// `root/a/b` for `a/b`.
pub fn abs_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_skips_temp_files_and_honours_include() {
        let f = PathFilter::new(&RootConfig::new("Documents", "/x"));
        assert!(f.accepts("Box.ecad/doc.json"));
        assert!(!f.accepts("Box.ecad/.cwsync-tmp-1"));
        assert!(!f.accepts("a/.DS_Store"));
        assert!(!f.accepts("part.step.tmp"));
        assert!(!f.accepts(".cwsync-trash/x/doc.json"));
        let s = PathFilter::new(&RootConfig::new("Settings", "/x").include("settings.json"));
        assert!(s.accepts("settings.json"));
        assert!(!s.accepts("google_client.json"));
        assert!(!s.accepts("sub/settings.json"));
    }
}
