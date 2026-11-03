//! The local side: scanning a root (with the stat cache), hashing, and
//! atomic writes into place.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use md5::Md5;
use sha2::{Digest, Sha256};

use crate::config::PathFilter;
use crate::state::Stat;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub size: u64,
    pub mtime_ns: i128,
    pub md5: String,
}

/// `(size, mtime in ns)` of a file, `None` when it is not a regular file.
pub fn stat(p: &Path) -> Option<(u64, i128)> {
    let m = std::fs::symlink_metadata(p).ok()?;
    if !m.is_file() {
        return None;
    }
    let t = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    Some((m.len(), t))
}

pub fn md5_file(p: &Path) -> std::io::Result<String> {
    hash_file::<Md5>(p)
}

pub fn sha256_file(p: &Path) -> std::io::Result<String> {
    hash_file::<Sha256>(p)
}

fn hash_file<D: Digest>(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = D::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Every accepted file under `root` (symlinks are skipped), hashed unless
/// `cache` has it at the same size and mtime. `cache` is updated in place
/// and pruned to what exists.
pub fn scan(root: &Path, filter: &PathFilter, cache: &mut BTreeMap<String, Stat>) -> std::io::Result<BTreeMap<String, LocalFile>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if rel.is_empty() => return Err(e),
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            let Ok(name) = ent.file_name().into_string() else { continue };
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let Ok(ft) = ent.file_type() else { continue };
            if ft.is_dir() {
                if filter.enters(&r) {
                    stack.push((ent.path(), r));
                }
            } else if ft.is_file() && filter.accepts(&r) {
                let p = ent.path();
                let Some((size, mtime_ns)) = stat(&p) else { continue };
                let md5 = match cache.get(&r) {
                    Some(c) if c.size == size && c.mtime_ns == mtime_ns => c.md5.clone(),
                    _ => match md5_file(&p) {
                        Ok(h) => h,
                        // vanished or unreadable mid-scan: next pass
                        Err(_) => continue,
                    },
                };
                cache.insert(r.clone(), Stat { size, mtime_ns, md5: md5.clone() });
                out.insert(r, LocalFile { size, mtime_ns, md5 });
            }
        }
    }
    cache.retain(|k, _| out.contains_key(k));
    Ok(out)
}

/// A temp path next to `dest` that the scanner ignores.
pub fn temp_path(dest: &Path) -> PathBuf {
    let n: u64 = rand::random();
    dest.with_file_name(format!(".cwsync-tmp-{n:016x}"))
}

/// Move a local file into `<root>/.cwsync-trash/<date>/<rel>` rather than
/// deleting it, then drop now-empty parent folders up to the root.
pub fn trash_local(root: &Path, rel: &str) -> std::io::Result<()> {
    let src = crate::config::abs_path(root, rel);
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let dst = crate::config::abs_path(&root.join(".cwsync-trash").join(day), rel);
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d)?;
    }
    if std::fs::rename(&src, &dst).is_err() {
        std::fs::remove_file(&src)?;
    }
    let mut dir = src.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || std::fs::remove_dir(&d).is_err() {
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    Ok(())
}

/// `X.ecad/blobs/<64 hex>` (also the old `.cwk`): content-addressed blobs.
pub fn blob_doc(rel: &str) -> Option<(&str, &str)> {
    let (dir, name) = rel.rsplit_once('/')?;
    let doc = dir.strip_suffix("/blobs")?;
    let is_doc = doc.ends_with(".ecad") || doc.ends_with(".cwk");
    (is_doc && name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())).then_some((doc, name))
}

/// `X.ecad` for `X.ecad/doc.json`.
pub fn doc_json_dir(rel: &str) -> Option<&str> {
    let doc = rel.strip_suffix("/doc.json")?;
    (doc.ends_with(".ecad") || doc.ends_with(".cwk")).then_some(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RootConfig;

    #[test]
    fn scan_uses_the_stat_cache() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("A.ecad/blobs")).unwrap();
        std::fs::write(d.path().join("A.ecad/doc.json"), b"{}").unwrap();
        std::fs::write(d.path().join("A.ecad/.cwsync-tmp-1"), b"x").unwrap();
        let f = PathFilter::new(&RootConfig::new("Documents", d.path()));
        let mut cache = BTreeMap::new();
        let s = scan(d.path(), &f, &mut cache).unwrap();
        assert_eq!(s.keys().collect::<Vec<_>>(), ["A.ecad/doc.json"]);
        assert_eq!(s["A.ecad/doc.json"].md5, "99914b932bd37a50b983c5e7c90ae93b");
        // a cached hash at the same size+mtime is trusted
        cache.get_mut("A.ecad/doc.json").unwrap().md5 = "cached".into();
        let s = scan(d.path(), &f, &mut cache).unwrap();
        assert_eq!(s["A.ecad/doc.json"].md5, "cached");
    }

    #[test]
    fn recognises_blobs_and_docs() {
        let h = "a".repeat(64);
        assert_eq!(blob_doc(&format!("x/B.ecad/blobs/{h}")), Some(("x/B.ecad", h.as_str())));
        assert_eq!(blob_doc("B.ecad/blobs/short"), None);
        assert_eq!(doc_json_dir("x/B.ecad/doc.json"), Some("x/B.ecad"));
        assert_eq!(doc_json_dir("doc.json"), None);
    }
}
