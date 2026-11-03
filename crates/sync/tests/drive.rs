//! `DriveClient` against wiremock: pagination, retry with backoff, the 401
//! refresh, and resumable upload continuing from the server's offset.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use easycad_sync::auth::{StaticToken, TokenProvider};
use easycad_sync::{AuthError, DriveClient, FileMeta, RemoteError, RemoteStore, UploadRequest};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn client(server: &MockServer) -> DriveClient {
    DriveClient::new(Arc::new(StaticToken("tok".into())))
        .with_base_url(&server.uri())
        .with_retry(4, Duration::from_millis(1))
}

fn file(id: &str, path: &str) -> serde_json::Value {
    json!({
        "id": id, "size": "3", "md5Checksum": "abc", "version": "2", "headRevisionId": "h",
        "modifiedTime": "2026-11-03T10:00:00.000Z",
        "appProperties": {"cw_root": "Documents", "cw_p0": path, "cw_dev": "lap"}
    })
}

#[tokio::test]
async fn list_follows_pages_and_reads_the_path_properties() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .and(query_param("pageToken", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": [file("2", "b.txt")]})))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "files": [file("1", "a.txt"), {"id": "x", "appProperties": {}}], "nextPageToken": "p2"
        })))
        .with_priority(2)
        .mount(&server)
        .await;
    let files = client(&server).list("Documents").await.unwrap();
    let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "b.txt"]);
    assert_eq!(files[0].size, 3);
    assert_eq!(files[0].version, Some(2));
    assert_eq!(files[0].meta.device.as_deref(), Some("lap"));
    let q = &server.received_requests().await.unwrap()[0];
    let query: String = q.url.query_pairs().filter(|(k, _)| k == "q").map(|(_, v)| v.into_owned()).collect();
    assert!(query.contains("key='cw_root' and value='Documents'"), "{query}");
}

#[tokio::test]
async fn transient_errors_are_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/f1"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/f1"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"error": {"errors": [{"reason": "userRateLimitExceeded"}], "message": "slow down"}})),
        )
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/f1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file("f1", "a.txt")))
        .with_priority(3)
        .mount(&server)
        .await;
    let f = client(&server).get("f1").await.unwrap().unwrap();
    assert_eq!(f.path, "a.txt");
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn retries_run_out_as_offline_and_404_is_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/drive/v3/files/gone")).respond_with(ResponseTemplate::new(404)).mount(&server).await;
    Mock::given(method("GET")).and(path("/drive/v3/files/down")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let c = client(&server);
    assert!(c.get("gone").await.unwrap().is_none());
    assert!(matches!(c.get("down").await.unwrap_err(), RemoteError::Offline(_)));
}

struct Counting {
    invalidated: AtomicUsize,
}

#[async_trait]
impl TokenProvider for Counting {
    async fn access_token(&self) -> Result<String, AuthError> {
        Ok(format!("t{}", self.invalidated.load(Ordering::SeqCst)))
    }
    fn invalidate(&self) {
        self.invalidated.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_401_refreshes_the_token_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/f1"))
        .and(header("authorization", "Bearer t1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file("f1", "a.txt")))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/drive/v3/files/f1")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    let auth = Arc::new(Counting { invalidated: AtomicUsize::new(0) });
    let c = DriveClient::new(auth.clone()).with_base_url(&server.uri()).with_retry(4, Duration::from_millis(1));
    assert!(c.get("f1").await.unwrap().is_some());
    assert_eq!(auth.invalidated.load(Ordering::SeqCst), 1);
    // a token that keeps failing is an auth error, not an endless loop
    Mock::given(method("GET")).and(path("/drive/v3/files/f2")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    assert!(matches!(c.get("f2").await.unwrap_err(), RemoteError::AuthRequired(_)));
}

#[tokio::test]
async fn resumable_upload_continues_from_the_server_offset() {
    let server = MockServer::start().await;
    let total = 512 * 1024u64;
    let session = format!("{}/upload/session-1", server.uri());
    Mock::given(method("POST"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": []})))
        .mount(&server)
        .await;
    // folder lookups find nothing and create
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "folder"})))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(query_param("uploadType", "resumable"))
        .and(header("x-upload-content-length", total.to_string().as_str()))
        .respond_with(ResponseTemplate::new(200).insert_header("Location", session.as_str()))
        .mount(&server)
        .await;
    // the server already has the first 256 KiB
    Mock::given(method("PUT"))
        .and(path("/upload/session-1"))
        .and(header("content-range", format!("bytes */{total}").as_str()))
        .respond_with(ResponseTemplate::new(308).insert_header("Range", "bytes=0-262143"))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/upload/session-1"))
        .and(header("content-range", format!("bytes 262144-{}/{total}", total - 1).as_str()))
        .respond_with(|r: &Request| {
            assert_eq!(r.body.len(), 256 * 1024);
            ResponseTemplate::new(200).set_body_json(file("new", "big.step"))
        })
        .expect(1)
        .mount(&server)
        .await;
    let t = tempfile::tempdir().unwrap();
    let local = t.path().join("big.step");
    std::fs::write(&local, vec![9u8; total as usize]).unwrap();
    let req = UploadRequest {
        root: "Documents".into(),
        path: "big.step".into(),
        local_path: local,
        existing_id: None,
        size: total,
        meta: FileMeta::default(),
    };
    let c = client(&server).with_chunk(256 * 1024);
    let s = c.begin_resumable(&req).await.unwrap();
    assert_eq!(s, session);
    let f = c.resume_upload(&s, &req, &|_, _| {}).await.unwrap();
    assert_eq!(f.id, "new");
}

#[tokio::test]
async fn an_expired_session_says_so() {
    let server = MockServer::start().await;
    Mock::given(method("PUT")).and(path("/upload/s")).respond_with(ResponseTemplate::new(404)).mount(&server).await;
    let t = tempfile::tempdir().unwrap();
    let local = t.path().join("f");
    std::fs::write(&local, b"abc").unwrap();
    let req = UploadRequest {
        root: "Documents".into(),
        path: "f".into(),
        local_path: local,
        existing_id: None,
        size: 3,
        meta: FileMeta::default(),
    };
    let e = client(&server).resume_upload(&format!("{}/upload/s", server.uri()), &req, &|_, _| {}).await.unwrap_err();
    assert!(matches!(e, RemoteError::SessionExpired), "{e}");
}

#[tokio::test]
async fn changes_report_each_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/changes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "newStartPageToken": "9",
            "changes": [
                {"fileId": "a", "removed": false, "file": {"mimeType": "application/octet-stream", "trashed": false, "version": "4"}},
                {"fileId": "b", "removed": false, "file": {"mimeType": "application/vnd.google-apps.folder", "version": "1"}},
                {"fileId": "c", "removed": false, "file": {"trashed": true, "version": "7"}},
                {"fileId": "d", "removed": true}
            ]
        })))
        .mount(&server)
        .await;
    let cs = client(&server).changes_since("5").await.unwrap();
    assert_eq!(cs.new_token, "9");
    let v: Vec<_> = cs.changes.iter().map(|c| (c.id.as_str(), c.removed, c.version, c.folder)).collect();
    assert_eq!(v, [("a", false, Some(4), false), ("b", false, Some(1), true), ("c", true, Some(7), false), ("d", true, None, false)]);
}
