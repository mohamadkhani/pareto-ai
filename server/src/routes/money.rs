//! Budget and token-cap administration (M4, decision D9).
//!
//! Caps are hard gates enforced by the reservation ledger
//! ([`crate::money`]); these routes configure them per user and period.
//! Reads are self-or-admin (so the dashboard can show "your budget");
//! writes are admin-only and audited (`budget.*` / `token_cap.*`).

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{AuthUser, ClientIp, RequireAdmin};
use crate::catalog::Actor;
use crate::error::{self, ApiError, ErrorBody};
use crate::money::{self, BudgetView, Period, TokenCapView};
use crate::routes::{bad_body, ensure_self_or_admin};

#[derive(Debug, Deserialize, ToSchema)]
pub struct BudgetInput {
    /// Hard spend limit in USD for the period; `0` blocks all spend.
    pub limit_usd: f64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TokenCapInput {
    /// Hard token limit for the model in the period; `0` blocks all use.
    pub limit_tokens: i64,
}

/// Parses a period path segment.
fn period_of(raw: &str, rid: &Option<String>) -> Result<Period, ApiError> {
    Period::parse(raw).ok_or_else(|| {
        ApiError::bad_request("period must be `day`, `week`, `month`, or `total`")
            .with_request_id(rid.clone())
    })
}

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

/// Lists the user's budget rows (missing rows mean "unlimited"). The user or
/// an admin may read.
#[utoipa::path(
    get,
    path = "/api/users/{id}/budgets",
    tag = "money",
    params(("id" = String, Path, description = "user id")),
    responses(
        (status = 200, description = "the user's budgets", body = [BudgetView]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not your account", body = ErrorBody)
    )
)]
pub async fn list_budgets(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<BudgetView>>, ApiError> {
    let rid = error::request_id(&headers);
    ensure_self_or_admin(&user, &id, &rid)?;
    let rows = money::list_budgets(&state.pool, state.config.tz, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(rows))
}

/// Sets (upserts) one budget period's USD limit. Admin only, audited as
/// `budget.set`.
#[utoipa::path(
    put,
    path = "/api/users/{id}/budgets/{period}",
    tag = "money",
    params(
        ("id" = String, Path, description = "user id"),
        ("period" = String, Path, description = "day, week, month, or total")
    ),
    request_body = BudgetInput,
    responses(
        (status = 200, description = "the budget row", body = BudgetView),
        (status = 400, description = "invalid body or period", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such user", body = ErrorBody)
    )
)]
pub async fn set_budget(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path((id, period)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<BudgetInput>, JsonRejection>,
) -> Result<Json<BudgetView>, ApiError> {
    let rid = error::request_id(&headers);
    let period = period_of(&period, &rid)?;
    let Json(input) = bad_body(body, "budget set", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let view = money::set_budget(
        &state.pool,
        &actor,
        state.config.tz,
        &id,
        period,
        input.limit_usd,
    )
    .await
    .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Removes one budget period's cap (unlimited again). Admin only, audited as
/// `budget.clear`.
#[utoipa::path(
    delete,
    path = "/api/users/{id}/budgets/{period}",
    tag = "money",
    params(
        ("id" = String, Path, description = "user id"),
        ("period" = String, Path, description = "day, week, month, or total")
    ),
    responses(
        (status = 204, description = "budget removed"),
        (status = 400, description = "invalid period", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such budget", body = ErrorBody)
    )
)]
pub async fn clear_budget(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path((id, period)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let rid = error::request_id(&headers);
    let period = period_of(&period, &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let deleted = money::clear_budget(&state.pool, &actor, &id, period)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    match deleted {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::not_found("budget").with_request_id(rid)),
    }
}

// ---------------------------------------------------------------------------
// Token caps
// ---------------------------------------------------------------------------

/// Lists the user's per-model token caps. The user or an admin may read.
#[utoipa::path(
    get,
    path = "/api/users/{id}/token-caps",
    tag = "money",
    params(("id" = String, Path, description = "user id")),
    responses(
        (status = 200, description = "the user's token caps", body = [TokenCapView]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not your account", body = ErrorBody)
    )
)]
pub async fn list_token_caps(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<TokenCapView>>, ApiError> {
    let rid = error::request_id(&headers);
    ensure_self_or_admin(&user, &id, &rid)?;
    let rows = money::list_token_caps(&state.pool, state.config.tz, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(rows))
}

/// Sets (upserts) one per-model token cap. Admin only, audited as
/// `token_cap.set`.
#[utoipa::path(
    put,
    path = "/api/users/{id}/token-caps/{model_id}/{period}",
    tag = "money",
    params(
        ("id" = String, Path, description = "user id"),
        ("model_id" = String, Path, description = "model id"),
        ("period" = String, Path, description = "day, week, month, or total")
    ),
    request_body = TokenCapInput,
    responses(
        (status = 200, description = "the token cap row", body = TokenCapView),
        (status = 400, description = "invalid body or period", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such user or model", body = ErrorBody)
    )
)]
pub async fn set_token_cap(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path((id, model_id, period)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Result<Json<TokenCapInput>, JsonRejection>,
) -> Result<Json<TokenCapView>, ApiError> {
    let rid = error::request_id(&headers);
    let period = period_of(&period, &rid)?;
    let Json(input) = bad_body(body, "token cap set", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let view = money::set_token_cap(
        &state.pool,
        &actor,
        state.config.tz,
        &id,
        &model_id,
        period,
        input.limit_tokens,
    )
    .await
    .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Removes one token cap. Admin only, audited as `token_cap.clear`.
#[utoipa::path(
    delete,
    path = "/api/users/{id}/token-caps/{model_id}/{period}",
    tag = "money",
    params(
        ("id" = String, Path, description = "user id"),
        ("model_id" = String, Path, description = "model id"),
        ("period" = String, Path, description = "day, week, month, or total")
    ),
    responses(
        (status = 204, description = "token cap removed"),
        (status = 400, description = "invalid period", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such token cap", body = ErrorBody)
    )
)]
pub async fn clear_token_cap(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path((id, model_id, period)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let rid = error::request_id(&headers);
    let period = period_of(&period, &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let deleted = money::clear_token_cap(&state.pool, &actor, &id, &model_id, period)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    match deleted {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::not_found("token cap").with_request_id(rid)),
    }
}
