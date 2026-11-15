//! Login, logout, and session inspection.
//!
//! Login is the only `/api/*` route outside the CSRF guard (the caller has no
//! session yet) and it is rate limited per `(ip, email)`. A locked-out pair
//! gets `429 Too Many Requests` with a `Retry-After` header; bad credentials
//! get `401` with one identical message whether the email is unknown or the
//! password is wrong. A successful login rotates the account's sessions: the
//! previous ones are deleted.

use std::time::Instant;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{self, AuthUser, ClientIp, UserRecord};
use crate::error::{self, ApiError, ErrorBody};
use crate::password;

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Email of the account to sign in to (matched case-insensitively).
    pub email: String,
    /// Account password.
    pub password: String,
}

/// The signed-in account. Never carries the password hash or session token.
#[derive(Debug, Serialize, ToSchema)]
pub struct UserSummary {
    pub id: String,
    pub email: String,
    pub display_name: String,
    /// `admin` or `user`.
    pub role: String,
}

impl UserSummary {
    fn from_record(user: &UserRecord) -> Self {
        Self {
            id: user.id.clone(),
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            role: user.role.as_str().to_string(),
        }
    }

    fn from_auth_user(user: &AuthUser) -> Self {
        Self {
            id: user.user_id.clone(),
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            role: user.role.as_str().to_string(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponse {
    pub user: UserSummary,
    /// CSRF token to send in `X-CSRF-Token` on state-changing requests.
    pub csrf_token: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    pub user: UserSummary,
    /// CSRF token to send in `X-CSRF-Token` on state-changing requests.
    pub csrf_token: String,
}

/// Signs in and starts a session.
///
/// On success the account's older sessions are deleted (session rotation), so
/// every previously issued cookie for the account stops working immediately.
#[utoipa::path(
    post,
    path = "/api/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "session created; sets the gateway_session cookie and invalidates the account's older sessions", body = LoginResponse),
        (status = 400, description = "malformed request body", body = ErrorBody),
        (status = 401, description = "invalid email or password", body = ErrorBody),
        (status = 429, description = "too many failed attempts (locked out)", body = ErrorBody)
    )
)]
pub async fn login(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let rid = error::request_id(&headers);
    let Json(request) = body.map_err(|e| {
        // The rejection can quote the rejected value (a mistyped password,
        // say), so the caller gets a generic message and the detail is logged.
        tracing::debug!(error = %e, "login body rejected");
        ApiError::bad_request("invalid request body").with_request_id(rid.clone())
    })?;

    let email = request.email.trim();
    let now = Instant::now();

    if let Some(remaining) = state.login_limiter.check(&ip.0, email, now) {
        let remaining = state
            .login_limiter
            .record_failure(&ip.0, email, now)
            .unwrap_or(remaining);
        audit_login_failure(&state.pool, None, email, &ip.0, "locked_out").await;
        return Err(ApiError::locked(remaining.as_secs()).with_request_id(rid));
    }

    let user = auth::find_user_by_email(&state.pool, email)
        .await
        .map_err(|e| ApiError::internal("looking up user", e).with_request_id(rid.clone()))?;

    let Some(user) = user else {
        // Do the same password work as a real verification so an unknown email
        // is not distinguishable from a wrong password by response timing.
        let _ = password::hash_password(&request.password);
        state.login_limiter.record_failure(&ip.0, email, now);
        audit_login_failure(&state.pool, None, email, &ip.0, "invalid_credentials").await;
        return Err(ApiError::invalid_credentials().with_request_id(rid));
    };

    let valid = password::verify_password(&request.password, &user.password_hash)
        .map_err(|e| ApiError::internal("verifying password", e).with_request_id(rid.clone()))?;
    if !valid {
        state.login_limiter.record_failure(&ip.0, email, now);
        audit_login_failure(
            &state.pool,
            Some(&user.id),
            email,
            &ip.0,
            "invalid_credentials",
        )
        .await;
        return Err(ApiError::invalid_credentials().with_request_id(rid));
    }

    if !user.is_active {
        // The password matched, but the login does not complete: the failure
        // budget counts this attempt like any other.
        state.login_limiter.record_failure(&ip.0, email, now);
        audit_login_failure(
            &state.pool,
            Some(&user.id),
            email,
            &ip.0,
            "account_disabled",
        )
        .await;
        return Err(ApiError::account_disabled().with_request_id(rid));
    }

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    // The failure budget is only cleared once the login actually completes; a
    // failure after the password matched counts and is audited like any other.
    let created = match auth::complete_login(&state.pool, &user.id, email, &ip.0, user_agent).await
    {
        Ok(created) => {
            state.login_limiter.record_success(&ip.0, email);
            created
        }
        Err(e) => {
            state.login_limiter.record_failure(&ip.0, email, now);
            audit_login_failure(&state.pool, Some(&user.id), email, &ip.0, "internal_error").await;
            return Err(ApiError::internal("creating session", e).with_request_id(rid));
        }
    };

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

/// Ends the session and clears the cookie. Requires the session's CSRF token.
#[utoipa::path(
    post,
    path = "/api/auth/logout",
    tag = "auth",
    responses(
        (status = 204, description = "session deleted; cookie cleared"),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "missing or invalid CSRF token", body = ErrorBody)
    )
)]
pub async fn logout(
    State(state): State<AppState>,
    user: AuthUser,
    ip: ClientIp,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let rid = error::request_id(&headers);
    auth::end_session(&state.pool, &user.session_id, &user.user_id, &ip.0)
        .await
        .map_err(|e| ApiError::internal("deleting session", e).with_request_id(rid.clone()))?;

    let cookie = auth::cleared_session_cookie(!state.config.insecure_cookies);
    let mut response = StatusCode::NO_CONTENT.into_response();
    let cookie = HeaderValue::from_str(&cookie)
        .map_err(|e| ApiError::internal("building session cookie", e).with_request_id(rid))?;
    response.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(response)
}

/// The signed-in account and its current CSRF token.
#[utoipa::path(
    get,
    path = "/api/auth/me",
    tag = "auth",
    responses(
        (status = 200, description = "the signed-in account", body = MeResponse),
        (status = 401, description = "not authenticated", body = ErrorBody)
    )
)]
pub async fn me(user: AuthUser) -> Json<MeResponse> {
    Json(MeResponse {
        user: UserSummary::from_auth_user(&user),
        csrf_token: user.csrf_token,
    })
}

/// Writes one `auth.login_failed` audit row; audit trouble is logged, never
/// allowed to replace the login failure itself.
async fn audit_login_failure(
    pool: &sqlx::SqlitePool,
    actor_user_id: Option<&str>,
    email: &str,
    ip: &str,
    reason: &str,
) {
    let detail = serde_json::json!({ "email": email, "reason": reason }).to_string();
    if let Err(e) = auth::audit(
        pool,
        actor_user_id,
        "auth.login_failed",
        actor_user_id.map(|_| "user"),
        actor_user_id,
        Some(&detail),
        Some(ip),
    )
    .await
    {
        tracing::error!(error = %e, "audit write failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::test_support::{self, TestApp, api_request, send};
    use axum::http::Method;

    #[tokio::test]
    async fn login_sets_cookie_and_me_returns_the_same_session() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();

        let response = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/login",
                Some(serde_json::json!({ "email": "Admin@example.com", "password": test_support::TEST_PASSWORD })),
                None,
                None,
            ),
        )
        .await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body["user"]["email"], "admin@example.com");
        assert_eq!(response.body["user"]["role"], "admin");
        assert!(response.body["csrf_token"].is_string());

        let set_cookie = response.headers[header::SET_COOKIE]
            .to_str()
            .expect("ASCII header");
        assert!(set_cookie.contains("gateway_session="));
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        assert!(set_cookie.contains("Path=/"));

        let cookie = test_support::session_cookie_from(&response.headers);
        let csrf = response.body["csrf_token"]
            .as_str()
            .expect("csrf")
            .to_string();
        let me = send(
            router.clone(),
            api_request(Method::GET, "/api/auth/me", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(me.status, StatusCode::OK);
        assert_eq!(me.body["user"]["email"], "admin@example.com");
        assert_eq!(me.body["csrf_token"], csrf);

        // Login is exempt from the CSRF guard even while a session cookie is
        // present (signing in as another account).
        let again = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/login",
                Some(serde_json::json!({ "email": "admin@example.com", "password": test_support::TEST_PASSWORD })),
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(again.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn wrong_password_and_unknown_email_return_identical_401() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();

        let wrong_password = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/login",
                Some(serde_json::json!({ "email": "admin@example.com", "password": "not-the-password" })),
                None,
                None,
            ),
        )
        .await;
        let unknown_email = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/login",
                Some(serde_json::json!({ "email": "nobody@example.com", "password": "not-the-password" })),
                None,
                None,
            ),
        )
        .await;

        assert_eq!(wrong_password.status, StatusCode::UNAUTHORIZED);
        assert_eq!(unknown_email.status, StatusCode::UNAUTHORIZED);
        assert_eq!(wrong_password.body["error"]["code"], "invalid_credentials");
        assert_eq!(unknown_email.body["error"]["code"], "invalid_credentials");
        assert_eq!(
            wrong_password.body["error"]["message"],
            unknown_email.body["error"]["message"]
        );
    }

    #[tokio::test]
    async fn login_is_rate_limited_and_every_failure_is_audited() {
        let app = TestApp::new().await;
        let user_id = app
            .create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();

        for _ in 0..5 {
            let response = send(
                router.clone(),
                api_request(
                    Method::POST,
                    "/api/auth/login",
                    Some(serde_json::json!({ "email": "admin@example.com", "password": "wrong" })),
                    None,
                    None,
                ),
            )
            .await;
            assert_eq!(response.status, StatusCode::UNAUTHORIZED);
        }

        let locked = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/login",
                Some(serde_json::json!({ "email": "admin@example.com", "password": test_support::TEST_PASSWORD })),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(locked.body["error"]["code"], "login_locked");
        assert!(locked.headers.get(header::RETRY_AFTER).is_some());

        let failures: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'auth.login_failed'")
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
        assert_eq!(
            failures, 6,
            "five credential failures plus the locked attempt"
        );

        // The newest failure is the locked-out attempt, which never resolved
        // an actor; the newest attributed failure must name the user.
        let last_actor: Option<String> = sqlx::query_scalar(
            "SELECT actor_user_id FROM audit_log \
             WHERE action = 'auth.login_failed' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("last audit actor");
        assert_eq!(last_actor, None, "the locked attempt has no resolved actor");

        let actor: Option<String> = sqlx::query_scalar(
            "SELECT actor_user_id FROM audit_log \
             WHERE action = 'auth.login_failed' AND actor_user_id IS NOT NULL \
             ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("audit actor");
        assert_eq!(actor.as_deref(), Some(user_id.as_str()));
    }

    #[tokio::test]
    async fn logging_in_rotates_the_session_and_deletes_older_ones() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();

        let (old_cookie, _) = test_support::login(&router, "admin@example.com").await;
        let (new_cookie, _) = test_support::login(&router, "admin@example.com").await;

        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&app.state.pool)
            .await
            .expect("session count");
        assert_eq!(sessions, 1, "only the fresh session survives a login");

        let old = send(
            router.clone(),
            api_request(Method::GET, "/api/auth/me", None, Some(&old_cookie), None),
        )
        .await;
        assert_eq!(
            old.status,
            StatusCode::UNAUTHORIZED,
            "older sessions are rotated out"
        );

        let fresh = send(
            router.clone(),
            api_request(Method::GET, "/api/auth/me", None, Some(&new_cookie), None),
        )
        .await;
        assert_eq!(fresh.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn session_creation_failure_counts_as_a_login_failure() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();

        // Four failures bring the budget to the edge of the lockout.
        for _ in 0..4 {
            let response = send(
                router.clone(),
                api_request(
                    Method::POST,
                    "/api/auth/login",
                    Some(serde_json::json!({ "email": "admin@example.com", "password": "wrong" })),
                    None,
                    None,
                ),
            )
            .await;
            assert_eq!(response.status, StatusCode::UNAUTHORIZED);
        }

        // The fifth attempt has the right password but the session cannot be
        // stored: it must count as a failure (keeping the budget) and be
        // audited, not reset the budget like a success.
        sqlx::query("DROP TABLE sessions")
            .execute(&app.state.pool)
            .await
            .expect("drop sessions");
        let broken = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/login",
                Some(serde_json::json!({ "email": "admin@example.com", "password": test_support::TEST_PASSWORD })),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(broken.status, StatusCode::INTERNAL_SERVER_ERROR);

        let locked = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/login",
                Some(serde_json::json!({ "email": "admin@example.com", "password": test_support::TEST_PASSWORD })),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(
            locked.status,
            StatusCode::TOO_MANY_REQUESTS,
            "the failed fifth attempt must lock the pair out"
        );

        let audited: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log \
             WHERE action = 'auth.login_failed' AND detail_json LIKE '%internal_error%'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("audit count");
        assert_eq!(audited, 1, "the broken login attempt is audited");
    }

    #[tokio::test]
    async fn logout_requires_csrf_and_removes_the_session() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;

        let missing = send(
            router.clone(),
            api_request(Method::POST, "/api/auth/logout", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(missing.status, StatusCode::FORBIDDEN);
        assert_eq!(missing.body["error"]["code"], "csrf_failed");

        let wrong = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/logout",
                None,
                Some(&cookie),
                Some("not-the-csrf-token"),
            ),
        )
        .await;
        assert_eq!(wrong.status, StatusCode::FORBIDDEN);

        let ok = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/logout",
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(ok.status, StatusCode::NO_CONTENT);

        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&app.state.pool)
            .await
            .expect("session count");
        assert_eq!(sessions, 0);

        let me = send(
            router.clone(),
            api_request(Method::GET, "/api/auth/me", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unauthenticated_requests_get_401_with_error_shape() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();

        let me = send(
            router.clone(),
            api_request(Method::GET, "/api/auth/me", None, None, None),
        )
        .await;
        assert_eq!(me.status, StatusCode::UNAUTHORIZED);
        assert_eq!(me.body["error"]["code"], "unauthorized");
        assert!(me.body["error"].get("request_id").is_some());

        let logout = send(
            router.clone(),
            api_request(Method::POST, "/api/auth/logout", None, None, None),
        )
        .await;
        assert_eq!(logout.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn expired_session_is_rejected_and_deleted() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, _csrf) = test_support::login(&router, "admin@example.com").await;

        sqlx::query("UPDATE sessions SET expires_at = '2000-01-01T00:00:00.000Z'")
            .execute(&app.state.pool)
            .await
            .expect("expire session");

        let me = send(
            router.clone(),
            api_request(Method::GET, "/api/auth/me", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(me.status, StatusCode::UNAUTHORIZED);

        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&app.state.pool)
            .await
            .expect("session count");
        assert_eq!(sessions, 0, "expired sessions must be deleted on sight");
    }

    #[tokio::test]
    async fn disabled_account_cannot_log_in() {
        let app = TestApp::new().await;
        app.create_user("gone@example.com", Role::User, false).await;
        let router = app.router();

        let response = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/login",
                Some(serde_json::json!({ "email": "gone@example.com", "password": test_support::TEST_PASSWORD })),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
        assert_eq!(response.body["error"]["code"], "account_disabled");
    }

    #[tokio::test]
    async fn disabled_account_attempts_count_toward_the_failure_budget() {
        let app = TestApp::new().await;
        app.create_user("gone@example.com", Role::User, false).await;
        let router = app.router();

        // Two attempts with the correct password: the account is disabled, so
        // each one must consume failure budget like any other failed login.
        for _ in 0..2 {
            let response = send(
                router.clone(),
                api_request(
                    Method::POST,
                    "/api/auth/login",
                    Some(serde_json::json!({ "email": "gone@example.com", "password": test_support::TEST_PASSWORD })),
                    None,
                    None,
                ),
            )
            .await;
            assert_eq!(response.status, StatusCode::FORBIDDEN);
            assert_eq!(response.body["error"]["code"], "account_disabled");
        }

        // Three ordinary credential failures then reach the threshold of
        // five and lock the pair out.
        for _ in 0..3 {
            let response = send(
                router.clone(),
                api_request(
                    Method::POST,
                    "/api/auth/login",
                    Some(serde_json::json!({ "email": "gone@example.com", "password": "wrong" })),
                    None,
                    None,
                ),
            )
            .await;
            assert_eq!(response.status, StatusCode::UNAUTHORIZED);
        }

        let locked = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/auth/login",
                Some(serde_json::json!({ "email": "gone@example.com", "password": test_support::TEST_PASSWORD })),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(
            locked.status,
            StatusCode::TOO_MANY_REQUESTS,
            "two disabled-account attempts plus three wrong passwords must lock the pair out"
        );
        assert_eq!(locked.body["error"]["code"], "login_locked");
    }
}
