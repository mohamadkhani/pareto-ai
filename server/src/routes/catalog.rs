//! Admin CRUD for models and offerings, the manual price override, and the
//! OpenAI-compatible `GET /v1/models` listing.
//!
//! Every mutation is admin-only (`RequireAdmin`), CSRF-protected by the
//! `/api` guard, and audited inside the mutation's transaction (`model.*`,
//! `offering.*`, `price.override`). `/v1/models` needs an admin session until
//! M4 introduces API keys.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;

use crate::AppState;
use crate::auth::{ClientIp, RequireAdmin};
use crate::catalog::{
    self, Actor, ModelInput, ModelPatch, ModelView, OfferingInput, OfferingPatch, OfferingView,
    PriceInput, PriceView, V1ModelList,
};
use crate::error::{self, ApiError, ErrorBody};
use crate::routes::bad_body;

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// Lists all models. Admin only.
#[utoipa::path(
    get,
    path = "/api/models",
    tag = "catalog",
    responses(
        (status = 200, description = "all models", body = [ModelView]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list_models(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
) -> Result<Json<Vec<ModelView>>, ApiError> {
    let rid = error::request_id(&headers);
    let rows = catalog::list_models(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(rows.iter().map(ModelView::from).collect()))
}

/// Creates a model. Admin only, audited as `model.create`.
#[utoipa::path(
    post,
    path = "/api/models",
    tag = "catalog",
    request_body = ModelInput,
    responses(
        (status = 201, description = "model created", body = ModelView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 409, description = "canonical_name already exists", body = ErrorBody)
    )
)]
pub async fn create_model(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<ModelInput>, JsonRejection>,
) -> Result<(StatusCode, Json<ModelView>), ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "model create", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let row = catalog::create_model(&state.pool, &actor, &input)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok((StatusCode::CREATED, Json(ModelView::from(&row))))
}

/// Fetches one model. Admin only.
#[utoipa::path(
    get,
    path = "/api/models/{id}",
    tag = "catalog",
    params(("id" = String, Path, description = "model id")),
    responses(
        (status = 200, description = "the model", body = ModelView),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody),
        (status = 404, description = "no such model", body = ErrorBody)
    )
)]
pub async fn get_model(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ModelView>, ApiError> {
    let rid = error::request_id(&headers);
    let row = catalog::get_model(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .ok_or_else(|| ApiError::not_found("model").with_request_id(rid))?;
    Ok(Json(ModelView::from(&row)))
}

/// Updates a model. Admin only, audited as `model.update`.
#[utoipa::path(
    patch,
    path = "/api/models/{id}",
    tag = "catalog",
    params(("id" = String, Path, description = "model id")),
    request_body = ModelPatch,
    responses(
        (status = 200, description = "model updated", body = ModelView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such model", body = ErrorBody),
        (status = 409, description = "canonical_name already exists", body = ErrorBody)
    )
)]
pub async fn update_model(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ModelPatch>, JsonRejection>,
) -> Result<Json<ModelView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(patch) = bad_body(body, "model update", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let row = catalog::update_model(&state.pool, &actor, &id, &patch)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(ModelView::from(&row)))
}

/// Deletes a model and cascades its offerings and prices. Admin only,
/// audited as `model.delete`.
#[utoipa::path(
    delete,
    path = "/api/models/{id}",
    tag = "catalog",
    params(("id" = String, Path, description = "model id")),
    responses(
        (status = 204, description = "model deleted (cascade)"),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such model", body = ErrorBody)
    )
)]
pub async fn delete_model(
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
    catalog::delete_model(&state.pool, &actor, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Offerings
// ---------------------------------------------------------------------------

/// Optional filters for `GET /api/offerings`.
#[derive(Debug, Deserialize)]
pub struct OfferingQuery {
    /// Only offerings of this provider.
    pub provider_id: Option<String>,
    /// Only offerings of this model.
    pub model_id: Option<String>,
}

/// Lists offerings, optionally filtered by provider and/or model. Admin only.
#[utoipa::path(
    get,
    path = "/api/offerings",
    tag = "catalog",
    params(
        ("provider_id" = Option<String>, Query, description = "filter by provider id"),
        ("model_id" = Option<String>, Query, description = "filter by model id")
    ),
    responses(
        (status = 200, description = "offerings", body = [OfferingView]),
        (status = 400, description = "invalid query parameters", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list_offerings(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
    query: Result<Query<OfferingQuery>, QueryRejection>,
) -> Result<Json<Vec<OfferingView>>, ApiError> {
    let rid = error::request_id(&headers);
    let Query(query) = query.map_err(|e| {
        tracing::debug!(error = %e, "query string rejected");
        ApiError::bad_request("invalid query parameters").with_request_id(rid.clone())
    })?;
    let rows = catalog::list_offerings(
        &state.pool,
        query.provider_id.as_deref(),
        query.model_id.as_deref(),
    )
    .await
    .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(rows.iter().map(OfferingView::from).collect()))
}

/// Creates an offering. Admin only, audited as `offering.create`.
#[utoipa::path(
    post,
    path = "/api/offerings",
    tag = "catalog",
    request_body = OfferingInput,
    responses(
        (status = 201, description = "offering created", body = OfferingView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such provider or model", body = ErrorBody),
        (status = 409, description = "upstream_model_id already offered", body = ErrorBody)
    )
)]
pub async fn create_offering(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<OfferingInput>, JsonRejection>,
) -> Result<(StatusCode, Json<OfferingView>), ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "offering create", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let row = catalog::create_offering(&state.pool, &state.secrets, &actor, &input)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok((StatusCode::CREATED, Json(OfferingView::from(&row))))
}

/// Updates an offering. Admin only, audited as `offering.update`.
#[utoipa::path(
    patch,
    path = "/api/offerings/{id}",
    tag = "catalog",
    params(("id" = String, Path, description = "offering id")),
    request_body = OfferingPatch,
    responses(
        (status = 200, description = "offering updated", body = OfferingView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such offering", body = ErrorBody),
        (status = 409, description = "upstream_model_id already offered", body = ErrorBody)
    )
)]
pub async fn update_offering(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<OfferingPatch>, JsonRejection>,
) -> Result<Json<OfferingView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(patch) = bad_body(body, "offering update", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let row = catalog::update_offering(&state.pool, &state.secrets, &actor, &id, &patch)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(OfferingView::from(&row)))
}

/// Deletes an offering and its price row. Admin only, audited as
/// `offering.delete`.
#[utoipa::path(
    delete,
    path = "/api/offerings/{id}",
    tag = "catalog",
    params(("id" = String, Path, description = "offering id")),
    responses(
        (status = 204, description = "offering deleted"),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such offering", body = ErrorBody)
    )
)]
pub async fn delete_offering(
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
    catalog::delete_offering(&state.pool, &actor, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Prices
// ---------------------------------------------------------------------------

/// Fetches the price row of an offering, carrying `source` and `fetched_at`
/// provenance. Admin only. `404` until the offering is priced.
#[utoipa::path(
    get,
    path = "/api/offerings/{id}/price",
    tag = "catalog",
    params(("id" = String, Path, description = "offering id")),
    responses(
        (status = 200, description = "the offering's price", body = PriceView),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody),
        (status = 404, description = "no such offering or no price yet", body = ErrorBody)
    )
)]
pub async fn get_price(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<PriceView>, ApiError> {
    let rid = error::request_id(&headers);
    let row = catalog::get_price(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .ok_or_else(|| ApiError::not_found("price").with_request_id(rid))?;
    Ok(Json(PriceView::from(&row)))
}

/// Sets the manual price of an offering (PUT replaces the row; missing
/// components stay NULL — unknown is never 0). Admin only, audited as
/// `price.override`. The row becomes `source='manual'` and no sync will ever
/// overwrite it.
#[utoipa::path(
    put,
    path = "/api/offerings/{id}/price",
    tag = "catalog",
    params(("id" = String, Path, description = "offering id")),
    request_body = PriceInput,
    responses(
        (status = 200, description = "manual price set", body = PriceView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such offering", body = ErrorBody)
    )
)]
pub async fn override_price(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<PriceInput>, JsonRejection>,
) -> Result<Json<PriceView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "price override", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let row = catalog::set_manual_price(&state.pool, &actor, &id, &input)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(PriceView::from(&row)))
}

// ---------------------------------------------------------------------------
// GET /v1/models
// ---------------------------------------------------------------------------

/// Lists the enabled offerings in OpenAI-compatible shape: one entry per
/// enabled offering of an enabled provider, with effective limits (offering
/// overrides win) and prices in USD per 1M tokens. Unknown price components
/// are null — never 0. Requires an admin session until M4 API keys exist.
#[utoipa::path(
    get,
    path = "/v1/models",
    tag = "catalog",
    responses(
        (status = 200, description = "enabled offerings", body = V1ModelList),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn v1_models(
    State(state): State<AppState>,
    crate::auth::DataPlaneAuth(caller): crate::auth::DataPlaneAuth,
    headers: HeaderMap,
) -> Result<Json<V1ModelList>, ApiError> {
    let rid = error::request_id(&headers);
    let entries = catalog::list_v1_models(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    // Access control (decision D9): the listing shows only the models the
    // caller may use through at least one enabled offering. Credentials are
    // not evaluated here — the chat pipeline checks them per request.
    let api_key_id = caller.api_key.as_ref().map(|key| key.key_id.as_str());
    let grants = crate::access::Grants::load(&state.pool, &caller.user_id, api_key_id)
        .await
        .map_err(|e| ApiError::internal("loading grants", e).with_request_id(rid.clone()))?;
    let data = entries
        .into_iter()
        .filter(|entry| {
            grants.allows(
                caller.role,
                &caller.user_id,
                api_key_id,
                &entry.model_id,
                &entry.offering_id,
                None,
            )
        })
        .map(|entry| entry.model)
        .collect();
    Ok(Json(V1ModelList { data }))
}

#[cfg(test)]
mod tests {
    use crate::auth::Role;
    use crate::test_support::{self, TestApp, api_request, send};
    use axum::http::{Method, StatusCode};
    use serde_json::json;

    /// Arranges provider + model over HTTP and returns their ids.
    async fn provider_and_model(
        router: &axum::Router,
        cookie: &str,
        csrf: &str,
    ) -> (String, String) {
        let provider = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "acme", "base_url": "https://api.acme.example/v1" })),
                Some(cookie),
                Some(csrf),
            ),
        )
        .await;
        assert_eq!(provider.status, StatusCode::CREATED, "{}", provider.body);
        let provider_id = provider.body["id"]
            .as_str()
            .expect("provider id")
            .to_string();

        let model = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/models",
                Some(json!({
                    "canonical_name": "acme/brain-1",
                    "family": "brain",
                    "modalities_input": ["text", "image"],
                    "modalities_output": ["text"],
                    "context_length": 200000,
                    "max_output_tokens": 8192,
                    "capabilities": { "supported_parameters": ["tools"] },
                })),
                Some(cookie),
                Some(csrf),
            ),
        )
        .await;
        assert_eq!(model.status, StatusCode::CREATED, "{}", model.body);
        let model_id = model.body["id"].as_str().expect("model id").to_string();
        (provider_id, model_id)
    }

    /// Creates an offering over HTTP and returns its id.
    async fn create_offering(
        router: &axum::Router,
        cookie: &str,
        csrf: &str,
        provider_id: &str,
        model_id: &str,
        upstream: &str,
    ) -> String {
        let response = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/offerings",
                Some(json!({
                    "provider_id": provider_id,
                    "model_id": model_id,
                    "upstream_model_id": upstream,
                })),
                Some(cookie),
                Some(csrf),
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
        response.body["id"]
            .as_str()
            .expect("offering id")
            .to_string()
    }

    #[tokio::test]
    async fn model_crud_round_trip_writes_one_audit_row_per_mutation() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;

        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/models",
                Some(json!({
                    "canonical_name": "openai/gpt-4o",
                    "family": "gpt",
                    "modalities_input": ["text", "image"],
                    "modalities_output": ["text"],
                    "context_length": 128000,
                    "max_output_tokens": 16384,
                    "capabilities": { "supported_parameters": ["tools"] },
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let id = created.body["id"].as_str().expect("model id").to_string();
        assert_eq!(created.body["modalities_input"], json!(["text", "image"]));
        assert_eq!(created.body["context_length"], 128000);

        let fetched = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/models/{id}"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(fetched.status, StatusCode::OK);
        assert_eq!(fetched.body["canonical_name"], "openai/gpt-4o");
        assert_eq!(
            fetched.body["capabilities"]["supported_parameters"],
            json!(["tools"])
        );

        let updated = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/models/{id}"),
                Some(json!({ "context_length": 64000, "family": "gpt-4" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
        assert_eq!(updated.body["context_length"], 64000);
        assert_eq!(updated.body["family"], "gpt-4");
        assert_eq!(
            updated.body["max_output_tokens"], 16384,
            "unpatched fields keep their values"
        );

        // Duplicate canonical names conflict.
        let duplicate = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/models",
                Some(json!({ "canonical_name": "openai/gpt-4o" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(duplicate.status, StatusCode::CONFLICT);

        let deleted = send(
            router.clone(),
            api_request(
                Method::DELETE,
                &format!("/api/models/{id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);

        for action in ["model.create", "model.update", "model.delete"] {
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = ?")
                .bind(action)
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
            assert_eq!(rows, 1, "one audit row for {action}");
        }
    }

    #[tokio::test]
    async fn offering_crud_round_trip_and_conflicts() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (provider_id, model_id) = provider_and_model(&router, &cookie, &csrf).await;

        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/offerings",
                Some(json!({
                    "provider_id": provider_id,
                    "model_id": model_id,
                    "upstream_model_id": "brain-1-latest",
                    "context_override": 100000,
                    "headers": { "x-route": "fast" },
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let id = created.body["id"]
            .as_str()
            .expect("offering id")
            .to_string();
        assert_eq!(created.body["context_override"], 100000);
        assert_eq!(created.body["header_names"], json!(["x-route"]));

        // Header values are sealed at rest (D17): the name stays plain, the
        // value is AEAD ciphertext.
        let headers_json: String =
            sqlx::query_scalar("SELECT headers_json FROM offerings WHERE id = ?")
                .bind(&id)
                .fetch_one(&app.state.pool)
                .await
                .expect("headers_json");
        assert!(
            !headers_json.contains("fast"),
            "the header value is plaintext: {headers_json}"
        );
        assert!(headers_json.contains("x-route"), "names stay plain");

        let updated = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/offerings/{id}"),
                Some(json!({ "enabled": false, "max_output_override": 4096 })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
        assert_eq!(updated.body["enabled"], false);
        assert_eq!(updated.body["max_output_override"], 4096);

        // Filtering by provider works and finds the offering.
        let filtered = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/offerings?provider_id={provider_id}"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(filtered.body.as_array().expect("list").len(), 1);
        let empty = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/offerings?provider_id=other",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(empty.body.as_array().expect("list").len(), 0);

        // The same upstream id cannot be offered twice by one provider.
        let duplicate = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/offerings",
                Some(json!({
                    "provider_id": provider_id,
                    "model_id": model_id,
                    "upstream_model_id": "brain-1-latest",
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(duplicate.status, StatusCode::CONFLICT);

        // Unknown provider or model is a 404 naming the missing entity.
        let unknown_provider = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/offerings",
                Some(json!({
                    "provider_id": "nope",
                    "model_id": model_id,
                    "upstream_model_id": "x",
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(unknown_provider.status, StatusCode::NOT_FOUND);
        assert_eq!(
            unknown_provider.body["error"]["message"],
            "provider not found"
        );

        let deleted = send(
            router.clone(),
            api_request(
                Method::DELETE,
                &format!("/api/offerings/{id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);

        for action in ["offering.create", "offering.update", "offering.delete"] {
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = ?")
                .bind(action)
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
            assert_eq!(rows, 1, "one audit row for {action}");
        }
    }

    #[tokio::test]
    async fn deleting_a_model_cascades_its_offerings_and_prices() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (provider_id, model_id) = provider_and_model(&router, &cookie, &csrf).await;
        let offering_id =
            create_offering(&router, &cookie, &csrf, &provider_id, &model_id, "brain-1").await;

        let priced = send(
            router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({ "input_per_m": 1.0, "output_per_m": 2.0 })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(priced.status, StatusCode::OK, "{}", priced.body);

        let deleted = send(
            router.clone(),
            api_request(
                Method::DELETE,
                &format!("/api/models/{model_id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);

        for table in ["offerings", "prices"] {
            let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&app.state.pool)
                .await
                .expect("count");
            assert_eq!(rows, 0, "{table} cascade with the model");
        }
        let detail: String =
            sqlx::query_scalar("SELECT detail_json FROM audit_log WHERE action = 'model.delete'")
                .fetch_one(&app.state.pool)
                .await
                .expect("delete detail");
        assert!(detail.contains("\"cascaded_offerings\":1"), "{detail}");
        assert!(detail.contains("\"cascaded_prices\":1"), "{detail}");
    }

    #[tokio::test]
    async fn price_override_replaces_the_row_and_marks_it_manual() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (provider_id, model_id) = provider_and_model(&router, &cookie, &csrf).await;
        let offering_id =
            create_offering(&router, &cookie, &csrf, &provider_id, &model_id, "brain-1").await;

        let first = send(
            router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({ "input_per_m": 3.5, "output_per_m": 7.0 })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        assert_eq!(first.body["input_per_m"], 3.5);
        assert_eq!(first.body["source"], "manual");
        assert_eq!(first.body["currency"], "USD");
        assert_eq!(
            first.body["cache_read_per_m"],
            json!(null),
            "unknown components stay null, never 0"
        );
        let admin_id: String =
            sqlx::query_scalar("SELECT id FROM users WHERE email = 'admin@example.com'")
                .fetch_one(&app.state.pool)
                .await
                .expect("admin id");
        assert_eq!(first.body["updated_by"], json!(admin_id));

        // The read endpoint exposes the same row with its provenance.
        let fetched = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/offerings/{offering_id}/price"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(fetched.status, StatusCode::OK, "{}", fetched.body);
        assert_eq!(fetched.body["source"], "manual");
        assert_eq!(fetched.body["input_per_m"], 3.5);
        assert!(
            fetched.body.get("fetched_at").is_some(),
            "provenance fields exist"
        );

        // PUT replaces: components missing this time become null.
        let second = send(
            router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({ "output_per_m": 9.0, "request_fee": 0.001 })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(second.status, StatusCode::OK);
        assert_eq!(second.body["input_per_m"], json!(null));
        assert_eq!(second.body["output_per_m"], 9.0);
        assert_eq!(second.body["request_fee"], 0.001);

        // Exactly one price row per offering.
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prices")
            .fetch_one(&app.state.pool)
            .await
            .expect("price count");
        assert_eq!(rows, 1);

        // The override is audited once per PUT.
        let audits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'price.override'")
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
        assert_eq!(audits, 2);
    }

    /// A request with a raw JSON body string (for numbers `serde_json::Value`
    /// cannot represent, like `1e999` or a `NaN` literal).
    fn raw_request(
        method: Method,
        uri: &str,
        raw: &str,
        cookie: &str,
        csrf: &str,
    ) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header(axum::http::header::COOKIE, cookie)
            .header(crate::auth::CSRF_HEADER, csrf)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(raw.to_string()))
            .expect("request")
    }

    #[tokio::test]
    async fn price_override_rejects_negative_and_non_finite_amounts() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (provider_id, model_id) = provider_and_model(&router, &cookie, &csrf).await;
        let offering_id =
            create_offering(&router, &cookie, &csrf, &provider_id, &model_id, "brain-1").await;
        let uri = format!("/api/offerings/{offering_id}/price");

        // A negative price is a clean 400 naming the field.
        let negative = send(
            router.clone(),
            api_request(
                Method::PUT,
                &uri,
                Some(json!({ "input_per_m": -1.0 })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(
            negative.status,
            StatusCode::BAD_REQUEST,
            "{}",
            negative.body
        );
        assert!(
            negative.body["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("must not be negative")),
            "{}",
            negative.body
        );

        // Non-finite numbers never reach the price row: `1e999` overflows to
        // infinity and a `NaN` literal is not valid JSON — both are 400.
        for raw in [r#"{"input_per_m": 1e999}"#, r#"{"input_per_m": NaN}"#] {
            let response = send(
                router.clone(),
                raw_request(Method::PUT, &uri, raw, &cookie, &csrf),
            )
            .await;
            assert_eq!(
                response.status,
                StatusCode::BAD_REQUEST,
                "{raw}: {}",
                response.body
            );
        }

        // None of the rejected inputs wrote a price row.
        let prices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prices")
            .fetch_one(&app.state.pool)
            .await
            .expect("price count");
        assert_eq!(prices, 0);
    }

    #[tokio::test]
    async fn offering_patch_rejects_provider_id_and_model_id_instead_of_dropping_them() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (provider_id, model_id) = provider_and_model(&router, &cookie, &csrf).await;
        let offering_id =
            create_offering(&router, &cookie, &csrf, &provider_id, &model_id, "brain-1").await;

        // Alignment finding B3: the identity fields are not patchable and are
        // never silently dropped — each is an explicit 400 naming the field.
        for (field, value) in [("provider_id", "other"), ("model_id", "other")] {
            let response = send(
                router.clone(),
                api_request(
                    Method::PATCH,
                    &format!("/api/offerings/{offering_id}"),
                    Some(json!({ field: value })),
                    Some(&cookie),
                    Some(&csrf),
                ),
            )
            .await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST, "{field}");
            assert!(
                response.body["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains(field)),
                "{field}: {}",
                response.body
            );
        }

        // A patch bundling an identity field with real changes applies
        // nothing: the whole request is rejected.
        let bundled = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/offerings/{offering_id}"),
                Some(json!({ "model_id": "other", "enabled": false })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(bundled.status, StatusCode::BAD_REQUEST);
        let view = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/offerings?provider_id={provider_id}"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        let offering = &view.body.as_array().expect("offerings")[0];
        assert_eq!(offering["enabled"], true, "nothing was applied");
        assert_eq!(offering["model_id"], model_id);
    }

    #[tokio::test]
    async fn v1_models_lists_only_enabled_offerings_with_effective_limits_and_prices() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (provider_id, model_id) = provider_and_model(&router, &cookie, &csrf).await;

        // One enabled offering with overrides and a manual price, one
        // disabled offering that must not appear.
        let enabled = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/offerings",
                Some(json!({
                    "provider_id": provider_id,
                    "model_id": model_id,
                    "upstream_model_id": "brain-1-latest",
                    "context_override": 100000,
                    "max_output_override": 2048,
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        let offering_id = enabled.body["id"].as_str().expect("id").to_string();
        let disabled = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/offerings",
                Some(json!({
                    "provider_id": provider_id,
                    "model_id": model_id,
                    "upstream_model_id": "brain-1-old",
                    "enabled": false,
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(disabled.status, StatusCode::CREATED);
        send(
            router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({
                    "input_per_m": 1.25,
                    "output_per_m": 2.5,
                    "cache_read_per_m": 0.5,
                    "request_fee": 0.0002,
                    "currency": "USD",
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;

        let listing = send(
            router.clone(),
            api_request(Method::GET, "/v1/models", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(listing.status, StatusCode::OK, "{}", listing.body);
        let data = listing.body["data"].as_array().expect("data");
        assert_eq!(data.len(), 1, "disabled offerings are not advertised");
        let entry = &data[0];

        assert_eq!(entry["id"], "brain-1-latest");
        assert_eq!(
            entry["name"], "acme/brain-1",
            "display name falls back to the canonical name"
        );
        assert_eq!(entry["context_length"], 100000, "the override wins");
        assert_eq!(entry["max_output_tokens"], 2048, "the override wins");
        assert_eq!(
            entry["modalities"],
            json!({ "input": ["text", "image"], "output": ["text"] })
        );
        assert_eq!(
            entry["price"],
            json!({
                "input_per_m": 1.25,
                "output_per_m": 2.5,
                "cache_read_per_m": 0.5,
                "cache_write_per_m": null,
                "reasoning_per_m": null,
                "request_fee": 0.0002,
                "currency": "USD",
            })
        );

        // The shape is exactly the spec's: id, name, limits, modalities, price.
        let mut keys: Vec<&str> = entry
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "context_length",
                "id",
                "max_output_tokens",
                "modalities",
                "name",
                "price"
            ]
        );

        // Disabling the provider removes the offering too.
        send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/providers/{provider_id}"),
                Some(json!({ "enabled": false })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        let empty = send(
            router.clone(),
            api_request(Method::GET, "/v1/models", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(empty.body["data"].as_array().expect("data").len(), 0);
    }

    #[tokio::test]
    async fn v1_models_gives_null_prices_for_unpriced_offerings_and_needs_an_admin() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        app.create_user("user@example.com", Role::User, true).await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (provider_id, model_id) = provider_and_model(&router, &cookie, &csrf).await;
        create_offering(&router, &cookie, &csrf, &provider_id, &model_id, "brain-1").await;

        let anonymous = send(
            router.clone(),
            api_request(Method::GET, "/v1/models", None, None, None),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

        let (user_cookie, _) = test_support::login(&router, "user@example.com").await;
        let non_admin = send(
            router.clone(),
            api_request(Method::GET, "/v1/models", None, Some(&user_cookie), None),
        )
        .await;
        assert_eq!(non_admin.status, StatusCode::FORBIDDEN);

        let listing = send(
            router.clone(),
            api_request(Method::GET, "/v1/models", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(listing.status, StatusCode::OK);
        let entry = &listing.body["data"].as_array().expect("data")[0];
        assert_eq!(
            entry["price"],
            json!(null),
            "an offering without a price row has no price object"
        );

        // And the price read endpoint says 404 until the offering is priced.
        let offering_id: String = sqlx::query_scalar("SELECT id FROM offerings LIMIT 1")
            .fetch_one(&app.state.pool)
            .await
            .expect("offering id");
        let unpriced = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/offerings/{offering_id}/price"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(unpriced.status, StatusCode::NOT_FOUND);
        assert_eq!(unpriced.body["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn catalog_mutations_are_csrf_protected() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, _csrf) = test_support::login(&router, "admin@example.com").await;

        for (method, uri) in [
            (Method::POST, "/api/models"),
            (Method::POST, "/api/offerings"),
        ] {
            let response = send(
                router.clone(),
                api_request(method.clone(), uri, Some(json!({})), Some(&cookie), None),
            )
            .await;
            assert_eq!(response.status, StatusCode::FORBIDDEN, "{uri}");
            assert_eq!(response.body["error"]["code"], "csrf_failed", "{uri}");
        }
    }
}
