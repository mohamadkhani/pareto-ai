//! User management and API keys (M4).
//!
//! `GET/POST/PATCH /api/users` are admin-only (create with role and initial
//! password, deactivate, reset passwords — every mutation audited). Keys
//! (`/api/users/{id}/keys`, `/api/keys/{id}`) may be managed by their owner
//! or by an admin: creation returns the plaintext key exactly once, every
//! later read shows only the prefix.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;
use utoipa::ToSchema;

use crate::AppState;
use crate::api_keys::{self, ApiKeyInput, ApiKeyPatch, ApiKeyView};
use crate::auth::{AuthUser, ClientIp, RequireAdmin};
use crate::catalog::Actor;
use crate::error::{self, ApiError, ErrorBody};
use crate::routes::{bad_body, ensure_self_or_admin};
use crate::users::{self, UserInput, UserPatch, UserView};

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

/// Lists all accounts. Admin only.
#[utoipa::path(
    get,
    path = "/api/users",
    tag = "users",
    responses(
        (status = 200, description = "all managed accounts; the local-operator placeholder is not included", body = [UserView]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list_users(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
) -> Result<Json<Vec<UserView>>, ApiError> {
    let rid = error::request_id(&headers);
    let rows = users::list_users(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(rows))
}

/// Creates an account with role and initial password. Admin only, audited as
/// `user.create`. The account is flagged `must_change_password` unless the
/// input waives it: an admin-set password must be rotated first. Refused while
/// the gateway is `unprotected` (D58, enforced by [`users::create_user`]):
/// without a password, a real account would make the next restart `protected`
/// with no administrator.
#[utoipa::path(
    post,
    path = "/api/users",
    tag = "users",
    request_body = UserInput,
    responses(
        (status = 201, description = "account created", body = UserView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 409, description = "email already exists, or setup is not complete (set a password first)", body = ErrorBody)
    )
)]
pub async fn create_user(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<UserInput>, JsonRejection>,
) -> Result<(StatusCode, Json<UserView>), ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "user create", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let view = users::create_user(&state.pool, &actor, &input, state.setup.mode())
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Updates an account (decision D20: `null` keeps the current value). Admin
/// only, audited as `user.update`. A `password` reset forces rotation.
#[utoipa::path(
    patch,
    path = "/api/users/{id}",
    tag = "users",
    params(("id" = String, Path, description = "user id")),
    request_body = UserPatch,
    responses(
        (status = 200, description = "account updated", body = UserView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such user", body = ErrorBody),
        (status = 409, description = "the last active administrator cannot be deactivated or demoted, or the target is the local operator (managed in Settings)", body = ErrorBody)
    )
)]
pub async fn update_user(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<UserPatch>, JsonRejection>,
) -> Result<Json<UserView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(patch) = bad_body(body, "user update", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let view = users::update_user(&state.pool, &actor, &id, &patch)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

// ---------------------------------------------------------------------------
// API keys
// ---------------------------------------------------------------------------

/// A freshly issued key: the view plus the plaintext `key`, which exists only
/// in this response.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyCreated {
    /// The full key (`pk_live_...`); returned exactly once and never again.
    pub key: String,
    pub api_key: ApiKeyView,
}

/// Issues an API key for a user. The user or an admin may do this; audited as
/// `api_key.create`. The plaintext key is in the response **once** — only its
/// hash is stored and lists show only the prefix.
#[utoipa::path(
    post,
    path = "/api/users/{id}/keys",
    tag = "users",
    params(("id" = String, Path, description = "user id")),
    request_body = ApiKeyInput,
    responses(
        (status = 201, description = "key issued; `key` is the only copy", body = ApiKeyCreated),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not your account (or CSRF failure)", body = ErrorBody),
        (status = 404, description = "no such user", body = ErrorBody)
    )
)]
pub async fn create_key(
    State(state): State<AppState>,
    user: AuthUser,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ApiKeyInput>, JsonRejection>,
) -> Result<(StatusCode, Json<ApiKeyCreated>), ApiError> {
    let rid = error::request_id(&headers);
    ensure_self_or_admin(&user, &id, &rid)?;
    let Json(input) = bad_body(body, "api key create", &rid)?;
    let actor = Actor {
        user_id: &user.user_id,
        ip: &ip.0,
    };
    let created = api_keys::create_key(&state.pool, &actor, &id, &input)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok((
        StatusCode::CREATED,
        Json(ApiKeyCreated {
            key: created.key,
            api_key: created.view,
        }),
    ))
}

/// Lists a user's API keys (prefixes only — never key material). The user or
/// an admin may read.
#[utoipa::path(
    get,
    path = "/api/users/{id}/keys",
    tag = "users",
    params(("id" = String, Path, description = "user id")),
    responses(
        (status = 200, description = "the user's keys", body = [ApiKeyView]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not your account", body = ErrorBody)
    )
)]
pub async fn list_keys(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<ApiKeyView>>, ApiError> {
    let rid = error::request_id(&headers);
    ensure_self_or_admin(&user, &id, &rid)?;
    let rows = api_keys::list_keys(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(rows))
}

/// Updates a key's name, expiry, or rate limits (decision D20: `null` keeps
/// the current value). The key's owner or an admin; audited as
/// `api_key.update`.
#[utoipa::path(
    patch,
    path = "/api/keys/{id}",
    tag = "users",
    params(("id" = String, Path, description = "api key id")),
    request_body = ApiKeyPatch,
    responses(
        (status = 200, description = "key updated", body = ApiKeyView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not your key (or CSRF failure)", body = ErrorBody),
        (status = 404, description = "no such key", body = ErrorBody)
    )
)]
pub async fn update_key(
    State(state): State<AppState>,
    user: AuthUser,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ApiKeyPatch>, JsonRejection>,
) -> Result<Json<ApiKeyView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(patch) = bad_body(body, "api key update", &rid)?;
    let actor = Actor {
        user_id: &user.user_id,
        ip: &ip.0,
    };
    ensure_key_owner(&state, &user, &id, &rid).await?;
    let view = api_keys::update_key(&state.pool, &actor, &id, &patch)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Revokes a key: it stops authenticating on its next request. The key's
/// owner or an admin; audited as `api_key.revoke`.
#[utoipa::path(
    delete,
    path = "/api/keys/{id}",
    tag = "users",
    params(("id" = String, Path, description = "api key id")),
    responses(
        (status = 204, description = "key revoked"),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not your key (or CSRF failure)", body = ErrorBody),
        (status = 404, description = "no such key", body = ErrorBody)
    )
)]
pub async fn revoke_key(
    State(state): State<AppState>,
    user: AuthUser,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let rid = error::request_id(&headers);
    let actor = Actor {
        user_id: &user.user_id,
        ip: &ip.0,
    };
    ensure_key_owner(&state, &user, &id, &rid).await?;
    api_keys::revoke_key(&state.pool, &actor, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(StatusCode::NO_CONTENT)
}

/// 403 unless the caller owns the key or is an admin; 404 for unknown keys.
async fn ensure_key_owner(
    state: &AppState,
    user: &AuthUser,
    key_id: &str,
    rid: &Option<String>,
) -> Result<(), ApiError> {
    let owner: Option<String> = sqlx::query_scalar("SELECT user_id FROM api_keys WHERE id = ?")
        .bind(key_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::internal("resolving api key", e).with_request_id(rid.clone()))?;
    let owner = owner.ok_or_else(|| ApiError::not_found("api key").with_request_id(rid.clone()))?;
    ensure_self_or_admin(user, &owner, rid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::test_support::{self, TestApp, api_request, api_request_from, send};
    use axum::body::Body;
    use axum::http::Method;

    #[tokio::test]
    async fn key_creation_returns_the_plaintext_once_and_lists_show_prefixes() {
        let app = TestApp::new().await;
        let user_id = app.create_user("user@example.com", Role::User, true).await;
        let router = app.router();
        let (admin_cookie, csrf) = test_support::login(&router, "user@example.com").await;

        // The user creates a key for themselves (self-service).
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/users/{user_id}/keys"),
                Some(serde_json::json!({ "name": "sdk", "rate_limit_rpm": 30 })),
                Some(&admin_cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let key = created.body["key"]
            .as_str()
            .expect("plaintext key")
            .to_string();
        assert!(key.starts_with("pk_live_"));
        assert_eq!(created.body["api_key"]["rate_limit_rpm"], 30);

        // Lists show the prefix and never the key.
        let listed = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/users/{user_id}/keys"),
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(listed.status, StatusCode::OK);
        assert_eq!(listed.body.as_array().expect("list").len(), 1);
        let text = listed.body.to_string();
        assert!(!text.contains(&key), "the plaintext never appears again");
        assert!(text.contains("pk_live_"), "the prefix is shown");

        // The key authenticates the data plane and is then revocable by its
        // owner; after revocation it stops working.
        let models = send(
            router.clone(),
            axum::http::Request::builder()
                .method(Method::GET)
                .uri("/v1/models")
                .header(axum::http::header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(models.status, StatusCode::OK, "{}", models.body);

        let key_id = created.body["api_key"]["id"]
            .as_str()
            .expect("key id")
            .to_string();
        let revoked = send(
            router.clone(),
            api_request(
                Method::DELETE,
                &format!("/api/keys/{key_id}"),
                None,
                Some(&admin_cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(revoked.status, StatusCode::NO_CONTENT);

        let denied = send(
            router.clone(),
            axum::http::Request::builder()
                .method(Method::GET)
                .uri("/v1/models")
                .header(axum::http::header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
        assert_eq!(denied.body["error"]["code"], "api_key_revoked");

        // The audit trail carries the mutations and no key material (an
        // admin reads it: the audit log is admin-only).
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let (audit_cookie, _) = test_support::login(&router, "admin@example.com").await;
        let audit = send(
            router.clone(),
            api_request(Method::GET, "/api/audit", None, Some(&audit_cookie), None),
        )
        .await;
        let text = audit.body.to_string();
        assert!(text.contains("api_key.create"), "{text}");
        assert!(text.contains("api_key.revoke"), "{text}");
        assert!(!text.contains(&key), "audit rows never carry the key");
    }

    #[tokio::test]
    async fn unprotected_mode_refuses_account_creation_until_the_first_password() {
        let app = TestApp::unprotected().await;
        let router = app.router();

        let refused = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/users",
                Some(serde_json::json!({
                    "email": "plain@example.com",
                    "display_name": "Plain",
                    "role": "user",
                    "password": "temporary-password-1",
                })),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
        assert_eq!(refused.body["error"]["code"], "setup_incomplete");
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_placeholder = 0")
            .fetch_one(&app.state.pool)
            .await
            .expect("users");
        assert_eq!(users, 0, "the refused request created no account");

        // Validation runs before the mode gate: a malformed body is a `400` in
        // every mode, as the route documents.
        let malformed = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/users",
                Some(serde_json::json!({
                    "email": "not-an-email",
                    "display_name": "Plain",
                    "role": "user",
                    "password": "temporary-password-1",
                })),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(
            malformed.status,
            StatusCode::BAD_REQUEST,
            "{}",
            malformed.body
        );

        // Setting the first password promotes the local operator and opens
        // account creation for the rest of the process.
        let set = send(
            router.clone(),
            api_request_from(
                Method::POST,
                "/api/setup/admin",
                Some(serde_json::json!({
                    "email": "admin@example.com",
                    "password": "first-admin-password",
                })),
                None,
                None,
                Some("127.0.0.1:5000"),
            ),
        )
        .await;
        assert_eq!(set.status, StatusCode::OK, "{}", set.body);
        let cookie = test_support::session_cookie_from(&set.headers);
        let csrf = set.body["csrf_token"].as_str().expect("csrf token");

        let created = send(
            router,
            api_request(
                Method::POST,
                "/api/users",
                Some(serde_json::json!({
                    "email": "plain@example.com",
                    "display_name": "Plain",
                    "role": "user",
                    "password": "temporary-password-1",
                })),
                Some(&cookie),
                Some(csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    }

    #[tokio::test]
    async fn the_local_operator_is_not_listed_as_an_account() {
        let app = TestApp::unprotected().await;
        let listed = send(
            app.router(),
            api_request(Method::GET, "/api/users", None, None, None),
        )
        .await;
        assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
        assert_eq!(
            listed.body.as_array().map(Vec::len),
            Some(0),
            "the placeholder is not a managed account: {}",
            listed.body
        );
    }

    #[tokio::test]
    async fn the_local_operator_placeholder_cannot_be_patched() {
        for patch in [
            serde_json::json!({ "role": "user" }),
            serde_json::json!({ "is_active": false }),
            serde_json::json!({ "password": "attacker-password-1" }),
        ] {
            let app = TestApp::unprotected().await;
            let router = app.router();

            let refused = send(
                router,
                api_request(
                    Method::PATCH,
                    &format!("/api/users/{}", crate::auth::LOCAL_OPERATOR_ID),
                    Some(patch.clone()),
                    None,
                    None,
                ),
            )
            .await;
            assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
            assert_eq!(refused.body["error"]["code"], "local_operator");

            let (role, active, placeholder): (String, i64, i64) =
                sqlx::query_as("SELECT role, is_active, is_placeholder FROM users WHERE id = ?")
                    .bind(crate::auth::LOCAL_OPERATOR_ID)
                    .fetch_one(&app.state.pool)
                    .await
                    .expect("placeholder row");
            assert_eq!(
                (role.as_str(), active, placeholder),
                ("admin", 1, 1),
                "the refused patch changed nothing (patch {patch})"
            );
        }
    }

    #[tokio::test]
    async fn users_crud_is_admin_only_and_forced_rotation_survives_a_reset() {
        let app = TestApp::new().await;
        let admin_id = app
            .create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (admin_cookie, csrf) = test_support::login(&router, "admin@example.com").await;

        // Anonymous and non-admin callers cannot manage users.
        let anonymous = send(
            router.clone(),
            api_request(Method::GET, "/api/users", None, None, None),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

        // The admin creates a user; it is flagged for rotation.
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/users",
                Some(serde_json::json!({
                    "email": "new@example.com",
                    "display_name": "New User",
                    "role": "user",
                    "password": "temporary-password-1",
                })),
                Some(&admin_cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        assert_eq!(created.body["must_change_password"], true);
        let user_id = created.body["id"].as_str().expect("id").to_string();

        // The admin resets the password: rotation is flagged again.
        let patched = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/users/{user_id}"),
                Some(serde_json::json!({ "password": "another-password-1" })),
                Some(&admin_cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
        assert_eq!(patched.body["must_change_password"], true);

        // Deactivation is a PATCH too.
        let deactivated = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/users/{user_id}"),
                Some(serde_json::json!({ "is_active": false })),
                Some(&admin_cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deactivated.status, StatusCode::OK);
        assert_eq!(deactivated.body["is_active"], false);

        // Listing works for the admin.
        let listed = send(
            router.clone(),
            api_request(Method::GET, "/api/users", None, Some(&admin_cookie), None),
        )
        .await;
        assert_eq!(listed.status, StatusCode::OK);
        assert_eq!(listed.body.as_array().expect("list").len(), 2);

        // A non-admin session cannot read the user list.
        let app2_user = app.create_user("plain@example.com", Role::User, true).await;
        assert!(!app2_user.is_empty());
        let (plain_cookie, _) = test_support::login(&router, "plain@example.com").await;
        let forbidden = send(
            router.clone(),
            api_request(Method::GET, "/api/users", None, Some(&plain_cookie), None),
        )
        .await;
        assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

        // CSRF still guards the mutations.
        let unguarded = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/users",
                Some(serde_json::json!({})),
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(unguarded.status, StatusCode::FORBIDDEN);
        assert_eq!(unguarded.body["error"]["code"], "csrf_failed");
        let _ = admin_id;
    }

    #[tokio::test]
    async fn keys_of_other_users_are_not_reachable() {
        let app = TestApp::new().await;
        let owner = app.create_user("owner@example.com", Role::User, true).await;
        let other = app.create_user("other@example.com", Role::User, true).await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "other@example.com").await;

        // Listing someone else's keys is 403.
        let listed = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/users/{owner}/keys"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(listed.status, StatusCode::FORBIDDEN);

        // And so is creating one for them.
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/users/{owner}/keys"),
                Some(serde_json::json!({ "name": "sneaky" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::FORBIDDEN);
        let _ = other;
    }
}
