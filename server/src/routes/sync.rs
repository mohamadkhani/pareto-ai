//! OpenRouter catalog/price sync trigger and status.
//!
//! `POST /api/sync/openrouter` runs a sync (audited as `sync.openrouter` by
//! the sync itself, in the same transaction as the run row). A failed fetch
//! is not a 5xx: the previous snapshot keeps serving and the outcome reports
//! `stale-if-error` (D10). `GET /api/sync/openrouter/status` reads the last
//! run and the snapshot's freshness against the 24h TTL.

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;

use crate::AppState;
use crate::auth::{ClientIp, RequireAdmin};
use crate::catalog::{Actor, SyncOutcomeView, SyncRunView, SyncStatusView};
use crate::error::{self, ApiError, ErrorBody};
use crate::pricing::openrouter::{self, SyncRunRow};

/// Maps a run row to its API view (detail parsed, never secret material).
fn run_view(row: &SyncRunRow) -> SyncRunView {
    SyncRunView {
        id: row.id.clone(),
        source: row.source.clone(),
        started_at: row.started_at.clone(),
        finished_at: row.finished_at.clone(),
        status: row.status.clone(),
        model_count: row.model_count,
        detail: row
            .detail_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok()),
    }
}

/// Triggers an OpenRouter catalog/price sync. Admin only.
///
/// The response is always 200 with the recorded outcome: `success`,
/// `stale-if-error` (fetch failed, previous snapshot served), or `error`
/// (fetch failed and no snapshot exists). Exactly one `price_sync_runs` row
/// and one `sync.openrouter` audit row are written per trigger.
#[utoipa::path(
    post,
    path = "/api/sync/openrouter",
    tag = "catalog",
    responses(
        (status = 200, description = "sync outcome (also recorded in price_sync_runs)", body = SyncOutcomeView),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 500, description = "the run could not be recorded", body = ErrorBody)
    )
)]
pub async fn trigger(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
) -> Result<Json<SyncOutcomeView>, ApiError> {
    let rid = error::request_id(&headers);
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let outcome = openrouter::run_sync(&state.pool, openrouter::fetch_models, &actor)
        .await
        .map_err(|e| ApiError::internal("openrouter sync", e).with_request_id(rid))?;

    Ok(Json(SyncOutcomeView {
        run_id: outcome.run_id,
        status: outcome.status.as_str().to_string(),
        model_count: outcome.model_count,
        stale: outcome.stale,
        started_at: outcome.started_at,
        finished_at: outcome.finished_at,
        error: outcome.error,
    }))
}

/// Reads the last sync run and whether the snapshot is past its 24h TTL.
/// Admin only, read-only (not audited).
#[utoipa::path(
    get,
    path = "/api/sync/openrouter/status",
    tag = "catalog",
    responses(
        (status = 200, description = "last sync run and snapshot freshness", body = SyncStatusView),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn status(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
) -> Result<Json<SyncStatusView>, ApiError> {
    let rid = error::request_id(&headers);
    let last = openrouter::last_run(&state.pool)
        .await
        .map_err(|e| ApiError::internal("reading sync status", e).with_request_id(rid.clone()))?;

    // Freshness is judged on the last *successful* run: that is the snapshot
    // being served, whatever the most recent attempt's status was.
    let snapshot_at = openrouter::snapshot_finished_at(&state.pool)
        .await
        .map_err(|e| ApiError::internal("reading sync status", e).with_request_id(rid))?;
    let now = chrono::Utc::now();
    let snapshot_stale = !openrouter::snapshot_is_fresh(snapshot_at.as_deref(), &now);

    Ok(Json(SyncStatusView {
        last_run: last.as_ref().map(run_view),
        snapshot_stale,
        ttl_hours: (openrouter::SNAPSHOT_TTL.as_secs() / 3600) as i64,
    }))
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use crate::auth::Role;
    use crate::catalog::Actor;
    use crate::pricing::openrouter::{self, SyncStatus};
    use crate::test_support::{self, TestApp, api_request, send};
    use axum::http::{Method, StatusCode};
    use serde_json::json;

    const FIXTURE: &str = include_str!("../../tests/fixtures/openrouter_models.json");

    fn fixture_fetch() -> impl Future<Output = Result<String, openrouter::SyncError>> {
        std::future::ready(Ok(FIXTURE.to_string()))
    }

    #[tokio::test]
    async fn sync_status_reports_the_last_run_and_snapshot_freshness() {
        let app = TestApp::new().await;
        let admin_id = app
            .create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, _csrf) = test_support::login(&router, "admin@example.com").await;

        // Before any sync: no run, and the missing snapshot is stale.
        let empty = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/sync/openrouter/status",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(empty.status, StatusCode::OK, "{}", empty.body);
        assert!(empty.body["last_run"].is_null());
        assert_eq!(empty.body["snapshot_stale"], true);
        assert_eq!(empty.body["ttl_hours"], 24);

        // A successful sync (fixture-driven; the live endpoint is unreachable
        // from this network) makes the snapshot fresh.
        let outcome = openrouter::run_sync(
            &app.state.pool,
            fixture_fetch,
            &Actor {
                user_id: &admin_id,
                ip: "127.0.0.1",
            },
        )
        .await
        .expect("sync");
        assert_eq!(outcome.status, SyncStatus::Success);

        let status = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/sync/openrouter/status",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(status.status, StatusCode::OK);
        assert_eq!(status.body["last_run"]["status"], "success");
        assert_eq!(status.body["last_run"]["model_count"], 5);
        assert_eq!(status.body["last_run"]["source"], "openrouter");
        assert_eq!(status.body["snapshot_stale"], false);
        assert!(
            status.body["last_run"]["detail"]["raw_prices"].is_object(),
            "the run detail carries the raw source prices"
        );

        // A failed attempt records its run and flips nothing about the
        // snapshot's freshness (the last success is still the snapshot).
        openrouter::run_sync(
            &app.state.pool,
            || std::future::ready(Err(openrouter::SyncError::Fetch("timed out".to_string()))),
            &Actor {
                user_id: &admin_id,
                ip: "127.0.0.1",
            },
        )
        .await
        .expect("failed run");
        let after_failure = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/sync/openrouter/status",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(after_failure.body["last_run"]["status"], "stale-if-error");
        assert_eq!(after_failure.body["snapshot_stale"], false);
    }

    #[tokio::test]
    async fn sync_trigger_requires_an_admin_session_and_csrf() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        app.create_user("user@example.com", Role::User, true).await;
        let router = app.router();

        // These reject before the handler runs, so no live fetch happens.
        let anonymous = send(
            router.clone(),
            api_request(Method::POST, "/api/sync/openrouter", None, None, None),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

        let (user_cookie, user_csrf) = test_support::login(&router, "user@example.com").await;
        let non_admin = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/sync/openrouter",
                None,
                Some(&user_cookie),
                Some(&user_csrf),
            ),
        )
        .await;
        assert_eq!(non_admin.status, StatusCode::FORBIDDEN);

        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;
        let no_csrf = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/sync/openrouter",
                None,
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
        assert_eq!(no_csrf.body["error"]["code"], "csrf_failed");
    }

    #[tokio::test]
    async fn manual_price_override_survives_a_later_sync() {
        let app = TestApp::new().await;
        let admin_id = app
            .create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let actor = Actor {
            user_id: &admin_id,
            ip: "127.0.0.1",
        };

        // The fixture sync creates the openrouter provider and offerings.
        openrouter::run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("first sync");
        let offering_id: String = sqlx::query_scalar(
            "SELECT o.id FROM offerings o JOIN models m ON m.id = o.model_id \
             WHERE m.canonical_name = 'openai/gpt-4o'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("offering id");

        // The admin overrides that price over HTTP.
        let override_response = send(
            router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({ "input_per_m": 42.0, "output_per_m": 84.0 })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(
            override_response.status,
            StatusCode::OK,
            "{}",
            override_response.body
        );
        assert_eq!(override_response.body["source"], "manual");

        // A later sync updates everything else but leaves the manual row.
        openrouter::run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("second sync");
        let (source, input, output): (String, Option<f64>, Option<f64>) = sqlx::query_as(
            "SELECT source, input_per_m, output_per_m FROM prices WHERE offering_id = ?",
        )
        .bind(&offering_id)
        .fetch_one(&app.state.pool)
        .await
        .expect("price row");
        assert_eq!(source, "manual");
        assert_eq!(
            input,
            Some(42.0),
            "the sync must not overwrite manual prices"
        );
        assert_eq!(output, Some(84.0));

        // And /v1/models advertises the manual price.
        let listing = send(
            router.clone(),
            api_request(Method::GET, "/v1/models", None, Some(&cookie), None),
        )
        .await;
        let entry = listing.body["data"]
            .as_array()
            .expect("data")
            .iter()
            .find(|m| m["id"] == "openai/gpt-4o")
            .expect("gpt-4o offering");
        assert_eq!(entry["price"]["input_per_m"], 42.0);
    }
}
