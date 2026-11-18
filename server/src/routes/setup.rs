//! First-run setup endpoints: status, first admin, and "skip for now".
//!
//! These routes are unauthenticated by necessity (there is no account yet).
//! `GET /api/setup/status` is readable from anywhere: the console needs it to
//! render the exposure banner, and it carries no secrets. The two mutations are
//! **loopback-only** (desktop-wrapper origins count as local) and only work
//! while no user exists; a request from the network gets `403
//! setup_local_only`, a second attempt after the admin exists gets `409
//! setup_already_complete`.
//!
//! `POST /api/setup/admin` is also the "set a password" action of the Settings
//! page while the gateway is unprotected. It creates the first admin, logs the
//! caller in (same body as `/api/auth/login`), and moves the mode to
//! `protected`.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{self, ClientIp};
use crate::error::{self, ApiError, ErrorBody};
use crate::routes::auth::{LoginResponse, UserSummary};
use crate::setup::{self, Mode, SetupError};

/// TLS is a separate task (there is no TLS listener yet); the field exists so
/// the console and the API agree on the shape before that lands.
#[derive(Debug, Serialize, ToSchema)]
pub struct TlsStatus {
    pub enabled: bool,
    /// Certificate hostname when TLS is enabled.
    pub domain: Option<String>,
}

/// The gateway's authentication state and exposure facts.
#[derive(Debug, Serialize, ToSchema)]
pub struct SetupStatus {
    /// `setup`, `unprotected`, or `protected`.
    pub mode: Mode,
    /// The configured bind address.
    pub bind: String,
    /// Whether this request counts as local (loopback peer or desktop wrapper).
    pub is_local: bool,
    /// True while the first-run wizard still has to run.
    pub setup_required: bool,
    /// Human-readable exposure note; `null` once protected.
    pub warning: Option<String>,
    pub tls: TlsStatus,
    /// `env` (GATEWAY_MASTER_KEY) or `file` (`<data>/master.key`).
    pub master_key_source: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetupAdminRequest {
    /// Sign-in address of the first admin (never leaves the server unhashed).
    pub email: String,
    /// Password for the first admin, at least 12 characters.
    pub password: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetupSkipRequest {
    /// Must be `true`: skipping leaves the gateway without a password.
    pub confirm: bool,
}

/// A `/api/setup/*` mutation caller from a local peer.
///
/// Rejects with `403 setup_local_only` when the peer is not loopback and the
/// request carries no desktop-wrapper origin.
pub struct SetupCaller;

impl FromRequestParts<AppState> for SetupCaller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if !setup::is_local(parts) || !setup::origin_is_trusted(&parts.headers) {
            return Err(ApiError::custom(
                StatusCode::FORBIDDEN,
                "setup_local_only",
                "setup is only available from this machine (or a desktop app on it)",
            )
            .with_request_id(error::request_id(&parts.headers)));
        }
        Ok(Self)
    }
}

/// The gateway's mode, bind address, and exposure warning.
#[utoipa::path(
    get,
    path = "/api/setup/status",
    tag = "setup",
    responses((status = 200, description = "current authentication mode and exposure facts", body = SetupStatus))
)]
pub async fn status(State(state): State<AppState>, request: Request) -> Json<SetupStatus> {
    let (parts, _body) = request.into_parts();
    Json(build_status(&state, setup::is_local(&parts)))
}

/// Creates the first admin, signs it in, and switches to `protected`.
#[utoipa::path(
    post,
    path = "/api/setup/admin",
    tag = "setup",
    request_body = SetupAdminRequest,
    responses(
        (status = 200, description = "first admin created; sets the gateway_session cookie", body = LoginResponse),
        (status = 400, description = "invalid email, too-short password, or malformed body", body = ErrorBody),
        (status = 403, description = "not a local request, or setup already skipped and no local admin exists", body = ErrorBody),
        (status = 409, description = "an account already exists", body = ErrorBody)
    )
)]
pub async fn create_admin(
    State(state): State<AppState>,
    _caller: SetupCaller,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<SetupAdminRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let rid = error::request_id(&headers);
    let Json(request) = body.map_err(|e| {
        tracing::debug!(error = %e, "setup admin body rejected");
        ApiError::bad_request("invalid request body").with_request_id(rid.clone())
    })?;
    ensure_setup_open(&state, &rid).await?;

    setup::create_first_admin(&state.pool, &request.email, &request.password, &ip.0)
        .await
        .map_err(|e| match e {
            SetupError::AlreadyComplete => already_complete(rid.clone()),
            SetupError::Invalid(message) => {
                ApiError::bad_request(message).with_request_id(rid.clone())
            }
            other => {
                ApiError::internal("creating the first admin", other).with_request_id(rid.clone())
            }
        })?;

    let email = request.email.trim();
    let user = auth::find_user_by_email(&state.pool, email)
        .await
        .map_err(|e| {
            ApiError::internal("looking up the first admin", e).with_request_id(rid.clone())
        })?
        .ok_or_else(|| {
            ApiError::internal("looking up the first admin", "row missing after insert")
                .with_request_id(rid.clone())
        })?;

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    let created = auth::complete_login(&state.pool, &user.id, &user.email, &ip.0, user_agent)
        .await
        .map_err(|e| {
            ApiError::internal("creating the setup session", e).with_request_id(rid.clone())
        })?;

    state.setup.set_mode(Mode::Protected);

    let cookie = auth::session_cookie(&created.token, !state.config.insecure_cookies);
    let mut response = (
        StatusCode::OK,
        Json(LoginResponse {
            user: UserSummary::from_record(&user),
            csrf_token: created.csrf_token,
        }),
    )
        .into_response();
    let cookie = HeaderValue::from_str(&cookie)
        .map_err(|e| ApiError::internal("building session cookie", e).with_request_id(rid))?;
    response.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(response)
}

/// Skips setup: the gateway keeps running without a password, as a trusted
/// local operator. Allowed only while no user exists.
#[utoipa::path(
    post,
    path = "/api/setup/skip",
    tag = "setup",
    request_body = SetupSkipRequest,
    responses(
        (status = 200, description = "gateway is now unprotected", body = SetupStatus),
        (status = 400, description = "confirm is not true, or malformed body", body = ErrorBody),
        (status = 403, description = "not a local request", body = ErrorBody),
        (status = 409, description = "an account already exists", body = ErrorBody)
    )
)]
pub async fn skip(
    State(state): State<AppState>,
    _caller: SetupCaller,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<SetupSkipRequest>, JsonRejection>,
) -> Result<Json<SetupStatus>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(request) = body.map_err(|e| {
        tracing::debug!(error = %e, "setup skip body rejected");
        ApiError::bad_request("invalid request body").with_request_id(rid.clone())
    })?;
    if !request.confirm {
        return Err(
            ApiError::bad_request("send {\"confirm\": true} to run without a password")
                .with_request_id(rid),
        );
    }
    ensure_setup_open(&state, &rid).await?;

    setup::mark_skipped(&state.pool, &ip.0)
        .await
        .map_err(|e| match e {
            SetupError::AlreadyComplete => already_complete(rid.clone()),
            other => ApiError::internal("recording the setup skip", other).with_request_id(rid),
        })?;
    state.setup.set_mode(Mode::Unprotected);
    tracing::warn!(
        mode = "unprotected",
        "setup was skipped: the gateway is running without a password"
    );
    Ok(Json(build_status(&state, true)))
}

/// Rejects a setup mutation once any account exists or the mode moved on.
async fn ensure_setup_open(state: &AppState, rid: &Option<String>) -> Result<(), ApiError> {
    if state.setup.mode().is_protected() {
        return Err(already_complete(rid.clone()));
    }
    let users = setup::real_user_count(&state.pool)
        .await
        .map_err(|e| ApiError::internal("counting users", e).with_request_id(rid.clone()))?;
    if users > 0 {
        return Err(already_complete(rid.clone()));
    }
    Ok(())
}

fn already_complete(rid: Option<String>) -> ApiError {
    ApiError::custom(
        StatusCode::CONFLICT,
        "setup_already_complete",
        "an account already exists; sign in instead",
    )
    .with_request_id(rid)
}

/// The status body for one request's locality.
fn build_status(state: &AppState, local: bool) -> SetupStatus {
    let mode = state.setup.mode();
    SetupStatus {
        mode,
        bind: state.config.listen.to_string(),
        is_local: local,
        setup_required: mode == Mode::Setup,
        warning: setup::warning_for(mode, local),
        tls: TlsStatus {
            enabled: false,
            domain: None,
        },
        master_key_source: state.setup.master_key_source().as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{self, TestApp, api_request, api_request_from, send};
    use axum::http::Method;

    fn status_request(uri: &str, peer: Option<&str>) -> Request {
        api_request_from(Method::GET, uri, None, None, None, peer)
    }

    fn post_local(uri: &str, body: serde_json::Value, peer: Option<&str>) -> Request {
        api_request_from(Method::POST, uri, Some(body), None, None, peer)
    }

    #[tokio::test]
    async fn status_moves_from_setup_to_protected_after_the_first_admin() {
        let app = TestApp::new().await;
        let router = app.router();

        let status = send(
            router.clone(),
            status_request("/api/setup/status", Some("127.0.0.1:5000")),
        )
        .await;
        assert_eq!(status.status, StatusCode::OK);
        assert_eq!(status.body["mode"], "setup");
        assert_eq!(status.body["setup_required"], true);
        assert_eq!(status.body["is_local"], true);
        assert_eq!(status.body["bind"], "127.0.0.1:0");
        assert_eq!(status.body["tls"]["enabled"], false);
        assert_eq!(status.body["tls"]["domain"], serde_json::Value::Null);
        assert_eq!(status.body["master_key_source"], "env");
        assert!(
            status.body["warning"]
                .as_str()
                .expect("warning")
                .contains("not protected yet"),
            "{}",
            status.body
        );

        let created = send(
            router.clone(),
            post_local(
                "/api/setup/admin",
                serde_json::json!({
                    "email": "boss@example.com",
                    "password": "first-admin-password",
                }),
                Some("127.0.0.1:5000"),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::OK, "{}", created.body);
        assert_eq!(created.body["user"]["email"], "boss@example.com");
        assert_eq!(created.body["user"]["role"], "admin");
        assert_eq!(created.body["user"]["must_change_password"], false);
        assert!(created.body["csrf_token"].is_string());
        let set_cookie = created.headers[header::SET_COOKIE]
            .to_str()
            .expect("cookie");
        assert!(set_cookie.contains("gateway_session="));

        // The session works immediately (the wizard lands in the console).
        let cookie = test_support::session_cookie_from(&created.headers);
        let me = send(
            router.clone(),
            api_request(Method::GET, "/api/auth/me", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(me.status, StatusCode::OK);
        assert_eq!(me.body["user"]["email"], "boss@example.com");

        let status = send(
            router.clone(),
            status_request("/api/setup/status", Some("127.0.0.1:5000")),
        )
        .await;
        assert_eq!(status.body["mode"], "protected");
        assert_eq!(status.body["setup_required"], false);
        assert_eq!(status.body["warning"], serde_json::Value::Null);

        // A second attempt is refused, and `/setup` has nothing left to do.
        let again = send(
            router.clone(),
            post_local(
                "/api/setup/admin",
                serde_json::json!({
                    "email": "second@example.com",
                    "password": "second-admin-password",
                }),
                Some("127.0.0.1:5000"),
            ),
        )
        .await;
        assert_eq!(again.status, StatusCode::CONFLICT);
        assert_eq!(again.body["error"]["code"], "setup_already_complete");
    }

    #[tokio::test]
    async fn setup_mutations_refuse_network_peers_but_accept_desktop_wrappers() {
        let app = TestApp::new().await;
        let router = app.router();

        let remote = send(
            router.clone(),
            post_local(
                "/api/setup/admin",
                serde_json::json!({ "email": "boss@example.com", "password": "first-admin-password" }),
                Some("203.0.113.9:5000"),
            ),
        )
        .await;
        assert_eq!(remote.status, StatusCode::FORBIDDEN);
        assert_eq!(remote.body["error"]["code"], "setup_local_only");

        let remote_skip = send(
            router.clone(),
            post_local(
                "/api/setup/skip",
                serde_json::json!({ "confirm": true }),
                Some("203.0.113.9:5000"),
            ),
        )
        .await;
        assert_eq!(remote_skip.status, StatusCode::FORBIDDEN);

        // No ConnectInfo at all is not local either.
        let no_peer = send(
            router.clone(),
            post_local(
                "/api/setup/skip",
                serde_json::json!({ "confirm": true }),
                None,
            ),
        )
        .await;
        assert_eq!(no_peer.status, StatusCode::FORBIDDEN);

        // A loopback peer is not enough for a cross-site browser request: a
        // random web page must not be able to create the first admin.
        let mut cross_site = post_local(
            "/api/setup/admin",
            serde_json::json!({ "email": "evil@example.com", "password": "attacker-password" }),
            Some("127.0.0.1:5000"),
        );
        cross_site.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example.com"),
        );
        let refused = send(router.clone(), cross_site).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
        assert_eq!(refused.body["error"]["code"], "setup_local_only");

        // A desktop wrapper (Tauri/Wails) on the same public peer counts as
        // local, which is what a packaged desktop build sends.
        let mut request = post_local(
            "/api/setup/skip",
            serde_json::json!({ "confirm": true }),
            Some("203.0.113.9:5000"),
        );
        request.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("https://tauri.localhost"),
        );
        let desktop = send(router.clone(), request).await;
        assert_eq!(desktop.status, StatusCode::OK, "{}", desktop.body);
        assert_eq!(desktop.body["mode"], "unprotected");
    }

    #[tokio::test]
    async fn status_is_readable_from_the_network_and_warns_loudly() {
        let app = TestApp::unprotected().await;
        let router = app.router();

        let local = send(
            router.clone(),
            status_request("/api/setup/status", Some("127.0.0.1:5000")),
        )
        .await;
        assert_eq!(local.body["mode"], "unprotected");
        let warning = local.body["warning"].as_str().expect("warning");
        assert!(warning.contains("on this machine"), "{warning}");

        let public = send(
            router.clone(),
            status_request("/api/setup/status", Some("203.0.113.9:5000")),
        )
        .await;
        assert_eq!(public.status, StatusCode::OK);
        assert_eq!(public.body["is_local"], false);
        let warning = public.body["warning"].as_str().expect("warning");
        assert!(warning.contains("reachable from the network"), "{warning}");
        assert!(warning.contains("Settings"), "{warning}");
    }

    #[tokio::test]
    async fn skip_requires_confirmation_and_moves_to_unprotected() {
        let app = TestApp::new().await;
        let router = app.router();

        let unconfirmed = send(
            router.clone(),
            post_local(
                "/api/setup/skip",
                serde_json::json!({ "confirm": false }),
                Some("127.0.0.1:5000"),
            ),
        )
        .await;
        assert_eq!(unconfirmed.status, StatusCode::BAD_REQUEST);
        assert_eq!(unconfirmed.body["error"]["code"], "invalid_request");
        assert_eq!(app.state.setup.mode(), Mode::Setup, "nothing changed yet");

        let skipped = send(
            router.clone(),
            post_local(
                "/api/setup/skip",
                serde_json::json!({ "confirm": true }),
                Some("127.0.0.1:5000"),
            ),
        )
        .await;
        assert_eq!(skipped.status, StatusCode::OK, "{}", skipped.body);
        assert_eq!(skipped.body["mode"], "unprotected");
        assert_eq!(app.state.setup.mode(), Mode::Unprotected);

        // Skipping persists: a restart resolves the same mode.
        assert_eq!(
            crate::setup::resolve_mode(&app.state.pool)
                .await
                .expect("mode"),
            Mode::Unprotected
        );

        // The admin plane now acts as the trusted local operator.
        let me = send(
            router.clone(),
            api_request(Method::GET, "/api/auth/me", None, None, None),
        )
        .await;
        assert_eq!(me.status, StatusCode::OK);
        assert_eq!(me.body["user"]["role"], "admin");
        assert_eq!(me.body["user"]["email"], "local@localhost");
        assert_eq!(me.body["user"]["must_change_password"], false);

        // A state-changing admin call works without a session or CSRF token.
        let users = send(
            router.clone(),
            api_request(Method::GET, "/api/users", None, None, None),
        )
        .await;
        assert_eq!(users.status, StatusCode::OK, "{}", users.body);

        // The data plane is trusted the same way.
        let models = send(
            router.clone(),
            api_request(Method::GET, "/v1/models", None, None, None),
        )
        .await;
        assert_eq!(models.status, StatusCode::OK, "{}", models.body);

        let skipped_again = send(
            router.clone(),
            post_local(
                "/api/setup/skip",
                serde_json::json!({ "confirm": true }),
                Some("127.0.0.1:5000"),
            ),
        )
        .await;
        assert_eq!(
            skipped_again.status,
            StatusCode::OK,
            "the skip is idempotent"
        );
    }

    #[tokio::test]
    async fn settings_can_set_the_first_password_after_skipping() {
        let app = TestApp::unprotected().await;
        let router = app.router();

        let created = send(
            router.clone(),
            post_local(
                "/api/setup/admin",
                serde_json::json!({
                    "email": "later@example.com",
                    "password": "later-admin-password",
                }),
                Some("127.0.0.1:5000"),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::OK, "{}", created.body);
        assert_eq!(created.body["user"]["role"], "admin");
        assert_eq!(app.state.setup.mode(), Mode::Protected);

        // The new session replaces the unprotected bypass.
        let cookie = test_support::session_cookie_from(&created.headers);
        let me = send(
            router.clone(),
            api_request(Method::GET, "/api/auth/me", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(me.body["user"]["email"], "later@example.com");

        // And the bypass is gone: no session, no access.
        let anonymous = send(
            router.clone(),
            api_request(Method::GET, "/api/users", None, None, None),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn skip_is_refused_once_a_user_exists() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", crate::auth::Role::Admin, true)
            .await;
        let router = app.router();

        let skipped = send(
            router.clone(),
            post_local(
                "/api/setup/skip",
                serde_json::json!({ "confirm": true }),
                Some("127.0.0.1:5000"),
            ),
        )
        .await;
        assert_eq!(skipped.status, StatusCode::CONFLICT);
        assert_eq!(skipped.body["error"]["code"], "setup_already_complete");

        let created = send(
            router.clone(),
            post_local(
                "/api/setup/admin",
                serde_json::json!({ "email": "x@example.com", "password": "another-admin-password" }),
                Some("127.0.0.1:5000"),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_admin_rejects_bad_input_with_the_error_shape() {
        let app = TestApp::new().await;
        let router = app.router();

        for body in [
            serde_json::json!({ "email": "not-an-email", "password": "long-enough-password" }),
            serde_json::json!({ "email": "a@example.com", "password": "short" }),
        ] {
            let response = send(
                router.clone(),
                post_local("/api/setup/admin", body, Some("127.0.0.1:5000")),
            )
            .await;
            assert_eq!(
                response.status,
                StatusCode::BAD_REQUEST,
                "{}",
                response.body
            );
            assert_eq!(response.body["error"]["code"], "invalid_request");
            assert!(response.body["error"].get("request_id").is_some());
        }
    }
}
