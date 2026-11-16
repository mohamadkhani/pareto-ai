//! HTTP routes.
//!
//! Public routes (`/health`, `/openapi.json`) need no session. The API router
//! is mounted at `/api` and carries its own fallbacks, so unknown `/api/*`
//! paths and wrong methods get the uniform [`ApiError`] shape (`404
//! route_not_found`, `405 method_not_allowed`) while every other path keeps
//! the embedded UI fallback. `/v1/*` carries the same fallbacks: it is the
//! OpenAI-compatible data plane (admin session until M4 API keys exist).
//! Everything under `/api/` goes through [`csrf_guard`]: state-changing
//! methods must carry the session's CSRF token in `X-CSRF-Token`, except for
//! login, which is mounted outside the guard because the caller has no
//! session yet. `/v1/*` is exempt from the guard: OpenAI-SDK clients never
//! send `X-CSRF-Token`, and the data plane authenticates with bearer API
//! keys (or the admin session), not with cookies a browser would attach
//! automatically. Everything except the rotation flow also goes through
//! [`rotation_guard`]: an account flagged `must_change_password` must change
//! its password first (review finding 7).

pub(crate) mod audit;
pub(crate) mod auth;
pub(crate) mod catalog;
pub(crate) mod chat;
pub(crate) mod grants;
pub(crate) mod health;
pub(crate) mod money;
pub(crate) mod openapi;
pub(crate) mod providers;
pub(crate) mod routing;
pub(crate) mod sync;
pub(crate) mod usage;
pub(crate) mod users;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::get;

use crate::AppState;
use crate::error::{self, ApiError};

/// Routes that do not require authentication.
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::get))
        .route("/openapi.json", get(openapi::get))
}

/// Auth, admin, and data-plane routes, mounted at `/api` and `/v1`.
/// Mutations require a valid session and its CSRF token.
///
/// Unknown paths and wrong methods answer with the uniform error shape
/// instead of leaking into the UI fallback or an empty framework response.
/// The router is self-contained: `main` merges it like any other router.
///
/// Takes a state handle because the CSRF guard resolves sessions itself,
/// before any handler runs; the same state is also applied to the router by
/// `main`.
pub fn api_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .nest("/api", api_surface(state.clone()))
        // The nested fallback covers `/api/*`, but the catch-all matches no
        // empty tail: the bare `/api/` is just as unknown and must not leak
        // into the UI fallback.
        .route("/api/", axum::routing::any(api_not_found))
        .nest("/v1", v1_surface(state.clone()))
}

/// The API surface as it sees itself, under the `/api` prefix.
fn api_surface(state: AppState) -> Router<AppState> {
    // Login sits outside the CSRF guard: the caller has no session yet.
    let csrf_protected = Router::new()
        .route("/auth/logout", axum::routing::post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route(
            "/auth/change-password",
            axum::routing::post(auth::change_password),
        )
        .route("/audit", get(audit::list))
        // Users, API keys, and their budgets (M4).
        .route("/users", get(users::list_users).post(users::create_user))
        .route("/users/{id}", axum::routing::patch(users::update_user))
        .route(
            "/users/{id}/keys",
            get(users::list_keys).post(users::create_key),
        )
        .route(
            "/keys/{id}",
            axum::routing::patch(users::update_key).delete(users::revoke_key),
        )
        .route("/users/{id}/budgets", get(money::list_budgets))
        .route(
            "/users/{id}/budgets/{period}",
            axum::routing::put(money::set_budget).delete(money::clear_budget),
        )
        .route("/users/{id}/token-caps", get(money::list_token_caps))
        .route(
            "/users/{id}/token-caps/{model_id}/{period}",
            axum::routing::put(money::set_token_cap).delete(money::clear_token_cap),
        )
        // Access grants (M4).
        .route(
            "/grants",
            get(grants::list_grants).post(grants::create_grant),
        )
        .route("/grants/{id}", axum::routing::delete(grants::delete_grant))
        // Reports and the request-log viewer (M4).
        .route("/usage/me", get(usage::usage_me))
        .route("/usage", get(usage::usage_admin))
        .route("/logs", get(usage::logs))
        // Providers and credentials.
        .route("/providers", get(providers::list).post(providers::create))
        .route(
            "/providers/{id}",
            get(providers::get)
                .patch(providers::update)
                .delete(providers::delete),
        )
        .route(
            "/providers/{id}/credentials",
            get(providers::list_credentials).post(providers::create_credential),
        )
        .route(
            "/credentials/{id}",
            get(providers::get_credential)
                .patch(providers::update_credential)
                .delete(providers::delete_credential),
        )
        .route(
            "/credentials/{id}/test",
            axum::routing::post(providers::test_credential),
        )
        // Routing (M5): combo models, benchmark import, quality overrides.
        .route(
            "/combos",
            get(routing::list_combos).post(routing::create_combo),
        )
        .route(
            "/combos/{id}",
            axum::routing::put(routing::update_combo).delete(routing::delete_combo),
        )
        .route(
            "/benchmarks/import",
            axum::routing::post(routing::import_benchmarks),
        )
        .route(
            "/models/{id}/quality-override",
            axum::routing::put(routing::set_quality_override)
                .delete(routing::clear_quality_override),
        )
        // Models, offerings, and prices.
        .route(
            "/models",
            get(catalog::list_models).post(catalog::create_model),
        )
        .route(
            "/models/{id}",
            get(catalog::get_model)
                .patch(catalog::update_model)
                .delete(catalog::delete_model),
        )
        .route(
            "/offerings",
            get(catalog::list_offerings).post(catalog::create_offering),
        )
        .route(
            "/offerings/{id}",
            axum::routing::patch(catalog::update_offering).delete(catalog::delete_offering),
        )
        .route(
            "/offerings/{id}/price",
            get(catalog::get_price).put(catalog::override_price),
        )
        // OpenRouter sync.
        .route("/sync/openrouter", axum::routing::post(sync::trigger))
        .route("/sync/openrouter/status", get(sync::status))
        // Forced password rotation (review finding 7): a flagged account may
        // only reach the rotation flow (`me`, `logout`, `change-password`).
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            rotation_guard,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            csrf_guard,
        ));

    Router::new()
        .route("/auth/login", axum::routing::post(auth::login))
        .merge(csrf_protected)
        .fallback(api_not_found)
        .method_not_allowed_fallback(method_not_allowed)
}

/// The OpenAI-compatible data plane as it sees itself, under `/v1`: `GET
/// /v1/models` (M2) and `POST /v1/chat/completions` (M3). No CSRF guard —
/// see the module docs.
fn v1_surface(_state: AppState) -> Router<AppState> {
    Router::new()
        .route("/models", get(catalog::v1_models))
        .route(
            "/chat/completions",
            axum::routing::post(chat::chat_completions),
        )
        .fallback(api_not_found)
        .method_not_allowed_fallback(method_not_allowed)
}

/// `403` unless the caller is the target user or an admin (M4: keys and
/// budgets are self-or-admin).
pub(crate) fn ensure_self_or_admin(
    user: &crate::auth::AuthUser,
    target_user_id: &str,
    rid: &Option<String>,
) -> Result<(), ApiError> {
    match user.role.is_admin() || user.user_id == target_user_id {
        true => Ok(()),
        false => Err(ApiError::forbidden().with_request_id(rid.clone())),
    }
}

/// The `/api` paths a forced-rotation account may still reach, in the full
/// and the nested (prefix-stripped) form.
const ROTATION_EXEMPT: [&str; 6] = [
    "/api/auth/me",
    "/api/auth/logout",
    "/api/auth/change-password",
    "/auth/me",
    "/auth/logout",
    "/auth/change-password",
];

/// Rejects every `/api` request of an account flagged `must_change_password`
/// with `403 password_change_required`, except the rotation flow itself
/// (review finding 7). Requests without a session cookie pass through: the
/// handlers answer those with `401`.
async fn rotation_guard(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let path = req.uri().path();
    if ROTATION_EXEMPT.contains(&path) {
        return Ok(next.run(req).await);
    }
    let rid = error::request_id(req.headers());
    if let Some(token) = crate::auth::session_cookie_value(req.headers()) {
        let flagged = crate::auth::session_must_change_password(&state.pool, &token)
            .await
            .map_err(|e| ApiError::internal("resolving session", e).with_request_id(rid.clone()))?;
        if flagged {
            return Err(crate::auth::password_change_required(rid));
        }
    }
    Ok(next.run(req).await)
}

/// Uniform body extractor rejection: generic message, detail only logged.
/// Shared by every JSON-accepting handler so the 400 shape cannot drift.
pub(crate) fn bad_body<T, E: std::fmt::Display>(
    result: Result<T, E>,
    what: &str,
    rid: &Option<String>,
) -> Result<T, ApiError> {
    result.map_err(|e| {
        tracing::debug!(error = %e, what, "request body rejected");
        ApiError::bad_request("invalid request body").with_request_id(rid.clone())
    })
}

/// Uniform `404 route_not_found` for unknown `/api/*` paths.
async fn api_not_found(headers: HeaderMap) -> ApiError {
    ApiError::route_not_found().with_request_id(error::request_id(&headers))
}

/// Uniform `405 method_not_allowed` when a route exists but not for the method.
async fn method_not_allowed(headers: HeaderMap) -> ApiError {
    ApiError::method_not_allowed().with_request_id(error::request_id(&headers))
}

/// Rejects state-changing requests whose `X-CSRF-Token` header does not match
/// the session's stored token (compared in constant time).
///
/// Requests without a session cookie pass through untouched: CSRF protection
/// only matters for requests riding an existing session, and the handlers
/// answer those without a session with `401`.
async fn csrf_guard(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let needs_check = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !needs_check {
        return Ok(next.run(req).await);
    }

    let rid = error::request_id(req.headers());
    if let Some(token) = crate::auth::session_cookie_value(req.headers()) {
        let session = crate::auth::load_session(&state.pool, &token)
            .await
            .map_err(|e| ApiError::internal("resolving session", e).with_request_id(rid.clone()))?;

        if let Some(session) = session {
            let provided = req
                .headers()
                .get(crate::auth::CSRF_HEADER)
                .and_then(|v| v.to_str().ok());
            let valid = provided.is_some_and(|p| crate::auth::csrf_matches(&session.csrf_token, p));
            if !valid {
                return Err(ApiError::csrf_failed().with_request_id(rid));
            }
        }
    }

    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestApp, api_request, send};
    use axum::http::{Method, StatusCode, header};

    #[tokio::test]
    async fn unknown_api_paths_get_the_uniform_404() {
        let app = TestApp::new().await;
        let router = app.router();

        for uri in ["/api", "/api/", "/api/nope", "/api/nope/deep"] {
            let response = send(
                router.clone(),
                api_request(Method::GET, uri, None, None, None),
            )
            .await;
            assert_eq!(response.status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(response.body["error"]["code"], "route_not_found", "{uri}");
            assert!(
                response.body["error"].get("request_id").is_some(),
                "{uri}: {}",
                response.body
            );
        }

        // A state-changing unknown path is also just a 404: nonsense paths
        // are not gated behind CSRF.
        let post = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/nope",
                Some(serde_json::json!({})),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(post.status, StatusCode::NOT_FOUND);
        assert_eq!(post.body["error"]["code"], "route_not_found");
    }

    #[tokio::test]
    async fn wrong_api_methods_get_the_uniform_405() {
        let app = TestApp::new().await;
        let router = app.router();

        for (method, uri) in [
            (Method::GET, "/api/auth/login"),
            (Method::DELETE, "/api/audit"),
            (Method::POST, "/api/auth/me"),
        ] {
            let response = send(router.clone(), api_request(method, uri, None, None, None)).await;
            assert_eq!(response.status, StatusCode::METHOD_NOT_ALLOWED, "{uri}");
            assert_eq!(
                response.body["error"]["code"], "method_not_allowed",
                "{uri}: {}",
                response.body
            );
            assert!(
                response.body["error"].get("request_id").is_some(),
                "{uri}: {}",
                response.body
            );
        }
    }

    #[cfg(feature = "embedded-ui")]
    #[tokio::test]
    async fn non_api_paths_still_serve_the_embedded_ui() {
        let app = TestApp::new().await;
        let router = app.router();

        let index = send(
            router.clone(),
            api_request(Method::GET, "/", None, None, None),
        )
        .await;
        assert_eq!(index.status, StatusCode::OK);
        assert!(
            index.headers[header::CONTENT_TYPE]
                .to_str()
                .expect("ASCII header")
                .starts_with("text/html"),
            "app routes return the UI shell"
        );

        let asset = send(
            router.clone(),
            api_request(Method::GET, "/favicon.ico", None, None, None),
        )
        .await;
        assert_eq!(asset.status, StatusCode::OK);
    }

    #[cfg(feature = "embedded-ui")]
    #[tokio::test]
    async fn deep_dot_less_routes_fall_back_to_the_ui_shell() {
        let app = TestApp::new().await;
        let router = app.router();

        // A deep Angular route reloaded after deploy: no file extension, so
        // the fallback must serve the shell, not a 404 or an asset lookup.
        for uri in ["/dashboard", "/dashboard/deep/route"] {
            let response = send(
                router.clone(),
                api_request(Method::GET, uri, None, None, None),
            )
            .await;
            assert_eq!(response.status, StatusCode::OK, "{uri}");
            assert!(
                response.headers[header::CONTENT_TYPE]
                    .to_str()
                    .expect("ASCII header")
                    .starts_with("text/html"),
                "{uri}: dot-less routes return the UI shell"
            );
        }
    }
}
