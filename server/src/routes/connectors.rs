//! Admin routes for provider connectors (D42), price refresh, and balance
//! refresh.
//!
//! Every mutation is admin-only, CSRF-protected by the `/api` guard, and
//! audited (`connector.update`, `provider.prices_refresh`). Sample and test
//! are read-like computations that write nothing; they still require an admin
//! session and CSRF because they send provider requests with a credential's
//! secret.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::HeaderMap;

use crate::AppState;
use crate::auth::{ClientIp, RequireAdmin};
use crate::catalog::Actor;
use crate::connectors::{self, ConnectorDraft, ConnectorKind, ConnectorPut};
use crate::error::{self, ApiError};
use crate::routes::bad_body;

fn parse_kind(raw: &str) -> Result<ConnectorKind, ApiError> {
    ConnectorKind::parse(raw).ok_or_else(|| {
        ApiError::bad_request("kind must be catalog, balance, or health".to_string())
    })
}

/// The shipped connector templates (plain JSON, fully editable). Admin only.
#[utoipa::path(
    get,
    path = "/api/providers/{id}/connectors/templates",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    responses(
        (status = 200, description = "selectable connector templates", body = [connectors::ConnectorTemplate]),
        (status = 401, description = "not authenticated", body = crate::error::ErrorBody),
        (status = 403, description = "not an admin", body = crate::error::ErrorBody),
        (status = 404, description = "no such provider", body = crate::error::ErrorBody)
    )
)]
pub async fn templates(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<connectors::ConnectorTemplate>>, ApiError> {
    let rid = error::request_id(&headers);
    if crate::catalog::get_provider(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .is_none()
    {
        return Err(ApiError::not_found("provider").with_request_id(rid));
    }
    Ok(Json(connectors::templates()))
}

/// Reads one connector. Admin only.
#[utoipa::path(
    get,
    path = "/api/providers/{id}/connectors/{kind}",
    tag = "admin",
    params(
        ("id" = String, Path, description = "provider id"),
        ("kind" = String, Path, description = "catalog, balance, or health")
    ),
    responses(
        (status = 200, description = "the connector (configured=false when absent)", body = connectors::ConnectorView),
        (status = 401, description = "not authenticated", body = crate::error::ErrorBody),
        (status = 403, description = "not an admin", body = crate::error::ErrorBody),
        (status = 404, description = "no such provider", body = crate::error::ErrorBody)
    )
)]
pub async fn get(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path((id, kind)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<connectors::ConnectorView>, ApiError> {
    let rid = error::request_id(&headers);
    let kind = parse_kind(&kind)?;
    if crate::catalog::get_provider(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .is_none()
    {
        return Err(ApiError::not_found("provider").with_request_id(rid));
    }
    let view = connectors::connector_view(&state.pool, &id, kind)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Validates and stores one connector, audited as `connector.update`.
#[utoipa::path(
    put,
    path = "/api/providers/{id}/connectors/{kind}",
    tag = "admin",
    params(
        ("id" = String, Path, description = "provider id"),
        ("kind" = String, Path, description = "catalog, balance, or health")
    ),
    request_body = ConnectorPut,
    responses(
        (status = 200, description = "connector stored", body = connectors::ConnectorView),
        (status = 400, description = "invalid config", body = crate::error::ErrorBody),
        (status = 401, description = "not authenticated", body = crate::error::ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = crate::error::ErrorBody),
        (status = 404, description = "no such provider", body = crate::error::ErrorBody)
    )
)]
pub async fn put(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path((id, kind)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ConnectorPut>, JsonRejection>,
) -> Result<Json<connectors::ConnectorView>, ApiError> {
    let rid = error::request_id(&headers);
    let kind = parse_kind(&kind)?;
    let Json(put) = bad_body(body, "connector update", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let view = connectors::put_connector(&state.pool, &actor, &id, kind, &put)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Fetches the document and returns a truncated raw JSON body (≤200 KiB).
#[utoipa::path(
    post,
    path = "/api/providers/{id}/connectors/{kind}/sample",
    tag = "admin",
    params(
        ("id" = String, Path, description = "provider id"),
        ("kind" = String, Path, description = "catalog, balance, or health")
    ),
    request_body = ConnectorDraft,
    responses(
        (status = 200, description = "raw sample (truncated)", body = connectors::SampleView),
        (status = 400, description = "invalid config", body = crate::error::ErrorBody),
        (status = 401, description = "not authenticated", body = crate::error::ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = crate::error::ErrorBody),
        (status = 404, description = "no such provider", body = crate::error::ErrorBody),
        (status = 502, description = "upstream fetch failed", body = crate::error::ErrorBody)
    )
)]
pub async fn sample(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path((id, kind)): Path<(String, String)>,
    headers: HeaderMap,
    body: Option<Json<ConnectorDraft>>,
) -> Result<Json<connectors::SampleView>, ApiError> {
    let rid = error::request_id(&headers);
    let kind = parse_kind(&kind)?;
    let (draft, credential_id) = match body {
        Some(Json(draft)) => (draft.config, draft.credential_id),
        None => (None, None),
    };
    let view = connectors::sample(
        &state.pool,
        &state.secrets,
        &id,
        kind,
        draft.as_ref(),
        credential_id.as_deref(),
    )
    .await
    .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Applies the current or draft config and returns extracted values. Writes
/// nothing.
#[utoipa::path(
    post,
    path = "/api/providers/{id}/connectors/{kind}/test",
    tag = "admin",
    params(
        ("id" = String, Path, description = "provider id"),
        ("kind" = String, Path, description = "catalog, balance, or health")
    ),
    request_body = ConnectorDraft,
    responses(
        (status = 200, description = "extraction preview", body = connectors::ConnectorTestView),
        (status = 400, description = "invalid config", body = crate::error::ErrorBody),
        (status = 401, description = "not authenticated", body = crate::error::ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = crate::error::ErrorBody),
        (status = 404, description = "no such provider", body = crate::error::ErrorBody),
        (status = 502, description = "upstream fetch failed", body = crate::error::ErrorBody)
    )
)]
pub async fn test(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path((id, kind)): Path<(String, String)>,
    headers: HeaderMap,
    body: Option<Json<ConnectorDraft>>,
) -> Result<Json<connectors::ConnectorTestView>, ApiError> {
    let rid = error::request_id(&headers);
    let kind = parse_kind(&kind)?;
    let (draft, credential_id) = match body {
        Some(Json(draft)) => (draft.config, draft.credential_id),
        None => (None, None),
    };
    let view = connectors::test_connector(
        &state.pool,
        &state.secrets,
        &id,
        kind,
        draft.as_ref(),
        credential_id.as_deref(),
    )
    .await
    .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Input of `POST /api/providers/{id}/prices/refresh`.
#[derive(Debug, Default, serde::Deserialize, utoipa::ToSchema)]
pub struct PriceRefreshInput {
    /// Credential to authenticate with; defaults to the provider's oldest.
    #[serde(default)]
    pub credential_id: Option<String>,
}

/// Applies the catalog connector to the provider's offerings.
#[utoipa::path(
    post,
    path = "/api/providers/{id}/prices/refresh",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    request_body = PriceRefreshInput,
    responses(
        (status = 200, description = "refresh counts", body = connectors::PriceRefreshView),
        (status = 400, description = "no catalog connector or invalid config", body = crate::error::ErrorBody),
        (status = 401, description = "not authenticated", body = crate::error::ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = crate::error::ErrorBody),
        (status = 404, description = "no such provider", body = crate::error::ErrorBody),
        (status = 502, description = "upstream fetch failed", body = crate::error::ErrorBody)
    )
)]
pub async fn refresh_prices(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<PriceRefreshInput>>,
) -> Result<Json<connectors::PriceRefreshView>, ApiError> {
    let rid = error::request_id(&headers);
    let credential_id = body.and_then(|Json(input)| input.credential_id);
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let view = connectors::refresh_prices(
        &state.pool,
        &state.secrets,
        Some(&actor),
        &id,
        credential_id.as_deref(),
    )
    .await
    .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Refreshes one credential's balance.
#[utoipa::path(
    post,
    path = "/api/providers/{id}/credentials/{cid}/balance/refresh",
    tag = "admin",
    params(
        ("id" = String, Path, description = "provider id"),
        ("cid" = String, Path, description = "credential id")
    ),
    responses(
        (status = 200, description = "balance result", body = connectors::BalanceRefreshView),
        (status = 400, description = "no balance connector", body = crate::error::ErrorBody),
        (status = 401, description = "not authenticated", body = crate::error::ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = crate::error::ErrorBody),
        (status = 404, description = "no such provider or credential", body = crate::error::ErrorBody)
    )
)]
pub async fn refresh_balance(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path((id, cid)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<connectors::BalanceRefreshView>, ApiError> {
    let rid = error::request_id(&headers);
    let view = connectors::refresh_balance(&state.pool, &state.secrets, &id, &cid)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Refreshes every credential's balance for one provider, isolating failures.
#[utoipa::path(
    post,
    path = "/api/providers/{id}/balances/refresh",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    responses(
        (status = 200, description = "per-credential balance results", body = connectors::ProviderBalancesView),
        (status = 401, description = "not authenticated", body = crate::error::ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = crate::error::ErrorBody),
        (status = 404, description = "no such provider", body = crate::error::ErrorBody)
    )
)]
pub async fn refresh_balances(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<connectors::ProviderBalancesView>, ApiError> {
    let rid = error::request_id(&headers);
    let view = connectors::refresh_provider_balances(&state.pool, &state.secrets, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

#[cfg(test)]
mod tests {
    use crate::auth::Role;
    use crate::test_support::{self, TestApp, api_request, send, spawn_static_server};
    use axum::http::{Method, StatusCode};
    use serde_json::json;

    const SAIL_MODELS: &str = include_str!("../../tests/fixtures/sailresearch_models.json");
    const SAIL_USAGE: &str = include_str!("../../tests/fixtures/sailresearch_usage.json");

    struct Seeded {
        app: TestApp,
        router: axum::Router,
        cookie: String,
        csrf: String,
        provider_id: String,
        credential_id: String,
    }

    async fn seeded(base_url: &str) -> Seeded {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "sail", "base_url": base_url })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let provider_id = created.body["id"]
            .as_str()
            .expect("provider id")
            .to_string();
        let credential = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{provider_id}/credentials"),
                Some(json!({ "label": "primary", "secret": "sk-live-secret" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(
            credential.status,
            StatusCode::CREATED,
            "{}",
            credential.body
        );
        Seeded {
            app,
            router,
            cookie,
            csrf,
            provider_id,
            credential_id: credential.body["id"]
                .as_str()
                .expect("credential id")
                .to_string(),
        }
    }

    fn catalog_config(url: &str, apply_on_discovery: bool) -> serde_json::Value {
        json!({
            "url": url,
            "headers": { "authorization": "Bearer {{secret}}" },
            "allow_private": true,
            "catalog": {
                "entries": "$.data",
                "model_id": "$.id",
                "input": "$.pricing_usd_per_million_tokens.balanced.input",
                "output": "$.pricing_usd_per_million_tokens.balanced.output",
                "cache_read": "$.pricing_usd_per_million_tokens.balanced.cached_input",
                "unit": {
                    "source": "custom",
                    "multiplier": 0.01,
                    "currency": "USD",
                    "fx_to_usd": null,
                    "display": { "symbol": "$", "decimals": 4 },
                    "note": "quoted in cents — verify against an invoice"
                },
                "modalities_input": "$.input_modalities",
                "modalities_output": "$.output_modalities",
                "context_length": "$.context_length",
                "max_output": "$.max_output_tokens",
                "apply_on_discovery": apply_on_discovery
            }
        })
    }

    fn balance_config(url: &str) -> serde_json::Value {
        json!({
            "url": url,
            "allow_private": true,
            "balance": {
                "balance": "$.balance",
                "available": "$.available",
                "currency": "$.currency",
                "period_spend": "$.period_spend",
                "burn_rate": "$.burn_rate",
                "days_remaining": "$.days_remaining",
                "refresh_seconds": 900
            }
        })
    }

    async fn put_connector(s: &Seeded, kind: &str, config: serde_json::Value) -> serde_json::Value {
        let response = send(
            s.router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/providers/{}/connectors/{kind}", s.provider_id),
                Some(json!({ "config": config, "enabled": true })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        response.body
    }

    async fn model(app: &TestApp, name: &str, modalities: Option<&str>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO models (id, canonical_name, modalities_input, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(modalities)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&app.state.pool)
        .await
        .expect("model");
        id
    }

    async fn offering(app: &TestApp, provider_id: &str, model_id: &str, upstream: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO offerings \
               (id, provider_id, model_id, upstream_model_id, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(&id)
        .bind(provider_id)
        .bind(model_id)
        .bind(upstream)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&app.state.pool)
        .await
        .expect("offering");
        id
    }

    #[tokio::test]
    async fn connector_crud_round_trips_and_audits() {
        let s = seeded("https://api.example.com/v1").await;
        let config = catalog_config("/models", false);
        let stored = put_connector(&s, "catalog", config.clone()).await;
        assert_eq!(stored["configured"], true);
        assert_eq!(stored["enabled"], true);
        assert_eq!(stored["config"]["url"], "/models");

        let fetched = send(
            s.router.clone(),
            api_request(
                Method::GET,
                &format!("/api/providers/{}/connectors/catalog", s.provider_id),
                None,
                Some(&s.cookie),
                None,
            ),
        )
        .await;
        assert_eq!(fetched.status, StatusCode::OK);
        assert_eq!(fetched.body["config"]["catalog"]["entries"], "$.data");

        let absent = send(
            s.router.clone(),
            api_request(
                Method::GET,
                &format!("/api/providers/{}/connectors/health", s.provider_id),
                None,
                Some(&s.cookie),
                None,
            ),
        )
        .await;
        assert_eq!(absent.status, StatusCode::OK);
        assert_eq!(absent.body["configured"], false);

        let audit: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'connector.update'")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("audit");
        assert_eq!(audit, 1);

        // The provider list exposes the connector additively.
        let providers = send(
            s.router.clone(),
            api_request(Method::GET, "/api/providers", None, Some(&s.cookie), None),
        )
        .await;
        assert_eq!(providers.body[0]["connectors"][0]["kind"], "catalog");
        assert_eq!(providers.body[0]["connectors"][0]["enabled"], true);
    }

    #[tokio::test]
    async fn templates_are_plain_json_and_include_the_three_names() {
        let s = seeded("https://api.example.com/v1").await;
        let response = send(
            s.router.clone(),
            api_request(
                Method::GET,
                &format!("/api/providers/{}/connectors/templates", s.provider_id),
                None,
                Some(&s.cookie),
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        let ids: Vec<&str> = response
            .body
            .as_array()
            .expect("templates array")
            .iter()
            .map(|template| template["id"].as_str().expect("id"))
            .collect();
        assert_eq!(
            ids,
            [
                "sailresearch",
                "openrouter_per_token",
                "generic_pricing_object"
            ]
        );
        assert_eq!(
            response.body[0]["configs"]["catalog"]["catalog"]["unit"]["source"],
            "custom"
        );
        assert_eq!(
            response.body[0]["configs"]["catalog"]["catalog"]["unit"]["multiplier"],
            0.01
        );
        assert_eq!(
            response.body[0]["configs"]["catalog"]["catalog"]["unit"]["note"],
            "quoted in cents — verify against an invoice"
        );
        assert_eq!(
            response.body[0]["configs"]["balance"]["url"],
            "/v2/usage/summary?range=30d"
        );
        assert_eq!(
            response.body[0]["configs"]["balance"]["balance"]["balance"],
            "$.balance"
        );
        // OpenRouter's balance is best effort and documented as such.
        assert_eq!(
            response.body[1]["configs"]["balance"]["url"],
            "/api/v1/credits"
        );
        assert_eq!(
            response.body[1]["configs"]["balance"]["balance"]["balance"],
            "$.data.total_credits - $.data.total_usage"
        );
        assert_eq!(
            response.body[1]["configs"]["balance"]["balance"]["period_spend"],
            "$.data.total_usage"
        );
    }

    #[tokio::test]
    async fn sample_returns_truncated_raw_json_and_test_extracts_values() {
        let (addr, _) = spawn_static_server("200 OK", SAIL_MODELS).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        put_connector(&s, "catalog", catalog_config("/models", false)).await;

        let sample = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/connectors/catalog/sample", s.provider_id),
                Some(json!({})),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(sample.status, StatusCode::OK, "{}", sample.body);
        assert_eq!(sample.body["truncated"], false);
        assert!(
            sample.body["body"]
                .as_str()
                .expect("body")
                .contains("deepseek")
        );

        let tested = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/connectors/catalog/test", s.provider_id),
                Some(json!({})),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(tested.status, StatusCode::OK, "{}", tested.body);
        assert_eq!(tested.body["ok"], true);
        assert_eq!(tested.body["entries_seen"], 10);
        assert_eq!(tested.body["matched"], 10);
        assert_eq!(tested.body["skipped"], 0);
        let input = tested.body["samples"][0]["input_per_m"]
            .as_f64()
            .expect("input");
        assert!((input - 0.0035).abs() < 1e-12, "{input}");
        assert_eq!(tested.body["samples"][0]["raw"]["input"], 0.35);
        let converted = tested.body["samples"][0]["converted_per_m"]["input"]
            .as_f64()
            .expect("converted");
        assert!((converted - 0.0035).abs() < 1e-12, "{converted}");
        assert!(
            tested.body["samples"][0]["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .all(|warning| !warning.as_str().unwrap_or("").contains("check the unit")),
            "the balanced input is inside the accepted band"
        );

        // Test and sample write nothing.
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM connector_runs")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("runs");
        assert_eq!(runs, 0);
    }

    #[tokio::test]
    async fn sample_accepts_an_empty_mapping_and_test_reports_missing_pointers() {
        let (addr, _) = spawn_static_server("200 OK", SAIL_MODELS).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;

        // Sampling only needs the URL: the draft mapping has no price
        // pointers yet (the "Fetch sample" flow before picking anything).
        let draft = json!({
            "config": {
                "url": "/models",
                "allow_private": true,
                "catalog": { "entries": "$.data", "model_id": "$.id" }
            }
        });
        let sample = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/connectors/catalog/sample", s.provider_id),
                Some(draft.clone()),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(sample.status, StatusCode::OK, "{}", sample.body);
        assert!(
            sample.body["body"]
                .as_str()
                .expect("body")
                .contains("deepseek")
        );

        // Preview answers with a warning instead of a global error.
        let tested = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/connectors/catalog/test", s.provider_id),
                Some(draft),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(tested.status, StatusCode::OK, "{}", tested.body);
        assert_eq!(tested.body["ok"], false);
        assert!(
            tested.body["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning
                    .as_str()
                    .unwrap_or("")
                    .contains("set input or output")),
            "{}",
            tested.body
        );

        // A malformed URL is still refused before any fetch.
        let bad = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/connectors/catalog/sample", s.provider_id),
                Some(json!({ "config": { "url": "" } })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
        assert!(
            bad.body["error"]["message"]
                .as_str()
                .expect("message")
                .contains("url"),
            "{}",
            bad.body
        );

        // A blocked destination is an upstream error, never a fetch.
        let blocked = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/connectors/catalog/sample", s.provider_id),
                Some(json!({
                    "config": { "url": "http://169.254.169.254/latest/meta-data" }
                })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(blocked.status, StatusCode::BAD_GATEWAY, "{}", blocked.body);
        assert!(
            blocked.body["error"]["message"]
                .as_str()
                .expect("message")
                .contains("metadata"),
            "{}",
            blocked.body
        );
    }

    #[tokio::test]
    async fn discovery_fills_metadata_on_create_only_and_refresh_is_idempotent() {
        let (addr, _) = spawn_static_server("200 OK", SAIL_MODELS).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        put_connector(&s, "catalog", catalog_config("/models", false)).await;

        // An existing model with operator-set modalities: discovery must not
        // overwrite the non-NULL value.
        let existing = model(&s.app, "zai-org/GLM-5", Some("[\"audio\"]")).await;

        let discovered = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/discover-models", s.provider_id),
                Some(json!({ "dry_run": false })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(discovered.status, StatusCode::OK, "{}", discovered.body);
        assert_eq!(discovered.body["discovered"], 10);
        assert_eq!(discovered.body["created_models"], 9);
        assert!(
            discovered.body.get("prices").is_none(),
            "apply_on_discovery is false"
        );

        let (modalities, context): (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT modalities_input, context_length FROM models WHERE id = ?")
                .bind(&existing)
                .fetch_one(&s.app.state.pool)
                .await
                .expect("existing model");
        assert_eq!(
            modalities.as_deref(),
            Some("[\"audio\"]"),
            "never overwritten"
        );
        assert_eq!(context, None, "never filled on an existing row");

        let created: (Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT modalities_input, context_length FROM models \
             WHERE canonical_name = 'deepseek-ai/DeepSeek-V4.1-Flash-1017' COLLATE NOCASE",
        )
        .fetch_one(&s.app.state.pool)
        .await
        .expect("new model");
        assert_eq!(created.0.as_deref(), Some("[\"text\"]"));
        assert_eq!(created.1, Some(163_840));

        let first = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/prices/refresh", s.provider_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        assert_eq!(first.body["ok"], true);
        assert_eq!(first.body["created"], 10);
        assert_eq!(first.body["skipped_unresolved"], 0);

        let second = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/prices/refresh", s.provider_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(second.body["created"], 0);
        assert_eq!(second.body["updated"], 0);
        assert_eq!(second.body["unchanged"], 10);

        // Provenance is provider_api with the fetched URL.
        let source: (String, String) = sqlx::query_as(
            "SELECT p.source, p.source_url FROM prices p \
             JOIN offerings o ON o.id = p.offering_id \
             JOIN models m ON m.id = o.model_id \
             WHERE m.canonical_name = 'deepseek-ai/DeepSeek-V4.1-Flash-1017' COLLATE NOCASE",
        )
        .fetch_one(&s.app.state.pool)
        .await
        .expect("price provenance");
        assert_eq!(source.0, "provider_api");
        assert!(source.1.ends_with("/models"), "{}", source.1);
    }

    #[tokio::test]
    async fn manual_prices_are_never_overwritten_by_a_refresh() {
        let (addr, _) = spawn_static_server("200 OK", SAIL_MODELS).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        put_connector(&s, "catalog", catalog_config("/models", false)).await;

        let model_id = model(&s.app, "deepseek-ai/DeepSeek-V4.1-Flash-1017", None).await;
        let offering_id = offering(
            &s.app,
            &s.provider_id,
            &model_id,
            "deepseek-ai/DeepSeek-V4.1-Flash-1017",
        )
        .await;
        let manual = send(
            s.router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({ "input_per_m": 99.0, "output_per_m": 199.0 })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(manual.status, StatusCode::OK, "{}", manual.body);

        let refreshed = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/prices/refresh", s.provider_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.body);
        assert_eq!(refreshed.body["skipped_manual"], 1);
        assert_eq!(refreshed.body["created"], 0);

        let (source, input): (String, f64) =
            sqlx::query_as("SELECT source, input_per_m FROM prices WHERE offering_id = ?")
                .bind(&offering_id)
                .fetch_one(&s.app.state.pool)
                .await
                .expect("manual price");
        assert_eq!(source, "manual");
        assert_eq!(input, 99.0);
    }

    #[tokio::test]
    async fn refresh_counts_unresolved_entries_and_marks_variable_prices() {
        let body = json!({
            "data": [
                {"id": "priced", "price": {"input": 1, "output": 2}},
                {"id": "negative", "price": {"input": -1, "output": -1}},
                {"id": "missing", "price": {"output": 2}},
                {"id": "no-offering", "price": {"input": 1, "output": 2}}
            ]
        })
        .to_string();
        let (addr, _) = spawn_static_server("200 OK", &body).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        let config = json!({
            "url": "/models",
            "allow_private": true,
            "catalog": {
                "entries": "$.data",
                "model_id": "$.id",
                "input": "$.price.input",
                "output": "$.price.output",
                "units": "per_million_tokens"
            }
        });
        put_connector(&s, "catalog", config).await;

        for name in ["priced", "negative", "missing"] {
            let model_id = model(&s.app, name, None).await;
            offering(&s.app, &s.provider_id, &model_id, name).await;
        }

        let refreshed = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/prices/refresh", s.provider_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.body);
        assert_eq!(refreshed.body["entries_seen"], 4);
        assert_eq!(refreshed.body["created"], 2, "priced + variable");
        assert_eq!(
            refreshed.body["skipped_unresolved"], 2,
            "missing + no offering"
        );

        let variable: i64 = sqlx::query_scalar(
            "SELECT p.variable_price FROM prices p JOIN offerings o ON o.id = p.offering_id \
             WHERE o.upstream_model_id = 'negative'",
        )
        .fetch_one(&s.app.state.pool)
        .await
        .expect("variable price");
        assert_eq!(variable, 1);
    }

    #[tokio::test]
    async fn non_usd_without_fx_stores_the_currency_and_stays_unpriced() {
        let (addr, _) = spawn_static_server(
            "200 OK",
            r#"{"data":[{"id":"eur","price":{"input":2,"output":4}}]}"#,
        )
        .await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        let config = json!({
            "url": "/models",
            "allow_private": true,
            "catalog": {
                "entries": "$.data",
                "model_id": "$.id",
                "input": "$.price.input",
                "output": "$.price.output",
                "unit": {
                    "source": "per_million_tokens",
                    "currency": "EUR",
                    "fx_to_usd": null,
                    "display": { "symbol": "€", "decimals": 4 }
                }
            }
        });
        put_connector(&s, "catalog", config).await;
        let model_id = model(&s.app, "eur", None).await;
        let offering_id = offering(&s.app, &s.provider_id, &model_id, "eur").await;

        let refreshed = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/prices/refresh", s.provider_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.body);
        assert_eq!(refreshed.body["created"], 1);

        let (currency, variable, input): (String, i64, Option<f64>) = sqlx::query_as(
            "SELECT currency, variable_price, input_per_m FROM prices WHERE offering_id = ?",
        )
        .bind(&offering_id)
        .fetch_one(&s.app.state.pool)
        .await
        .expect("price");
        assert_eq!(currency, "EUR", "the source currency is kept for display");
        assert_eq!(variable, 1, "non-USD without FX is unpriced for routing");
        assert!(input.is_none(), "never a fake USD number");
    }

    #[tokio::test]
    async fn discovery_apply_merges_the_price_refresh() {
        let (addr, _) = spawn_static_server("200 OK", SAIL_MODELS).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        put_connector(&s, "catalog", catalog_config("/models", true)).await;

        let discovered = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/discover-models", s.provider_id),
                Some(json!({ "dry_run": false })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(discovered.status, StatusCode::OK, "{}", discovered.body);
        assert_eq!(discovered.body["prices"]["ok"], true);
        assert_eq!(discovered.body["prices"]["created"], 10);

        // A dry run never refreshes prices.
        let preview = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/discover-models", s.provider_id),
                Some(json!({ "dry_run": true })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert!(preview.body.get("prices").is_none());
    }

    #[tokio::test]
    async fn the_retired_pointer_syntax_is_refused_by_put() {
        let s = seeded("https://api.example.com/v1").await;
        let mut config = catalog_config("/models", false);
        config["catalog"]["input"] = json!("/pricing/input");
        let response = send(
            s.router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/providers/{}/connectors/catalog", s.provider_id),
                Some(json!({ "config": config })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(
            response.status,
            StatusCode::BAD_REQUEST,
            "{}",
            response.body
        );
        let message = response.body["error"]["message"].as_str().expect("message");
        assert!(message.contains("catalog.input"), "{message}");
        assert!(
            message.contains("$/") || message.contains("column") || message.contains("unexpected"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn an_arithmetic_price_field_scales_the_extracted_value() {
        let (addr, _) = spawn_static_server(
            "200 OK",
            r#"{"data":[{"id":"alpha","price":{"prompt":"0.0003","completion":"0.0006"}}]}"#,
        )
        .await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        let mut config = catalog_config("/models", false);
        config["catalog"]["input"] = json!("$.price.prompt * 1000000");
        config["catalog"]["output"] = json!("$.price.completion * 1000000");
        config["catalog"]["unit"]["source"] = json!("per_million_tokens");
        config["catalog"]["unit"]["multiplier"] = json!(1.0);
        put_connector(&s, "catalog", config).await;

        let test = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/connectors/catalog/test", s.provider_id),
                Some(json!({})),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(test.status, StatusCode::OK, "{}", test.body);
        assert_eq!(test.body["entries_seen"], 1);
        assert_eq!(test.body["samples"][0]["input_per_m"], 300.0);
        assert_eq!(test.body["samples"][0]["output_per_m"], 600.0);
        assert_eq!(test.body["samples"][0]["error"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn a_text_price_skips_the_entry_and_null_is_not_zero() {
        let (addr, _) = spawn_static_server(
            "200 OK",
            r#"{"data":[{"id":"free","price":{"prompt":null,"completion":null}},{"id":"text","price":{"prompt":"call us","completion":2}}]}"#,
        )
        .await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        let mut config = catalog_config("/models", false);
        config["catalog"]["input"] = json!("$.price.prompt");
        config["catalog"]["output"] = json!("$.price.completion");
        put_connector(&s, "catalog", config).await;

        let test = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/connectors/catalog/test", s.provider_id),
                Some(json!({})),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(test.status, StatusCode::OK, "{}", test.body);
        assert_eq!(test.body["entries_seen"], 2);
        assert_eq!(test.body["matched"], 0, "neither entry is priced");
        assert_eq!(test.body["skipped"], 2);
        let errors = test.body["errors"].as_array().expect("errors");
        assert!(
            errors
                .iter()
                .any(|error| error.as_str().is_some_and(|text| text.contains("null"))),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.as_str().is_some_and(|text| text.contains("call us"))),
            "{errors:?}"
        );
    }

    #[tokio::test]
    async fn a_negative_calculated_price_becomes_variable_not_a_number() {
        let (addr, _) = spawn_static_server(
            "200 OK",
            r#"{"data":[{"id":"auto","price":{"prompt":-5,"completion":2}}]}"#,
        )
        .await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        let mut config = catalog_config("/models", false);
        config["catalog"]["input"] = json!("$.price.prompt * 2");
        config["catalog"]["output"] = json!("$.price.completion");
        put_connector(&s, "catalog", config).await;

        let test = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/connectors/catalog/test", s.provider_id),
                Some(json!({})),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(test.status, StatusCode::OK, "{}", test.body);
        assert_eq!(test.body["matched"], 1, "a variable entry is still mapped");
        assert_eq!(test.body["samples"][0]["variable_price"], true);
        assert_eq!(
            test.body["samples"][0]["input_per_m"],
            serde_json::Value::Null
        );
    }

    #[tokio::test]
    async fn balance_refresh_stores_provider_values_and_never_fabricates_zero() {
        let (addr, _) = spawn_static_server("200 OK", SAIL_USAGE).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        put_connector(&s, "balance", balance_config("/v2/usage/summary?range=30d")).await;

        let refreshed = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!(
                    "/api/providers/{}/credentials/{}/balance/refresh",
                    s.provider_id, s.credential_id
                ),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.body);
        assert_eq!(refreshed.body["ok"], true);
        assert_eq!(refreshed.body["available"], true);
        assert_eq!(refreshed.body["balance_usd"], 12.34);
        assert_eq!(refreshed.body["days_remaining"], 29.4);

        let row: (Option<f64>, Option<i64>, Option<String>, String) = sqlx::query_as(
            "SELECT balance_usd, balance_available, balance_error, balance_source \
             FROM provider_credentials WHERE id = ?",
        )
        .bind(&s.credential_id)
        .fetch_one(&s.app.state.pool)
        .await
        .expect("credential balance");
        assert_eq!(row.0, Some(12.34));
        assert_eq!(row.1, Some(1));
        assert_eq!(row.2, None);
        assert_eq!(row.3, "provider_api");
    }

    #[tokio::test]
    async fn balance_available_false_stores_null_with_the_flag() {
        let (addr, _) = spawn_static_server(
            "200 OK",
            r#"{"balance":0,"available":false,"currency":"USD"}"#,
        )
        .await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        put_connector(&s, "balance", balance_config("/usage")).await;

        let refreshed = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!(
                    "/api/providers/{}/credentials/{}/balance/refresh",
                    s.provider_id, s.credential_id
                ),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.body);
        assert_eq!(refreshed.body["available"], false);
        assert!(refreshed.body["balance_usd"].is_null());

        let row: (Option<f64>, Option<i64>) = sqlx::query_as(
            "SELECT balance_usd, balance_available FROM provider_credentials WHERE id = ?",
        )
        .bind(&s.credential_id)
        .fetch_one(&s.app.state.pool)
        .await
        .expect("credential");
        assert!(row.0.is_none(), "unavailable is NULL, never 0");
        assert_eq!(row.1, Some(0));
    }

    #[tokio::test]
    async fn balance_failure_records_the_error_and_keeps_the_last_value() {
        let (addr, _) = spawn_static_server("200 OK", SAIL_USAGE).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        put_connector(&s, "balance", balance_config("/usage")).await;

        send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!(
                    "/api/providers/{}/credentials/{}/balance/refresh",
                    s.provider_id, s.credential_id
                ),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;

        // Point the connector at a closed port: the fetch fails.
        put_connector(&s, "balance", balance_config("http://127.0.0.1:1/usage")).await;
        let failed = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!(
                    "/api/providers/{}/credentials/{}/balance/refresh",
                    s.provider_id, s.credential_id
                ),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(failed.status, StatusCode::OK, "{}", failed.body);
        assert_eq!(failed.body["ok"], false);
        assert!(
            failed.body["error"]
                .as_str()
                .expect("error")
                .contains("connect")
        );

        let row: (Option<f64>, Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT balance_usd, balance_available, balance_error FROM provider_credentials \
             WHERE id = ?",
        )
        .bind(&s.credential_id)
        .fetch_one(&s.app.state.pool)
        .await
        .expect("credential");
        assert_eq!(row.0, Some(12.34), "last known value is kept");
        assert_eq!(row.1, Some(0), "unconfirmed");
        assert!(
            row.2
                .as_deref()
                .is_some_and(|error| error.contains("connect"))
        );
    }

    #[tokio::test]
    async fn provider_balance_refresh_visits_every_credential() {
        let (addr, _) = spawn_static_server("200 OK", SAIL_USAGE).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        put_connector(&s, "balance", balance_config("/usage")).await;
        let second = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/credentials", s.provider_id),
                Some(json!({ "label": "secondary", "secret": "sk-second" })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(second.status, StatusCode::CREATED);

        let refreshed = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/balances/refresh", s.provider_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.body);
        assert_eq!(refreshed.body["refreshed"], 2);
        assert_eq!(refreshed.body["failed"], 0);
        assert_eq!(
            refreshed.body["results"].as_array().expect("results").len(),
            2
        );

        let stored: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM provider_credentials \
             WHERE provider_id = ? AND balance_source = 'provider_api'",
        )
        .bind(&s.provider_id)
        .fetch_one(&s.app.state.pool)
        .await
        .expect("stored balances");
        assert_eq!(stored, 2);
    }

    #[tokio::test]
    async fn health_connector_drives_the_credential_test_action() {
        let (addr, seen) = spawn_static_server("200 OK", "{}").await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        put_connector(
            &s,
            "health",
            json!({ "url": "/healthz", "allow_private": true }),
        )
        .await;

        let tested = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/credentials/{}/test", s.credential_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(tested.status, StatusCode::OK, "{}", tested.body);
        assert_eq!(tested.body["ok"], true);
        let requests = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            requests[0].starts_with("GET /healthz HTTP/1.1"),
            "the health connector replaces /models: {}",
            requests[0]
        );
    }

    /// A `provider_api` price row must bill exactly like a `manual` row with
    /// the same components: the connector path (source, currency, provenance)
    /// never enters `money::compute_cost`. This pins the settlement path
    /// against the D41/D44 price-table and currency changes.
    #[tokio::test]
    async fn provider_api_prices_charge_exactly_like_manual_prices() {
        let (addr, _) = spawn_static_server(
            "200 OK",
            r#"{"data":[{"id":"billable","pricing_usd_per_million_tokens":
                {"balanced":{"input":150,"output":250,"cached_input":50}}}]}"#,
        )
        .await;
        let s = seeded(&format!("http://{addr}/v1")).await;
        let model_id = model(&s.app, "billable", None).await;
        let offering_id = offering(&s.app, &s.provider_id, &model_id, "billable").await;
        put_connector(&s, "catalog", catalog_config("/models", false)).await;

        let refreshed = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/prices/refresh", s.provider_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.body);
        assert_eq!(refreshed.body["created"], 1, "{}", refreshed.body);

        let api_price = crate::catalog::get_price(&s.app.state.pool, &offering_id)
            .await
            .expect("read price")
            .expect("provider_api price row");
        assert_eq!(api_price.source, "provider_api");

        let usage = crate::upstream::sse::Usage {
            input_tokens: Some(2_000_000),
            output_tokens: Some(1_000_000),
            cache_read_tokens: Some(500_000),
            ..Default::default()
        };
        let api_cost = crate::money::compute_cost(Some(&api_price), Some(&usage));
        assert!(api_cost.priced, "a provider_api price is a real price");
        assert!(api_cost.cost_usd.is_some_and(|cost| cost > 0.0));

        // Same numbers through the manual price path (source = 'manual').
        let manual = send(
            s.router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({
                    "input_per_m": api_price.input_per_m,
                    "output_per_m": api_price.output_per_m,
                    "cache_read_per_m": api_price.cache_read_per_m,
                    "cache_write_per_m": api_price.cache_write_per_m,
                    "reasoning_per_m": api_price.reasoning_per_m,
                    "request_fee": api_price.request_fee,
                })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(manual.status, StatusCode::OK, "{}", manual.body);
        let manual_price = crate::catalog::get_price(&s.app.state.pool, &offering_id)
            .await
            .expect("read price")
            .expect("manual price row");
        assert_eq!(manual_price.source, "manual");
        let manual_cost = crate::money::compute_cost(Some(&manual_price), Some(&usage));

        assert_eq!(
            api_cost.cost_usd, manual_cost.cost_usd,
            "source and provenance never change what a request bills"
        );
        assert_eq!(api_cost.priced, manual_cost.priced);
    }

    #[tokio::test]
    async fn balance_refresh_does_not_touch_prices_or_billing_rows() {
        let (addr, _) = spawn_static_server("200 OK", SAIL_USAGE).await;
        let base = format!("http://{addr}/v1");
        let s = seeded(&base).await;
        let model_id = model(&s.app, "billable", None).await;
        let offering_id = offering(&s.app, &s.provider_id, &model_id, "billable").await;
        send(
            s.router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({ "input_per_m": 1.0, "output_per_m": 2.0 })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        put_connector(&s, "balance", balance_config("/usage")).await;

        let prices_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prices")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("prices before");
        let usage_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("usage before");

        send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/balances/refresh", s.provider_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;

        let prices_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prices")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("prices after");
        let usage_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("usage after");
        assert_eq!(prices_before, prices_after, "balance refresh never prices");
        assert_eq!(usage_before, usage_after, "billing rows are untouched");
    }

    #[tokio::test]
    async fn invalid_connector_config_and_unknown_kind_are_refused() {
        let s = seeded("https://api.example.com/v1").await;
        let bad = send(
            s.router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/providers/{}/connectors/catalog", s.provider_id),
                Some(json!({
                    "config": { "url": "/models", "catalog": { "entries": "data", "model_id": "$.id", "input": "$.price" } }
                })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
        assert!(
            bad.body["error"]["message"]
                .as_str()
                .expect("message")
                .contains("catalog.entries")
        );

        let unknown = send(
            s.router.clone(),
            api_request(
                Method::GET,
                &format!("/api/providers/{}/connectors/nope", s.provider_id),
                None,
                Some(&s.cookie),
                None,
            ),
        )
        .await;
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn refresh_without_a_connector_names_the_problem() {
        let s = seeded("https://api.example.com/v1").await;
        let response = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/prices/refresh", s.provider_id),
                None,
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(
            response.body["error"]["message"]
                .as_str()
                .expect("message")
                .contains("catalog connector")
        );
    }

    #[tokio::test]
    async fn credential_views_expose_the_balance_policy_fields() {
        let s = seeded("https://api.example.com/v1").await;
        let patched = send(
            s.router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/credentials/{}", s.credential_id),
                Some(json!({ "balance_gate": "block" })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
        assert_eq!(patched.body["balance_gate"], "block");
        assert!(patched.body.get("balance_available").is_some());
        assert!(patched.body.get("balance_error").is_some());
    }
}
