//! Google Drive v3 as a [`RemoteStore`].
//!
//! Layout: `easyCAD/<root>/…` in My Drive, real subfolders mirroring the local
//! tree so the user can browse it, but files are found by `appProperties`
//! (`cw_root` plus the path in `cw_p0..cw_pN`), never by folder walking.
//! Transient failures (429, 5xx, rate-limit 403, network) are retried with
//! jittered exponential backoff; a Drive 401 refreshes the token and retries once.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::auth::TokenProvider;
use crate::error::RemoteError;
use crate::remote::{Change, ChangeSet, FileMeta, ProgressFn, RemoteFile, RemoteStore, UploadRequest};

const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const FIELDS: &str = "id,size,md5Checksum,headRevisionId,version,modifiedTime,appProperties,trashed";
/// Resumable chunk size (a multiple of 256 KiB, as Drive requires).
pub const CHUNK: u64 = 8 * 1024 * 1024;
/// Drive caps each appProperties key + value at 124 bytes.
const PATH_PART: usize = 100;

pub struct DriveClient {
    auth: Arc<dyn TokenProvider>,
    http: reqwest::Client,
    api: String,
    app_folder: String,
    attempts: u32,
    backoff: Duration,
    chunk: u64,
    /// `root id + "/" + dir path` → folder id
    folders: Mutex<HashMap<String, String>>,
    /// root name → its folder id, refreshed by every `root_id` call
    roots: Mutex<HashMap<String, String>>,
}

/// The signed-in account, from `about.get`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Account {
    pub email: Option<String>,
    pub name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFile {
    id: String,
    size: Option<String>,
    md5_checksum: Option<String>,
    head_revision_id: Option<String>,
    version: Option<String>,
    modified_time: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    app_properties: BTreeMap<String, String>,
    #[serde(default)]
    trashed: bool,
}

impl DriveFile {
    /// `None` for files that are not ours (no `cw_root`).
    fn into_remote(self) -> Option<RemoteFile> {
        let p = &self.app_properties;
        let root = p.get("cw_root")?.clone();
        let mut path = String::new();
        for i in 0.. {
            match p.get(&format!("cw_p{i}")) {
                Some(s) => path.push_str(s),
                None => break,
            }
        }
        if path.is_empty() {
            return None;
        }
        Some(RemoteFile {
            meta: FileMeta {
                device: p.get("cw_dev").cloned(),
                doc_id: p.get("cw_doc").cloned(),
                revision: p.get("cw_rev").and_then(|r| r.parse().ok()),
            },
            id: self.id,
            root,
            path,
            size: self.size.and_then(|s| s.parse().ok()).unwrap_or(0),
            md5: self.md5_checksum,
            head_revision_id: self.head_revision_id,
            version: self.version.and_then(|s| s.parse().ok()),
            modified_time: self.modified_time,
        })
    }
}

/// `cw_*` appProperties for a file.
fn app_properties(root: &str, path: &str, meta: &FileMeta) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("cw_root".into(), root.into());
    let mut rest = path;
    let mut i = 0;
    while !rest.is_empty() {
        let mut cut = rest.len().min(PATH_PART);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        m.insert(format!("cw_p{i}"), rest[..cut].into());
        rest = &rest[cut..];
        i += 1;
    }
    if let Some(d) = &meta.device {
        m.insert("cw_dev".into(), d.chars().take(100).collect::<String>().into());
    }
    if let Some(d) = &meta.doc_id {
        m.insert("cw_doc".into(), d.clone().into());
    }
    if let Some(r) = meta.revision {
        m.insert("cw_rev".into(), r.to_string().into());
    }
    Value::Object(m)
}

/// Quote a value for a Drive `q` string.
fn q_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dir_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

enum Transient {
    Yes(RemoteError),
    No(RemoteError),
}

impl DriveClient {
    pub fn new(auth: Arc<dyn TokenProvider>) -> DriveClient {
        let policy = reqwest::redirect::Policy::custom(|a| {
            // 308 is Drive's "resume incomplete", not a redirect
            if a.status().as_u16() == 308 {
                a.stop()
            } else {
                a.follow()
            }
        });
        DriveClient {
            auth,
            http: reqwest::Client::builder()
                .redirect(policy)
                .connect_timeout(Duration::from_secs(20))
                .read_timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            api: "https://www.googleapis.com".into(),
            app_folder: "easyCAD".into(),
            attempts: 5,
            backoff: Duration::from_millis(500),
            chunk: CHUNK,
            folders: Mutex::new(HashMap::new()),
            roots: Mutex::new(HashMap::new()),
        }
    }

    /// Point at another server (tests use wiremock).
    pub fn with_base_url(mut self, base: &str) -> Self {
        self.api = base.trim_end_matches('/').into();
        self
    }

    /// Name of the top folder in My Drive (default `easyCAD`).
    pub fn with_app_folder(mut self, name: &str) -> Self {
        self.app_folder = name.into();
        self
    }

    pub fn with_retry(mut self, attempts: u32, base: Duration) -> Self {
        self.attempts = attempts.max(1);
        self.backoff = base;
        self
    }

    /// Resumable chunk size (rounded to a multiple of 256 KiB).
    pub fn with_chunk(mut self, bytes: u64) -> Self {
        self.chunk = (bytes / (256 * 1024)).max(1) * 256 * 1024;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}/drive/v3/{path}", self.api)
    }

    fn upload_url(&self, path: &str) -> String {
        format!("{}/upload/drive/v3/{path}", self.api)
    }

    fn delay(&self, attempt: u32) -> Duration {
        let base = self.backoff.as_millis() as u64;
        let exp = base.saturating_mul(1 << attempt.min(6));
        let jitter = rand::rng().random_range(0..=base.max(1));
        Duration::from_millis(exp + jitter)
    }

    /// Send with auth, retries and error mapping. 2xx and 308 are returned.
    async fn send(
        &self,
        mk: &(dyn Fn() -> reqwest::RequestBuilder + Send + Sync),
    ) -> Result<reqwest::Response, RemoteError> {
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            match self.send_once(mk, &mut refreshed).await {
                Ok(r) => return Ok(r),
                Err(Transient::Yes(e)) if attempt + 1 < self.attempts => {
                    tracing::debug!("drive: retrying after {e}");
                    tokio::time::sleep(self.delay(attempt)).await;
                    attempt += 1;
                }
                Err(Transient::Yes(e)) => {
                    return Err(match e {
                        RemoteError::Offline(m) => RemoteError::Offline(m),
                        other => RemoteError::Offline(format!("Drive kept failing: {other}")),
                    })
                }
                Err(Transient::No(e)) => return Err(e),
            }
        }
    }

    async fn send_once(
        &self,
        mk: &(dyn Fn() -> reqwest::RequestBuilder + Send + Sync),
        refreshed: &mut bool,
    ) -> Result<reqwest::Response, Transient> {
        let token = self.auth.access_token().await.map_err(|e| {
            let e = RemoteError::from(e);
            if matches!(e, RemoteError::Offline(_)) {
                Transient::Yes(e)
            } else {
                Transient::No(e)
            }
        })?;
        let resp = mk()
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| Transient::Yes(RemoteError::Offline(e.to_string())))?;
        let status = resp.status().as_u16();
        if resp.status().is_success() || status == 308 {
            return Ok(resp);
        }
        if status == 401 {
            self.auth.invalidate();
            if !*refreshed {
                *refreshed = true;
                return Err(Transient::Yes(RemoteError::Http { status, message: "unauthorized".into() }));
            }
            return Err(Transient::No(RemoteError::AuthRequired("Drive rejected the access token".into())));
        }
        let body = resp.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| body.chars().take(200).collect());
        let e = match status {
            404 => RemoteError::NotFound,
            _ => RemoteError::Http { status, message },
        };
        let rate_limited = status == 403 && (body.contains("rateLimitExceeded") || body.contains("userRateLimitExceeded"));
        if status == 429 || status >= 500 || rate_limited {
            Err(Transient::Yes(e))
        } else {
            Err(Transient::No(e))
        }
    }

    async fn json(&self, mk: &(dyn Fn() -> reqwest::RequestBuilder + Send + Sync)) -> Result<Value, RemoteError> {
        let r = self.send(mk).await?;
        r.json().await.map_err(|e| RemoteError::Offline(e.to_string()))
    }

    fn file_from(v: Value) -> Result<RemoteFile, RemoteError> {
        let f: DriveFile = serde_json::from_value(v).map_err(|e| RemoteError::Other(format!("bad Drive file: {e}")))?;
        f.into_remote().ok_or_else(|| RemoteError::Other("Drive file lacks easyCAD properties".into()))
    }

    /// The signed-in user (works with the `drive.file` scope).
    pub async fn account(&self) -> Result<Account, RemoteError> {
        let url = self.url("about");
        let v = self.json(&|| self.http.get(&url).query(&[("fields", "user(emailAddress,displayName)")])).await?;
        Ok(Account {
            email: v["user"]["emailAddress"].as_str().map(String::from),
            name: v["user"]["displayName"].as_str().map(String::from),
        })
    }

    /// Find a folder by name under `parent`, else create it.
    async fn folder(&self, parent: &str, name: &str) -> Result<String, RemoteError> {
        let q = format!(
            "name = {} and mimeType = '{FOLDER_MIME}' and {} in parents and trashed = false",
            q_str(name),
            q_str(parent)
        );
        let url = self.url("files");
        let v = self
            .json(&|| self.http.get(&url).query(&[("q", q.as_str()), ("fields", "files(id)"), ("spaces", "drive")]))
            .await?;
        if let Some(id) = v["files"].get(0).and_then(|f| f["id"].as_str()) {
            return Ok(id.into());
        }
        let body = json!({ "name": name, "mimeType": FOLDER_MIME, "parents": [parent] });
        let v = self.json(&|| self.http.post(&url).query(&[("fields", "id")]).json(&body)).await?;
        v["id"].as_str().map(String::from).ok_or_else(|| RemoteError::Other("folder create returned no id".into()))
    }

    /// The folder for `dir` (`/`-separated, may be empty) under the root folder.
    async fn dir_id(&self, root: &str, dir: &str) -> Result<String, RemoteError> {
        let cached = self.roots.lock().unwrap().get(root).cloned();
        let mut id = match cached {
            Some(id) => id,
            None => self.root_id(root).await?,
        };
        let mut key = id.clone();
        for part in dir.split('/').filter(|p| !p.is_empty()) {
            key = format!("{key}/{part}");
            let cached = self.folders.lock().unwrap().get(&key).cloned();
            id = match cached {
                Some(c) => c,
                None => {
                    let c = self.folder(&id, part).await?;
                    self.folders.lock().unwrap().insert(key.clone(), c.clone());
                    c
                }
            };
        }
        Ok(id)
    }

    fn metadata(&self, req: &UploadRequest, parent: Option<&str>) -> Value {
        let mut m = json!({
            "name": name_of(&req.path),
            "appProperties": app_properties(&req.root, &req.path, &req.meta),
        });
        if let Some(p) = parent {
            m["parents"] = json!([p]);
        }
        m
    }

    /// Ask a resumable session how far it got.
    async fn session_offset(&self, session: &str, total: u64) -> Result<Result<u64, RemoteFile>, RemoteError> {
        let range = format!("bytes */{total}");
        let r = self.send(&|| self.http.put(session).header("Content-Range", &range).header("Content-Length", "0")).await;
        let r = match r {
            Err(RemoteError::NotFound) | Err(RemoteError::Http { status: 410, .. }) => {
                return Err(RemoteError::SessionExpired)
            }
            Err(e) => return Err(e),
            Ok(r) => r,
        };
        if r.status().as_u16() == 308 {
            return Ok(Ok(next_offset(&r)));
        }
        let v: Value = r.json().await.map_err(|e| RemoteError::Offline(e.to_string()))?;
        Ok(Err(Self::file_from(v)?))
    }

    /// Send chunks from `offset`; a failed chunk is not resent blindly, the
    /// caller asks the session for its offset again.
    async fn put_chunks(
        &self,
        session: &str,
        req: &UploadRequest,
        mut offset: u64,
        progress: ProgressFn<'_>,
    ) -> Result<RemoteFile, Transient> {
        let total = req.size;
        let mut f = tokio::fs::File::open(&req.local_path)
            .await
            .map_err(|e| Transient::No(RemoteError::Io(e)))?;
        loop {
            let len = self.chunk.min(total - offset);
            let mut buf = vec![0u8; len as usize];
            f.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| Transient::No(e.into()))?;
            f.read_exact(&mut buf).await.map_err(|e| Transient::No(e.into()))?;
            let range = if total == 0 {
                "bytes */0".to_string()
            } else {
                format!("bytes {offset}-{}/{total}", offset + len - 1)
            };
            let token = self.auth.access_token().await.map_err(|e| Transient::No(e.into()))?;
            let r = self
                .http
                .put(session)
                .bearer_auth(token)
                .header("Content-Range", range)
                .body(buf)
                .send()
                .await
                .map_err(|e| Transient::Yes(RemoteError::Offline(e.to_string())))?;
            let status = r.status().as_u16();
            match status {
                308 => {
                    offset = next_offset(&r);
                    progress(offset, total);
                }
                200 | 201 => {
                    progress(total, total);
                    let v: Value = r.json().await.map_err(|e| Transient::Yes(RemoteError::Offline(e.to_string())))?;
                    return Self::file_from(v).map_err(Transient::No);
                }
                404 | 410 => return Err(Transient::No(RemoteError::SessionExpired)),
                401 => {
                    self.auth.invalidate();
                    return Err(Transient::Yes(RemoteError::Http { status, message: "unauthorized".into() }));
                }
                s if s == 429 || s >= 500 => {
                    return Err(Transient::Yes(RemoteError::Http { status, message: "transient".into() }))
                }
                _ => {
                    let message = r.text().await.unwrap_or_default();
                    return Err(Transient::No(RemoteError::Http { status, message }));
                }
            }
        }
    }
}

/// From a 308's `Range: bytes=0-N`, the next byte to send.
fn next_offset(r: &reqwest::Response) -> u64 {
    r.headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.rsplit('-').next())
        .and_then(|n| n.trim().parse::<u64>().ok())
        .map_or(0, |n| n + 1)
}

#[async_trait]
impl RemoteStore for DriveClient {
    async fn root_id(&self, root: &str) -> Result<String, RemoteError> {
        // looked up afresh each pass: a trashed and recreated folder gets a new id
        let app = self.folder("root", &self.app_folder).await?;
        let id = self.folder(&app, root).await?;
        self.roots.lock().unwrap().insert(root.into(), id.clone());
        Ok(id)
    }

    async fn list(&self, root: &str) -> Result<Vec<RemoteFile>, RemoteError> {
        let q = format!("appProperties has {{ key='cw_root' and value={} }} and trashed = false", q_str(root));
        let fields = format!("nextPageToken,files({FIELDS})");
        let url = self.url("files");
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut query = vec![
                ("q", q.clone()),
                ("fields", fields.clone()),
                ("pageSize", "1000".into()),
                ("spaces", "drive".into()),
            ];
            if let Some(p) = &page {
                query.push(("pageToken", p.clone()));
            }
            let v = self.json(&|| self.http.get(&url).query(&query)).await?;
            if let Some(files) = v["files"].as_array() {
                for f in files {
                    if let Ok(df) = serde_json::from_value::<DriveFile>(f.clone()) {
                        if let Some(rf) = df.into_remote().filter(|r| r.root == root) {
                            out.push(rf);
                        }
                    }
                }
            }
            match v["nextPageToken"].as_str() {
                Some(t) => page = Some(t.into()),
                None => return Ok(out),
            }
        }
    }

    async fn get(&self, id: &str) -> Result<Option<RemoteFile>, RemoteError> {
        let url = self.url(&format!("files/{id}"));
        match self.json(&|| self.http.get(&url).query(&[("fields", FIELDS)])).await {
            Ok(v) => {
                let f: DriveFile = serde_json::from_value(v).map_err(|e| RemoteError::Other(e.to_string()))?;
                Ok(if f.trashed { None } else { f.into_remote() })
            }
            Err(RemoteError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn download(&self, file: &RemoteFile, dest: &Path, progress: ProgressFn<'_>) -> Result<(), RemoteError> {
        let url = self.url(&format!("files/{}", file.id));
        let mut r = self.send(&|| self.http.get(&url).query(&[("alt", "media")])).await?;
        let mut out = tokio::fs::File::create(dest).await?;
        let mut done = 0u64;
        while let Some(chunk) = r.chunk().await.map_err(|e| RemoteError::Offline(e.to_string()))? {
            out.write_all(&chunk).await?;
            done += chunk.len() as u64;
            progress(done, file.size);
        }
        out.flush().await?;
        out.sync_all().await?;
        Ok(())
    }

    async fn upload(&self, req: &UploadRequest) -> Result<RemoteFile, RemoteError> {
        let bytes = tokio::fs::read(&req.local_path).await?;
        let (url, meta, patch) = match &req.existing_id {
            Some(id) => (self.upload_url(&format!("files/{id}")), self.metadata(req, None), true),
            None => {
                let parent = self.dir_id(&req.root, dir_of(&req.path)).await?;
                (self.upload_url("files"), self.metadata(req, Some(&parent)), false)
            }
        };
        let boundary = format!("cwsync{:016x}", rand::rng().random::<u64>());
        let mut body = format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(&bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let ct = format!("multipart/related; boundary={boundary}");
        let v = self
            .json(&|| {
                let b = if patch { self.http.patch(&url) } else { self.http.post(&url) };
                b.query(&[("uploadType", "multipart"), ("fields", FIELDS)])
                    .header("Content-Type", &ct)
                    .body(body.clone())
            })
            .await?;
        Self::file_from(v)
    }

    async fn begin_resumable(&self, req: &UploadRequest) -> Result<String, RemoteError> {
        let (url, meta, patch) = match &req.existing_id {
            Some(id) => (self.upload_url(&format!("files/{id}")), self.metadata(req, None), true),
            None => {
                let parent = self.dir_id(&req.root, dir_of(&req.path)).await?;
                (self.upload_url("files"), self.metadata(req, Some(&parent)), false)
            }
        };
        let size = req.size.to_string();
        let r = self
            .send(&|| {
                let b = if patch { self.http.patch(&url) } else { self.http.post(&url) };
                b.query(&[("uploadType", "resumable"), ("fields", FIELDS)])
                    .header("X-Upload-Content-Type", "application/octet-stream")
                    .header("X-Upload-Content-Length", &size)
                    .json(&meta)
            })
            .await?;
        r.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .ok_or_else(|| RemoteError::Other("resumable upload returned no session URI".into()))
    }

    async fn resume_upload(
        &self,
        session: &str,
        req: &UploadRequest,
        progress: ProgressFn<'_>,
    ) -> Result<RemoteFile, RemoteError> {
        let mut failures = 0;
        loop {
            let offset = match self.session_offset(session, req.size).await? {
                Ok(o) => o,
                Err(done) => return Ok(done),
            };
            progress(offset, req.size);
            match self.put_chunks(session, req, offset, progress).await {
                Ok(f) => return Ok(f),
                Err(Transient::Yes(e)) if failures + 1 < self.attempts => {
                    tracing::debug!("drive: chunk failed ({e}); asking the session where it is");
                    tokio::time::sleep(self.delay(failures)).await;
                    failures += 1;
                }
                Err(Transient::Yes(e)) => {
                    return Err(match e {
                        RemoteError::Offline(m) => RemoteError::Offline(m),
                        other => RemoteError::Offline(other.to_string()),
                    })
                }
                Err(Transient::No(e)) => return Err(e),
            }
        }
    }

    async fn copy(&self, src_id: &str, root: &str, path: &str, meta: &FileMeta) -> Result<RemoteFile, RemoteError> {
        let parent = self.dir_id(root, dir_of(path)).await?;
        let body = json!({
            "name": name_of(path),
            "parents": [parent],
            "appProperties": app_properties(root, path, meta),
        });
        let url = self.url(&format!("files/{src_id}/copy"));
        let v = self.json(&|| self.http.post(&url).query(&[("fields", FIELDS)]).json(&body)).await?;
        Self::file_from(v)
    }

    async fn trash(&self, id: &str) -> Result<(), RemoteError> {
        let url = self.url(&format!("files/{id}"));
        match self.send(&|| self.http.patch(&url).query(&[("fields", "id")]).json(&json!({"trashed": true}))).await {
            Ok(_) | Err(RemoteError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn start_page_token(&self) -> Result<String, RemoteError> {
        let url = self.url("changes/startPageToken");
        let v = self.json(&|| self.http.get(&url).query(&[("fields", "startPageToken")])).await?;
        v["startPageToken"].as_str().map(String::from).ok_or_else(|| RemoteError::Other("no startPageToken".into()))
    }

    async fn changes_since(&self, token: &str) -> Result<ChangeSet, RemoteError> {
        let url = self.url("changes");
        let mut page = token.to_string();
        let mut changed = 0;
        let mut changes = Vec::new();
        loop {
            let query = [
                ("pageToken", page.clone()),
                ("pageSize", "1000".into()),
                ("spaces", "drive".into()),
                ("fields", "nextPageToken,newStartPageToken,changes(fileId,removed,file(mimeType,trashed,version))".into()),
            ];
            let v = self.json(&|| self.http.get(&url).query(&query)).await?;
            for c in v["changes"].as_array().into_iter().flatten() {
                changed += 1;
                let f = &c["file"];
                changes.push(Change {
                    id: c["fileId"].as_str().unwrap_or_default().into(),
                    removed: c["removed"].as_bool().unwrap_or(false) || f["trashed"].as_bool().unwrap_or(false),
                    version: f["version"].as_str().and_then(|s| s.parse().ok()),
                    folder: f["mimeType"].as_str() == Some(FOLDER_MIME),
                });
            }
            if let Some(t) = v["newStartPageToken"].as_str() {
                return Ok(ChangeSet { changed, new_token: t.into(), changes });
            }
            match v["nextPageToken"].as_str() {
                Some(t) => page = t.into(),
                None => return Ok(ChangeSet { changed, new_token: page, changes }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_paths_split_into_properties() {
        let path = format!("{}/doc.json", "é".repeat(120));
        let v = app_properties("Documents", &path, &FileMeta::default());
        let m = v.as_object().unwrap();
        let mut joined = String::new();
        for i in 0.. {
            match m.get(&format!("cw_p{i}")) {
                Some(s) => {
                    let s = s.as_str().unwrap();
                    assert!(s.len() + format!("cw_p{i}").len() <= 124);
                    joined.push_str(s);
                }
                None => break,
            }
        }
        assert_eq!(joined, path);
        assert_eq!(q_str("it's"), r"'it\'s'");
    }
}
