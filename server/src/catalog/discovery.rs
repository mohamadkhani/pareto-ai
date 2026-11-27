//! Provider model discovery: `POST /api/providers/{id}/discover-models`.
//!
//! The gateway asks the provider's own OpenAI-compatible `GET {base_url}/models`
//! endpoint (with the credential's merged headers and its secret as bearer
//! auth, the same machinery as the credential health probe) and materializes
//! the answer in the catalog:
//!
//! * every usable upstream model id becomes an `offerings` row under a catalog
//!   model. The model is resolved in this order: the exact same
//!   `canonical_name` ignoring ASCII case (`COLLATE NOCASE`), then a unique
//!   **external canonical id** from `model_registry` (models.dev
//!   `canonical_model_id`, then OpenRouter `hugging_face_id`; see D60), then a
//!   unique **catalog identity** ([`crate::model_identity::catalog_identity`]):
//!   the tail id with case and punctuation normalized and the path vendor
//!   dropped, keeping tier and build stamps. So
//!   `deepseek-ai/DeepSeek-V4.1-Flash-1017` and
//!   `deepseek/deepseek-v4.1-flash-1017` become one model with two offerings,
//!   while `deepseek-v4-flash` and `deepseek-v4-flash-0731` stay separate.
//!   Several models under one identity mean the operator keeps them apart, so a
//!   new row is created instead of guessing. A new model's `canonical_name` is
//!   the discovered id; `family`, modalities, context length and max output
//!   stay `NULL` unless a catalog connector supplied them, because the OpenAI
//!   list payload carries no such fields;
//! * the provider gets one `offerings` row per id (uniqued by
//!   `(provider_id, upstream_model_id)`). New offerings are enabled, carry no
//!   overrides, no headers, and no price: unpriced is fine because routing
//!   already imputes missing prices.
//!
//! **Idempotent, and manual work always wins.** An existing offering is never
//! rewritten except for re-linking its `model_id` to the canonical model row
//! (the only field discovery owns); `context_override`,
//! `max_output_override`, `enabled`, `headers_json`, and its price row are
//! left exactly as the operator set them. An existing model row is never
//! updated at all, so admin-entered family, limits, modalities, and
//! capabilities survive every later discovery run.
//!
//! **All-or-nothing.** The upstream fetch happens before any write; the
//! materialization then runs in one transaction that also writes the
//! `provider.discover_models` audit row. Any database failure rolls the whole
//! batch back. A failed fetch (unreachable, timeout, 401/403, other non-2xx,
//! malformed payload) returns an `upstream_error` and writes nothing.
//!
//! **Dry run.** `dry_run = true` materializes inside the transaction only to
//! compute the summary, then rolls it back, so it writes no catalog rows. It
//! still records the audit row (with `dry_run: true`), which is the only
//! persistence of the attempt. This POST dry run is the preview; there is no
//! separate `GET` preview endpoint, so the accepted payload, parsing, and
//! counts cannot drift between two code paths.
//!
//! **Last load is persisted.** An apply run also writes the provider's
//! `last_discovery_at`, `last_discovery_created` (models inserted) and
//! `last_discovery_unchanged` (offerings that already matched), which
//! `GET /api/providers` exposes and the providers list renders. A dry run
//! never writes them: its transaction rolls back.
//!
//! Accepted payload shapes (documented because providers differ): an object
//! with a `data` array (OpenAI), an object with a `models` array, or a bare
//! array. Entries may be objects with a non-empty string `id` or plain
//! strings; unusable entries are ignored, duplicate ids are collapsed keeping
//! the first occurrence, and ids reserved for the `combo:` namespace, longer
//! than a model name may be, or a `:batch` tier variant are skipped. The id
//! filter is [`super::is_discoverable_model_id`], shared verbatim with the
//! OpenRouter price sync so both paths agree on the catalog. An empty array is
//! a valid answer
//! (`discovered: 0`); a non-empty array with no usable id, or a body that is
//! not one of the shapes above, is a malformed payload (`502`).

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;

use super::headers;
use super::{Actor, CatalogError, audit, get_provider, now_ts};
use crate::connectors::{self, ConnectorKind, PriceRefreshView};
use crate::probe::{DISCOVERY_TIMEOUT, ModelsFetchError, fetch_models_document_with_timeout};

/// Request body of `POST /api/providers/{id}/discover-models`.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct DiscoveryInput {
    /// Credential to authenticate with. Defaults to the provider's first
    /// active credential (not in an unexpired cooldown, then oldest).
    pub credential_id: Option<String>,
    /// Preview only: compute the summary and write no catalog rows. Defaults
    /// to `false`.
    pub dry_run: Option<bool>,
}

/// What one discovery run found and changed.
#[derive(Debug, Serialize, ToSchema)]
pub struct DiscoverySummary {
    /// True when the run was a preview and wrote no catalog rows.
    pub dry_run: bool,
    /// The credential actually used (resolved, not necessarily the one sent).
    pub credential_id: String,
    /// Usable upstream model ids in the payload.
    pub discovered: usize,
    /// `models` rows inserted (by canonical_name).
    pub created_models: usize,
    /// `offerings` rows inserted for this provider.
    pub created_offerings: usize,
    /// Existing offerings whose `model_id` was re-linked to the canonical
    /// model row. Overrides, headers, enabled, and prices are untouched.
    pub updated_offerings: usize,
    /// Existing offerings that already matched the discovered model.
    pub unchanged: usize,
    /// The price refresh merged into a successful apply when the provider's
    /// catalog connector has `apply_on_discovery`. A dry run never has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prices: Option<PriceRefreshView>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("provider has no credentials; add one before discovering models")]
    NoCredentials,
    #[error("{0}")]
    Upstream(String),
    #[error("the provider's /models payload is malformed: {0}")]
    Malformed(String),
}

impl DiscoveryError {
    /// Maps discovery failures onto the uniform error shape: missing
    /// provider/credential rows are `404`, a provider without credentials is
    /// `400`, and every upstream failure (timeout, unreachable, 401/403,
    /// malformed payload) is `502 upstream_error`.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        use crate::error::ApiError;
        use axum::http::StatusCode;
        match self {
            Self::Catalog(e) => e.into_api_error(rid),
            Self::NoCredentials => ApiError::bad_request(self.to_string()).with_request_id(rid),
            Self::Upstream(message) | Self::Malformed(message) => {
                ApiError::custom(StatusCode::BAD_GATEWAY, "upstream_error", message)
                    .with_request_id(rid)
            }
            Self::Db(e) => ApiError::internal("model discovery", e).with_request_id(rid),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ActiveCredential {
    id: String,
    secret_cipher: String,
    headers_json: Option<String>,
    cooldown_until: Option<String>,
}

/// Discovers the provider's models with the default 15s timeout.
///
/// Failure modes: [`DiscoveryError`] — `404` for an unknown provider or a
/// credential of another provider, `400` when the provider has no credential,
/// `502` for every upstream failure (and no catalog write happens).
pub async fn discover_models(
    pool: &SqlitePool,
    secrets: &crate::secrets::SecretBox,
    actor: &Actor<'_>,
    provider_id: &str,
    input: &DiscoveryInput,
) -> Result<DiscoverySummary, DiscoveryError> {
    discover_models_with_timeout(pool, secrets, actor, provider_id, input, DISCOVERY_TIMEOUT).await
}

/// [`discover_models`] with an explicit timeout, so tests can exercise the
/// timeout path without waiting 15 seconds.
pub(crate) async fn discover_models_with_timeout(
    pool: &SqlitePool,
    secrets: &crate::secrets::SecretBox,
    actor: &Actor<'_>,
    provider_id: &str,
    input: &DiscoveryInput,
    timeout: Duration,
) -> Result<DiscoverySummary, DiscoveryError> {
    let provider = get_provider(pool, provider_id)
        .await?
        .ok_or(CatalogError::NotFound("provider"))?;
    let credential = resolve_credential(pool, provider_id, input.credential_id.as_deref()).await?;

    let merged_headers = headers::merge_headers(
        secrets,
        credential
            .headers_json
            .as_deref()
            .map(|raw| (raw, credential.id.as_str())),
        None,
    )?;
    // Zeroized on drop: the plaintext secret lives only for the fetch.
    let secret = zeroize::Zeroizing::new(
        secrets
            .decrypt(&credential.secret_cipher, &credential.id)
            .map_err(CatalogError::from)?,
    );

    let raw = fetch_models_document_with_timeout(
        &provider.base_url,
        &merged_headers,
        Some(secret.as_str()),
        timeout,
    )
    .await
    .map_err(map_fetch_error)?;

    // A catalog connector, when present, supplies metadata on top of the ids.
    // A broken connector must not break discovery: fall back to the plain id
    // parser and log the reason (D42: connector failure never affects the
    // catalog).
    let mut entries =
        match connectors::enabled_config(pool, provider_id, ConnectorKind::Catalog).await {
            Ok(Some(config)) => match config.catalog.as_ref() {
                Some(catalog_config) => match serde_json::from_str::<serde_json::Value>(&raw)
                    .map_err(|error| error.to_string())
                    .and_then(|document| {
                        connectors::extract_discovery_entries(&document, catalog_config)
                            .map_err(|error| error.to_string())
                    }) {
                    Ok(entries) => entries,
                    Err(error) => {
                        tracing::warn!(
                            provider_id,
                            error,
                            "catalog connector metadata could not be read; discovering ids only"
                        );
                        Vec::new()
                    }
                },
                None => Vec::new(),
            },
            Ok(None) => Vec::new(),
            Err(error) => {
                tracing::warn!(provider_id, error = %error, "catalog connector could not be read");
                Vec::new()
            }
        };
    if entries.is_empty() {
        entries = parse_models_payload(&raw)?
            .into_iter()
            .map(|model_id| connectors::DiscoveryEntry {
                model_id,
                modalities_input: None,
                modalities_output: None,
                context_length: None,
                max_output_tokens: None,
            })
            .collect();
    }

    let summary = apply(
        pool,
        secrets,
        actor,
        provider_id,
        &credential.id,
        &entries,
        input.dry_run.unwrap_or(false),
    )
    .await?;
    Ok(summary)
}

/// Resolves the credential discovery authenticates with. An explicit id must
/// belong to this provider. Otherwise the provider's first active credential
/// wins: not in an unexpired cooldown, then oldest (`created_at`, `id`); when
/// every credential is cooling, the oldest is still used because discovery is
/// an admin action, not request traffic.
async fn resolve_credential(
    pool: &SqlitePool,
    provider_id: &str,
    requested: Option<&str>,
) -> Result<ActiveCredential, DiscoveryError> {
    if let Some(id) = requested {
        let row = super::get_credential(pool, id)
            .await?
            .filter(|row| row.provider_id == provider_id)
            .ok_or(CatalogError::NotFound("credential"))?;
        return Ok(ActiveCredential {
            id: row.id,
            secret_cipher: row.secret_cipher,
            headers_json: row.headers_json,
            cooldown_until: row.cooldown_until,
        });
    }

    let rows = sqlx::query_as::<_, ActiveCredential>(
        "SELECT id, secret_cipher, headers_json, cooldown_until \
         FROM provider_credentials WHERE provider_id = ? ORDER BY created_at, id",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?;
    let now = now_ts();
    let active = rows
        .iter()
        .position(|row| {
            row.cooldown_until
                .as_deref()
                .is_none_or(|until| until <= now.as_str())
        })
        .unwrap_or(0);
    rows.into_iter()
        .nth(active)
        .ok_or(DiscoveryError::NoCredentials)
}

/// Parses an OpenAI-style models document into unique, usable upstream ids.
///
/// Accepts `{"data": [...]}`, `{"models": [...]}`, or a bare array; entries may
/// be `{"id": "..."}` objects or strings. Duplicate and unusable ids are
/// dropped. Failure mode: [`DiscoveryError::Malformed`] for invalid JSON, an
/// unsupported top-level shape, or a non-empty entry list with no usable id.
pub fn parse_models_payload(raw: &str) -> Result<Vec<String>, DiscoveryError> {
    use serde_json::Value;

    let value: Value = serde_json::from_str(raw)
        .map_err(|e| DiscoveryError::Malformed(format!("invalid JSON: {e}")))?;
    let entries = match value {
        Value::Array(entries) => entries,
        Value::Object(object) => object
            .get("data")
            .or_else(|| object.get("models"))
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                DiscoveryError::Malformed(
                    "expected an object with a data or models array, or a bare array".to_string(),
                )
            })?,
        _ => {
            return Err(DiscoveryError::Malformed(
                "expected an object with a data or models array, or a bare array".to_string(),
            ));
        }
    };

    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for entry in entries.iter() {
        let candidate = match entry {
            Value::String(id) => Some(id.as_str()),
            Value::Object(object) => object.get("id").and_then(Value::as_str),
            _ => None,
        };
        let Some(id) = candidate.map(str::trim).filter(|id| !id.is_empty()) else {
            continue;
        };
        // Reserved/bogus names cannot become models; skip them instead of
        // failing the whole discovery run. The same filter runs in the
        // OpenRouter price sync (`catalog::is_discoverable_model_id`).
        if !super::is_discoverable_model_id(id) {
            continue;
        }
        if seen.insert(id.to_string()) {
            ids.push(id.to_string());
        }
    }

    if ids.is_empty() && !entries.is_empty() {
        return Err(DiscoveryError::Malformed(
            "no usable model ids (each entry needs a non-empty string id)".to_string(),
        ));
    }
    Ok(ids)
}

fn map_fetch_error(error: ModelsFetchError) -> DiscoveryError {
    match error {
        ModelsFetchError::Http(status @ (401 | 403)) => DiscoveryError::Upstream(format!(
            "the provider rejected the credential (HTTP {status}) for /models"
        )),
        ModelsFetchError::Http(status) => {
            DiscoveryError::Upstream(format!("the provider returned HTTP {status} for /models"))
        }
        ModelsFetchError::Timeout => {
            DiscoveryError::Upstream("the provider did not answer /models in time".to_string())
        }
        ModelsFetchError::Transport => {
            DiscoveryError::Upstream("the provider could not be reached".to_string())
        }
        ModelsFetchError::InvalidHeader(message) => DiscoveryError::Upstream(message),
    }
}

/// Materializes the discovered entries in one transaction and audits the run.
/// A dry run rolls the catalog changes back and records only the audit row.
/// Metadata from a catalog connector fills a **new** model row only; an
/// existing model keeps every non-NULL value the operator set.
async fn apply(
    pool: &SqlitePool,
    secrets: &crate::secrets::SecretBox,
    actor: &Actor<'_>,
    provider_id: &str,
    credential_id: &str,
    entries: &[connectors::DiscoveryEntry],
    dry_run: bool,
) -> Result<DiscoverySummary, DiscoveryError> {
    let now = now_ts();
    let mut tx = pool.begin().await?;
    let mut models = super::identity::load_index(&mut tx).await?;
    let mut summary = DiscoverySummary {
        dry_run,
        credential_id: credential_id.to_string(),
        discovered: entries.len(),
        created_models: 0,
        created_offerings: 0,
        updated_offerings: 0,
        unchanged: 0,
        prices: None,
    };

    for entry in entries {
        let upstream_id = &entry.model_id;
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT id, model_id FROM offerings \
             WHERE provider_id = ? AND upstream_model_id = ? AND route = ''",
        )
        .bind(provider_id)
        .bind(upstream_id)
        .fetch_optional(&mut *tx)
        .await?;

        let model_id = match models.resolve(upstream_id) {
            crate::model_identity::IdentityMatch::Exact(id)
            | crate::model_identity::IdentityMatch::Unique(id) => id,
            crate::model_identity::IdentityMatch::Ambiguous if existing.is_some() => {
                // An operator already linked this offering to a model. An
                // ambiguous identity cannot prove a different link, so keep it
                // and create nothing.
                if let Some((offering_id, model_id)) = &existing {
                    tracing::warn!(
                        provider_id,
                        upstream_model_id = %upstream_id,
                        offering_id = %offering_id,
                        model_id = %model_id,
                        "ambiguous model identity; keeping the existing offering's model"
                    );
                }
                summary.unchanged += 1;
                continue;
            }
            crate::model_identity::IdentityMatch::Ambiguous
            | crate::model_identity::IdentityMatch::Unknown => {
                let id = uuid::Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO models \
                       (id, canonical_name, modalities_input, modalities_output, \
                        context_length, max_output_tokens, created_at, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(upstream_id)
                .bind(modalities_json(entry.modalities_input.as_deref()))
                .bind(modalities_json(entry.modalities_output.as_deref()))
                .bind(entry.context_length)
                .bind(entry.max_output_tokens)
                .bind(&now)
                .bind(&now)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    super::map_write(e, "a model with this canonical_name already exists")
                })?;
                models.insert(&id, upstream_id);
                summary.created_models += 1;
                id
            }
        };

        match existing {
            Some((offering_id, existing_model_id)) if existing_model_id != model_id => {
                // The only offering field discovery owns: keep overrides,
                // headers, enabled, and prices exactly as configured.
                sqlx::query("UPDATE offerings SET model_id = ?, updated_at = ? WHERE id = ?")
                    .bind(&model_id)
                    .bind(&now)
                    .bind(&offering_id)
                    .execute(&mut *tx)
                    .await?;
                summary.updated_offerings += 1;
            }
            Some(_) => summary.unchanged += 1,
            None => {
                sqlx::query(
                    "INSERT INTO offerings \
                       (id, provider_id, model_id, upstream_model_id, enabled, created_at, updated_at) \
                     VALUES (?, ?, ?, ?, 1, ?, ?)",
                )
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(provider_id)
                .bind(&model_id)
                .bind(upstream_id)
                .bind(&now)
                .bind(&now)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    super::map_write(
                        e,
                        "this provider already has an offering for that upstream_model_id",
                    )
                })?;
                summary.created_offerings += 1;
            }
        }
    }

    let detail = serde_json::json!({
        "dry_run": dry_run,
        "credential_id": credential_id,
        "discovered": summary.discovered,
        "created_models": summary.created_models,
        "created_offerings": summary.created_offerings,
        "updated_offerings": summary.updated_offerings,
        "unchanged": summary.unchanged,
    });
    if dry_run {
        tx.rollback().await?;
        let mut audit_tx = pool.begin().await?;
        audit(
            &mut audit_tx,
            actor,
            "provider.discover_models",
            "provider",
            provider_id,
            &detail,
        )
        .await?;
        audit_tx.commit().await?;
    } else {
        // Persist the result for the providers list ("Last load"), so it
        // survives a page reload. Only an apply run writes this: a dry run
        // rolls the whole transaction back below.
        sqlx::query(
            "UPDATE providers SET last_discovery_at = ?, last_discovery_created = ?, \
               last_discovery_unchanged = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(summary.created_models as i64)
        .bind(summary.unchanged as i64)
        .bind(&now)
        .bind(provider_id)
        .execute(&mut *tx)
        .await?;
        audit(
            &mut tx,
            actor,
            "provider.discover_models",
            "provider",
            provider_id,
            &detail,
        )
        .await?;
        tx.commit().await?;

        // D42: after a real apply, a catalog connector with
        // `apply_on_discovery` refreshes prices. A failure is reported inside
        // the merged object and never fails the discovery itself.
        summary.prices = connectors::refresh_prices_for_discovery(
            pool,
            secrets,
            actor,
            provider_id,
            credential_id,
        )
        .await;
    }
    Ok(summary)
}

/// Serializes a modality list for `models.modalities_*`; `None` stays NULL.
fn modalities_json(modalities: Option<&[String]>) -> Option<String> {
    modalities.map(|items| {
        serde_json::Value::Array(
            items
                .iter()
                .map(|item| serde_json::Value::String(item.clone()))
                .collect(),
        )
        .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::test_support::{
        self, TestApp, api_request, send, spawn_hanging_server, spawn_static_server,
    };
    use axum::http::{Method, StatusCode};
    use serde_json::json;

    #[test]
    fn payload_shapes_are_accepted_and_ids_are_deduplicated() {
        let ids = parse_models_payload(
            r#"{"data":[{"id":"alpha"},{"id":" alpha "},{"id":"beta"},{"id":""},{"nope":1}]}"#,
        )
        .expect("openai shape");
        assert_eq!(ids, ["alpha", "beta"]);

        let ids = parse_models_payload(r#"{"models":[{"id":"gamma"}]}"#).expect("models shape");
        assert_eq!(ids, ["gamma"]);

        let ids = parse_models_payload(r#"[{"id":"delta"},"epsilon"]"#).expect("bare array");
        assert_eq!(ids, ["delta", "epsilon"]);

        let ids = parse_models_payload("[]").expect("empty is valid");
        assert!(ids.is_empty());

        let ids = parse_models_payload(r#"{"data":[{"id":"combo:shadow"},{"id":"ok"}]}"#)
            .expect("reserved ids are skipped, not fatal");
        assert_eq!(ids, ["ok"]);
    }

    #[test]
    fn the_shared_id_filter_skips_batch_but_keeps_free_variants() {
        assert!(super::super::is_discoverable_model_id("openai/gpt-4o"));
        assert!(super::super::is_discoverable_model_id("openai/gpt-4o:free"));
        assert!(!super::super::is_discoverable_model_id(
            "openai/gpt-4o:batch"
        ));
        assert!(!super::super::is_discoverable_model_id(
            "openai/gpt-4o:BATCH"
        ));
        assert!(!super::super::is_discoverable_model_id("combo:shadow"));
        assert!(!super::super::is_discoverable_model_id("   "));
        assert!(!super::super::is_discoverable_model_id(&"x".repeat(201)));

        // The parser applies the same filter discovery uses.
        let ids = parse_models_payload(
            r#"{"data":[{"id":"a/base"},{"id":"a/base:batch"},{"id":"a/base:free"}]}"#,
        )
        .expect("parse");
        assert_eq!(ids, ["a/base", "a/base:free"]);
    }

    #[test]
    fn malformed_payloads_are_rejected() {
        for raw in [
            "not json",
            r#"{"nope":[]}"#,
            "42",
            r#"{"data":[{"nope":1},""]}"#,
        ] {
            let error = parse_models_payload(raw).expect_err("must reject");
            assert!(
                matches!(error, DiscoveryError::Malformed(_)),
                "{raw}: {error}"
            );
        }
    }

    struct Seeded {
        app: TestApp,
        router: axum::Router,
        cookie: String,
        csrf: String,
        provider_id: String,
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
                Some(json!({ "name": "acme", "base_url": base_url })),
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
                Some(json!({ "label": "primary", "secret": "sk-live-secret",
                             "headers": { "x-tenant": "team-a" } })),
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
        }
    }

    async fn discover(s: &Seeded, body: serde_json::Value) -> crate::test_support::TestResponse {
        send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{}/discover-models", s.provider_id),
                Some(body),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await
    }

    async fn counts(s: &Seeded) -> (i64, i64) {
        let models: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM models")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("model count");
        let offerings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM offerings")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("offering count");
        (models, offerings)
    }

    #[tokio::test]
    async fn discovery_round_trip_is_audited_idempotent_and_preserves_manual_work() {
        let (addr, seen) = spawn_static_server(
            "200 OK",
            r#"{"data":[{"id":"alpha"},{"id":"beta"},{"id":"alpha"}]}"#,
        )
        .await;
        let s = seeded(&format!("http://{addr}/v1")).await;

        let first = discover(&s, json!({})).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        assert_eq!(first.body["discovered"], 2);
        assert_eq!(first.body["created_models"], 2);
        assert_eq!(first.body["created_offerings"], 2);
        assert_eq!(first.body["unchanged"], 0);
        assert_eq!(first.body["dry_run"], false);
        assert!(first.body["credential_id"].as_str().is_some());
        assert_eq!(counts(&s).await, (2, 2));

        // Discovery authenticated with the sealed header and the secret.
        let request = seen.lock().unwrap_or_else(|e| e.into_inner())[0].to_ascii_lowercase();
        assert!(
            request.contains("authorization: bearer sk-live-secret"),
            "{request}"
        );
        assert!(request.contains("x-tenant: team-a"), "{request}");
        assert!(request.starts_with("get /v1/models"), "{request}");

        // Manual work on an offering: overrides, headers, and price.
        let offering_id: String =
            sqlx::query_scalar("SELECT id FROM offerings WHERE upstream_model_id = 'alpha'")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("offering");
        let patched = send(
            s.router.clone(),
            api_request(
                Method::PATCH,
                &format!("/api/offerings/{offering_id}"),
                Some(
                    json!({ "context_override": 123456, "max_output_override": 4096,
                             "enabled": false, "headers": { "x-route": "fast" } }),
                ),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
        let priced = send(
            s.router.clone(),
            api_request(
                Method::PUT,
                &format!("/api/offerings/{offering_id}/price"),
                Some(json!({ "input_per_m": 1.5 })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(priced.status, StatusCode::OK, "{}", priced.body);

        // A later run changes nothing: same summary, same rows, manual fields
        // and the manual price intact, and the model row was not rewritten.
        let second = discover(&s, json!({})).await;
        assert_eq!(second.status, StatusCode::OK, "{}", second.body);
        assert_eq!(second.body["created_models"], 0);
        assert_eq!(second.body["created_offerings"], 0);
        assert_eq!(second.body["unchanged"], 2);
        let (offering, enabled, headers_json): (String, i64, Option<String>) =
            sqlx::query_as("SELECT id, enabled, headers_json FROM offerings WHERE id = ?")
                .bind(&offering_id)
                .fetch_one(&s.app.state.pool)
                .await
                .expect("offering row");
        assert_eq!(offering, offering_id);
        assert_eq!(enabled, 0, "admin-disabled offerings stay disabled");
        assert!(
            headers_json
                .as_deref()
                .is_some_and(|raw| raw.contains("x-route")),
            "offering headers survive: {headers_json:?}"
        );
        let overrides: (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT context_override, max_output_override FROM offerings WHERE id = ?",
        )
        .bind(&offering_id)
        .fetch_one(&s.app.state.pool)
        .await
        .expect("overrides");
        assert_eq!(overrides, (Some(123456), Some(4096)));
        let source: String = sqlx::query_scalar("SELECT source FROM prices WHERE offering_id = ?")
            .bind(&offering_id)
            .fetch_one(&s.app.state.pool)
            .await
            .expect("manual price");
        assert_eq!(source, "manual");

        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'provider.discover_models'",
        )
        .fetch_one(&s.app.state.pool)
        .await
        .expect("audit count");
        assert_eq!(audits, 2, "one audit row per run");
    }

    #[tokio::test]
    async fn a_discovered_model_does_not_overwrite_an_existing_model_row() {
        let (addr, _) = spawn_static_server("200 OK", r#"{"data":[{"id":"alpha"}]}"#).await;
        let s = seeded(&format!("http://{addr}")).await;
        let model = send(
            s.router.clone(),
            api_request(
                Method::POST,
                "/api/models",
                Some(json!({ "canonical_name": "alpha", "family": "alpha-family",
                             "context_length": 999_999 })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(model.status, StatusCode::CREATED, "{}", model.body);
        let model_id = model.body["id"].as_str().expect("model id").to_string();

        let result = discover(&s, json!({})).await;
        assert_eq!(result.status, StatusCode::OK, "{}", result.body);
        assert_eq!(result.body["created_models"], 0);
        assert_eq!(result.body["created_offerings"], 1);
        let row: (String, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT canonical_name, family, context_length FROM models WHERE id = ?",
        )
        .bind(&model_id)
        .fetch_one(&s.app.state.pool)
        .await
        .expect("model row");
        assert_eq!(
            row,
            (
                "alpha".to_string(),
                Some("alpha-family".to_string()),
                Some(999_999)
            )
        );
    }

    #[tokio::test]
    async fn a_case_only_name_reuses_the_existing_model_and_keeps_its_spelling() {
        let (addr, _) = spawn_static_server("200 OK", r#"{"data":[{"id":"vendor/Model"}]}"#).await;
        let s = seeded(&format!("http://{addr}")).await;
        let existing = send(
            s.router.clone(),
            api_request(
                Method::POST,
                "/api/models",
                Some(json!({ "canonical_name": "vendor/model", "family": "vendor" })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(existing.status, StatusCode::CREATED, "{}", existing.body);
        let existing_id = existing.body["id"].as_str().expect("model id").to_string();

        let result = discover(&s, json!({})).await;
        assert_eq!(result.status, StatusCode::OK, "{}", result.body);
        assert_eq!(
            result.body["created_models"], 0,
            "the case-only duplicate is not a new model: {}",
            result.body
        );
        assert_eq!(result.body["created_offerings"], 1);
        assert_eq!(counts(&s).await, (1, 1));

        // The stored spelling belongs to the existing row and is untouched.
        let (id, name): (String, String) = sqlx::query_as("SELECT id, canonical_name FROM models")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("model row");
        assert_eq!(id, existing_id);
        assert_eq!(name, "vendor/model");
        let linked: String = sqlx::query_scalar(
            "SELECT model_id FROM offerings WHERE upstream_model_id = 'vendor/Model'",
        )
        .fetch_one(&s.app.state.pool)
        .await
        .expect("offering link");
        assert_eq!(linked, existing_id, "the offering points at the one model");
    }

    #[tokio::test]
    async fn last_discovery_is_persisted_on_apply_only() {
        let (addr, _) = spawn_static_server("200 OK", r#"{"data":[{"id":"alpha"}]}"#).await;
        let s = seeded(&format!("http://{addr}")).await;

        // Nothing yet.
        let providers = send(
            s.router.clone(),
            api_request(Method::GET, "/api/providers", None, Some(&s.cookie), None),
        )
        .await;
        assert_eq!(providers.status, StatusCode::OK, "{}", providers.body);
        assert!(providers.body[0]["last_discovery_at"].is_null());

        // A dry run writes nothing, not even the summary.
        let preview = discover(&s, json!({ "dry_run": true })).await;
        assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
        let after_preview = send(
            s.router.clone(),
            api_request(Method::GET, "/api/providers", None, Some(&s.cookie), None),
        )
        .await;
        assert!(after_preview.body[0]["last_discovery_at"].is_null());

        // An apply persists the timestamp and the counts on the provider, so a
        // page reload renders them instead of "never".
        let applied = discover(&s, json!({})).await;
        assert_eq!(applied.status, StatusCode::OK, "{}", applied.body);
        let reloaded = send(
            s.router.clone(),
            api_request(Method::GET, "/api/providers", None, Some(&s.cookie), None),
        )
        .await;
        let provider = &reloaded.body[0];
        assert!(provider["last_discovery_at"].as_str().is_some());
        assert_eq!(provider["last_discovery_created"], 1);
        assert_eq!(provider["last_discovery_unchanged"], 0);
        let stored: Option<String> = sqlx::query_scalar("SELECT last_discovery_at FROM providers")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("stored timestamp");
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn dry_run_reports_counts_writes_no_catalog_rows_but_audits_the_flag() {
        let (addr, _) =
            spawn_static_server("200 OK", r#"{"models":[{"id":"one"},{"id":"two"}]}"#).await;
        let s = seeded(&format!("http://{addr}")).await;

        let result = discover(&s, json!({ "dry_run": true })).await;
        assert_eq!(result.status, StatusCode::OK, "{}", result.body);
        assert_eq!(result.body["dry_run"], true);
        assert_eq!(result.body["discovered"], 2);
        assert_eq!(result.body["created_models"], 2);
        assert_eq!(result.body["created_offerings"], 2);
        assert_eq!(counts(&s).await, (0, 0), "a dry run writes no catalog rows");

        let detail: String = sqlx::query_scalar(
            "SELECT detail_json FROM audit_log WHERE action = 'provider.discover_models'",
        )
        .fetch_one(&s.app.state.pool)
        .await
        .expect("audit detail");
        assert!(detail.contains("\"dry_run\":true"), "{detail}");
        assert!(detail.contains("\"created_offerings\":2"), "{detail}");
    }

    #[tokio::test]
    async fn upstream_auth_failures_are_502_and_write_nothing() {
        for (status_line, status) in [("401 Unauthorized", 401), ("403 Forbidden", 403)] {
            let (addr, _) = spawn_static_server(status_line, r#"{"error":"nope"}"#).await;
            let s = seeded(&format!("http://{addr}")).await;
            let result = discover(&s, json!({})).await;
            assert_eq!(result.status, StatusCode::BAD_GATEWAY, "{}", result.body);
            assert_eq!(result.body["error"]["code"], "upstream_error");
            assert!(
                result.body["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains(&status.to_string())),
                "{}",
                result.body
            );
            assert_eq!(counts(&s).await, (0, 0));
            let audits: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'provider.discover_models'",
            )
            .fetch_one(&s.app.state.pool)
            .await
            .expect("audit count");
            assert_eq!(audits, 0, "a failed run is not audited as a discovery");
        }
    }

    #[tokio::test]
    async fn malformed_payload_is_502_and_writes_nothing() {
        let (addr, _) = spawn_static_server("200 OK", r#"{"nope":[{"id":"alpha"}]}"#).await;
        let s = seeded(&format!("http://{addr}")).await;
        let result = discover(&s, json!({})).await;
        assert_eq!(result.status, StatusCode::BAD_GATEWAY, "{}", result.body);
        assert_eq!(result.body["error"]["code"], "upstream_error");
        assert_eq!(counts(&s).await, (0, 0));
    }

    #[tokio::test]
    async fn a_silent_provider_times_out_without_writing() {
        let addr = spawn_hanging_server().await;
        let s = seeded(&format!("http://{addr}")).await;
        let credential_id: String =
            sqlx::query_scalar("SELECT id FROM provider_credentials LIMIT 1")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("credential");
        let actor = Actor {
            user_id: "admin",
            ip: "127.0.0.1",
        };
        let error = discover_models_with_timeout(
            &s.app.state.pool,
            &s.app.state.secrets,
            &actor,
            &s.provider_id,
            &DiscoveryInput {
                credential_id: Some(credential_id),
                dry_run: None,
            },
            Duration::from_millis(100),
        )
        .await
        .expect_err("silent provider");
        assert!(matches!(error, DiscoveryError::Upstream(_)), "{error}");
        assert_eq!(counts(&s).await, (0, 0));
    }

    #[tokio::test]
    async fn an_explicit_credential_must_belong_to_the_provider() {
        let (addr, _) = spawn_static_server("200 OK", r#"{"data":[]}"#).await;
        let s = seeded(&format!("http://{addr}")).await;
        let other = send(
            s.router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "other", "base_url": "https://other.example/v1" })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        let other_id = other.body["id"].as_str().expect("other id");
        let other_credential = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{other_id}/credentials"),
                Some(json!({ "label": "other", "secret": "sk-other" })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        let foreign = other_credential.body["id"].as_str().expect("credential id");

        let result = discover(&s, json!({ "credential_id": foreign })).await;
        assert_eq!(result.status, StatusCode::NOT_FOUND, "{}", result.body);
        assert_eq!(result.body["error"]["code"], "not_found");
        assert_eq!(counts(&s).await, (0, 0));
    }

    #[tokio::test]
    async fn provider_spellings_with_one_identity_share_a_model() {
        // Two providers publish the same weights under their own spelling;
        // discovery must create one model with two offerings, not two models.
        let (addr_a, _) = spawn_static_server(
            "200 OK",
            r#"{"data":[{"id":"deepseek-ai/DeepSeek-V4.1-Flash-1017"}]}"#,
        )
        .await;
        let s = seeded(&format!("http://{addr_a}")).await;
        let first = discover(&s, json!({})).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        assert_eq!(first.body["created_models"], 1);

        let (addr_b, _) = spawn_static_server(
            "200 OK",
            r#"{"data":[{"id":"deepseek/deepseek-v4.1-flash-1017"}]}"#,
        )
        .await;
        let created = send(
            s.router.clone(),
            api_request(
                Method::POST,
                "/api/providers",
                Some(json!({ "name": "other", "base_url": format!("http://{addr_b}") })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let other_id = created.body["id"].as_str().expect("provider id");
        let credential = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{other_id}/credentials"),
                Some(json!({ "label": "primary", "secret": "sk-other" })),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(
            credential.status,
            StatusCode::CREATED,
            "{}",
            credential.body
        );

        let second = send(
            s.router.clone(),
            api_request(
                Method::POST,
                &format!("/api/providers/{other_id}/discover-models"),
                Some(json!({})),
                Some(&s.cookie),
                Some(&s.csrf),
            ),
        )
        .await;
        assert_eq!(second.status, StatusCode::OK, "{}", second.body);
        assert_eq!(
            second.body["created_models"], 0,
            "the identity joins the existing model: {}",
            second.body
        );
        assert_eq!(second.body["created_offerings"], 1);
        assert_eq!(counts(&s).await, (1, 2));
        let canonical: String = sqlx::query_scalar("SELECT canonical_name FROM models")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("model");
        assert_eq!(
            canonical, "deepseek-ai/DeepSeek-V4.1-Flash-1017",
            "the first spelling stays"
        );
    }

    #[tokio::test]
    async fn an_ambiguous_identity_creates_a_new_model_instead_of_guessing() {
        let (addr, _) =
            spawn_static_server("200 OK", r#"{"data":[{"id":"vendor/family-7b"}]}"#).await;
        let s = seeded(&format!("http://{addr}")).await;
        // Two rows the operator keeps apart already share the identity.
        for (id, name) in [("keep-a", "alpha/family-7b"), ("keep-b", "beta/family-7b")] {
            sqlx::query(
                "INSERT INTO models (id, canonical_name, created_at, updated_at) \
                 VALUES (?, ?, '2026-11-01T00:00:00.000Z', '2026-11-01T00:00:00.000Z')",
            )
            .bind(id)
            .bind(name)
            .execute(&s.app.state.pool)
            .await
            .expect("model");
        }

        let result = discover(&s, json!({})).await;
        assert_eq!(result.status, StatusCode::OK, "{}", result.body);
        assert_eq!(
            result.body["created_models"], 1,
            "several candidates is never a guess: {}",
            result.body
        );
        assert_eq!(counts(&s).await, (3, 1));
    }

    #[tokio::test]
    async fn an_ambiguous_discovery_keeps_an_existing_offering_and_creates_no_model() {
        let (addr, _) =
            spawn_static_server("200 OK", r#"{"data":[{"id":"vendor/family-7b"}]}"#).await;
        let s = seeded(&format!("http://{addr}")).await;
        let now = "2026-11-01T00:00:00.000Z";
        for (id, name) in [("keep-a", "alpha/family-7b"), ("keep-b", "beta/family-7b")] {
            sqlx::query(
                "INSERT INTO models (id, canonical_name, created_at, updated_at) VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(name)
            .bind(now)
            .bind(now)
            .execute(&s.app.state.pool)
            .await
            .expect("model");
        }
        sqlx::query(
            "INSERT INTO offerings \
               (id, provider_id, model_id, upstream_model_id, enabled, created_at, updated_at) \
             VALUES ('off-x', ?, 'keep-a', 'vendor/family-7b', 1, ?, ?)",
        )
        .bind(&s.provider_id)
        .bind(now)
        .bind(now)
        .execute(&s.app.state.pool)
        .await
        .expect("offering");

        let result = discover(&s, json!({})).await;
        assert_eq!(result.status, StatusCode::OK, "{}", result.body);
        assert_eq!(
            result.body["created_models"], 0,
            "an existing offering blocks the guess: {}",
            result.body
        );
        assert_eq!(result.body["unchanged"], 1);
        assert_eq!(counts(&s).await, (2, 1));
        let linked: String =
            sqlx::query_scalar("SELECT model_id FROM offerings WHERE id = 'off-x'")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("offering");
        assert_eq!(linked, "keep-a", "the offering is not repointed");
    }

    #[tokio::test]
    async fn the_external_registry_joins_spellings_that_do_not_normalize_alike() {
        let (addr, _) =
            spawn_static_server("200 OK", r#"{"data":[{"id":"vendor/new-id-9x"}]}"#).await;
        let s = seeded(&format!("http://{addr}")).await;
        let now = "2026-11-01T00:00:00.000Z";
        // A model discovered earlier under a spelling with no shared tail.
        sqlx::query(
            "INSERT INTO models (id, canonical_name, created_at, updated_at) \
             VALUES ('keeper', 'sail/old-spelling-7b', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&s.app.state.pool)
        .await
        .expect("model");
        // The registry says both spellings are one model.
        sqlx::query(
            "INSERT INTO model_registry (source, alias, canonical, fetched_at) VALUES \
             ('models_dev', 'sail/old-spelling-7b', 'canonical-key', ?), \
             ('models_dev', 'vendor/new-id-9x', 'canonical-key', ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&s.app.state.pool)
        .await
        .expect("registry");

        let result = discover(&s, json!({})).await;
        assert_eq!(result.status, StatusCode::OK, "{}", result.body);
        assert_eq!(
            result.body["created_models"], 0,
            "the registry joins the existing row: {}",
            result.body
        );
        assert_eq!(result.body["created_offerings"], 1);
        let linked: String =
            sqlx::query_scalar("SELECT model_id FROM offerings WHERE upstream_model_id = ?")
                .bind("vendor/new-id-9x")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("offering");
        assert_eq!(linked, "keeper");
    }

    #[tokio::test]
    async fn a_provider_without_credentials_is_a_clear_400() {
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
                Some(json!({ "name": "bare", "base_url": "https://bare.example/v1" })),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        let provider_id = created.body["id"].as_str().expect("provider id");
        let result = send(
            router,
            api_request(
                Method::POST,
                &format!("/api/providers/{provider_id}/discover-models"),
                Some(json!({})),
                Some(&cookie),
                Some(&csrf),
            ),
        )
        .await;
        assert_eq!(result.status, StatusCode::BAD_REQUEST, "{}", result.body);
        assert!(
            result.body["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("no credentials")),
            "{}",
            result.body
        );
    }
}
