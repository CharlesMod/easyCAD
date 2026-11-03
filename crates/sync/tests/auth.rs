//! OAuth against wiremock: refresh (and token rotation), invalid_grant,
//! transient failures, revoke, and the whole loopback sign-in.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use easycad_sync::auth::{Authenticator, ClientConfig, MemoryStore, TokenProvider};
use easycad_sync::AuthError;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> ClientConfig {
    let mut c = ClientConfig::new("cid", "csecret");
    c.auth_uri = format!("{}/auth", server.uri());
    c.token_uri = format!("{}/token", server.uri());
    c.revoke_uri = format!("{}/revoke", server.uri());
    c
}

#[tokio::test]
async fn refresh_caches_and_saves_a_rotated_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=r1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "a1", "expires_in": 3600, "refresh_token": "r2", "token_type": "Bearer"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryStore::with_token("r1"));
    let auth = Authenticator::new(client(&server), store.clone());
    assert_eq!(auth.access_token().await.unwrap(), "a1");
    assert_eq!(auth.access_token().await.unwrap(), "a1", "cached: one request only");
    assert_eq!(store.get().as_deref(), Some("r2"));
}

#[tokio::test]
async fn invalid_grant_deletes_the_token_and_asks_for_sign_in() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "Token has been expired or revoked."
        })))
        .mount(&server)
        .await;
    let store = Arc::new(MemoryStore::with_token("r1"));
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let auth = Authenticator::new(client(&server), store.clone()).on_auth_required(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let e = auth.access_token().await.unwrap_err();
    assert!(e.needs_sign_in(), "{e}");
    assert!(store.get().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(auth.access_token().await.unwrap_err(), AuthError::NotSignedIn));
}

#[tokio::test]
async fn a_5xx_keeps_the_token_and_reports_offline() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token")).respond_with(ResponseTemplate::new(503)).mount(&server).await;
    let store = Arc::new(MemoryStore::with_token("r1"));
    let auth = Authenticator::new(client(&server), store.clone());
    assert!(matches!(auth.access_token().await.unwrap_err(), AuthError::Network(_)));
    assert_eq!(store.get().as_deref(), Some("r1"));
}

#[tokio::test]
async fn invalidate_forces_a_refresh() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "a", "expires_in": 3600})))
        .expect(2)
        .mount(&server)
        .await;
    let auth = Authenticator::new(client(&server), Arc::new(MemoryStore::with_token("r1")));
    auth.access_token().await.unwrap();
    auth.invalidate();
    auth.access_token().await.unwrap();
}

#[tokio::test]
async fn sign_out_revokes_then_deletes_even_if_revoke_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=r1"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryStore::with_token("r1"));
    let auth = Authenticator::new(client(&server), store.clone());
    assert!(auth.is_signed_in().await.unwrap());
    auth.sign_out().await.unwrap();
    assert!(store.get().is_none());
    assert!(!auth.is_signed_in().await.unwrap());
}

#[tokio::test]
async fn loopback_sign_in_end_to_end() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains("code_verifier="))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fresh", "expires_in": 3599, "refresh_token": "refresh-1"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryStore::default());
    let auth = Authenticator::new(client(&server), store.clone())
        .with_login_timeout(Duration::from_secs(10))
        .with_browser(|auth_url| {
            // play the browser: check the request, then follow Google's redirect
            let u = url::Url::parse(auth_url).unwrap();
            let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
            assert_eq!(q["code_challenge_method"], "S256");
            assert_eq!(q["scope"], "https://www.googleapis.com/auth/drive.file");
            assert_eq!(q["access_type"], "offline");
            assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
            let back = format!("{}/?state={}&code=the-code&scope=x", q["redirect_uri"], q["state"]);
            let favicon = format!("{}/favicon.ico", q["redirect_uri"]);
            tokio::spawn(async move {
                let c = reqwest::Client::new();
                let _ = c.get(&favicon).send().await;
                let r = c.get(&back).send().await.unwrap();
                assert!(r.status().is_success());
                assert!(r.text().await.unwrap().contains("Signed in"));
            });
            Ok(())
        });
    auth.sign_in().await.unwrap();
    assert_eq!(store.get().as_deref(), Some("refresh-1"));
    assert_eq!(auth.access_token().await.unwrap(), "fresh");
}

#[tokio::test]
async fn sign_in_reports_a_denied_consent() {
    let server = MockServer::start().await;
    let auth = Authenticator::new(client(&server), Arc::new(MemoryStore::default())).with_browser(|auth_url| {
        let u = url::Url::parse(auth_url).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        let back = format!("{}/?state={}&error=access_denied", q["redirect_uri"], q["state"]);
        tokio::spawn(async move {
            let _ = reqwest::get(&back).await;
        });
        Ok(())
    });
    assert!(matches!(auth.sign_in().await.unwrap_err(), AuthError::Denied(_)));
}
