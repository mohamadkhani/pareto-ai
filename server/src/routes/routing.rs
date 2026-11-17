//! Admin plane for M5 routing: combo CRUD, benchmark import, and model
//! quality overrides (spec M5 scope items 2, 4, 5).
//!
//! The handlers are thin — validate, call [`crate::combos`] /
//! [`crate::benchmarks`], map the result. Every mutation is audited by the
//! service in the same transaction as the change (`combo.create`,
//! `combo.update`, `combo.delete`, `sync.benchmarks`, `quality.override`,
//! `quality.override_clear`). The two list reads are audited too
//! (`benchmarks.read`, `quality.override_read`), matching `GET /api/audit`'s
//! `audit.read`: access to the imported scores and the operator's overrides
//! is visible in the trail, and an audit failure fails the read rather than
//! returning unaudited data. The data plane stays untouched: a combo is
//! addressed as the model id `combo:<name>` in `/v1/chat/completions`, and
//! the "why this model" explanation rides in `decision_json` (surfaced by
//! `GET /api/logs`).

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{self, ClientIp, RequireAdmin};
use crate::benchmark_sync::{self, HttpFetcher, SourceId};
use crate::benchmarks::{self, BenchmarkScoreRow, QualityOverrideRow};
use crate::catalog::Actor;
use crate::chat;
use crate::combos::{self, ComboInput, ComboView, PreviewResult};
use crate::error::{self, ApiError, ErrorBody};
use crate::routes::bad_body;

/// What one benchmark import run changed.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImportOutcomeView {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    /// Entries whose model matched no catalog model.
    pub skipped_unknown_model: usize,
}

/// `POST /api/benchmarks/sync` body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BenchmarkSyncInput {
    /// Sources to sync; omitted or empty means all three.
    pub sources: Option<Vec<String>>,
    /// Report the counts without writing benchmarks or run rows.
    pub dry_run: Option<bool>,
}

/// Per-source counts of one sync.
#[derive(Debug, Serialize, ToSchema)]
pub struct BenchmarkSyncCountsView {
    /// Source entries the import could use.
    pub published: usize,
    /// Catalog models that received a benchmark row (one row per model; a
    /// hint can attach to several models of the same normalized identity).
    pub matched: usize,
    /// Same value as `matched`, named for the model-level contract.
    pub matched_models: usize,
    /// Source entries that resolved to at least one model.
    pub matched_hints: usize,
    pub skipped_unknown: usize,
    /// Entries naming several distinct models (never guessed). Duplicate
    /// entries for one model keep the better rank and are not counted here.
    pub ambiguous: usize,
    /// Pages that failed after every retry before the source aborted.
    pub failed_pages: usize,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
}

/// One source's result in a sync response.
#[derive(Debug, Serialize, ToSchema)]
pub struct BenchmarkSyncResultView {
    pub source: String,
    /// `success` or `error`.
    pub status: String,
    pub dry_run: bool,
    pub counts: BenchmarkSyncCountsView,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: i64,
    /// The attribution URL stored on this source's rows.
    pub source_url: String,
    pub error: Option<String>,
}

/// The sync response: one result per requested source.
#[derive(Debug, Serialize, ToSchema)]
pub struct BenchmarkSyncResponse {
    pub dry_run: bool,
    pub results: Vec<BenchmarkSyncResultView>,
}

/// One recorded benchmark sync run, as shown by the status endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct BenchmarkSyncRunView {
    pub id: String,
    pub source: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: i64,
    pub published: i64,
    pub matched: i64,
    pub matched_models: i64,
    pub matched_hints: i64,
    pub skipped_unknown: i64,
    pub ambiguous: i64,
    pub failed_pages: i64,
    pub inserted: i64,
    pub updated: i64,
    pub unchanged: i64,
    pub source_url: Option<String>,
    pub error: Option<String>,
}

/// One source's last-run status. The scalar fields mirror `last_run` at the
/// top level so the Models toolbar can read one object per source; `last_run`
/// keeps the full run row for a detail view.
#[derive(Debug, Serialize, ToSchema)]
pub struct BenchmarkSourceStatusView {
    pub source: String,
    pub license: String,
    /// The attribution URL the UI shows next to the scores.
    pub attribution_url: String,
    /// Whether the last success is inside the 5 h TTL.
    pub fresh: bool,
    /// `success`, `error`, or `never` before the first run.
    pub status: String,
    pub counts: Option<BenchmarkSyncCountsView>,
    pub error: Option<String>,
    pub finished_at: Option<String>,
    /// The last run's stored attribution URL (the source's for `never`).
    pub source_url: Option<String>,
    pub last_run: Option<BenchmarkSyncRunView>,
}

/// `GET /api/benchmarks/sync/status`.
#[derive(Debug, Serialize, ToSchema)]
pub struct BenchmarkSyncStatusView {
    pub ttl_hours: i64,
    pub sources: Vec<BenchmarkSourceStatusView>,
}

/// The operator's quality override for a model.
#[derive(Debug, Deserialize, ToSchema)]
pub struct QualityOverrideInput {
    /// The model's quality score on the imported benchmark scale (the
    /// source's native Elo-like scale, decision D25). A value far outside the
    /// imported benchmark range is rejected as a likely scale error.
    pub score: f64,
}

/// The stored quality override.
#[derive(Debug, Serialize, ToSchema)]
pub struct QualityOverrideView {
    pub model_id: String,
    pub score: f64,
}

/// Lists every combo. Admin only.
#[utoipa::path(
    get,
    path = "/api/combos",
    tag = "routing",
    responses(
        (status = 200, description = "the combos", body = [ComboView]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list_combos(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
) -> Result<Json<Vec<ComboView>>, ApiError> {
    let rid = error::request_id(&headers);
    let combos = combos::list(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(combos.iter().map(ComboView::from).collect()))
}

/// Creates one combo. Admin only, audited as `combo.create`.
#[utoipa::path(
    post,
    path = "/api/combos",
    tag = "routing",
    request_body = ComboInput,
    responses(
        (status = 201, description = "combo created", body = ComboView),
        (status = 400, description = "invalid combo (strategy, weights, members)", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody)
    )
)]
pub async fn create_combo(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<ComboInput>, JsonRejection>,
) -> Result<(StatusCode, Json<ComboView>), ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "combo create", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let combo = combos::create(&state.pool, &actor, &input)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok((StatusCode::CREATED, Json(ComboView::from(&combo))))
}

/// Replaces a combo's strategy, weights, members, and enabled flag (its name
/// is immutable). Admin only, audited as `combo.update`.
#[utoipa::path(
    put,
    path = "/api/combos/{id}",
    tag = "routing",
    params(("id" = String, Path, description = "combo id")),
    request_body = ComboInput,
    responses(
        (status = 200, description = "combo updated", body = ComboView),
        (status = 400, description = "invalid combo", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such combo", body = ErrorBody)
    )
)]
pub async fn update_combo(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ComboInput>, JsonRejection>,
) -> Result<Json<ComboView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "combo update", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let combo = combos::update(&state.pool, &actor, &id, &input)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    match combo {
        Some(combo) => Ok(Json(ComboView::from(&combo))),
        None => Err(ApiError::not_found("combo").with_request_id(rid)),
    }
}

/// Deletes one combo. Admin only, audited as `combo.delete`.
#[utoipa::path(
    delete,
    path = "/api/combos/{id}",
    tag = "routing",
    params(("id" = String, Path, description = "combo id")),
    responses(
        (status = 204, description = "combo deleted"),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such combo", body = ErrorBody)
    )
)]
pub async fn delete_combo(
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
    let deleted = combos::delete(&state.pool, &actor, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    match deleted {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::not_found("combo").with_request_id(rid)),
    }
}

/// A draft combo plus the preview limit (default 25, max 100).
#[derive(Debug, Deserialize, ToSchema)]
pub struct ComboPreviewInput {
    #[serde(flatten)]
    pub config: ComboInput,
    pub limit: Option<usize>,
}

/// Optional query for a saved combo's preview.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ComboPreviewQuery {
    /// An `X-Session-Id`-shaped key to show the current pin for.
    pub session_key: Option<String>,
    /// Candidate limit (default 25, max 100).
    pub limit: Option<usize>,
}

/// Previews a draft combo without saving it: the ranked candidates and the
/// exclusions, computed by the same engine the runtime uses. Admin only. This
/// is a read-like computation, so it is mounted **outside** the CSRF guard
/// (like login) and writes nothing — no audit row, no pin.
#[utoipa::path(
    post,
    path = "/api/combos/preview",
    tag = "routing",
    request_body = ComboPreviewInput,
    responses(
        (status = 200, description = "ranked candidates and exclusions", body = PreviewResult),
        (status = 400, description = "invalid combo", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn preview_combo(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
    body: Result<Json<ComboPreviewInput>, JsonRejection>,
) -> Result<Json<PreviewResult>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "combo preview", &rid)?;
    let limit = input.limit.unwrap_or(25).clamp(1, 100);
    let combo = combos::validate_config(&state.pool, &input.config)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    let preview = chat::preview_combo(&state, &combo, None, limit)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(preview))
}

/// Previews a saved combo (GET, so no CSRF token). For a sticky combo the
/// response includes the current pin when `session_key` is supplied.
#[utoipa::path(
    get,
    path = "/api/combos/{id}/preview",
    tag = "routing",
    params(
        ("id" = String, Path, description = "combo id"),
        ("session_key" = Option<String>, Query, description = "session key to show the current pin for"),
        ("limit" = Option<usize>, Query, description = "candidate limit (default 25, max 100)")
    ),
    responses(
        (status = 200, description = "ranked candidates, exclusions, and the current pin", body = PreviewResult),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody),
        (status = 404, description = "no such combo", body = ErrorBody)
    )
)]
pub async fn preview_saved_combo(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<String>,
    Query(query): Query<ComboPreviewQuery>,
    headers: HeaderMap,
) -> Result<Json<PreviewResult>, ApiError> {
    let rid = error::request_id(&headers);
    let combo = combos::get(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .ok_or_else(|| ApiError::not_found("combo").with_request_id(rid.clone()))?;
    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    let preview = chat::preview_combo(&state, &combo, query.session_key.as_deref(), limit)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(preview))
}

/// Lists every imported benchmark score with its model's name, newest first
/// (`fetched_at` descending; ties — rows of one imported document — break
/// deterministically on `model_id`, then `category`; see
/// [`benchmarks::list_scores`]). Admin only. Reading the list is itself
/// audited (`benchmarks.read`, like `GET /api/audit`'s `audit.read`), and an
/// audit failure fails the request rather than returning unaudited data.
#[utoipa::path(
    get,
    path = "/api/benchmarks",
    tag = "routing",
    responses(
        (status = 200, description = "imported benchmark scores, newest first", body = [BenchmarkScoreRow]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list_benchmarks(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
) -> Result<Json<Vec<BenchmarkScoreRow>>, ApiError> {
    let rid = error::request_id(&headers);
    let rows = benchmarks::list_scores(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    let detail = serde_json::json!({ "rows": rows.len() }).to_string();
    auth::audit(
        &state.pool,
        Some(&admin.user_id),
        "benchmarks.read",
        Some("benchmarks"),
        None,
        Some(&detail),
        Some(&ip.0),
    )
    .await
    .map_err(|e| ApiError::internal("auditing the benchmark read", e).with_request_id(rid))?;
    Ok(Json(rows))
}

/// Imports one leaderboard document (an LMArena-style JSON body; the recorded
/// fixture stands in for the live fetch, which is blocked in this
/// environment). Admin only, audited as `sync.benchmarks`.
#[utoipa::path(
    post,
    path = "/api/benchmarks/import",
    tag = "routing",
    request_body = String,
    responses(
        (status = 200, description = "what the import changed", body = ImportOutcomeView),
        (status = 400, description = "malformed leaderboard document", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody)
    )
)]
pub async fn import_benchmarks(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<axum::body::Bytes, axum::extract::rejection::BytesRejection>,
) -> Result<Json<ImportOutcomeView>, ApiError> {
    let rid = error::request_id(&headers);
    let raw = body.map_err(|e| {
        tracing::debug!(error = %e, "benchmark import body rejected");
        ApiError::bad_request("invalid request body").with_request_id(rid.clone())
    })?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let outcome = benchmarks::run_import(&state.pool, &actor, &String::from_utf8_lossy(&raw))
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(ImportOutcomeView {
        inserted: outcome.inserted,
        updated: outcome.updated,
        unchanged: outcome.unchanged,
        skipped_unknown_model: outcome.skipped_unknown_model,
    }))
}

fn benchmark_counts_view(counts: benchmark_sync::Counts) -> BenchmarkSyncCountsView {
    BenchmarkSyncCountsView {
        published: counts.published,
        matched: counts.matched,
        matched_models: counts.matched,
        matched_hints: counts.matched_hints,
        skipped_unknown: counts.skipped_unknown,
        ambiguous: counts.ambiguous,
        failed_pages: counts.failed_pages,
        inserted: counts.inserted,
        updated: counts.updated,
        unchanged: counts.unchanged,
    }
}

fn benchmark_result_view(outcome: &benchmark_sync::SourceOutcome) -> BenchmarkSyncResultView {
    BenchmarkSyncResultView {
        source: outcome.source.as_str().to_string(),
        status: outcome.status.as_str().to_string(),
        dry_run: outcome.dry_run,
        counts: benchmark_counts_view(outcome.counts),
        started_at: outcome.started_at.clone(),
        finished_at: outcome.finished_at.clone(),
        duration_ms: outcome.duration_ms,
        source_url: outcome.source_url.clone(),
        error: outcome.error.clone(),
    }
}

fn every_source_failed(dry_run: bool, results: &[BenchmarkSyncResultView]) -> bool {
    !dry_run && !results.is_empty() && results.iter().all(|result| result.status == "error")
}

fn benchmark_run_view(row: &benchmark_sync::RunRow) -> BenchmarkSyncRunView {
    BenchmarkSyncRunView {
        id: row.id.clone(),
        source: row.source.clone(),
        status: row.status.clone(),
        started_at: row.started_at.clone(),
        finished_at: row.finished_at.clone(),
        duration_ms: row.duration_ms,
        published: row.published,
        matched: row.matched,
        matched_models: row.matched,
        matched_hints: row.matched_hints,
        skipped_unknown: row.skipped_unknown,
        ambiguous: row.ambiguous,
        failed_pages: row.failed_pages,
        inserted: row.inserted,
        updated: row.updated,
        unchanged: row.unchanged,
        source_url: row.source_url.clone(),
        error: row.error.clone(),
    }
}

/// Downloads benchmark scores from the requested sources (all three by
/// default). Admin only, audited per source as `sync.benchmark_sources`.
///
/// One source failing never stops the others: its run row has
/// `status = "error"` and the response still lists every result. The request
/// itself only fails with `502` when *every* requested source failed. A run
/// overlapping the background job (or another trigger) is rejected with
/// `409`. `dry_run` writes nothing — no benchmarks, run rows, or audits.
#[utoipa::path(
    post,
    path = "/api/benchmarks/sync",
    tag = "routing",
    request_body = BenchmarkSyncInput,
    responses(
        (status = 200, description = "per-source sync results", body = BenchmarkSyncResponse),
        (status = 400, description = "unknown source name", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 409, description = "another sync is running", body = ErrorBody),
        (status = 502, description = "every requested source failed", body = ErrorBody)
    )
)]
pub async fn sync_benchmarks(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<BenchmarkSyncInput>, JsonRejection>,
) -> Result<Json<BenchmarkSyncResponse>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "benchmark sync", &rid)?;

    let mut sources = Vec::new();
    for raw in input.sources.unwrap_or_default() {
        let source = SourceId::parse(&raw).ok_or_else(|| {
            ApiError::bad_request(format!("unknown benchmark source {raw:?}"))
                .with_request_id(rid.clone())
        })?;
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    if sources.is_empty() {
        sources.extend(SourceId::ALL);
    }
    let dry_run = input.dry_run.unwrap_or(false);

    let _guard = state.benchmark_sync.try_lock().map_err(|_| {
        ApiError::conflict("another benchmark sync is already running").with_request_id(rid.clone())
    })?;

    let fetcher = HttpFetcher::new().map_err(|e| {
        ApiError::internal("building the benchmark sync client", e).with_request_id(rid.clone())
    })?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let outcomes =
        benchmark_sync::run_sources(&state.pool, &sources, &fetcher, dry_run, Some(&actor)).await;
    let results: Vec<BenchmarkSyncResultView> =
        outcomes.iter().map(benchmark_result_view).collect();

    if every_source_failed(dry_run, &results) {
        return Err(
            ApiError::bad_gateway("every requested benchmark source failed").with_request_id(rid),
        );
    }
    Ok(Json(BenchmarkSyncResponse { dry_run, results }))
}

/// The last run per benchmark source, with each source's licence and the 5 h
/// freshness flag. Admin only, read-only (not audited).
#[utoipa::path(
    get,
    path = "/api/benchmarks/sync/status",
    tag = "routing",
    responses(
        (status = 200, description = "last run per source and freshness", body = BenchmarkSyncStatusView),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn benchmark_sync_status(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
) -> Result<Json<BenchmarkSyncStatusView>, ApiError> {
    let rid = error::request_id(&headers);
    let now = chrono::Utc::now();
    let mut sources = Vec::with_capacity(SourceId::ALL.len());
    for source in SourceId::ALL {
        let last = benchmark_sync::last_run(&state.pool, source)
            .await
            .map_err(|e| e.into_api_error(rid.clone()))?;
        let last_success = benchmark_sync::last_success_at(&state.pool, source)
            .await
            .map_err(|e| e.into_api_error(rid.clone()))?;
        let counts = last.as_ref().map(|row| BenchmarkSyncCountsView {
            published: row.published as usize,
            matched: row.matched as usize,
            matched_models: row.matched as usize,
            matched_hints: row.matched_hints as usize,
            skipped_unknown: row.skipped_unknown as usize,
            ambiguous: row.ambiguous as usize,
            failed_pages: row.failed_pages as usize,
            inserted: row.inserted as usize,
            updated: row.updated as usize,
            unchanged: row.unchanged as usize,
        });
        sources.push(BenchmarkSourceStatusView {
            source: source.as_str().to_string(),
            license: source.license().to_string(),
            attribution_url: source.source_url().to_string(),
            fresh: benchmark_sync::is_fresh(last_success.as_deref(), &now),
            status: last
                .as_ref()
                .map_or_else(|| "never".to_string(), |row| row.status.clone()),
            counts,
            error: last.as_ref().and_then(|row| row.error.clone()),
            finished_at: last.as_ref().map(|row| row.finished_at.clone()),
            source_url: Some(
                last.as_ref()
                    .and_then(|row| row.source_url.clone())
                    .unwrap_or_else(|| source.source_url().to_string()),
            ),
            last_run: last.as_ref().map(benchmark_run_view),
        });
    }
    Ok(Json(BenchmarkSyncStatusView {
        ttl_hours: (benchmark_sync::SYNC_TTL.as_secs() / 3600) as i64,
        sources,
    }))
}

/// Lists every stored quality override with its model's name, newest first/// (`updated_at` descending; ties break on `model_id`; see
/// [`benchmarks::list_overrides`]). Admin only. Reading the list is itself
/// audited (`quality.override_read`, like `GET /api/audit`'s `audit.read`),
/// and an audit failure fails the request rather than returning unaudited
/// data.
#[utoipa::path(
    get,
    path = "/api/quality-overrides",
    tag = "routing",
    responses(
        (status = 200, description = "stored quality overrides, newest first", body = [QualityOverrideRow]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list_quality_overrides(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
) -> Result<Json<Vec<QualityOverrideRow>>, ApiError> {
    let rid = error::request_id(&headers);
    let rows = benchmarks::list_overrides(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    let detail = serde_json::json!({ "rows": rows.len() }).to_string();
    auth::audit(
        &state.pool,
        Some(&admin.user_id),
        "quality.override_read",
        Some("model_quality_overrides"),
        None,
        Some(&detail),
        Some(&ip.0),
    )
    .await
    .map_err(|e| {
        ApiError::internal("auditing the quality-override read", e).with_request_id(rid)
    })?;
    Ok(Json(rows))
}

/// Sets the operator's quality override for a model (it wins over every
/// synced benchmark). Admin only, audited as `quality.override`.
#[utoipa::path(
    put,
    path = "/api/models/{id}/quality-override",
    tag = "routing",
    params(("id" = String, Path, description = "model id")),
    request_body = QualityOverrideInput,
    responses(
        (status = 200, description = "override stored", body = QualityOverrideView),
        (status = 400, description = "score must be finite and on the imported benchmark scale", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody)
    )
)]
pub async fn set_quality_override(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<QualityOverrideInput>, JsonRejection>,
) -> Result<Json<QualityOverrideView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "quality override", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    benchmarks::set_override(&state.pool, &actor, &id, input.score)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(QualityOverrideView {
        model_id: id,
        score: input.score,
    }))
}

/// Removes a model's quality override (synced benchmarks take over again).
/// Admin only, audited as `quality.override_clear`.
#[utoipa::path(
    delete,
    path = "/api/models/{id}/quality-override",
    tag = "routing",
    params(("id" = String, Path, description = "model id")),
    responses(
        (status = 204, description = "override removed"),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no override for this model", body = ErrorBody)
    )
)]
pub async fn clear_quality_override(
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
    let cleared = benchmarks::clear_override(&state.pool, &actor, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    match cleared {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::not_found("quality override").with_request_id(rid)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::http::Method;
    use serde_json::json;

    use super::*;
    use crate::auth::Role;
    use crate::catalog::{self, ModelInput, ProviderInput};
    use crate::test_support::{TestApp, api_request, login, send};

    const FIXTURE: &str = include_str!("../../tests/fixtures/leaderboard.json");

    /// Seeds a provider with one offering and returns (offering_id, model_id).
    async fn seeded_offering(app: &TestApp, uid: &str) -> (String, String) {
        let actor = Actor {
            user_id: uid,
            ip: "127.0.0.1",
        };
        let provider = catalog::create_provider(
            &app.state.pool,
            &actor,
            &ProviderInput {
                name: "p1".to_string(),
                base_url: "http://127.0.0.1:1".to_string(),
                kind: None,
                enabled: None,
            },
        )
        .await
        .expect("provider");
        let model = catalog::create_model(
            &app.state.pool,
            &actor,
            &ModelInput {
                canonical_name: "m".to_string(),
                family: None,
                modalities_input: None,
                modalities_output: None,
                context_length: None,
                max_output_tokens: None,
                capabilities: None,
            },
        )
        .await
        .expect("model");
        let offering = catalog::create_offering(
            &app.state.pool,
            &app.state.secrets,
            &actor,
            &catalog::OfferingInput {
                provider_id: provider.id,
                model_id: model.id.clone(),
                upstream_model_id: "up".to_string(),
                context_override: None,
                max_output_override: None,
                enabled: None,
                headers: None,
            },
        )
        .await
        .expect("offering");
        (offering.id, model.id)
    }

    /// Creates one catalog model per canonical name (the fixture's models).
    async fn seed_models(app: &TestApp, uid: &str) -> HashMap<String, String> {
        let actor = Actor {
            user_id: uid,
            ip: "127.0.0.1",
        };
        let mut models = HashMap::new();
        for name in [
            "gemini-2.5-pro",
            "claude-sonnet-4.5",
            "gpt-4o",
            "deepseek-v3",
        ] {
            let model = catalog::create_model(
                &app.state.pool,
                &actor,
                &ModelInput {
                    canonical_name: name.to_string(),
                    family: None,
                    modalities_input: None,
                    modalities_output: None,
                    context_length: None,
                    max_output_tokens: None,
                    capabilities: None,
                },
            )
            .await
            .expect("model");
            models.insert(name.to_string(), model.id);
        }
        models
    }

    #[tokio::test]
    async fn combo_crud_round_trips_through_the_api() {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let (offering_id, _) = seeded_offering(&app, &uid).await;
        let (cookie, csrf) = login(&app.router(), "admin@example.test").await;

        let created = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/combos",
                Some(json!({
                    "name": "test",
                    "strategy": "weighted",
                    "weights": {"preset": "cheapest"},
                    "members": [{"offering_id": offering_id}],
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        assert_eq!(created.body["name"], "test");
        assert_eq!(created.body["preset"], "cheapest");
        let combo_id = created.body["id"].as_str().expect("id").to_string();

        let list = send(
            app.router(),
            api_request(Method::GET, "/api/combos", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(list.status, StatusCode::OK);
        assert_eq!(list.body.as_array().expect("list").len(), 1);

        let updated = send(
            app.router(),
            api_request(
                Method::PUT,
                &format!("/api/combos/{combo_id}"),
                Some(json!({
                    "name": "test",
                    "strategy": "ordered",
                    "members": [{"offering_id": offering_id, "weight": 2.0}],
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
        assert_eq!(updated.body["strategy"], "ordered");
        assert_eq!(updated.body["members"][0]["weight"], 2.0);

        let deleted = send(
            app.router(),
            api_request(
                Method::DELETE,
                &format!("/api/combos/{combo_id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn the_routing_admin_endpoints_require_an_admin_session_and_csrf() {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let (offering_id, model_id) = seeded_offering(&app, &uid).await;

        // No session at all: 401 everywhere.
        for (method, uri) in [
            (Method::GET, "/api/combos".to_string()),
            (Method::POST, "/api/benchmarks/import".to_string()),
            (
                Method::DELETE,
                format!("/api/models/{model_id}/quality-override"),
            ),
        ] {
            let response = send(app.router(), api_request(method, &uri, None, None, None)).await;
            assert_eq!(response.status, StatusCode::UNAUTHORIZED, "{uri}");
        }

        // A session without the CSRF token cannot mutate.
        let (cookie, _csrf) = login(&app.router(), "admin@example.test").await;
        let no_csrf = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/combos",
                Some(json!({
                    "name": "x",
                    "strategy": "ordered",
                    "members": [{"offering_id": offering_id}],
                })),
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_list_endpoints_are_admin_only() {
        let app = TestApp::new().await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        app.create_user("user@example.test", Role::User, true).await;
        let router = app.router();

        for uri in ["/api/benchmarks", "/api/quality-overrides"] {
            let anonymous = send(
                router.clone(),
                api_request(Method::GET, uri, None, None, None),
            )
            .await;
            assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED, "{uri}");

            let (user_cookie, _) = login(&router, "user@example.test").await;
            let user = send(
                router.clone(),
                api_request(Method::GET, uri, None, Some(&user_cookie), None),
            )
            .await;
            assert_eq!(user.status, StatusCode::FORBIDDEN, "{uri}");
            assert_eq!(user.body["error"]["code"], "forbidden", "{uri}");
        }
    }

    #[tokio::test]
    async fn the_lists_show_what_the_write_paths_stored() {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let models = seed_models(&app, &uid).await;
        let (cookie, csrf) = login(&app.router(), "admin@example.test").await;

        // The write paths: one leaderboard import and one quality override.
        let imported = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/benchmarks/import",
                Some(serde_json::from_str(FIXTURE).expect("fixture is JSON")),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(imported.status, StatusCode::OK, "{}", imported.body);
        assert_eq!(imported.body["inserted"], 6);

        let claude = &models["claude-sonnet-4.5"];
        let overridden = send(
            app.router(),
            api_request(
                Method::PUT,
                &format!("/api/models/{claude}/quality-override"),
                Some(json!({ "score": 1450.0 })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(overridden.status, StatusCode::OK, "{}", overridden.body);

        // GET /api/benchmarks: a bare array with the exact row shape.
        let scores = send(
            app.router(),
            api_request(Method::GET, "/api/benchmarks", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(scores.status, StatusCode::OK, "{}", scores.body);
        let rows = scores.body.as_array().expect("bare array");
        assert_eq!(rows.len(), 6, "the fixture's six entries");
        for row in rows {
            assert_eq!(
                row.as_object().expect("object").len(),
                11,
                "exact row shape: {row}"
            );
        }
        let gemini = rows
            .iter()
            .find(|row| row["model_name"] == "gemini-2.5-pro" && row["category"] == "text")
            .expect("gemini text row");
        assert_eq!(gemini["id"].as_str().expect("id").len(), 36);
        assert_eq!(gemini["model_id"], models["gemini-2.5-pro"].as_str());
        assert_eq!(gemini["source"], "lmarena");
        assert_eq!(gemini["scale"], "elo");
        assert_eq!(gemini["score"], 1432.5);
        assert_eq!(gemini["ci"], 45.0);
        assert_eq!(gemini["rank"], 1);
        assert_eq!(gemini["source_url"], "https://lmarena.ai/leaderboard");
        assert_eq!(gemini["fetched_at"], "2026-11-10T00:00:00.000Z");

        // GET /api/quality-overrides: a bare array with the exact row shape.
        let overrides = send(
            app.router(),
            api_request(
                Method::GET,
                "/api/quality-overrides",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(overrides.status, StatusCode::OK, "{}", overrides.body);
        let rows = overrides.body.as_array().expect("bare array");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            row.as_object().expect("object").len(),
            5,
            "exact row shape: {row}"
        );
        assert_eq!(row["model_id"], claude.as_str());
        assert_eq!(row["model_name"], "claude-sonnet-4.5");
        assert_eq!(row["score"], 1450.0);
        assert_eq!(row["updated_by"], uid.as_str());
        assert!(row["updated_at"].is_string());

        // Reading is audited like GET /api/audit.
        for action in ["benchmarks.read", "quality.override_read"] {
            let reads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = ?")
                .bind(action)
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
            assert_eq!(reads, 1, "{action}");
        }
    }

    #[tokio::test]
    async fn the_lists_are_empty_when_nothing_is_stored() {
        let app = TestApp::new().await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        let (cookie, _) = login(&app.router(), "admin@example.test").await;

        for uri in ["/api/benchmarks", "/api/quality-overrides"] {
            let response = send(
                app.router(),
                api_request(Method::GET, uri, None, Some(&cookie), None),
            )
            .await;
            assert_eq!(response.status, StatusCode::OK, "{uri}");
            assert_eq!(response.body, json!([]), "{uri}");
        }
    }

    fn result(source: &str, status: &str) -> BenchmarkSyncResultView {
        BenchmarkSyncResultView {
            source: source.to_string(),
            status: status.to_string(),
            dry_run: false,
            counts: BenchmarkSyncCountsView {
                published: 1,
                matched: 1,
                matched_models: 1,
                matched_hints: 1,
                skipped_unknown: 0,
                ambiguous: 0,
                failed_pages: 0,
                inserted: 1,
                updated: 0,
                unchanged: 0,
            },
            started_at: "2026-11-16T00:00:00.000Z".to_string(),
            finished_at: "2026-11-16T00:00:01.000Z".to_string(),
            duration_ms: 1000,
            source_url: "https://example.test".to_string(),
            error: None,
        }
    }

    #[test]
    fn only_an_all_failed_run_maps_to_502() {
        assert!(every_source_failed(
            false,
            &[result("arena", "error"), result("benchlm", "error")]
        ));
        assert!(
            !every_source_failed(
                false,
                &[result("arena", "error"), result("benchlm", "success")]
            ),
            "one success keeps the response a 200"
        );
        assert!(
            !every_source_failed(true, &[result("arena", "error")]),
            "a dry run reports the failure in the body, not as 502"
        );
        assert!(!every_source_failed(false, &[]), "no request, no 502");
    }

    #[tokio::test]
    async fn benchmark_sync_requires_admin_csrf_and_known_sources() {
        let app = TestApp::new().await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        app.create_user("user@example.test", Role::User, true).await;
        let router = app.router();

        let anonymous = send(
            router.clone(),
            api_request(Method::POST, "/api/benchmarks/sync", None, None, None),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

        let (user_cookie, user_csrf) = login(&router, "user@example.test").await;
        let non_admin = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/benchmarks/sync",
                Some(json!({"sources": ["arena"]})),
                Some(&user_cookie),
                Some(&user_csrf),
            ),
        )
        .await;
        assert_eq!(non_admin.status, StatusCode::FORBIDDEN);

        let (admin_cookie, admin_csrf) = login(&router, "admin@example.test").await;
        let no_csrf = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/benchmarks/sync",
                Some(json!({"sources": ["arena"]})),
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);

        // Unknown names are rejected before any fetch happens.
        let unknown = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/benchmarks/sync",
                Some(json!({"sources": ["nope"]})),
                Some(&admin_cookie),
                Some(&admin_csrf),
            ),
        )
        .await;
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "{}", unknown.body);
        assert!(
            unknown.body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("nope")),
            "{}",
            unknown.body
        );
    }

    #[tokio::test]
    async fn a_running_sync_rejects_a_second_trigger() {
        let app = TestApp::new().await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        let (cookie, csrf) = login(&app.router(), "admin@example.test").await;

        let _guard = app
            .state
            .benchmark_sync
            .try_lock()
            .expect("the gate starts free");
        let conflict = send(
            app.router(),
            api_request(
                Method::POST,
                "/api/benchmarks/sync",
                Some(json!({"sources": ["arena"], "dry_run": true})),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.body);
        assert_eq!(conflict.body["error"]["code"], "conflict");
    }

    #[tokio::test]
    async fn benchmark_sync_status_reports_match_and_page_counts() {
        let app = TestApp::new().await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        let (cookie, _) = login(&app.router(), "admin@example.test").await;

        sqlx::query(
            "INSERT INTO benchmark_sync_runs \
               (id, source, status, started_at, finished_at, duration_ms, published, matched, \
                matched_hints, skipped_unknown, ambiguous, failed_pages, inserted, updated, \
                unchanged, source_url) \
             VALUES ('run-1', 'benchmarklist', 'error', ?, ?, 120, 10, 6, 4, 2, 1, 1, 5, 1, 0, ?)",
        )
        .bind("2026-11-18T00:00:00.000Z")
        .bind("2026-11-18T00:00:00.120Z")
        .bind(benchmark_sync::BENCHMARKLIST_ATTRIBUTION_URL)
        .execute(&app.state.pool)
        .await
        .expect("run row");

        let response = send(
            app.router(),
            api_request(
                Method::GET,
                "/api/benchmarks/sync/status",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        let source = &response.body["sources"][1];
        assert_eq!(source["source"], "benchmarklist");
        let counts = &source["counts"];
        assert_eq!(counts["published"], 10);
        assert_eq!(counts["matched"], 6);
        assert_eq!(counts["matched_models"], 6);
        assert_eq!(counts["matched_hints"], 4);
        assert_eq!(counts["skipped_unknown"], 2);
        assert_eq!(counts["ambiguous"], 1);
        assert_eq!(counts["failed_pages"], 1);
        let last_run = &source["last_run"];
        assert_eq!(last_run["matched_models"], 6);
        assert_eq!(last_run["matched_hints"], 4);
        assert_eq!(last_run["failed_pages"], 1);
    }

    #[tokio::test]
    async fn benchmark_sync_status_lists_every_source_and_the_ttl() {
        let app = TestApp::new().await;
        app.create_user("admin@example.test", Role::Admin, true)
            .await;
        app.create_user("user@example.test", Role::User, true).await;
        let router = app.router();

        let (user_cookie, _) = login(&router, "user@example.test").await;
        let non_admin = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/benchmarks/sync/status",
                None,
                Some(&user_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(non_admin.status, StatusCode::FORBIDDEN);

        let (cookie, _) = login(&router, "admin@example.test").await;
        let response = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/benchmarks/sync/status",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert_eq!(response.body["ttl_hours"], 5);
        let sources = response.body["sources"].as_array().expect("sources");
        assert_eq!(sources.len(), 3);
        let names: Vec<&str> = sources
            .iter()
            .map(|source| source["source"].as_str().expect("name"))
            .collect();
        assert_eq!(names, ["arena", "benchmarklist", "benchlm"]);
        let arena = &sources[0];
        assert_eq!(arena["license"], "CC BY 4.0");
        assert_eq!(
            arena["attribution_url"],
            benchmark_sync::ARENA_ATTRIBUTION_URL
        );
        assert_eq!(arena["fresh"], false, "nothing downloaded yet");
        assert_eq!(arena["status"], "never");
        assert!(arena["counts"].is_null());
        assert!(arena["last_run"].is_null());
    }
}
