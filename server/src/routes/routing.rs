//! Admin plane for M5 routing: combo CRUD, benchmark import, and model
//! quality overrides (spec M5 scope items 2, 4, 5).
//!
//! The handlers are thin — validate, call [`crate::combos`] /
//! [`crate::benchmarks`], map the result. Every mutation is audited by the
//! service in the same transaction as the change (`combo.create`,
//! `combo.update`, `combo.delete`, `sync.benchmarks`, `quality.override`,
//! `quality.override_clear`). The data plane stays untouched: a combo is
//! addressed as the model id `combo:<name>` in `/v1/chat/completions`, and
//! the "why this model" explanation rides in `decision_json` (surfaced by
//! `GET /api/logs`).

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{ClientIp, RequireAdmin};
use crate::benchmarks;
use crate::catalog::Actor;
use crate::combos::{self, ComboInput, ComboView};
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
    use axum::http::Method;
    use serde_json::json;

    use super::*;
    use crate::auth::Role;
    use crate::catalog::{self, ModelInput, ProviderInput};
    use crate::test_support::{TestApp, api_request, login, send};

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
}
