//! OAuth2 for an installed app: authorization code + PKCE (S256) with a
//! loopback redirect on `127.0.0.1:<random port>`, the system browser for
//! consent, and the refresh token in the OS keychain.

pub mod client;
pub mod pkce;
pub mod store;

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub use client::{ClientConfig, DRIVE_FILE_SCOPE};
pub use store::{KeychainStore, MemoryStore, TokenStore};

use crate::error::AuthError;

/// What the Drive client needs from auth. Lets tests use a fixed token.
#[async_trait]
pub trait TokenProvider: Send + Sync {
    /// A valid access token, refreshed when needed.
    async fn access_token(&self) -> Result<String, AuthError>;
    /// Forget the cached access token (after a Drive 401).
    fn invalidate(&self);
}

/// A fixed token (tests, or a token obtained elsewhere).
pub struct StaticToken(pub String);

#[async_trait]
impl TokenProvider for StaticToken {
    async fn access_token(&self) -> Result<String, AuthError> {
        Ok(self.0.clone())
    }
    fn invalidate(&self) {}
}

#[derive(Clone)]
struct Access {
    token: String,
    expires: Instant,
}

type AuthCallback = Arc<dyn Fn(&str) + Send + Sync>;
type Browser = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// Signs in, keeps the access token fresh, signs out.
pub struct Authenticator {
    client: ClientConfig,
    http: reqwest::Client,
    store: Arc<dyn TokenStore>,
    access: tokio::sync::Mutex<Option<Access>>,
    /// Read without awaiting by `invalidate`.
    stale: std::sync::atomic::AtomicBool,
    on_auth_required: Option<AuthCallback>,
    browser: Browser,
    login_timeout: Duration,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct OAuthErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Refresh this long before Google's stated expiry.
const EARLY_REFRESH: Duration = Duration::from_secs(60);

impl Authenticator {
    pub fn new(client: ClientConfig, store: Arc<dyn TokenStore>) -> Authenticator {
        Authenticator {
            client,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            store,
            access: tokio::sync::Mutex::new(None),
            stale: false.into(),
            on_auth_required: None,
            browser: Arc::new(|url| open::that_detached(url).map_err(|e| e.to_string())),
            login_timeout: Duration::from_secs(300),
        }
    }

    /// Called (with a reason) when the stored token is rejected and deleted.
    pub fn on_auth_required(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.on_auth_required = Some(Arc::new(f));
        self
    }

    /// Replace "open the system browser" (tests drive the redirect themselves).
    pub fn with_browser(mut self, f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) -> Self {
        self.browser = Arc::new(f);
        self
    }

    pub fn with_login_timeout(mut self, t: Duration) -> Self {
        self.login_timeout = t;
        self
    }

    async fn store_call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn TokenStore) -> Result<T, AuthError> + Send + 'static,
    ) -> Result<T, AuthError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(store.as_ref()))
            .await
            .map_err(|e| AuthError::Keychain(e.to_string()))?
    }

    /// Whether a refresh token is stored (the user signed in on this device).
    pub async fn is_signed_in(&self) -> Result<bool, AuthError> {
        Ok(self.store_call(|s| s.load()).await?.is_some())
    }

    /// The interactive flow: open the browser at Google's consent page, wait
    /// for the redirect on the loopback port, exchange the code, store the
    /// refresh token.
    pub async fn sign_in(&self) -> Result<(), AuthError> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let redirect = format!("http://127.0.0.1:{port}");
        let verifier = pkce::verifier();
        let state = pkce::state();
        let mut url = url::Url::parse(&self.client.auth_uri)
            .map_err(|e| AuthError::Config(format!("bad auth_uri: {e}")))?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client.client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("response_type", "code")
            .append_pair("scope", DRIVE_FILE_SCOPE)
            .append_pair("code_challenge", &pkce::challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
        (self.browser)(url.as_str()).map_err(|e| AuthError::OAuth(format!("cannot open the browser: {e}")))?;
        let code = tokio::time::timeout(self.login_timeout, wait_for_code(&listener, &state))
            .await
            .map_err(|_| AuthError::Timeout)??;
        let resp = self
            .http
            .post(&self.client.token_uri)
            .form(&[
                ("code", code.as_str()),
                ("client_id", &self.client.client_id),
                ("client_secret", &self.client.client_secret),
                ("redirect_uri", &redirect),
                ("grant_type", "authorization_code"),
                ("code_verifier", &verifier),
            ])
            .send()
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        let tok = token_response(resp).await?;
        let refresh = tok
            .refresh_token
            .clone()
            .ok_or_else(|| AuthError::OAuth("Google returned no refresh token".into()))?;
        self.store_call(move |s| s.save(&refresh)).await?;
        self.cache(&tok).await;
        Ok(())
    }

    async fn cache(&self, tok: &TokenResponse) {
        let life = Duration::from_secs(tok.expires_in.unwrap_or(3600));
        *self.access.lock().await = Some(Access { token: tok.access_token.clone(), expires: Instant::now() + life });
        self.stale.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    async fn refresh(&self) -> Result<Access, AuthError> {
        let refresh = self.store_call(|s| s.load()).await?.ok_or(AuthError::NotSignedIn)?;
        let resp = self
            .http
            .post(&self.client.token_uri)
            .form(&[
                ("client_id", self.client.client_id.as_str()),
                ("client_secret", &self.client.client_secret),
                ("refresh_token", &refresh),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        match token_response(resp).await {
            Ok(tok) => {
                if let Some(r) = tok.refresh_token.clone().filter(|r| *r != refresh) {
                    self.store_call(move |s| s.save(&r)).await?;
                }
                let life = Duration::from_secs(tok.expires_in.unwrap_or(3600));
                Ok(Access { token: tok.access_token, expires: Instant::now() + life })
            }
            Err(AuthError::ReauthRequired(why)) => {
                self.store_call(|s| s.delete()).await?;
                if let Some(cb) = &self.on_auth_required {
                    cb(&why);
                }
                Err(AuthError::ReauthRequired(why))
            }
            Err(e) => Err(e),
        }
    }

    /// Revoke the token at Google (best effort), then delete it here even if
    /// the revoke failed.
    pub async fn sign_out(&self) -> Result<(), AuthError> {
        let token = self.store_call(|s| s.load()).await.ok().flatten();
        if let Some(t) = token {
            let r = self.http.post(&self.client.revoke_uri).form(&[("token", t.as_str())]).send().await;
            if let Err(e) = r {
                tracing::warn!("token revoke failed (deleting it anyway): {e}");
            }
        }
        *self.access.lock().await = None;
        self.store_call(|s| s.delete()).await
    }
}

#[async_trait]
impl TokenProvider for Authenticator {
    async fn access_token(&self) -> Result<String, AuthError> {
        // holding the lock across the refresh makes it single-flight
        let mut guard = self.access.lock().await;
        let stale = self.stale.swap(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(a) = guard.as_ref() {
            if !stale && a.expires > Instant::now() + EARLY_REFRESH {
                return Ok(a.token.clone());
            }
        }
        let a = self.refresh().await?;
        let t = a.token.clone();
        *guard = Some(a);
        Ok(t)
    }

    fn invalidate(&self) {
        self.stale.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Map a token-endpoint response: `invalid_grant` means sign in again, 5xx is
/// transient (keep the token, report offline), other 4xx is an OAuth error.
async fn token_response(resp: reqwest::Response) -> Result<TokenResponse, AuthError> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| AuthError::Network(e.to_string()))?;
    if status.is_success() {
        return serde_json::from_str(&body).map_err(|e| AuthError::OAuth(format!("bad token response: {e}")));
    }
    if status.is_server_error() || status.as_u16() == 429 {
        return Err(AuthError::Network(format!("token endpoint HTTP {status}")));
    }
    match serde_json::from_str::<OAuthErrorBody>(&body) {
        Ok(e) if e.error == "invalid_grant" => Err(AuthError::ReauthRequired(
            e.error_description.unwrap_or_else(|| "the Google sign-in expired or was revoked".into()),
        )),
        Ok(e) => Err(AuthError::OAuth(match e.error_description {
            Some(d) => format!("{}: {d}", e.error),
            None => e.error,
        })),
        Err(_) => Err(AuthError::OAuth(format!("token endpoint HTTP {status}"))),
    }
}

const PAGE_OK: &str = "<!doctype html><meta charset=utf-8><title>easyCAD</title>\
<body style=\"font:16px system-ui;margin:4em;text-align:center\"><h2>Signed in to easyCAD</h2>\
<p>You can close this tab and go back to easyCAD.</p>";
const PAGE_FAIL: &str = "<!doctype html><meta charset=utf-8><title>easyCAD</title>\
<body style=\"font:16px system-ui;margin:4em;text-align:center\"><h2>Sign-in did not complete</h2>\
<p>Go back to easyCAD and try again.</p>";

/// Serve the loopback port until the browser brings back `code` (or an error)
/// for our `state`. Stray requests (favicon, probes) get a 404.
async fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, AuthError> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = Vec::with_capacity(2048);
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let head = String::from_utf8_lossy(&buf);
        let target = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
        let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
            let _ = respond(&mut sock, 400, PAGE_FAIL).await;
            continue;
        };
        let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
        if !q.contains_key("code") && !q.contains_key("error") {
            let _ = respond(&mut sock, 404, "").await;
            continue;
        }
        if q.get("state").map(String::as_str) != Some(state) {
            let _ = respond(&mut sock, 400, PAGE_FAIL).await;
            return Err(AuthError::OAuth("the sign-in reply had the wrong state".into()));
        }
        if let Some(e) = q.get("error") {
            let _ = respond(&mut sock, 200, PAGE_FAIL).await;
            return Err(if e == "access_denied" {
                AuthError::Denied(e.clone())
            } else {
                AuthError::OAuth(e.clone())
            });
        }
        let _ = respond(&mut sock, 200, PAGE_OK).await;
        return Ok(q["code"].clone());
    }
}

async fn respond(sock: &mut tokio::net::TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let msg = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(msg.as_bytes()).await?;
    sock.shutdown().await
}
