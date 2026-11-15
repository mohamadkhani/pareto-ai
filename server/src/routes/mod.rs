//! HTTP routes.

pub(crate) mod health;
pub(crate) mod openapi;

use axum::Router;
use axum::routing::get;

use crate::AppState;

/// Routes that do not require authentication.
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::get))
        .route("/openapi.json", get(openapi::get))
}
