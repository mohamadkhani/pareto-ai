//! Shared helpers for handler and route tests: a throwaway gateway state on a
//! temporary database, request builders, and response capture.

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use tower::ServiceExt;

use crate::auth::LoginRateLimiter;
use crate::config::Config;
use crate::secrets::{MasterKey, SecretBox};
use crate::{AppState, auth, db, password, routes};

/// The password given to every [`TestApp::create_user`] account.
pub const TEST_PASSWORD: &str = "test-password-1234";

/// The single iteration count keeps password hashing fast in tests;
/// `verify_password` honours whatever count is stored in the hash.
const TEST_HASH_ITERATIONS: u32 = 1_000;

/// A throwaway gateway instance backed by a temporary SQLite database.
pub struct TestApp {
    pub state: AppState,
    dir: std::path::PathBuf,
}

impl TestApp {
    pub async fn new() -> Self {
        Self::with_config(|_| {}).await
    }

    /// Like [`TestApp::new`], with the config adjusted first (tests use this
    /// to shorten timeout budgets).
    pub async fn with_config(mutate: impl FnOnce(&mut Config)) -> Self {
        // Test hashing is cheap hashing; production always uses the full
        // PBKDF2 count (see `password::current_iterations`).
        password::set_test_iterations(TEST_HASH_ITERATIONS);
        let dir = std::env::temp_dir().join(format!("gateway-routes-{}", uuid::Uuid::new_v4()));
        let pool = db::open(&dir.join("gateway.db"))
            .await
            .expect("open test database");
        let mut config = Config {
            listen: "127.0.0.1:0".parse().expect("static address"),
            data_dir: dir.clone(),
            master_key: Some(MasterKey::from_bytes([7u8; 32])),
            timezone: "UTC".to_string(),
            tz: chrono_tz::Tz::UTC,
            insecure_cookies: true,
            trusted_proxies: Vec::new(),
            max_body_bytes: crate::config::DEFAULT_MAX_BODY_BYTES,
            timeout_connect: crate::config::DEFAULT_TIMEOUT_CONNECT,
            timeout_first_byte: crate::config::DEFAULT_TIMEOUT_FIRST_BYTE,
            timeout_idle: crate::config::DEFAULT_TIMEOUT_IDLE,
            timeout_total: crate::config::DEFAULT_TIMEOUT_TOTAL,
            version: "test".to_string(),
        };
        mutate(&mut config);
        let state = AppState {
            config: Arc::new(config),
            pool,
            secrets: Arc::new(SecretBox::new(MasterKey::from_bytes([7u8; 32]))),
            upstream: Arc::new(
                crate::upstream::UpstreamClient::new(std::time::Duration::from_secs(5))
                    .expect("test upstream client"),
            ),
            started: Instant::now(),
            login_limiter: Arc::new(LoginRateLimiter::new()),
            key_limiter: Arc::new(crate::api_keys::KeyRateLimiter::new()),
        };
        Self { state, dir }
    }

    /// The full router, wired like `main`: the request-id stack, public
    /// routes, the API router with its error-shape fallbacks, and (when built
    /// with `embedded-ui`) the UI fallback for everything else.
    pub fn router(&self) -> Router {
        let app = crate::request_id::stack(
            Router::new()
                .merge(routes::public_routes())
                .merge(routes::api_routes(self.state.clone())),
        );
        #[cfg(feature = "embedded-ui")]
        let app = app.fallback(crate::static_files::serve);
        app.with_state(self.state.clone())
    }

    /// Creates a user with [`TEST_PASSWORD`] and returns the new user id.
    pub async fn create_user(&self, email: &str, role: auth::Role, is_active: bool) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let hash = password::hash_password_with_iterations(TEST_PASSWORD, TEST_HASH_ITERATIONS)
            .expect("hash test password");
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(email)
        .bind(email)
        .bind(&hash)
        .bind(role.as_str())
        .bind(i64::from(is_active))
        .execute(&self.state.pool)
        .await
        .expect("insert test user");
        id
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// A captured response: status, headers, and the body as JSON (null when the
/// body is empty or not JSON).
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: serde_json::Value,
}

pub async fn send(router: Router, request: Request<Body>) -> TestResponse {
    let response = router.oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("response body");
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    TestResponse {
        status,
        headers,
        body,
    }
}

/// Like [`send`], but hands back the raw body instead of buffering it:
/// streaming tests read chunk by chunk and the disconnect test drops it
/// mid-stream (which is what hyper does when the client hangs up).
pub async fn send_streaming(
    router: Router,
    request: Request<Body>,
) -> (StatusCode, HeaderMap, Body) {
    let response = router.oneshot(request).await.expect("router response");
    (
        response.status(),
        response.headers().clone(),
        response.into_body(),
    )
}

/// Drains a response body into one string (SSE or JSON).
pub async fn body_text(body: Body) -> String {
    use futures_util::StreamExt;
    let mut stream = body.into_data_stream();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("body chunk"));
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Builds an API request with optional JSON body, session cookie, and CSRF
/// token.
pub fn api_request(
    method: Method,
    uri: &str,
    body: Option<serde_json::Value>,
    cookie: Option<&str>,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(axum::http::header::COOKIE, cookie);
    }
    if let Some(csrf) = csrf {
        builder = builder.header(auth::CSRF_HEADER, csrf);
    }
    match body {
        Some(value) => builder
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    }
}

/// The `gateway_session=...` pair from a response's `Set-Cookie` header, ready
/// to be sent back in a `Cookie` header.
pub fn session_cookie_from(headers: &HeaderMap) -> String {
    let raw = headers
        .get(axum::http::header::SET_COOKIE)
        .expect("Set-Cookie header")
        .to_str()
        .expect("ASCII cookie");
    raw.split(';').next().expect("cookie pair").to_string()
}

/// Logs in and returns `(session cookie, csrf token)`.
pub async fn login(router: &Router, email: &str) -> (String, String) {
    let response = send(
        router.clone(),
        api_request(
            Method::POST,
            "/api/auth/login",
            Some(serde_json::json!({ "email": email, "password": TEST_PASSWORD })),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "login for {email}");
    let cookie = session_cookie_from(&response.headers);
    let csrf = response.body["csrf_token"]
        .as_str()
        .expect("csrf_token in login response")
        .to_string();
    (cookie, csrf)
}

/// Serves one canned HTTP response per connection and records the raw request
/// bytes of each, so tests can assert what went on the wire (used by the
/// credential probe tests against a local "provider").
pub async fn spawn_static_server(
    status_line: &'static str,
    body: &str,
) -> (std::net::SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("test listener address");
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen);
    let body = body.to_string();

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let captured = Arc::clone(&captured);
            let body = body.clone();
            tokio::spawn(async move {
                let mut stream = stream;
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                // Read until the end of the request headers; probes send no
                // body.
                loop {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                captured
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(String::from_utf8_lossy(&buf).to_string());

                let response = format!(
                    "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (addr, seen)
}
