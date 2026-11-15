//! HTTP routes.

mod health;

use axum::Router;
use axum::routing::get;

use crate::AppState;

/// Routes that do not require authentication.
pub fn public_routes() -> Router<AppState> {
    Router::new().route("/health", get(health::get))
}
