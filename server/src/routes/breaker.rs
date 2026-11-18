//! Operator visibility for the per-(credential, offering) breaker (D50).
//!
//! The registry itself is in-memory: this module exposes its snapshot and the
//! manual reset. Trip history lives in `breaker_trips` (written by the chat
//! runner when a pair opens) and is served from the database, so a restart
//! clears the live state but not the record of why it opened.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{ClientIp, RequireAdmin};
use crate::breaker::BreakerStatus;
use crate::error::{self, ApiError, ErrorBody};
use crate::routes::bad_body;

/// One pair's live breaker state.
#[derive(Debug, Serialize, ToSchema)]
pub struct BreakerEntry {
    pub credential_id: String,
    pub offering_id: String,
    /// `closed`, `open`, or `half_open`.
    pub status: String,
    /// Consecutive counted failures with no success in between.
    pub consecutive: u32,
    /// How many times this pair has opened since the process started.
    pub trips: u32,
    /// Remaining milliseconds of the open window; `null` when not open.
    pub open_for_ms: Option<u64>,
    /// The taxonomy class of the last counted failure.
    pub last_error_class: Option<String>,
    /// The bounded, redacted provider text of the last counted failure (D49).
    pub last_error_detail: Option<String>,
}

/// Every pair with breaker state.
#[derive(Debug, Serialize, ToSchema)]
pub struct BreakerListResponse {
    pub entries: Vec<BreakerEntry>,
}

/// The pair to close manually.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BreakerResetInput {
    pub credential_id: String,
    pub offering_id: String,
}

/// Lists every pair that has breaker state. Admin only.
#[utoipa::path(
    get,
    path = "/api/breaker",
    tag = "admin",
    responses(
        (status = 200, description = "live breaker state", body = BreakerListResponse),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> Json<BreakerListResponse> {
    let entries = state
        .breaker
        .snapshot(std::time::Instant::now())
        .into_iter()
        .map(|row| BreakerEntry {
            credential_id: row.credential_id,
            offering_id: row.offering_id,
            status: match row.status {
                BreakerStatus::Closed => "closed",
                BreakerStatus::Open { .. } => "open",
                BreakerStatus::HalfOpen => "half_open",
            }
            .to_string(),
            consecutive: row.consecutive,
            trips: row.trips,
            open_for_ms: row.open_for_ms,
            last_error_class: row.last_error_class.map(str::to_string),
            last_error_detail: row.last_error_detail,
        })
        .collect();
    Json(BreakerListResponse { entries })
}

/// Closes one pair and clears its streak, audited as `breaker.reset`. Admin
/// only, CSRF-protected by the `/api` guard. Idempotent: resetting a pair with
/// no state still answers `204`.
#[utoipa::path(
    post,
    path = "/api/breaker/reset",
    tag = "admin",
    request_body = BreakerResetInput,
    responses(
        (status = 204, description = "the pair is closed"),
        (status = 400, description = "invalid body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn reset(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<BreakerResetInput>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "breaker reset", &rid)?;
    if input.credential_id.trim().is_empty() || input.offering_id.trim().is_empty() {
        return Err(
            ApiError::bad_request("credential_id and offering_id are required")
                .with_request_id(rid),
        );
    }
    let existed = state
        .breaker
        .reset(input.credential_id.trim(), input.offering_id.trim());
    if existed {
        let detail = serde_json::json!({
            "credential_id": input.credential_id.trim(),
            "offering_id": input.offering_id.trim(),
        });
        let target = format!(
            "{}:{}",
            input.credential_id.trim(),
            input.offering_id.trim()
        );
        let mut tx = state.pool.begin().await.map_err(|e| {
            ApiError::internal("recording the breaker reset", e).with_request_id(rid.clone())
        })?;
        crate::auth::audit(
            &mut *tx,
            Some(&admin.user_id),
            "breaker.reset",
            Some("credential_offering"),
            Some(&target),
            Some(&detail.to_string()),
            Some(&ip.0),
        )
        .await
        .map_err(|e| {
            ApiError::internal("recording the breaker reset", e).with_request_id(rid.clone())
        })?;
        tx.commit().await.map_err(|e| {
            ApiError::internal("recording the breaker reset", e).with_request_id(rid)
        })?;
    }
    Ok(StatusCode::NO_CONTENT)
}
