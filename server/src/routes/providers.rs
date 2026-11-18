//! Admin CRUD for providers and their credentials, plus the credential health
//! probe.
//!
//! Every mutation is admin-only (`RequireAdmin`), CSRF-protected by the
//! `/api` guard, and audited inside the mutation's transaction
//! (`provider.*`, `credential.*` action names). The secret is write-only:
//! requests supply it once, responses carry `has_secret` and a fixed mask,
//! and header values come back as names only — see [`crate::catalog::types`].

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};

use crate::AppState;
use crate::auth::{ClientIp, RequireAdmin};
use crate::catalog::{
    self, Actor, CredentialInput, CredentialPatch, CredentialTestView, CredentialView,
    DiscoveryInput, DiscoverySummary, ProviderInput, ProviderPatch, ProviderView,
};
use crate::error::{self, ApiError, ErrorBody};
use crate::probe;
use crate::routes::bad_body;

/// Lists all providers. Admin only.
#[utoipa::path(
    get,
    path = "/api/providers",
    tag = "admin",
    responses(
        (status = 200, description = "all providers", body = [ProviderView]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn list(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    headers: HeaderMap,
) -> Result<Json<Vec<ProviderView>>, ApiError> {
    let rid = error::request_id(&headers);
    let rows = catalog::list_providers(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    let mut connectors = crate::connectors::summaries_by_provider(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(
        rows.iter()
            .map(|row| {
                ProviderView::from(row)
                    .with_connectors(connectors.remove(&row.id).unwrap_or_default())
            })
            .collect(),
    ))
}

/// Creates a provider. Admin only, audited as `provider.create`.
#[utoipa::path(
    post,
    path = "/api/providers",
    tag = "admin",
    request_body = ProviderInput,
    responses(
        (status = 201, description = "provider created", body = ProviderView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 409, description = "name already exists", body = ErrorBody)
    )
)]
pub async fn create(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    headers: HeaderMap,
    body: Result<Json<ProviderInput>, JsonRejection>,
) -> Result<(StatusCode, Json<ProviderView>), ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "provider create", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let row = catalog::create_provider(&state.pool, &actor, &input)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?;
    let connectors = crate::connectors::summaries_by_provider(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid))?
        .remove(&row.id)
        .unwrap_or_default();
    Ok((
        StatusCode::CREATED,
        Json(ProviderView::from(&row).with_connectors(connectors)),
    ))
}

/// The provider-preset registry: id, label, description, and default base URL.
#[utoipa::path(
    get,
    path = "/api/provider-presets",
    tag = "admin",
    responses(
        (status = 200, description = "shipped provider presets", body = [crate::presets::ProviderPreset]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody)
    )
)]
pub async fn provider_presets(
    RequireAdmin(_admin): RequireAdmin,
) -> Json<Vec<crate::presets::ProviderPreset>> {
    Json(crate::presets::all())
}

/// Body of `POST /api/providers/{id}/apply-preset`.
#[derive(Debug, Default, serde::Deserialize, utoipa::ToSchema)]
pub struct ApplyPresetInput {
    /// Preset to apply; defaults to the provider's stored preset.
    #[serde(default)]
    pub preset: Option<String>,
    /// Also replace connectors the operator edited.
    #[serde(default)]
    pub force: bool,
}

/// Re-applies a preset's connector defaults to an existing provider (D52).
/// Unmodified connectors are updated; operator-edited ones are kept unless
/// `force` is set. Audited as `provider.apply_preset`.
#[utoipa::path(
    post,
    path = "/api/providers/{id}/apply-preset",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    request_body = ApplyPresetInput,
    responses(
        (status = 200, description = "what changed", body = crate::presets::ApplyPresetView),
        (status = 400, description = "unknown or missing preset", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such provider", body = ErrorBody)
    )
)]
pub async fn apply_preset(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<ApplyPresetInput>>,
) -> Result<Json<crate::presets::ApplyPresetView>, ApiError> {
    let rid = error::request_id(&headers);
    let input = body.map(|Json(input)| input).unwrap_or_default();
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let view = crate::presets::apply_preset(
        &state.pool,
        &actor,
        &id,
        input.preset.as_deref(),
        input.force,
    )
    .await
    .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(view))
}

/// Fetches one provider. Admin only.
#[utoipa::path(
    get,
    path = "/api/providers/{id}",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    responses(
        (status = 200, description = "the provider", body = ProviderView),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody),
        (status = 404, description = "no such provider", body = ErrorBody)
    )
)]
pub async fn get(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ProviderView>, ApiError> {
    let rid = error::request_id(&headers);
    let row = catalog::get_provider(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .ok_or_else(|| ApiError::not_found("provider").with_request_id(rid.clone()))?;
    let connectors = crate::connectors::summaries_by_provider(&state.pool)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .remove(&row.id)
        .unwrap_or_default();
    Ok(Json(ProviderView::from(&row).with_connectors(connectors)))
}

/// Updates a provider. Admin only, audited as `provider.update`.
#[utoipa::path(
    patch,
    path = "/api/providers/{id}",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    request_body = ProviderPatch,
    responses(
        (status = 200, description = "provider updated", body = ProviderView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such provider", body = ErrorBody),
        (status = 409, description = "name already exists", body = ErrorBody)
    )
)]
pub async fn update(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ProviderPatch>, JsonRejection>,
) -> Result<Json<ProviderView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(patch) = bad_body(body, "provider update", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let row = catalog::update_provider(&state.pool, &actor, &id, &patch)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(ProviderView::from(&row)))
}

/// Deletes a provider and cascades its credentials, offerings, and prices.
/// Admin only, audited as `provider.delete`.
#[utoipa::path(
    delete,
    path = "/api/providers/{id}",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    responses(
        (status = 204, description = "provider deleted (cascade)"),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such provider", body = ErrorBody)
    )
)]
pub async fn delete(
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
    catalog::delete_provider(&state.pool, &actor, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Lists a provider's credentials. Admin only.
///
/// Responses carry `has_secret`, a fixed mask, and header *names* only; the
/// secret cipher and header values never leave the server.
#[utoipa::path(
    get,
    path = "/api/providers/{id}/credentials",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    responses(
        (status = 200, description = "credentials of the provider", body = [CredentialView]),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody),
        (status = 404, description = "no such provider", body = ErrorBody)
    )
)]
pub async fn list_credentials(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<CredentialView>>, ApiError> {
    let rid = error::request_id(&headers);
    if catalog::get_provider(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .is_none()
    {
        return Err(ApiError::not_found("provider").with_request_id(rid));
    }
    let rows = catalog::list_credentials(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(rows.iter().map(CredentialView::from).collect()))
}

/// Creates a credential (the secret is write-only). Admin only, audited as
/// `credential.create`.
#[utoipa::path(
    post,
    path = "/api/providers/{id}/credentials",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    request_body = CredentialInput,
    responses(
        (status = 201, description = "credential created", body = CredentialView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such provider", body = ErrorBody)
    )
)]
pub async fn create_credential(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<CredentialInput>, JsonRejection>,
) -> Result<(StatusCode, Json<CredentialView>), ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "credential create", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let row = catalog::create_credential(&state.pool, &state.secrets, &actor, &id, &input)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok((StatusCode::CREATED, Json(CredentialView::from(&row))))
}

/// Fetches one credential (redacted view). Admin only.
#[utoipa::path(
    get,
    path = "/api/credentials/{id}",
    tag = "admin",
    params(("id" = String, Path, description = "credential id")),
    responses(
        (status = 200, description = "the credential (redacted)", body = CredentialView),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody),
        (status = 404, description = "no such credential", body = ErrorBody)
    )
)]
pub async fn get_credential(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CredentialView>, ApiError> {
    let rid = error::request_id(&headers);
    let row = catalog::get_credential(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .ok_or_else(|| ApiError::not_found("credential").with_request_id(rid))?;
    Ok(Json(CredentialView::from(&row)))
}

/// Updates a credential. A supplied `secret` replaces the stored one and the
/// mutation is audited as `credential.rotate`; anything else is
/// `credential.update`.
#[utoipa::path(
    patch,
    path = "/api/credentials/{id}",
    tag = "admin",
    params(("id" = String, Path, description = "credential id")),
    request_body = CredentialPatch,
    responses(
        (status = 200, description = "credential updated", body = CredentialView),
        (status = 400, description = "invalid request body", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such credential", body = ErrorBody)
    )
)]
pub async fn update_credential(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<CredentialPatch>, JsonRejection>,
) -> Result<Json<CredentialView>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(patch) = bad_body(body, "credential update", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let row = catalog::update_credential(&state.pool, &state.secrets, &actor, &id, &patch)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(CredentialView::from(&row)))
}

/// Deletes a credential. Admin only, audited as `credential.delete`.
#[utoipa::path(
    delete,
    path = "/api/credentials/{id}",
    tag = "admin",
    params(("id" = String, Path, description = "credential id")),
    responses(
        (status = 204, description = "credential deleted"),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such credential", body = ErrorBody)
    )
)]
pub async fn delete_credential(
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
    catalog::delete_credential(&state.pool, &actor, &id)
        .await
        .map_err(|e| e.into_api_error(rid))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Probes `GET {base_url}/models` with the credential's headers and secret,
/// records the outcome on the credential, and audits it as
/// `credential.test`. Admin only.
#[utoipa::path(
    post,
    path = "/api/credentials/{id}/test",
    tag = "admin",
    params(("id" = String, Path, description = "credential id")),
    responses(
        (status = 200, description = "probe outcome (also recorded on the credential)", body = CredentialTestView),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such credential", body = ErrorBody)
    )
)]
pub async fn test_credential(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CredentialTestView>, ApiError> {
    let rid = error::request_id(&headers);
    let row = catalog::get_credential(&state.pool, &id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .ok_or_else(|| ApiError::not_found("credential").with_request_id(rid.clone()))?;
    let provider = catalog::get_provider(&state.pool, &row.provider_id)
        .await
        .map_err(|e| e.into_api_error(rid.clone()))?
        .ok_or_else(|| ApiError::not_found("provider").with_request_id(rid.clone()))?;

    // The header merge is the same pure function M3 will use per request.
    // Header values are sealed (D17) and open under the credential row id.
    let merged_headers = catalog::headers::merge_headers(
        &state.secrets,
        row.headers_json
            .as_deref()
            .map(|raw| (raw, row.id.as_str())),
        None,
    )
    .map_err(|e| e.into_api_error(rid.clone()))?;
    // Zeroized on drop: the plaintext secret lives only for the probe.
    let secret =
        zeroize::Zeroizing::new(state.secrets.decrypt(&row.secret_cipher, &row.id).map_err(
            |e| ApiError::internal("decrypting credential secret", e).with_request_id(rid.clone()),
        )?);

    let result = match crate::connectors::health_probe(
        &state.pool,
        &state.secrets,
        &row.provider_id,
        &row.id,
    )
    .await
    .map_err(|e| e.into_api_error(rid.clone()))?
    {
        Some(result) => result,
        None => {
            probe::probe_models(&provider.base_url, &merged_headers, Some(secret.as_str())).await
        }
    };
    let row = catalog::record_credential_test(
        &state.pool,
        &Actor {
            user_id: &admin.user_id,
            ip: &ip.0,
        },
        &id,
        &result,
    )
    .await
    .map_err(|e| e.into_api_error(rid))?;

    Ok(Json(CredentialTestView {
        status: result.status().to_string(),
        ok: result.ok,
        http_status: result.http_status,
        latency_ms: result.latency_ms,
        error: result.error,
        tested_at: row.last_tested_at.unwrap_or_default(),
    }))
}

/// Discovers the provider's models from its `GET {base_url}/models` endpoint
/// and materializes them as models and offerings. Admin only and audited as
/// `provider.discover_models`.
///
/// The credential defaults to the provider's first active one; `dry_run`
/// previews the counts without writing catalog rows. Upstream failures
/// (unreachable, timeout, 401/403, malformed payload) answer `502
/// upstream_error` and write nothing.
#[utoipa::path(
    post,
    path = "/api/providers/{id}/discover-models",
    tag = "admin",
    params(("id" = String, Path, description = "provider id")),
    request_body = DiscoveryInput,
    responses(
        (status = 200, description = "discovery summary (dry-run included)", body = DiscoverySummary),
        (status = 400, description = "invalid body or provider has no credentials", body = ErrorBody),
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "forbidden or CSRF failure", body = ErrorBody),
        (status = 404, description = "no such provider or credential", body = ErrorBody),
        (status = 502, description = "provider /models request failed", body = ErrorBody)
    )
)]
pub async fn discover_models(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    ip: ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<DiscoveryInput>, JsonRejection>,
) -> Result<Json<DiscoverySummary>, ApiError> {
    let rid = error::request_id(&headers);
    let Json(input) = bad_body(body, "provider discover models", &rid)?;
    let actor = Actor {
        user_id: &admin.user_id,
        ip: &ip.0,
    };
    let summary =
        catalog::discovery::discover_models(&state.pool, &state.secrets, &actor, &id, &input)
            .await
            .map_err(|e| e.into_api_error(rid))?;
    Ok(Json(summary))
}

#[cfg(test)]
mod tests {
    use crate::auth::Role;
    use crate::test_support::{self, TestApp, api_request, send};
    use axum::http::{Method, StatusCode};
    use serde_json::json;

    /// True when any database file (including WAL) contains `needle`.
    fn database_contains(app: &TestApp, needle: &str) -> bool {
        for name in ["gateway.db", "gateway.db-wal", "gateway.db-shm"] {
            if let Ok(bytes) = std::fs::read(app.state.config.data_dir.join(name))
                && String::from_utf8_lossy(&bytes).contains(needle)
            {
                return true;
            }
        }
        false
    }

    async fn create_provider_over_http(
        router: &axum::Router,
        cookie: &str,
        csrf: &str,
        name: &str,
        base_url: &str,
    ) -> serde_json::Value {
        let response = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": name, "base_url": base_url })),
                Some(cookie),
                Some(csrf),
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
        response.body
    }

    #[tokio::test]
    async fn provider_crud_round_trip_writes_one_audit_row_per_mutation() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;

        // Create.
        let created = create_provider_over_http(
            &router,
            &cookie,
            &csrf,
            "acme",
            "https://api.acme.example/v1",
        )
        .await;
        let id = created["id"].as_str().expect("provider id").to_string();
        assert_eq!(created["name"], "acme");
        assert_eq!(created["kind"], "openai_compatible");
        assert_eq!(created["enabled"], true);

        // Read one and list.
        let fetched = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/providers/{id}"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(fetched.status, StatusCode::OK);
        assert_eq!(fetched.body["base_url"], "https://api.acme.example/v1");

        let list = send(
            router.clone(),
            api_request(Method::GET, "/api/providers", None, Some(&cookie), None),
        )
        .await;
        assert_eq!(list.status, StatusCode::OK);
        assert_eq!(list.body.as_array().expect("list").len(), 1);

        // Update.
        let updated = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/providers/{id}"),
                Some(json!({ "name": "acme-renamed", "enabled": false })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
        assert_eq!(updated.body["name"], "acme-renamed");
        assert_eq!(updated.body["enabled"], false);

        // Delete, then 404.
        let deleted = send(
            router.clone(),
            api_request(
                Method::DELETE,
                &format!("/api/providers/{id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
        let gone = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/providers/{id}"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(gone.status, StatusCode::NOT_FOUND);
        assert_eq!(gone.body["error"]["code"], "not_found");

        // Exactly one audit row per mutation, naming what changed.
        for (action, count) in [
            ("provider.create", 1),
            ("provider.update", 1),
            ("provider.delete", 1),
        ] {
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = ?")
                .bind(action)
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
            assert_eq!(rows, count, "one audit row for {action}");
        }
        let detail: String = sqlx::query_scalar(
            "SELECT detail_json FROM audit_log WHERE action = 'provider.create'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("audit detail");
        assert!(detail.contains("acme"), "the audit says what was created");
    }

    #[tokio::test]
    async fn duplicate_provider_name_is_a_conflict_and_invalid_input_is_400() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;

        create_provider_over_http(&router, &cookie, &csrf, "acme", "https://a.example/v1").await;
        let duplicate = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "acme", "base_url": "https://b.example/v1" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(duplicate.status, StatusCode::CONFLICT);
        assert_eq!(duplicate.body["error"]["code"], "conflict");

        let bad_url = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "bad", "base_url": "not a url" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(bad_url.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn provider_mutations_require_an_admin_session_and_csrf() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        app.create_user("user@example.com", Role::User, true).await;
        let router = app.router();

        let anonymous = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "x", "base_url": "https://x.example/v1" })),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

        let (user_cookie, user_csrf) = test_support::login(&router, "user@example.com").await;
        let non_admin = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "x", "base_url": "https://x.example/v1" })),
                Some(&user_cookie),
                Some(&user_csrf),
            ),
        )
        .await;
        assert_eq!(non_admin.status, StatusCode::FORBIDDEN);
        assert_eq!(non_admin.body["error"]["code"], "forbidden");

        let (admin_cookie, _) = test_support::login(&router, "admin@example.com").await;
        let no_csrf = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "x", "base_url": "https://x.example/v1" })),
                Some(&admin_cookie),
                None,
            ),
        )
        .await;
        assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
        assert_eq!(no_csrf.body["error"]["code"], "csrf_failed");
    }

    #[tokio::test]
    async fn unprotected_mode_mutations_work_and_their_audit_rows_are_flagged_insecure() {
        let app = TestApp::unprotected().await;
        let router = app.router();

        // The pre-fix failure: an audited mutation as the synthetic local
        // operator broke its foreign key onto `users` and rolled back.
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "acme", "base_url": "https://api.acme.example/v1" })),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

        let (actor, insecure): (Option<String>, i64) = sqlx::query_as(
            "SELECT actor_user_id, insecure FROM audit_log WHERE action = 'provider.create'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("audit row");
        assert_eq!(actor.as_deref(), Some("local-operator"));
        assert_eq!(insecure, 1, "done in insecure mode");

        // The flag is visible through the API too.
        let audits = send(
            router.clone(),
            api_request(Method::GET, "/api/audit?limit=50", None, None, None),
        )
        .await;
        assert_eq!(audits.status, StatusCode::OK, "{}", audits.body);
        let entry = audits.body["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .find(|e| e["action"] == "provider.create")
            .expect("provider.create entry");
        assert_eq!(entry["insecure"], json!(true));
    }

    #[tokio::test]
    async fn secret_never_reaches_the_database_and_header_values_are_redacted_everywhere() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;

        let provider = create_provider_over_http(
            &router,
            &cookie,
            &csrf,
            "acme",
            "https://api.acme.example/v1",
        )
        .await;
        let provider_id = provider["id"].as_str().expect("provider id");

        const SECRET: &str = "sk-plaintext-secret-12345";
        const HEADER_VALUE: &str = "header-value-that-is-secret";
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{provider_id}/credentials"),
                Some(json!({
                    "label": "primary key",
                    "secret": SECRET,
                    "headers": { "x-api-key": HEADER_VALUE },
                    "balance_usd": 25.5,
                    "balance_low_threshold": 5.0,
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let credential_id = created.body["id"]
            .as_str()
            .expect("credential id")
            .to_string();

        // Every response exposes the redacted shape and no secret material.
        // Header values are sealed at rest like the secret (D17); a response
        // or an audit row never carries them.
        let list = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/providers/{provider_id}/credentials"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        let fetched = send(
            router.clone(),
            api_request(
                Method::GET,
                &format!("/api/credentials/{credential_id}"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        let audits = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/audit?limit=200",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;

        for response in [&created.body, &list.body, &fetched.body, &audits.body] {
            let text = response.to_string();
            assert!(!text.contains(SECRET), "secret leaked in {text}");
            assert!(
                !text.contains(HEADER_VALUE),
                "header value leaked in {text}"
            );
        }

        let view = &fetched.body;
        assert_eq!(view["has_secret"], true);
        assert_eq!(view["secret_masked"], "********");
        assert_eq!(view["header_names"], json!(["x-api-key"]));
        assert_eq!(view["balance_usd"], 25.5);
        assert_eq!(view["balance_source"], "manual");
        assert!(view["balance_updated_at"].is_string());
        assert_eq!(
            view["balance_updated_by"],
            json!(created_user_id(&app).await)
        );

        // The SQLite files never hold the secret or a header value in
        // plaintext — only AEAD ciphertext (with the row id as AAD) and the
        // plain header names.
        assert!(
            !database_contains(&app, SECRET),
            "the secret is plaintext in the database"
        );
        assert!(
            !database_contains(&app, HEADER_VALUE),
            "the header value is plaintext in the database"
        );
        assert!(
            database_contains(&app, "x-api-key"),
            "header names stay plain so views and audit can show them"
        );

        // But the ciphertext is really there and really decryptable.
        let cipher: String =
            sqlx::query_scalar("SELECT secret_cipher FROM provider_credentials WHERE id = ?")
                .bind(&credential_id)
                .fetch_one(&app.state.pool)
                .await
                .expect("secret_cipher");
        assert!(cipher.starts_with("v1:"), "AEAD envelope: {cipher}");
        assert_eq!(
            app.state
                .secrets
                .decrypt(&cipher, &credential_id)
                .expect("cipher decrypts with the row id as AAD"),
            SECRET
        );
    }

    /// The admin user id, for `balance_updated_by` assertions.
    async fn created_user_id(app: &TestApp) -> String {
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'admin@example.com'")
            .fetch_one(&app.state.pool)
            .await
            .expect("admin id")
    }

    #[tokio::test]
    async fn credential_crud_audits_create_update_rotate_and_delete() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let provider = create_provider_over_http(
            &router,
            &cookie,
            &csrf,
            "acme",
            "https://api.acme.example/v1",
        )
        .await;
        let provider_id = provider["id"].as_str().expect("provider id");

        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{provider_id}/credentials"),
                Some(json!({ "label": "first", "secret": "sk-one" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED);
        let id = created.body["id"]
            .as_str()
            .expect("credential id")
            .to_string();

        // Label-only update is `credential.update`.
        let renamed = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/credentials/{id}"),
                Some(json!({ "label": "renamed" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
        assert_eq!(renamed.body["label"], "renamed");

        // A new secret rotates it and is audited as `credential.rotate`.
        let rotated = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/credentials/{id}"),
                Some(json!({ "secret": "sk-two" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(rotated.status, StatusCode::OK);
        let cipher: String =
            sqlx::query_scalar("SELECT secret_cipher FROM provider_credentials WHERE id = ?")
                .bind(&id)
                .fetch_one(&app.state.pool)
                .await
                .expect("cipher");
        assert_eq!(
            app.state
                .secrets
                .decrypt(&cipher, &id)
                .expect("rotated secret decrypts"),
            "sk-two"
        );

        let deleted = send(
            router.clone(),
            api_request(
                Method::DELETE,
                &format!("/api/credentials/{id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);

        for (action, count) in [
            ("credential.create", 1),
            ("credential.update", 1),
            ("credential.rotate", 1),
            ("credential.delete", 1),
        ] {
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = ?")
                .bind(action)
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
            assert_eq!(rows, count, "one audit row for {action}");
        }
    }

    #[tokio::test]
    async fn deleting_a_provider_cascades_its_credentials_offerings_and_prices() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let provider = create_provider_over_http(
            &router,
            &cookie,
            &csrf,
            "acme",
            "https://api.acme.example/v1",
        )
        .await;
        let provider_id = provider["id"].as_str().expect("provider id");

        for label in ["a", "b"] {
            let created = send(
                router.clone(),
                api_request(
                    Method::POST,
                    &format!("/api/providers/{provider_id}/credentials"),
                    Some(json!({ "label": label, "secret": format!("sk-{label}") })),
                    Some(&cookie),
                    Some(&csrf),
                ),
            )
            .await;
            assert_eq!(created.status, StatusCode::CREATED);
        }

        // One offering with a price rides along on the cascade.
        let model = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/models",
                Some(json!({ "canonical_name": "acme/brain-1" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        let model_id = model.body["id"].as_str().expect("model id").to_string();
        let offering = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/offerings",
                Some(json!({
                    "provider_id": provider_id,
                    "model_id": model_id,
                    "upstream_model_id": "brain-1",
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        let offering_id = offering.body["id"]
            .as_str()
            .expect("offering id")
            .to_string();
        let priced = send(
            router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({ "input_per_m": 1.0 })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(priced.status, StatusCode::OK);

        let deleted = send(
            router.clone(),
            api_request(
                Method::DELETE,
                &format!("/api/providers/{provider_id}"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);

        for table in ["provider_credentials", "offerings", "prices"] {
            let remaining: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&app.state.pool)
                .await
                .expect("count");
            assert_eq!(remaining, 0, "{table} cascade with their provider");
        }
        // The model itself has no provider and stays.
        let models: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM models")
            .fetch_one(&app.state.pool)
            .await
            .expect("model count");
        assert_eq!(models, 1);

        // The cascade counts land in the audit detail.
        let detail: String = sqlx::query_scalar(
            "SELECT detail_json FROM audit_log WHERE action = 'provider.delete'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("delete detail");
        assert!(detail.contains("\"cascaded_credentials\":2"), "{detail}");
        assert!(detail.contains("\"cascaded_offerings\":1"), "{detail}");
        assert!(detail.contains("\"cascaded_prices\":1"), "{detail}");
    }

    /// Arranges provider + credential over HTTP and returns the ids.
    async fn provider_and_credential(
        router: &axum::Router,
        cookie: &str,
        csrf: &str,
        base_url: &str,
    ) -> (String, String) {
        let provider = create_provider_over_http(router, cookie, csrf, "acme", base_url).await;
        let provider_id = provider["id"].as_str().expect("provider id").to_string();
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{provider_id}/credentials"),
                Some(json!({
                    "label": "primary",
                    "secret": "sk-live-secret",
                    "headers": { "x-tenant": "team-a" },
                })),
                Some(cookie),
                Some(csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let credential_id = created.body["id"].as_str().expect("id").to_string();
        (provider_id, credential_id)
    }

    #[tokio::test]
    async fn credential_test_probe_hits_the_provider_and_records_the_outcome() {
        let (addr, seen) = test_support::spawn_static_server("200 OK", r#"{"data":[]}"#).await;
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (_provider_id, credential_id) =
            provider_and_credential(&router, &cookie, &csrf, &format!("http://{addr}/v1")).await;

        let tested = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/credentials/{credential_id}/test"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(tested.status, StatusCode::OK, "{}", tested.body);
        assert_eq!(tested.body["status"], "ok");
        assert_eq!(tested.body["ok"], true);
        assert_eq!(tested.body["http_status"], 200);
        assert!(tested.body["tested_at"].is_string());

        // The wire request carried the secret and the merged headers.
        let captured = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(captured.len(), 1);
        let request = captured[0].to_ascii_lowercase();
        assert!(request.starts_with("get /v1/models http/1.1"), "{request}");
        assert!(
            request.contains("authorization: bearer sk-live-secret"),
            "{request}"
        );
        assert!(request.contains("x-tenant: team-a"), "{request}");

        // The outcome is recorded on the credential and audited, without
        // secret material.
        let (status, last_tested): (String, Option<String>) =
            sqlx::query_as("SELECT status, last_tested_at FROM provider_credentials WHERE id = ?")
                .bind(&credential_id)
                .fetch_one(&app.state.pool)
                .await
                .expect("credential row");
        assert_eq!(status, "ok");
        assert!(last_tested.is_some());

        let (audit_count, detail): (i64, String) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(MAX(detail_json), '') FROM audit_log \
             WHERE action = 'credential.test'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("audit row");
        assert_eq!(audit_count, 1);
        assert!(detail.contains("\"ok\":true"), "{detail}");
        assert!(!detail.contains("sk-live-secret"), "{detail}");
    }

    #[tokio::test]
    async fn credential_test_probe_failure_is_recorded_without_leaking() {
        let (addr, _) = test_support::spawn_static_server("401 Unauthorized", "{}").await;
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (_provider_id, credential_id) =
            provider_and_credential(&router, &cookie, &csrf, &format!("http://{addr}")).await;

        let tested = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/credentials/{credential_id}/test"),
                None,
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(tested.status, StatusCode::OK);
        assert_eq!(tested.body["status"], "error");
        assert_eq!(tested.body["ok"], false);
        assert_eq!(tested.body["http_status"], 401);
        assert!(!tested.body.to_string().contains("sk-live-secret"));

        let status: String =
            sqlx::query_scalar("SELECT status FROM provider_credentials WHERE id = ?")
                .bind(&credential_id)
                .fetch_one(&app.state.pool)
                .await
                .expect("credential row");
        assert_eq!(status, "error");
    }

    #[tokio::test]
    async fn secret_validation_accepts_long_untrimmed_keys_and_caps_at_8kb() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let provider = create_provider_over_http(
            &router,
            &cookie,
            &csrf,
            "acme",
            "https://api.acme.example/v1",
        )
        .await;
        let provider_id = provider["id"].as_str().expect("provider id");

        // A 300-character secret (past the old 200-char name cap) is accepted
        // verbatim — whitespace is key material and is never trimmed.
        let long_secret = format!(" {} ", "a".repeat(300));
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{provider_id}/credentials"),
                Some(json!({ "label": "oauth", "secret": long_secret })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let id = created.body["id"]
            .as_str()
            .expect("credential id")
            .to_string();
        let cipher: String =
            sqlx::query_scalar("SELECT secret_cipher FROM provider_credentials WHERE id = ?")
                .bind(&id)
                .fetch_one(&app.state.pool)
                .await
                .expect("cipher");
        assert_eq!(
            app.state.secrets.decrypt(&cipher, &id).expect("decrypts"),
            long_secret,
            "the secret round-trips untrimmed"
        );

        // 8 KB is the cap: 8193 bytes are rejected with a clear 400 on
        // create and on rotate.
        let oversized = "a".repeat(8193);
        for (method, uri, body) in [
            (
                Method::POST,
                format!("/api/providers/{provider_id}/credentials"),
                json!({ "label": "big", "secret": oversized.clone() }),
            ),
            (
                Method::PATCH,
                format!("/api/credentials/{id}"),
                json!({ "secret": oversized.clone() }),
            ),
        ] {
            let response = send(
                router.clone(),
                api_request(method, &uri, Some(body), Some(&cookie), Some(&csrf)),
            )
            .await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST, "{uri}");
            assert!(
                response.body["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("8192")),
                "{uri}: {}",
                response.body
            );
        }
    }

    #[tokio::test]
    async fn balance_source_is_stored_trimmed_and_unknown_values_are_400() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        let (_provider_id, credential_id) =
            provider_and_credential(&router, &cookie, &csrf, "https://api.acme.example/v1").await;

        // `" manual"` normalizes to `manual` (stored trimmed) instead of
        // tripping the DB CHECK as a 500.
        let normalized = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/credentials/{credential_id}"),
                Some(json!({ "balance_usd": 10.0, "balance_source": " manual" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(normalized.status, StatusCode::OK, "{}", normalized.body);
        assert_eq!(normalized.body["balance_source"], "manual");
        let stored: String =
            sqlx::query_scalar("SELECT balance_source FROM provider_credentials WHERE id = ?")
                .bind(&credential_id)
                .fetch_one(&app.state.pool)
                .await
                .expect("balance_source");
        assert_eq!(stored, "manual", "the trimmed value is what is stored");

        // An unknown source is a clean 400 naming the field.
        let bogus = send(
            router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/credentials/{credential_id}"),
                Some(json!({ "balance_source": "by hand" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(bogus.status, StatusCode::BAD_REQUEST);
        assert!(
            bogus.body["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("balance_source")),
            "{}",
            bogus.body
        );
    }

    #[tokio::test]
    async fn unknown_ids_get_the_uniform_404() {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;

        for (method, uri, body) in [
            (Method::GET, "/api/providers/nope".to_string(), None),
            (Method::GET, "/api/credentials/nope".to_string(), None),
            (Method::DELETE, "/api/credentials/nope".to_string(), None),
            (
                Method::PATCH,
                "/api/credentials/nope".to_string(),
                Some(json!({ "label": "x" })),
            ),
            (
                Method::POST,
                "/api/providers/nope/credentials".to_string(),
                Some(json!({ "label": "x", "secret": "sk" })),
            ),
            (Method::POST, "/api/credentials/nope/test".to_string(), None),
        ] {
            let response = send(
                router.clone(),
                api_request(method.clone(), &uri, body, Some(&cookie), Some(&csrf)),
            )
            .await;
            assert_eq!(response.status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(response.body["error"]["code"], "not_found", "{uri}");
        }
    }

    async fn admin_router() -> (TestApp, axum::Router, String, String) {
        let app = TestApp::new().await;
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = test_support::login(&router, "admin@example.com").await;
        (app, router, cookie, csrf)
    }

    #[tokio::test]
    async fn provider_presets_endpoint_lists_the_registry() {
        let (_app, router, cookie, _csrf) = admin_router().await;
        let response = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/provider-presets",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        let presets = response.body.as_array().expect("presets");
        let ids: Vec<&str> = presets
            .iter()
            .map(|preset| preset["id"].as_str().expect("id"))
            .collect();
        assert_eq!(ids, ["openrouter", "sailresearch", "generic"]);
        assert_eq!(presets[0]["base_url"], "https://openrouter.ai/api/v1");
        assert_eq!(presets[1]["base_url"], "https://api.sailresearch.com/v1");
        assert!(presets[2]["configs"].is_null(), "generic seeds nothing");
    }

    #[tokio::test]
    async fn create_with_a_preset_seeds_its_connectors_in_one_transaction() {
        let (app, router, cookie, csrf) = admin_router().await;
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "sail", "preset": "sailresearch" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        assert_eq!(created.body["preset"], "sailresearch");
        assert_eq!(
            created.body["base_url"], "https://api.sailresearch.com/v1",
            "the preset's default base URL is applied"
        );
        assert_eq!(created.body["kind"], "openai_compatible");
        let kinds: Vec<&str> = created.body["connectors"]
            .as_array()
            .expect("connectors")
            .iter()
            .map(|connector| connector["kind"].as_str().expect("kind"))
            .collect();
        assert_eq!(kinds, ["balance", "catalog", "health"]);
        assert!(
            created.body["connectors"]
                .as_array()
                .expect("connectors")
                .iter()
                .all(|connector| connector["enabled"] == true)
        );

        let provider_id = created.body["id"].as_str().expect("id");
        let stored: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM provider_connectors WHERE provider_id = ?")
                .bind(provider_id)
                .fetch_one(&app.state.pool)
                .await
                .expect("connectors");
        assert_eq!(stored, 3);
        let audit: String = sqlx::query_scalar(
            "SELECT detail_json FROM audit_log WHERE action = 'provider.create'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("audit");
        assert!(audit.contains("sailresearch"), "{audit}");
    }

    #[tokio::test]
    async fn explicit_base_url_and_connectors_win_over_the_preset() {
        let (app, router, cookie, csrf) = admin_router().await;
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({
                    "name": "mock",
                    "preset": "sailresearch",
                    "base_url": "http://127.0.0.1:9/v1",
                    "connectors": {
                        "catalog": {
                            "url": "/models",
                            "allow_private": true,
                            "catalog": {
                                "entries": "/data",
                                "model_id": "/id",
                                "input": "/price/input",
                                "output": "/price/output"
                            }
                        }
                    }
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        assert_eq!(created.body["preset"], "sailresearch");
        assert_eq!(
            created.body["base_url"], "http://127.0.0.1:9/v1",
            "the explicit override wins"
        );
        let provider_id = created.body["id"].as_str().expect("id");
        let catalog: String = sqlx::query_scalar(
            "SELECT config_json FROM provider_connectors WHERE provider_id = ? AND kind = 'catalog'",
        )
        .bind(provider_id)
        .fetch_one(&app.state.pool)
        .await
        .expect("catalog");
        assert!(catalog.contains("/price/input"), "{catalog}");
        assert!(
            !catalog.contains("pricing_usd_per_million_tokens"),
            "{catalog}"
        );
        // The other kinds still come from the preset.
        let balance: String = sqlx::query_scalar(
            "SELECT config_json FROM provider_connectors WHERE provider_id = ? AND kind = 'balance'",
        )
        .bind(provider_id)
        .fetch_one(&app.state.pool)
        .await
        .expect("balance");
        assert!(balance.contains("/v2/usage/summary?range=30d"), "{balance}");
    }

    #[tokio::test]
    async fn apply_preset_keeps_operator_edits_unless_forced() {
        let (app, router, cookie, csrf) = admin_router().await;
        let created = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "sail", "preset": "sailresearch" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        let provider_id = created.body["id"].as_str().expect("id").to_string();

        // The operator edits the catalog connector.
        let edited = send(
            router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/providers/{provider_id}/connectors/catalog"),
                Some(json!({
                    "config": {
                        "url": "/models",
                        "catalog": {
                            "entries": "/data",
                            "model_id": "/id",
                            "input": "/price/input",
                            "output": "/price/output"
                        }
                    },
                    "enabled": true
                })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);

        // Without force: the edited catalog is skipped, untouched kinds are
        // unchanged (they already equal the preset).
        let applied = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{provider_id}/apply-preset"),
                Some(json!({})),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(applied.status, StatusCode::OK, "{}", applied.body);
        assert_eq!(applied.body["preset"], "sailresearch");
        let changes = applied.body["changes"].as_array().expect("changes");
        assert!(
            changes
                .iter()
                .any(|change| change["kind"] == "catalog" && change["action"] == "skipped"),
            "{}",
            applied.body
        );
        let catalog: String = sqlx::query_scalar(
            "SELECT config_json FROM provider_connectors WHERE provider_id = ? AND kind = 'catalog'",
        )
        .bind(&provider_id)
        .fetch_one(&app.state.pool)
        .await
        .expect("catalog");
        assert!(catalog.contains("/price/input"), "edit kept: {catalog}");

        // With force: the preset replaces the edit.
        let forced = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{provider_id}/apply-preset"),
                Some(json!({ "force": true })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(forced.status, StatusCode::OK, "{}", forced.body);
        assert!(
            forced.body["changes"]
                .as_array()
                .expect("changes")
                .iter()
                .any(|change| change["kind"] == "catalog" && change["action"] == "overwritten"),
            "{}",
            forced.body
        );
        let catalog: String = sqlx::query_scalar(
            "SELECT config_json FROM provider_connectors WHERE provider_id = ? AND kind = 'catalog'",
        )
        .bind(&provider_id)
        .fetch_one(&app.state.pool)
        .await
        .expect("catalog");
        assert!(
            catalog.contains("pricing_usd_per_million_tokens"),
            "preset restored: {catalog}"
        );

        // A provider created without a preset can adopt one explicitly.
        let plain = create_provider_over_http(
            &router,
            &cookie,
            &csrf,
            "plain",
            "https://api.example.com/v1",
        )
        .await;
        let plain_id = plain["id"].as_str().expect("id");
        let adopted = send(
            router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{plain_id}/apply-preset"),
                Some(json!({ "preset": "openrouter" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(adopted.status, StatusCode::OK, "{}", adopted.body);
        let preset: Option<String> =
            sqlx::query_scalar("SELECT preset FROM providers WHERE id = ?")
                .bind(plain_id)
                .fetch_one(&app.state.pool)
                .await
                .expect("preset");
        assert_eq!(preset.as_deref(), Some("openrouter"));
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'provider.apply_preset'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("audit");
        assert_eq!(
            audits, 3,
            "skip-only run + forced run + adoption are all audited"
        );
    }

    #[tokio::test]
    async fn an_unknown_preset_is_refused() {
        let (_app, router, cookie, csrf) = admin_router().await;
        let response = send(
            router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "x", "preset": "nope" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(
            response.status,
            StatusCode::BAD_REQUEST,
            "{}",
            response.body
        );
        assert!(
            response.body["error"]["message"]
                .as_str()
                .expect("message")
                .contains("unknown provider preset"),
            "{}",
            response.body
        );
    }
}
