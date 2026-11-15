//! Liveness and readiness of the process itself.
//!
//! This probe touches only the local database. It never calls an upstream
//! provider, so an upstream outage cannot take the health endpoint down.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;

use crate::{AppState, db};

#[derive(Debug, Serialize, ToSchema)]
pub struct Health {
    pub status: &'static str,
    pub version: String,
    pub db: &'static str,
    pub uptime_seconds: u64,
}

/// Process and database liveness.
#[utoipa::path(
    get,
    path = "/health",
    tag = "system",
    responses((status = 200, description = "process is healthy", body = Health))
)]
pub async fn get(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    let db_ok = db::ping(&state.pool).await.is_ok();
    if !db_ok {
        tracing::error!("database ping failed");
    }

    let (code, status, db) = match db_ok {
        true => (StatusCode::OK, "ok", "ok"),
        false => (StatusCode::SERVICE_UNAVAILABLE, "degraded", "error"),
    };

    (
        code,
        Json(Health {
            status,
            version: state.config.version.clone(),
            db,
            uptime_seconds: state.started.elapsed().as_secs(),
        }),
    )
}
