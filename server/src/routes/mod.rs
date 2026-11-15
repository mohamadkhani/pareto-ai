//! HTTP routes.
//!
//! Public routes (`/health`, `/openapi.json`) need no session. The API router
//! is mounted at `/api` and carries its own fallbacks, so unknown `/api/*`
//! paths and wrong methods get the uniform [`ApiError`] shape (`404
//! route_not_found`, `405 method_not_allowed`) while every other path keeps
//! the embedded UI fallback. Everything under `/api/` goes through
//! [`csrf_guard`]: state-changing methods must carry the session's CSRF token
//! in `X-CSRF-Token`, except for login, which is mounted outside the guard
//! because the caller has no session yet.

pub(crate) mod audit;
pub(crate) mod auth;
pub(crate) mod health;
pub(crate) mod openapi;

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

/// Auth and admin routes, mounted at `/api`. Mutations require a valid
/// session and its CSRF token.
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
}

/// The API surface as it sees itself, under the `/api` prefix.
fn api_surface(state: AppState) -> Router<AppState> {
    // Login sits outside the CSRF guard: the caller has no session yet.
    let csrf_protected = Router::new()
        .route("/auth/logout", axum::routing::post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/audit", get(audit::list))
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
