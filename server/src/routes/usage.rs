//! Usage reports and the request-log viewer (M4).
//!
//! `GET /api/usage/me` is self-service; `GET /api/usage` aggregates for
//! admins (`group_by=day|user|offering`); both read the `usage_daily` rollup,
//! which settlement keeps current and the job/command rebuild from
//! `usage_events`. `GET /api/logs` is the admin request-log viewer: metadata
//! only (decision D11), paginated with the same exclusive
//! `(occurred_at, request_id)` composite cursor as `/api/audit`.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;

use crate::AppState;
use crate::auth::{AuthUser, RequireAdmin, TS_FMT};
use crate::error::{self, ApiError, ErrorBody};
use crate::reports::{
    self, AttemptEntry, ErrorSummaryRow, GroupBy, LogListResponse, MyUsageReport, UsageReport,
};

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    /// Inclusive start day (`YYYY-MM-DD`).
    pub from: Option<String>,
    /// Inclusive end day (`YYYY-MM-DD`).
    pub to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AdminUsageQuery {
    /// Narrow to one user id.
    pub user: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    /// `day`, `user`, or `offering`. Defaults to `day`.
    pub group_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    /// Maximum entries to return (1–200). Defaults to 50.
    pub limit: Option<i64>,
    /// Exclusive cursor timestamp from `next_cursor`.
    pub before: Option<String>,
    /// Exclusive request id breaking ties at the `before` timestamp.
    pub before_request_id: Option<String>,
    /// Narrow to one user id.
    pub user: Option<String>,
    /// Exact settled status (for example `502`).
    pub status: Option<i64>,
    /// Taxonomy code such as `upstream_auth` or `upstream_timeout`.
    pub error_class: Option<String>,
    /// Only requests served (or attempted) by this provider id.
    pub provider: Option<String>,
    /// Only requests whose resolved offering is this id.
    pub offering: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ErrorSummaryQuery {
    /// Window in hours (1–168). Defaults to 24.
    pub hours: Option<i64>,
}

/// Validates an inclusive `YYYY-MM-DD` day key.
fn day_of(
    raw: Option<String>,
    name: &str,
    rid: &Option<String>,
) -> Result<Option<String>, ApiError> {
    match raw {
        None => Ok(None),
        Some(value) => {
            let valid =
                value.len() == 10 && chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d").is_ok();
            match valid {
                true => Ok(Some(value)),
                false => Err(
                    ApiError::bad_request(format!("{name} must be a YYYY-MM-DD day"))
                        .with_request_id(rid.clone()),
                ),
            }
        }
    }
}

/// The caller's own usage: totals plus one row per local day.
#[utoipa::path(
    get,
    path = "/api/usage/me",
    tag = "usage",
    params(
        ("from" = Option<String>, Query, description = "inclusive start day (YYYY-MM-DD)"),
        ("to" = Option<String>, Query, description = "inclusive end day (YYYY-MM-DD)")
    ),
    responses(
        (status = 200, description = "the caller's usage", body = MyUsageReport),
        (status = 400, description = "invalid query parameters", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody)
    )
)]
pub async fn usage_me(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    query: Result<Query<UsageQuery>, QueryRejection>,
) -> Result<Json<MyUsageReport>, ApiError> {
    let rid = error::request_id(&headers);
    let Query(query) = query.map_err(|e| {
        tracing::debug!(error = %e, "usage query rejected");
        ApiError::bad_request("invalid query parameters").with_request_id(rid.clone())
    })?;
    let from = day_of(query.from, "from", &rid)?;
    let to = day_of(query.to, "to", &rid)?;
    let report =
        reports::usage_for_user(&state.pool, &user.user_id, from.as_deref(), to.as_deref())
            .await
            .map_err(|e| ApiError::internal("reading usage", e).with_request_id(rid))?;
    Ok(Json(report))
}

/// Aggregated usage across accounts, grouped by day, user, or offering.
/// Admin only.
#[utoipa::path(
    get,
    path = "/api/usage",
    tag = "usage",
    params(
        ("user" = Option<String>, Query, description = "narrow to one user id"),
        ("from" = Option<String>, Query, description = "inclusive start day (YYYY-MM-DD)"),
        ("to" = Option<String>, Query, description = "inclusive end day (YYYY-MM-DD)"),
        ("group_by" = Option<String>, Query, description = "day, user, or offering (default day)")
    ),
    responses(
        (status = 200, description = "the aggregate report", body = UsageReport),
        (status = 400, description = "invalid query parameters", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn usage_admin(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
    query: Result<Query<AdminUsageQuery>, QueryRejection>,
) -> Result<Json<UsageReport>, ApiError> {
    let rid = error::request_id(&headers);
    let Query(query) = query.map_err(|e| {
        tracing::debug!(error = %e, "usage query rejected");
        ApiError::bad_request("invalid query parameters").with_request_id(rid.clone())
    })?;
    let group_by = match query.group_by.as_deref() {
        None => GroupBy::Day,
        Some(raw) => GroupBy::parse(raw).ok_or_else(|| {
            ApiError::bad_request("group_by must be `day`, `user`, or `offering`")
                .with_request_id(rid.clone())
        })?,
    };
    let from = day_of(query.from, "from", &rid)?;
    let to = day_of(query.to, "to", &rid)?;
    let report = reports::usage_admin(
        &state.pool,
        query.user.as_deref(),
        from.as_deref(),
        to.as_deref(),
        group_by,
    )
    .await
    .map_err(|e| ApiError::internal("reading usage", e).with_request_id(rid))?;
    Ok(Json(report))
}

/// One page of request logs, newest first: request metadata plus its settled
/// usage counters, never bodies (decision D11). Admin only.
#[utoipa::path(
    get,
    path = "/api/logs",
    tag = "usage",
    params(
        ("limit" = Option<i64>, Query, description = "Maximum entries to return (1-200). Default 50."),
        ("before" = Option<String>, Query, description = "RFC-3339 cursor timestamp; only strictly older entries are returned."),
        ("before_request_id" = Option<String>, Query, description = "Cursor request id from next_cursor; breaks ties with before."),
        ("user" = Option<String>, Query, description = "narrow to one user id"),
        ("status" = Option<i64>, Query, description = "exact settled status, e.g. 502"),
        ("error_class" = Option<String>, Query, description = "taxonomy code, e.g. upstream_auth"),
        ("provider" = Option<String>, Query, description = "narrow to one provider id"),
        ("offering" = Option<String>, Query, description = "narrow to one offering id")
    ),
    responses(
        (status = 200, description = "request log entries, newest first", body = LogListResponse),
        (status = 400, description = "invalid query parameters", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn logs(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
    query: Result<Query<LogQuery>, QueryRejection>,
) -> Result<Json<LogListResponse>, ApiError> {
    let rid = error::request_id(&headers);
    let Query(query) = query.map_err(|e| {
        tracing::debug!(error = %e, "log query rejected");
        ApiError::bad_request("invalid query parameters").with_request_id(rid.clone())
    })?;
    if let Some(status) = query.status
        && !(100..600).contains(&status)
    {
        return Err(
            ApiError::bad_request("status must be an HTTP status between 100 and 599")
                .with_request_id(rid),
        );
    }
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let (before, before_request_id) = match (query.before, query.before_request_id) {
        (Some(raw), before_request_id) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(&raw).map_err(|_| {
                ApiError::bad_request("before must be an ISO-8601 timestamp")
                    .with_request_id(rid.clone())
            })?;
            (
                Some(parsed.to_utc().format(TS_FMT).to_string()),
                before_request_id,
            )
        }
        (None, Some(_)) => {
            return Err(
                ApiError::bad_request("before_request_id requires before").with_request_id(rid)
            );
        }
        (None, None) => (None, None),
    };
    let report = reports::list_request_logs(
        &state.pool,
        query.user.as_deref(),
        query.status,
        query.error_class.as_deref(),
        query.provider.as_deref(),
        query.offering.as_deref(),
        limit,
        before.as_deref(),
        before_request_id.as_deref(),
    )
    .await
    .map_err(|e| ApiError::internal("reading request logs", e).with_request_id(rid))?;
    Ok(Json(report))
}

/// Every attempt of one request in attempt order, with the provider's own
/// error status, code, and bounded redacted text (D49). Admin only.
#[utoipa::path(
    get,
    path = "/api/logs/{request_id}/attempts",
    tag = "usage",
    params(("request_id" = String, Path, description = "the request id")),
    responses(
        (status = 200, description = "attempts, oldest first", body = [AttemptEntry]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn log_attempts(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(request_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<AttemptEntry>>, ApiError> {
    let rid = error::request_id(&headers);
    let attempts = reports::list_attempts(&state.pool, &request_id)
        .await
        .map_err(|e| ApiError::internal("reading request attempts", e).with_request_id(rid))?;
    Ok(Json(attempts))
}

/// Failed attempts in the last `hours` grouped by provider, worst first, for
/// the dashboard card (D49). Admin only.
#[utoipa::path(
    get,
    path = "/api/errors/summary",
    tag = "usage",
    params(("hours" = Option<i64>, Query, description = "window in hours (1-168, default 24)")),
    responses(
        (status = 200, description = "providers with failed attempts, worst first", body = [ErrorSummaryRow]),
        (status = 400, description = "invalid query parameters", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn error_summary(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
    query: Result<Query<ErrorSummaryQuery>, QueryRejection>,
) -> Result<Json<Vec<ErrorSummaryRow>>, ApiError> {
    let rid = error::request_id(&headers);
    let Query(query) = query.map_err(|e| {
        tracing::debug!(error = %e, "error summary query rejected");
        ApiError::bad_request("invalid query parameters").with_request_id(rid.clone())
    })?;
    let hours = query.hours.unwrap_or(24).clamp(1, 168);
    let rows = reports::error_summary(&state.pool, hours)
        .await
        .map_err(|e| ApiError::internal("reading the error summary", e).with_request_id(rid))?;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use crate::auth::Role;
    use crate::test_support::{self, TestApp, api_request, send};
    use axum::http::{Method, StatusCode};

    async fn seed_usage(
        app: &TestApp,
        user_id: &str,
        request_id: &str,
        day: &str,
        cost: Option<f64>,
    ) {
        sqlx::query(
            "INSERT INTO usage_events \
               (request_id, user_id, offering_id, model_id, input_tokens, output_tokens, \
                tokens_estimated, cost_usd, priced, started_at, occurred_at) \
             VALUES (?, ?, NULL, NULL, 10, 5, 0, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(user_id)
        .bind(cost)
        .bind(i64::from(cost.is_some()))
        .bind(day)
        .bind(day)
        .execute(&app.state.pool)
        .await
        .expect("usage event");
    }

    #[tokio::test]
    async fn usage_endpoints_are_rbac_gated_and_answer_report_shapes() {
        let app = TestApp::new().await;
        let admin_id = app
            .create_user("admin@example.com", Role::Admin, true)
            .await;
        let user_id = app.create_user("user@example.com", Role::User, true).await;
        let router = app.router();
        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;
        let (user_cookie, _) = test_support::login(&router, "user@example.com").await;

        seed_usage(&app, &user_id, "r1", "2026-11-16T10:00:00.000Z", Some(0.25)).await;
        seed_usage(&app, &user_id, "r2", "2026-11-16T11:00:00.000Z", None).await;
        crate::reports::rebuild_rollups(&app.state.pool, app.state.config.tz)
            .await
            .expect("rebuild");

        // Self-service: the user sees only their own two requests.
        let mine = send(
            router.clone(),
            api_request(Method::GET, "/api/usage/me", None, Some(&user_cookie), None),
        )
        .await;
        assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
        assert_eq!(mine.body["totals"]["requests"], 2);
        assert_eq!(mine.body["totals"]["unpriced_requests"], 1);
        assert_eq!(mine.body["days"].as_array().expect("days").len(), 1);

        // The admin aggregate groups and filters.
        let aggregate = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/usage?group_by=user",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(aggregate.status, StatusCode::OK, "{}", aggregate.body);
        assert_eq!(
            aggregate.body["groups"].as_array().expect("groups").len(),
            1
        );
        assert_eq!(aggregate.body["totals"]["requests"], 2);

        let ranged = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/usage?from=2026-11-17&group_by=day",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(ranged.body["totals"]["requests"], 0, "{}", ranged.body);

        // RBAC: the plain user may not aggregate, read logs, or pass bad
        // query parameters.
        for uri in [
            "/api/usage?group_by=day",
            "/api/logs",
            "/api/usage?group_by=bogus",
        ] {
            let response = send(
                router.clone(),
                api_request(Method::GET, uri, None, Some(&user_cookie), None),
            )
            .await;
            assert!(
                response.status == StatusCode::FORBIDDEN
                    || response.status == StatusCode::BAD_REQUEST,
                "{uri}: {}",
                response.body
            );
        }

        // The log viewer carries request metadata and the settled usage.
        sqlx::query(
            "INSERT INTO request_logs (request_id, occurred_at, method, path, status, user_id) \
             VALUES ('log-1', '2026-11-16T10:00:00.000Z', 'POST', '/v1/chat/completions', 200, ?)",
        )
        .bind(&user_id)
        .execute(&app.state.pool)
        .await
        .expect("log row");
        let logs = send(
            router.clone(),
            api_request(Method::GET, "/api/logs", None, Some(&admin_cookie), None),
        )
        .await;
        assert_eq!(logs.status, StatusCode::OK, "{}", logs.body);
        let entries = logs.body["entries"].as_array().expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["request_id"], "log-1");
        assert_eq!(entries[0]["path"], "/v1/chat/completions");
        assert!(
            logs.body.to_string().contains("decision") || entries[0].get("cost_usd").is_some(),
            "metadata and usage, never bodies: {}",
            logs.body
        );
        let _ = admin_id;
    }

    #[tokio::test]
    async fn upstream_error_detail_is_queryable_per_request_and_provider() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;

        sqlx::query(
            "INSERT INTO providers (id, name, base_url) VALUES ('p1', 'P', 'https://p/v1')",
        )
        .execute(&app.state.pool)
        .await
        .expect("provider");
        sqlx::query("INSERT INTO models (id, canonical_name) VALUES ('m1', 'm')")
            .execute(&app.state.pool)
            .await
            .expect("model");
        sqlx::query(
            "INSERT INTO offerings (id, provider_id, model_id, upstream_model_id) \
             VALUES ('o1', 'p1', 'm1', 'm-upstream')",
        )
        .execute(&app.state.pool)
        .await
        .expect("offering");
        sqlx::query(
            "INSERT INTO request_logs \
             (request_id, occurred_at, method, path, resolved_offering_id, status, error_class, \
              error_detail) \
             VALUES ('r1', '2026-11-19T10:00:00.000Z', 'POST', '/v1/chat/completions', 'o1', \
                     502, 'upstream_error', 'the provider exploded')",
        )
        .execute(&app.state.pool)
        .await
        .expect("log row");
        for (id, outcome, status, code, detail) in [
            (
                1i64,
                Some("upstream_error"),
                Some(502i64),
                Some("bad_gateway"),
                Some("boom"),
            ),
            (2, None, None, None, None),
        ] {
            sqlx::query(
                "INSERT INTO telemetry_attempts \
                 (id, request_id, offering_id, outcome, upstream_status, provider_error_code, \
                  error_detail, occurred_at) \
                 VALUES (?, 'r1', 'o1', ?, ?, ?, ?, '2026-11-19T10:00:01.000Z')",
            )
            .bind(id)
            .bind(outcome)
            .bind(status)
            .bind(code)
            .bind(detail)
            .execute(&app.state.pool)
            .await
            .expect("attempt");
        }

        let filtered = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/logs?error_class=upstream_error&provider=p1",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(filtered.status, StatusCode::OK, "{}", filtered.body);
        let entries = filtered.body["entries"].as_array().expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["error_detail"], "the provider exploded");

        let wrong_status = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/logs?status=200",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(
            wrong_status.body["entries"]
                .as_array()
                .expect("entries")
                .len(),
            0
        );

        let attempts = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/logs/r1/attempts",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(attempts.status, StatusCode::OK, "{}", attempts.body);
        let rows = attempts.body.as_array().expect("attempts");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["error_detail"], "boom");
        assert_eq!(rows[0]["upstream_status"], 502);
        assert_eq!(rows[0]["provider_error_code"], "bad_gateway");
        assert_eq!(rows[1]["outcome"], serde_json::Value::Null);

        let summary = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/errors/summary?hours=168",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(summary.status, StatusCode::OK, "{}", summary.body);
        let rows = summary.body.as_array().expect("summary");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["provider_id"], "p1");
        assert_eq!(rows[0]["errors"], 1);
    }

    #[tokio::test]
    async fn breaker_snapshot_and_reset_are_admin_only() {
        let app = TestApp::new().await;
        let admin_id = app
            .create_user("admin@example.com", Role::Admin, true)
            .await;
        app.create_user("user@example.com", Role::User, true).await;
        let router = app.router();
        let (admin_cookie, admin_csrf) = test_support::login(&router, "admin@example.com").await;
        let (user_cookie, _) = test_support::login(&router, "user@example.com").await;

        app.state.breaker.record_failure(
            "cred-1",
            "o1",
            "upstream_error",
            Some("boom"),
            std::time::Instant::now(),
        );
        let list = send(
            router.clone(),
            api_request(Method::GET, "/api/breaker", None, Some(&admin_cookie), None),
        )
        .await;
        assert_eq!(list.status, StatusCode::OK, "{}", list.body);
        let entries = list.body["entries"].as_array().expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["credential_id"], "cred-1");
        assert_eq!(entries[0]["last_error_class"], "upstream_error");
        assert_eq!(entries[0]["last_error_detail"], "boom");

        let denied = send(
            router.clone(),
            api_request(Method::GET, "/api/breaker", None, Some(&user_cookie), None),
        )
        .await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);

        let reset = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/breaker/reset",
                Some(serde_json::json!({"credential_id": "cred-1", "offering_id": "o1"})),
                Some(&admin_cookie),
                Some(&admin_csrf),
            ),
        )
        .await;
        assert_eq!(reset.status, StatusCode::NO_CONTENT, "{}", reset.body);
        assert!(
            !app.state
                .breaker
                .is_open("cred-1", "o1", std::time::Instant::now())
        );

        let audited: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'breaker.reset'")
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
        assert_eq!(audited, 1, "actor {}", admin_id);
    }
}
