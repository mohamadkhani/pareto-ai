//! Admin CRUD for the access-grants matrix (M4, decision D9).
//!
//! Every change is audited (`grant.create` / `grant.delete`) in the same
//! transaction as the change itself. Evaluation semantics live in
//! [`crate::access`]: `deny` beats `allow`, admins are allowed by default,
//! users are denied unless granted, a key with no grants inherits its owner,
//! and a key with grants can only narrow that owner's access (D59).

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;

use crate::AppState;
use crate::access::{self, GrantInput, GrantView};
use crate::auth::{ClientIp, RequireAdmin};
use crate::catalog::Actor;
use crate::error::{self, ApiError, ErrorBody};
use crate::routes::bad_body;

#[derive(Debug, Deserialize)]
pub struct GrantQuery {
    /// Filter by subject kind (`user` or `api_key`).
    pub subject_type: Option<String>,
    /// Filter by subject id.
    pub subject_id: Option<String>,
}

/// Lists grants, optionally filtered by subject. Admin only.
#[utoipa::path(
    get,
    path = "/api/grants",
    tag = "grants",
    params(
        ("subject_type" = Option<String>, Query, description = "`user` or `api_key`"),
        ("subject_id" = Option<String>, Query, description = "user or api key id")
    ),
    responses(
        (status = 200, description = "the grants", body = [GrantView]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list_grants(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
    query: Result<Query<GrantQuery>, QueryRejection>,
) -> Result<Json<Vec<GrantView>>, ApiError> {
    let rid = error::request_id(&headers);
    let Query(query) = query.map_err(|e| {
        tracing::debug!(error = %e, "grant query rejected");
        ApiError::bad_request("invalid query parameters").with_request_id(rid.clone())
    })?;
    let rows = access::list_grants(
        &state.pool,
        query.subject_type.as_deref(),
        query.subject_id.as_deref(),
    )
    .await
    .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(rows))
}

/// Creates one grant binding a subject to a resource. Admin only, audited as
/// `grant.create`.
#[utoipa::path(
    post,
    path = "/api/grants",
    tag = "grants",
    request_body = GrantInput,
    responses(
        (status = 201, description = "grant created", body = GrantView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "unknown subject or resource", body = ErrorBody),
        (status = 409, description = "the subject and resource already have a grant", body = ErrorBody)
    )
)]
pub async fn create_grant(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<GrantInput>, JsonRejection>,
) -> Result<(StatusCode, Json<GrantView>), ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "grant create", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let view = access::create_grant(&state.pool, &actor, &input)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Deletes one grant. Admin only, audited as `grant.delete`.
#[utoipa::path(
    delete,
    path = "/api/grants/{id}",
    tag = "grants",
    params(("id" = String, Path, description = "grant id")),
    responses(
        (status = 204, description = "grant deleted"),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such grant", body = ErrorBody)
    )
)]
pub async fn delete_grant(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let rid = error::request_id(&headers);
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let deleted = access::delete_grant(&state.pool, &actor, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    match deleted {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::not_found("grant").with_request_id(rid)),
    }
}
