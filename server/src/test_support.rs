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
        let dir = std::env::temp_dir().join(format!("gateway-routes-{}", uuid::Uuid::new_v4()));
        let pool = db::open(&dir.join("gateway.db"))
            .await
            .expect("open test database");
        let state = AppState {
            config: Arc::new(Config {
                listen: "127.0.0.1:0".parse().expect("static address"),
                data_dir: dir.clone(),
                master_key: None,
                timezone: "UTC".to_string(),
                insecure_cookies: true,
                trusted_proxies: Vec::new(),
                version: "test".to_string(),
            }),
            pool,
            started: Instant::now(),
            login_limiter: Arc::new(LoginRateLimiter::new()),
        };
        Self { state, dir }
    }

    /// The full router, wired like `main`: public routes, the API router with
    /// its error-shape fallbacks, and (when built with `embedded-ui`) the UI
    /// fallback for everything else.
    pub fn router(&self) -> Router {
        let app = Router::new()
            .merge(routes::public_routes())
            .merge(routes::api_routes(self.state.clone()));
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
