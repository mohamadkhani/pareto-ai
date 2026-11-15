//! OpenAPI document endpoint.

use axum::Json;
use axum::extract::State;

use crate::{AppState, openapi::ApiDoc};

/// The full API surface as an OpenAPI 3 document.
#[utoipa::path(
    get,
    path = "/openapi.json",
    tag = "system",
    responses((status = 200, description = "OpenAPI document"))
)]
pub async fn get(State(state): State<AppState>) -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::build(&state.config.version))
}
