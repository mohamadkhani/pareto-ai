//! Provider catalog domain: providers, credentials, models, offerings, and
//! prices (milestone M2).
//!
//! Business logic lives here, not in the HTTP handlers: every mutation runs in
//! one transaction that also writes exactly one `audit_log` row, so "who
//! changed what" can never be lost between the change and its audit entry.
//! Secrets and custom header values are sealed with [`crate::secrets`] before
//! they reach a row (D16, D17), and no type that can serialize a secret or a
//! header value exists in this module's response path (see [`types`]).
//!
//! Prices follow decision D10: one `prices` row per offering, an unknown
//! component is `NULL` (never 0), and a `source = 'manual'` row is never
//! overwritten by a sync.

pub(crate) mod headers;
pub(crate) mod types;

pub use types::*;

use serde_json::json;

use crate::auth;
use crate::probe::ProbeResult;
use crate::secrets::SecretError;

/// The admin performing a mutation; stamped onto every audit row.
#[derive(Debug, Clone, Copy)]
pub struct Actor<'a> {
    pub user_id: &'a str,
    pub ip: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit write failed: {0}")]
    Audit(#[from] auth::AuthError),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("secret material could not be processed: {0}")]
    Secret(#[from] SecretError),
}

impl CatalogError {
    /// Maps the domain error onto the uniform API error shape: `404` for
    /// missing entities, `409` for uniqueness conflicts, `400` for invalid
    /// input, and a bare `500` (with the detail logged) for the rest.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        use crate::error::ApiError;
        match self {
            CatalogError::NotFound(what) => ApiError::not_found(what).with_request_id(rid),
            CatalogError::Conflict(message) => ApiError::conflict(message).with_request_id(rid),
            CatalogError::Invalid(message) => ApiError::bad_request(message).with_request_id(rid),
            other => ApiError::internal("catalog", other).with_request_id(rid),
        }
    }
}

/// A price as applied by a sync: USD per 1M tokens plus a flat per-request
/// fee. Unknown components are `None` and stay `NULL`.
#[derive(Debug, Clone, Default)]
pub struct SyncPrice {
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub request_fee: Option<f64>,
}

/// What [`apply_sync_price`] did with one offering's price row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPriceOutcome {
    /// A row was inserted, or its values or provenance changed.
    Changed,
    /// The row already matched the sync's price; only the fetch metadata
    /// (`fetched_at`, `updated_at`) moved.
    Unchanged,
    /// A `source = 'manual'` row was left untouched (D10).
    SkippedManual,
}

const SELECT_PROVIDER_COLUMNS: &str = "id, name, kind, base_url, enabled, created_at, updated_at";
const SELECT_CREDENTIAL_COLUMNS: &str = "id, provider_id, label, secret_cipher, headers_json, \
     max_concurrency, balance_usd, balance_source, balance_updated_at, balance_updated_by, \
     balance_low_threshold, status, cooldown_until, last_tested_at, created_at, updated_at";
const SELECT_MODEL_COLUMNS: &str = "id, canonical_name, family, modalities_input, \
     modalities_output, context_length, max_output_tokens, capabilities_json, created_at, updated_at";
const SELECT_OFFERING_COLUMNS: &str = "id, provider_id, model_id, upstream_model_id, \
     context_override, max_output_override, enabled, headers_json, created_at, updated_at";
const SELECT_PRICE_COLUMNS: &str = "offering_id, input_per_m, output_per_m, cache_read_per_m, \
     cache_write_per_m, reasoning_per_m, request_fee, currency, source, source_url, fetched_at, \
     updated_by, created_at, updated_at";

/// UTC timestamp in the same shape SQLite writes.
pub(crate) fn now_ts() -> String {
    chrono::Utc::now().format(auth::TS_FMT).to_string()
}

/// Writes one `audit_log` row inside the mutation's transaction. Visible to
/// the pricing sync so its run and its audit row commit together.
pub(crate) async fn audit(
    conn: &mut sqlx::SqliteConnection,
    actor: &Actor<'_>,
    action: &str,
    target_type: &str,
    target_id: &str,
    detail: &serde_json::Value,
) -> Result<(), CatalogError> {
    auth::audit(
        conn,
        Some(actor.user_id),
        action,
        Some(target_type),
        Some(target_id),
        Some(detail.to_string().as_str()),
        Some(actor.ip),
    )
    .await?;
    Ok(())
}

/// Turns a UNIQUE-constraint database failure into a 409-worthy conflict and
/// leaves every other failure untouched.
fn map_write(e: sqlx::Error, conflict: &str) -> CatalogError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            CatalogError::Conflict(conflict.to_string())
        }
        _ => CatalogError::Db(e),
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn require_name(raw: &str, field: &str) -> Result<String, CatalogError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CatalogError::Invalid(format!("{field} must not be empty")));
    }
    if trimmed.len() > 200 {
        return Err(CatalogError::Invalid(format!(
            "{field} must be at most 200 characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn validate_base_url(raw: &str) -> Result<String, CatalogError> {
    let trimmed = raw.trim();
    let url = reqwest::Url::parse(trimmed)
        .map_err(|_| CatalogError::Invalid("base_url must be a valid URL".to_string()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(CatalogError::Invalid(
            "base_url must be an http(s) URL with a host".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// Validates a credential secret: no trimming (leading/trailing whitespace is
/// key material, not padding), non-empty, capped at 8 KB. API keys, JWTs, and
/// OAuth client secrets regularly exceed name-like 200-char limits.
fn validate_secret(raw: &str) -> Result<&str, CatalogError> {
    if raw.is_empty() {
        return Err(CatalogError::Invalid(
            "secret must not be empty".to_string(),
        ));
    }
    if raw.len() > 8192 {
        return Err(CatalogError::Invalid(
            "secret must be at most 8192 bytes".to_string(),
        ));
    }
    Ok(raw)
}

/// Validates `balance_source` and returns the trimmed value for storage, so
/// `" manual"` stores `manual` instead of violating the DB CHECK. Empty or
/// absent means "keep whatever is stored".
fn validate_balance_source(raw: Option<&str>) -> Result<Option<String>, CatalogError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value @ ("manual" | "provider_api" | "unknown")) => Ok(Some(value.to_string())),
        Some(other) => Err(CatalogError::Invalid(format!(
            "balance_source must be manual, provider_api, or unknown (got {other:?})"
        ))),
    }
}

fn validate_count(value: Option<i64>, field: &str) -> Result<Option<i64>, CatalogError> {
    match value {
        Some(v) if v < 0 => Err(CatalogError::Invalid(format!(
            "{field} must not be negative"
        ))),
        other => Ok(other),
    }
}

fn validate_amount(value: Option<f64>, field: &str) -> Result<Option<f64>, CatalogError> {
    match value {
        Some(v) if !v.is_finite() => Err(CatalogError::Invalid(format!(
            "{field} must be a finite number"
        ))),
        other => Ok(other),
    }
}

fn validate_modalities(
    value: Option<Vec<String>>,
    field: &str,
) -> Result<Option<String>, CatalogError> {
    let Some(list) = value else {
        return Ok(None);
    };
    for entry in &list {
        if entry.trim().is_empty() || entry.len() > 64 {
            return Err(CatalogError::Invalid(format!(
                "{field} entries must be 1-64 characters"
            )));
        }
    }
    Ok(Some(serde_json::to_string(&list).map_err(|e| {
        CatalogError::Invalid(format!("{field} could not be stored: {e}"))
    })?))
}

fn validate_capabilities(value: Option<serde_json::Value>) -> Result<Option<String>, CatalogError> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) if v.is_object() || v.is_array() => Ok(Some(v.to_string())),
        Some(_) => Err(CatalogError::Invalid(
            "capabilities must be a JSON object or array".to_string(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

/// Lists all providers, newest first.
pub async fn list_providers(pool: &sqlx::SqlitePool) -> Result<Vec<ProviderRow>, CatalogError> {
    let rows = sqlx::query_as::<_, ProviderRow>(&format!(
        "SELECT {SELECT_PROVIDER_COLUMNS} FROM providers ORDER BY name"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Fetches one provider by id, or `None` when it does not exist.
pub async fn get_provider(
    pool: &sqlx::SqlitePool,
    id: &str,
) -> Result<Option<ProviderRow>, CatalogError> {
    let row = sqlx::query_as::<_, ProviderRow>(&format!(
        "SELECT {SELECT_PROVIDER_COLUMNS} FROM providers WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Creates a provider and audits it as `provider.create`.
///
/// Fails with [`CatalogError::Conflict`] when the name is taken and with
/// [`CatalogError::Invalid`] for a malformed name or base URL.
pub async fn create_provider(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    input: &ProviderInput,
) -> Result<ProviderRow, CatalogError> {
    let name = require_name(&input.name, "name")?;
    let base_url = validate_base_url(&input.base_url)?;
    match input.kind.as_deref() {
        None | Some("openai_compatible") => {}
        Some(other) => {
            return Err(CatalogError::Invalid(format!(
                "kind {other:?} is not supported; only openai_compatible exists"
            )));
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ts();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, enabled, created_at, updated_at) \
         VALUES (?, ?, 'openai_compatible', ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&base_url)
    .bind(i64::from(input.enabled.unwrap_or(true)))
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_write(e, "a provider with this name already exists"))?;

    audit(
        &mut tx,
        actor,
        "provider.create",
        "provider",
        &id,
        &json!({
            "name": name,
            "base_url": base_url,
            "enabled": input.enabled.unwrap_or(true),
        }),
    )
    .await?;
    tx.commit().await?;

    get_provider(pool, &id)
        .await?
        .ok_or(CatalogError::NotFound("provider"))
}

/// Applies a partial provider update and audits it as `provider.update`.
///
/// An empty patch is rejected; unset fields keep their values (a field cannot
/// be cleared through PATCH). Fails with `NotFound`/`Conflict` as appropriate.
pub async fn update_provider(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    id: &str,
    patch: &ProviderPatch,
) -> Result<ProviderRow, CatalogError> {
    if patch.name.is_none() && patch.base_url.is_none() && patch.enabled.is_none() {
        return Err(CatalogError::Invalid(
            "no fields to update; send at least one of name, base_url, enabled".to_string(),
        ));
    }
    let name = patch
        .name
        .as_deref()
        .map(|v| require_name(v, "name"))
        .transpose()?;
    let base_url = patch
        .base_url
        .as_deref()
        .map(validate_base_url)
        .transpose()?;

    let now = now_ts();
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "UPDATE providers SET \
           name = COALESCE(?, name), \
           base_url = COALESCE(?, base_url), \
           enabled = COALESCE(?, enabled), \
           updated_at = ? \
         WHERE id = ?",
    )
    .bind(&name)
    .bind(&base_url)
    .bind(patch.enabled.map(i64::from))
    .bind(&now)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_write(e, "a provider with this name already exists"))?;
    if updated.rows_affected() == 0 {
        return Err(CatalogError::NotFound("provider"));
    }

    audit(
        &mut tx,
        actor,
        "provider.update",
        "provider",
        id,
        &json!({ "name": name, "base_url": base_url, "enabled": patch.enabled }),
    )
    .await?;
    tx.commit().await?;

    get_provider(pool, id)
        .await?
        .ok_or(CatalogError::NotFound("provider"))
}

/// Deletes a provider and everything attached to it (credentials, offerings,
/// and their prices cascade in the database), audited as `provider.delete`
/// with the cascaded counts.
pub async fn delete_provider(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    id: &str,
) -> Result<(), CatalogError> {
    let mut tx = pool.begin().await?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM providers WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(name) = name else {
        return Err(CatalogError::NotFound("provider"));
    };

    let credentials: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM provider_credentials WHERE provider_id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let offerings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM offerings WHERE provider_id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let prices: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM prices p JOIN offerings o ON o.id = p.offering_id \
         WHERE o.provider_id = ?",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM providers WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        actor,
        "provider.delete",
        "provider",
        id,
        &json!({
            "name": name,
            "cascaded_credentials": credentials,
            "cascaded_offerings": offerings,
            "cascaded_prices": prices,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// Lists a provider's credentials.
pub async fn list_credentials(
    pool: &sqlx::SqlitePool,
    provider_id: &str,
) -> Result<Vec<CredentialRow>, CatalogError> {
    let rows = sqlx::query_as::<_, CredentialRow>(&format!(
        "SELECT {SELECT_CREDENTIAL_COLUMNS} FROM provider_credentials \
         WHERE provider_id = ? ORDER BY created_at, id"
    ))
    .bind(provider_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Fetches one credential by id, or `None`.
pub async fn get_credential(
    pool: &sqlx::SqlitePool,
    id: &str,
) -> Result<Option<CredentialRow>, CatalogError> {
    let row = sqlx::query_as::<_, CredentialRow>(&format!(
        "SELECT {SELECT_CREDENTIAL_COLUMNS} FROM provider_credentials WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Creates a credential for a provider, sealing the secret and the custom
/// header values with AES-256-GCM under the row id as associated data (D16,
/// D17), audited as `credential.create`.
///
/// The audit detail carries the label and header *names* only; the secret and
/// header values never leave this function.
pub async fn create_credential(
    pool: &sqlx::SqlitePool,
    secrets: &crate::secrets::SecretBox,
    actor: &Actor<'_>,
    provider_id: &str,
    input: &CredentialInput,
) -> Result<CredentialRow, CatalogError> {
    let label = require_name(&input.label, "label")?;
    let secret = validate_secret(&input.secret)?;
    let normalized_headers = match input.headers {
        Some(ref raw) => Some(headers::normalize(raw)?),
        None => None,
    };
    let max_concurrency = validate_count(input.max_concurrency, "max_concurrency")?;
    let balance_usd = validate_amount(input.balance_usd, "balance_usd")?;
    let balance_low_threshold =
        validate_amount(input.balance_low_threshold, "balance_low_threshold")?;
    let balance_source = validate_balance_source(input.balance_source.as_deref())?;

    // The row id is the AEAD associated data, so generate it before sealing.
    let id = uuid::Uuid::new_v4().to_string();
    let secret_cipher = secrets.encrypt(secret, &id)?;
    let headers_json = match normalized_headers {
        Some(ref map) => Some(headers::seal(map, secrets, &id)?),
        None => None,
    };
    let now = now_ts();
    let (balance_updated_at, balance_updated_by) = balance_stamp(balance_usd, actor, &now);

    let mut tx = pool.begin().await?;
    let provider_exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM providers WHERE id = ?")
        .bind(provider_id)
        .fetch_optional(&mut *tx)
        .await?;
    if provider_exists.is_none() {
        return Err(CatalogError::NotFound("provider"));
    }

    sqlx::query(
        "INSERT INTO provider_credentials \
           (id, provider_id, label, secret_cipher, headers_json, max_concurrency, \
            balance_usd, balance_source, balance_updated_at, balance_updated_by, \
            balance_low_threshold, status, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'unknown', ?, ?)",
    )
    .bind(&id)
    .bind(provider_id)
    .bind(&label)
    .bind(&secret_cipher)
    .bind(&headers_json)
    .bind(max_concurrency)
    .bind(balance_usd)
    .bind(balance_source.as_deref().unwrap_or("manual"))
    .bind(&balance_updated_at)
    .bind(&balance_updated_by)
    .bind(balance_low_threshold)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_write(e, "this credential already exists"))?;

    audit(
        &mut tx,
        actor,
        "credential.create",
        "credential",
        &id,
        &json!({
            "provider_id": provider_id,
            "label": label,
            "header_names": headers::names(headers_json.as_deref()),
        }),
    )
    .await?;
    tx.commit().await?;

    get_credential(pool, &id)
        .await?
        .ok_or(CatalogError::NotFound("credential"))
}

/// Applies a partial credential update, audited as `credential.rotate` when a
/// new secret is supplied and `credential.update` otherwise.
///
/// A new secret is sealed under the same row id; the old ciphertext is
/// replaced, never echoed. Header input replaces the stored map wholesale
/// (sealed the same way, `{}` clears it).
pub async fn update_credential(
    pool: &sqlx::SqlitePool,
    secrets: &crate::secrets::SecretBox,
    actor: &Actor<'_>,
    id: &str,
    patch: &CredentialPatch,
) -> Result<CredentialRow, CatalogError> {
    let label = patch
        .label
        .as_deref()
        .map(|v| require_name(v, "label"))
        .transpose()?;
    let secret_cipher = patch
        .secret
        .as_deref()
        .map(validate_secret)
        .transpose()?
        .map(|secret| secrets.encrypt(secret, id))
        .transpose()?;
    let headers_json = match patch.headers {
        Some(ref raw) => Some(headers::seal(&headers::normalize(raw)?, secrets, id)?),
        None => None,
    };
    let max_concurrency = validate_count(patch.max_concurrency, "max_concurrency")?;
    let balance_usd = validate_amount(patch.balance_usd, "balance_usd")?;
    let balance_low_threshold =
        validate_amount(patch.balance_low_threshold, "balance_low_threshold")?;
    let balance_source = validate_balance_source(patch.balance_source.as_deref())?;

    if label.is_none()
        && secret_cipher.is_none()
        && headers_json.is_none()
        && max_concurrency.is_none()
        && balance_usd.is_none()
        && balance_low_threshold.is_none()
        && balance_source.is_none()
    {
        return Err(CatalogError::Invalid(
            "no fields to update; send at least one credential field".to_string(),
        ));
    }

    let now = now_ts();
    let (balance_updated_at, balance_updated_by) = balance_stamp(balance_usd, actor, &now);
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "UPDATE provider_credentials SET \
           label = COALESCE(?, label), \
           secret_cipher = COALESCE(?, secret_cipher), \
           headers_json = COALESCE(?, headers_json), \
           max_concurrency = COALESCE(?, max_concurrency), \
           balance_usd = COALESCE(?, balance_usd), \
           balance_source = COALESCE(?, balance_source), \
           balance_updated_at = COALESCE(?, balance_updated_at), \
           balance_updated_by = COALESCE(?, balance_updated_by), \
           balance_low_threshold = COALESCE(?, balance_low_threshold), \
           updated_at = ? \
         WHERE id = ?",
    )
    .bind(&label)
    .bind(&secret_cipher)
    .bind(&headers_json)
    .bind(max_concurrency)
    .bind(balance_usd)
    .bind(&balance_source)
    .bind(&balance_updated_at)
    .bind(&balance_updated_by)
    .bind(balance_low_threshold)
    .bind(&now)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(CatalogError::NotFound("credential"));
    }

    let action = if patch.secret.is_some() {
        "credential.rotate"
    } else {
        "credential.update"
    };
    audit(
        &mut tx,
        actor,
        action,
        "credential",
        id,
        &json!({
            "label": label,
            "rotated_secret": patch.secret.is_some(),
            "header_names": headers_json.as_deref().map(|raw| headers::names(Some(raw))),
            "balance_usd": balance_usd,
            "balance_source": balance_source,
        }),
    )
    .await?;
    tx.commit().await?;

    get_credential(pool, id)
        .await?
        .ok_or(CatalogError::NotFound("credential"))
}

/// Deletes a credential, audited as `credential.delete`.
pub async fn delete_credential(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    id: &str,
) -> Result<(), CatalogError> {
    let mut tx = pool.begin().await?;
    let label: Option<String> =
        sqlx::query_scalar("SELECT label FROM provider_credentials WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(label) = label else {
        return Err(CatalogError::NotFound("credential"));
    };

    sqlx::query("DELETE FROM provider_credentials WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        actor,
        "credential.delete",
        "credential",
        id,
        &json!({ "label": label }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Records a health-probe result on the credential (`status`, `last_tested_at`)
/// and audits it as `credential.test`. The audit detail carries the probe's
/// status and latency only — never headers or secret material.
pub async fn record_credential_test(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    id: &str,
    result: &ProbeResult,
) -> Result<CredentialRow, CatalogError> {
    let now = now_ts();
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "UPDATE provider_credentials SET status = ?, last_tested_at = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(result.status())
    .bind(&now)
    .bind(&now)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(CatalogError::NotFound("credential"));
    }

    audit(
        &mut tx,
        actor,
        "credential.test",
        "credential",
        id,
        &json!({
            "status": result.status(),
            "ok": result.ok,
            "http_status": result.http_status,
            "latency_ms": result.latency_ms,
        }),
    )
    .await?;
    tx.commit().await?;

    get_credential(pool, id)
        .await?
        .ok_or(CatalogError::NotFound("credential"))
}

/// Balance bookkeeping columns for a manual balance write (decision D8).
fn balance_stamp(
    balance_usd: Option<f64>,
    actor: &Actor<'_>,
    now: &str,
) -> (Option<String>, Option<String>) {
    match balance_usd {
        Some(_) => (Some(now.to_string()), Some(actor.user_id.to_string())),
        None => (None, None),
    }
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// Lists all models, ordered by canonical name.
pub async fn list_models(pool: &sqlx::SqlitePool) -> Result<Vec<ModelRow>, CatalogError> {
    let rows = sqlx::query_as::<_, ModelRow>(&format!(
        "SELECT {SELECT_MODEL_COLUMNS} FROM models ORDER BY canonical_name"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Fetches one model by id, or `None`.
pub async fn get_model(
    pool: &sqlx::SqlitePool,
    id: &str,
) -> Result<Option<ModelRow>, CatalogError> {
    let row = sqlx::query_as::<_, ModelRow>(&format!(
        "SELECT {SELECT_MODEL_COLUMNS} FROM models WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Creates a model and audits it as `model.create`.
pub async fn create_model(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    input: &ModelInput,
) -> Result<ModelRow, CatalogError> {
    let canonical_name = require_name(&input.canonical_name, "canonical_name")?;
    let family = input
        .family
        .as_deref()
        .map(|v| require_name(v, "family"))
        .transpose()?;
    let modalities_input = validate_modalities(input.modalities_input.clone(), "modalities_input")?;
    let modalities_output =
        validate_modalities(input.modalities_output.clone(), "modalities_output")?;
    let context_length = validate_count(input.context_length, "context_length")?;
    let max_output_tokens = validate_count(input.max_output_tokens, "max_output_tokens")?;
    let capabilities_json = validate_capabilities(input.capabilities.clone())?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ts();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO models \
           (id, canonical_name, family, modalities_input, modalities_output, \
            context_length, max_output_tokens, capabilities_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&canonical_name)
    .bind(&family)
    .bind(&modalities_input)
    .bind(&modalities_output)
    .bind(context_length)
    .bind(max_output_tokens)
    .bind(&capabilities_json)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_write(e, "a model with this canonical_name already exists"))?;

    audit(
        &mut tx,
        actor,
        "model.create",
        "model",
        &id,
        &json!({ "canonical_name": canonical_name, "family": family }),
    )
    .await?;
    tx.commit().await?;

    get_model(pool, &id)
        .await?
        .ok_or(CatalogError::NotFound("model"))
}

/// Applies a partial model update and audits it as `model.update`.
pub async fn update_model(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    id: &str,
    patch: &ModelPatch,
) -> Result<ModelRow, CatalogError> {
    let canonical_name = patch
        .canonical_name
        .as_deref()
        .map(|v| require_name(v, "canonical_name"))
        .transpose()?;
    let family = patch
        .family
        .as_deref()
        .map(|v| require_name(v, "family"))
        .transpose()?;
    let modalities_input = validate_modalities(patch.modalities_input.clone(), "modalities_input")?;
    let modalities_output =
        validate_modalities(patch.modalities_output.clone(), "modalities_output")?;
    let context_length = validate_count(patch.context_length, "context_length")?;
    let max_output_tokens = validate_count(patch.max_output_tokens, "max_output_tokens")?;
    let capabilities_json = validate_capabilities(patch.capabilities.clone())?;

    if canonical_name.is_none()
        && family.is_none()
        && modalities_input.is_none()
        && modalities_output.is_none()
        && context_length.is_none()
        && max_output_tokens.is_none()
        && capabilities_json.is_none()
    {
        return Err(CatalogError::Invalid(
            "no fields to update; send at least one model field".to_string(),
        ));
    }

    let now = now_ts();
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "UPDATE models SET \
           canonical_name = COALESCE(?, canonical_name), \
           family = COALESCE(?, family), \
           modalities_input = COALESCE(?, modalities_input), \
           modalities_output = COALESCE(?, modalities_output), \
           context_length = COALESCE(?, context_length), \
           max_output_tokens = COALESCE(?, max_output_tokens), \
           capabilities_json = COALESCE(?, capabilities_json), \
           updated_at = ? \
         WHERE id = ?",
    )
    .bind(&canonical_name)
    .bind(&family)
    .bind(&modalities_input)
    .bind(&modalities_output)
    .bind(context_length)
    .bind(max_output_tokens)
    .bind(&capabilities_json)
    .bind(&now)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_write(e, "a model with this canonical_name already exists"))?;
    if updated.rows_affected() == 0 {
        return Err(CatalogError::NotFound("model"));
    }

    audit(
        &mut tx,
        actor,
        "model.update",
        "model",
        id,
        &json!({
            "canonical_name": canonical_name,
            "family": family,
            "context_length": context_length,
            "max_output_tokens": max_output_tokens,
        }),
    )
    .await?;
    tx.commit().await?;

    get_model(pool, id)
        .await?
        .ok_or(CatalogError::NotFound("model"))
}

/// Deletes a model; its offerings and prices cascade away, audited as
/// `model.delete` with the cascaded counts.
pub async fn delete_model(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    id: &str,
) -> Result<(), CatalogError> {
    let mut tx = pool.begin().await?;
    let name: Option<String> = sqlx::query_scalar("SELECT canonical_name FROM models WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(name) = name else {
        return Err(CatalogError::NotFound("model"));
    };

    let offerings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM offerings WHERE model_id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let prices: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM prices p JOIN offerings o ON o.id = p.offering_id \
         WHERE o.model_id = ?",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM models WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        actor,
        "model.delete",
        "model",
        id,
        &json!({
            "canonical_name": name,
            "cascaded_offerings": offerings,
            "cascaded_prices": prices,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Offerings
// ---------------------------------------------------------------------------

/// Lists offerings, optionally filtered by provider and/or model.
pub async fn list_offerings(
    pool: &sqlx::SqlitePool,
    provider_id: Option<&str>,
    model_id: Option<&str>,
) -> Result<Vec<OfferingRow>, CatalogError> {
    let rows = sqlx::query_as::<_, OfferingRow>(&format!(
        "SELECT {SELECT_OFFERING_COLUMNS} FROM offerings \
         WHERE (? IS NULL OR provider_id = ?) AND (? IS NULL OR model_id = ?) \
         ORDER BY created_at, id"
    ))
    .bind(provider_id)
    .bind(provider_id)
    .bind(model_id)
    .bind(model_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Fetches one offering by id, or `None`.
pub async fn get_offering(
    pool: &sqlx::SqlitePool,
    id: &str,
) -> Result<Option<OfferingRow>, CatalogError> {
    let row = sqlx::query_as::<_, OfferingRow>(&format!(
        "SELECT {SELECT_OFFERING_COLUMNS} FROM offerings WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Creates an offering (provider + model + upstream model id) and audits it as
/// `offering.create`. Custom header values are sealed under the row id (D17).
/// Fails with `NotFound` for an unknown provider or model and `Conflict` when
/// the provider already offers that upstream id.
pub async fn create_offering(
    pool: &sqlx::SqlitePool,
    secrets: &crate::secrets::SecretBox,
    actor: &Actor<'_>,
    input: &OfferingInput,
) -> Result<OfferingRow, CatalogError> {
    let upstream_model_id = require_name(&input.upstream_model_id, "upstream_model_id")?;
    let context_override = validate_count(input.context_override, "context_override")?;
    let max_output_override = validate_count(input.max_output_override, "max_output_override")?;
    let normalized_headers = match input.headers {
        Some(ref raw) => Some(headers::normalize(raw)?),
        None => None,
    };

    // The row id is the AEAD associated data, so generate it before sealing.
    let id = uuid::Uuid::new_v4().to_string();
    let headers_json = match normalized_headers {
        Some(ref map) => Some(headers::seal(map, secrets, &id)?),
        None => None,
    };
    let now = now_ts();
    let mut tx = pool.begin().await?;
    let provider_exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM providers WHERE id = ?")
        .bind(&input.provider_id)
        .fetch_optional(&mut *tx)
        .await?;
    if provider_exists.is_none() {
        return Err(CatalogError::NotFound("provider"));
    }
    let model_exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM models WHERE id = ?")
        .bind(&input.model_id)
        .fetch_optional(&mut *tx)
        .await?;
    if model_exists.is_none() {
        return Err(CatalogError::NotFound("model"));
    }

    sqlx::query(
        "INSERT INTO offerings \
           (id, provider_id, model_id, upstream_model_id, context_override, \
            max_output_override, enabled, headers_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.provider_id)
    .bind(&input.model_id)
    .bind(&upstream_model_id)
    .bind(context_override)
    .bind(max_output_override)
    .bind(i64::from(input.enabled.unwrap_or(true)))
    .bind(&headers_json)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        map_write(
            e,
            "this provider already has an offering for that upstream_model_id",
        )
    })?;

    audit(
        &mut tx,
        actor,
        "offering.create",
        "offering",
        &id,
        &json!({
            "provider_id": input.provider_id,
            "model_id": input.model_id,
            "upstream_model_id": upstream_model_id,
            "header_names": headers::names(headers_json.as_deref()),
        }),
    )
    .await?;
    tx.commit().await?;

    get_offering(pool, &id)
        .await?
        .ok_or(CatalogError::NotFound("offering"))
}

/// Applies a partial offering update and audits it as `offering.update`.
/// Synced overrides and admin overrides both live in the same columns.
///
/// `provider_id` and `model_id` are an offering's identity and cannot be
/// patched: a PATCH carrying either is rejected with `Invalid` instead of
/// silently dropping it (alignment finding B3).
pub async fn update_offering(
    pool: &sqlx::SqlitePool,
    secrets: &crate::secrets::SecretBox,
    actor: &Actor<'_>,
    id: &str,
    patch: &OfferingPatch,
) -> Result<OfferingRow, CatalogError> {
    if patch.provider_id.is_some() || patch.model_id.is_some() {
        return Err(CatalogError::Invalid(
            "provider_id and model_id are fixed at creation and cannot be patched; \
             delete the offering and create a new one"
                .to_string(),
        ));
    }
    let upstream_model_id = patch
        .upstream_model_id
        .as_deref()
        .map(|v| require_name(v, "upstream_model_id"))
        .transpose()?;
    let context_override = validate_count(patch.context_override, "context_override")?;
    let max_output_override = validate_count(patch.max_output_override, "max_output_override")?;
    let headers_json = match patch.headers {
        Some(ref raw) => Some(headers::seal(&headers::normalize(raw)?, secrets, id)?),
        None => None,
    };

    if upstream_model_id.is_none()
        && context_override.is_none()
        && max_output_override.is_none()
        && patch.enabled.is_none()
        && headers_json.is_none()
    {
        return Err(CatalogError::Invalid(
            "no fields to update; send at least one offering field".to_string(),
        ));
    }

    let now = now_ts();
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "UPDATE offerings SET \
           upstream_model_id = COALESCE(?, upstream_model_id), \
           context_override = COALESCE(?, context_override), \
           max_output_override = COALESCE(?, max_output_override), \
           enabled = COALESCE(?, enabled), \
           headers_json = COALESCE(?, headers_json), \
           updated_at = ? \
         WHERE id = ?",
    )
    .bind(&upstream_model_id)
    .bind(context_override)
    .bind(max_output_override)
    .bind(patch.enabled.map(i64::from))
    .bind(&headers_json)
    .bind(&now)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        map_write(
            e,
            "this provider already has an offering for that upstream_model_id",
        )
    })?;
    if updated.rows_affected() == 0 {
        return Err(CatalogError::NotFound("offering"));
    }

    audit(
        &mut tx,
        actor,
        "offering.update",
        "offering",
        id,
        &json!({
            "upstream_model_id": upstream_model_id,
            "context_override": context_override,
            "max_output_override": max_output_override,
            "enabled": patch.enabled,
            "header_names": headers_json.as_deref().map(|raw| headers::names(Some(raw))),
        }),
    )
    .await?;
    tx.commit().await?;

    get_offering(pool, id)
        .await?
        .ok_or(CatalogError::NotFound("offering"))
}

/// Deletes an offering and its price row, audited as `offering.delete`.
pub async fn delete_offering(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    id: &str,
) -> Result<(), CatalogError> {
    let mut tx = pool.begin().await?;
    let upstream: Option<String> =
        sqlx::query_scalar("SELECT upstream_model_id FROM offerings WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(upstream) = upstream else {
        return Err(CatalogError::NotFound("offering"));
    };

    sqlx::query("DELETE FROM offerings WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        actor,
        "offering.delete",
        "offering",
        id,
        &json!({ "upstream_model_id": upstream }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Prices
// ---------------------------------------------------------------------------

/// Fetches the price row of an offering, or `None` when unpriced.
pub async fn get_price(
    pool: &sqlx::SqlitePool,
    offering_id: &str,
) -> Result<Option<PriceRow>, CatalogError> {
    let row = sqlx::query_as::<_, PriceRow>(&format!(
        "SELECT {SELECT_PRICE_COLUMNS} FROM prices WHERE offering_id = ?"
    ))
    .bind(offering_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Sets the manual price of an offering (PUT semantics: the input replaces the
/// row, and missing components become NULL — unknown is never 0), audited as
/// `price.override`. The row's `source` becomes `manual`, so no sync will ever
/// touch it again (D10). One upsert statement under the `UNIQUE (offering_id)`
/// constraint: no update-then-insert window and no duplicate rows.
pub async fn set_manual_price(
    pool: &sqlx::SqlitePool,
    actor: &Actor<'_>,
    offering_id: &str,
    input: &PriceInput,
) -> Result<PriceRow, CatalogError> {
    let input_per_m = validate_amount(input.input_per_m, "input_per_m")?;
    let output_per_m = validate_amount(input.output_per_m, "output_per_m")?;
    let cache_read_per_m = validate_amount(input.cache_read_per_m, "cache_read_per_m")?;
    let cache_write_per_m = validate_amount(input.cache_write_per_m, "cache_write_per_m")?;
    let reasoning_per_m = validate_amount(input.reasoning_per_m, "reasoning_per_m")?;
    let request_fee = validate_amount(input.request_fee, "request_fee")?;
    for (value, field) in [
        (input_per_m, "input_per_m"),
        (output_per_m, "output_per_m"),
        (cache_read_per_m, "cache_read_per_m"),
        (cache_write_per_m, "cache_write_per_m"),
        (reasoning_per_m, "reasoning_per_m"),
        (request_fee, "request_fee"),
    ] {
        if value.is_some_and(|v| v < 0.0) {
            return Err(CatalogError::Invalid(format!(
                "{field} must not be negative"
            )));
        }
    }
    let currency = match input.currency.as_deref() {
        None | Some("") => "USD".to_string(),
        Some(raw) => require_name(raw, "currency")?,
    };

    let now = now_ts();
    let mut tx = pool.begin().await?;
    let offering_exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM offerings WHERE id = ?")
        .bind(offering_id)
        .fetch_optional(&mut *tx)
        .await?;
    if offering_exists.is_none() {
        return Err(CatalogError::NotFound("offering"));
    }

    let inserted = sqlx::query(
        "INSERT INTO prices \
           (id, offering_id, input_per_m, output_per_m, cache_read_per_m, \
            cache_write_per_m, reasoning_per_m, request_fee, currency, source, \
            source_url, fetched_at, updated_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'manual', NULL, NULL, ?, ?, ?) \
         ON CONFLICT (offering_id) DO UPDATE SET \
           input_per_m = excluded.input_per_m, \
           output_per_m = excluded.output_per_m, \
           cache_read_per_m = excluded.cache_read_per_m, \
           cache_write_per_m = excluded.cache_write_per_m, \
           reasoning_per_m = excluded.reasoning_per_m, \
           request_fee = excluded.request_fee, \
           currency = excluded.currency, \
           source = 'manual', \
           source_url = NULL, \
           fetched_at = NULL, \
           updated_by = excluded.updated_by, \
           updated_at = excluded.updated_at",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(offering_id)
    .bind(input_per_m)
    .bind(output_per_m)
    .bind(cache_read_per_m)
    .bind(cache_write_per_m)
    .bind(reasoning_per_m)
    .bind(request_fee)
    .bind(&currency)
    .bind(actor.user_id)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    debug_assert_eq!(
        inserted.rows_affected(),
        1,
        "the upsert writes exactly one row"
    );

    audit(
        &mut tx,
        actor,
        "price.override",
        "offering",
        offering_id,
        &json!({
            "input_per_m": input_per_m,
            "output_per_m": output_per_m,
            "cache_read_per_m": cache_read_per_m,
            "cache_write_per_m": cache_write_per_m,
            "reasoning_per_m": reasoning_per_m,
            "request_fee": request_fee,
            "currency": currency,
        }),
    )
    .await?;
    tx.commit().await?;

    get_price(pool, offering_id)
        .await?
        .ok_or(CatalogError::NotFound("price"))
}

/// Upserts a synced price for an offering (one row per offering under the
/// `UNIQUE (offering_id)` constraint) unless the existing row is
/// `source = 'manual'` — those are never overwritten (D10).
///
/// Returns [`SyncPriceOutcome`] so the caller can keep raw source values only
/// for rows that actually changed or were skipped (D19). Unknown components
/// stay NULL. Runs inside the caller's transaction.
pub async fn apply_sync_price(
    conn: &mut sqlx::SqliteConnection,
    offering_id: &str,
    price: &SyncPrice,
    source: &str,
    source_url: &str,
    at: &str,
) -> Result<SyncPriceOutcome, CatalogError> {
    let existing: Option<PriceRow> = sqlx::query_as::<_, PriceRow>(&format!(
        "SELECT {SELECT_PRICE_COLUMNS} FROM prices WHERE offering_id = ?"
    ))
    .bind(offering_id)
    .fetch_optional(&mut *conn)
    .await?;
    if existing.as_ref().is_some_and(|row| row.source == "manual") {
        return Ok(SyncPriceOutcome::SkippedManual);
    }

    // The values the sync writes, compared with what is stored to tell a real
    // change from a repeat. Provenance counts: a source flip is a change.
    let changed = existing
        .as_ref()
        .map(|row| {
            row.input_per_m != price.input_per_m
                || row.output_per_m != price.output_per_m
                || row.cache_read_per_m != price.cache_read_per_m
                || row.cache_write_per_m != price.cache_write_per_m
                || row.request_fee != price.request_fee
                || row.currency != "USD"
                || row.source != source
                || row.source_url.as_deref() != Some(source_url)
        })
        .unwrap_or(true);

    sqlx::query(
        "INSERT INTO prices \
           (id, offering_id, input_per_m, output_per_m, cache_read_per_m, \
            cache_write_per_m, reasoning_per_m, request_fee, currency, source, \
            source_url, fetched_at, updated_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL, ?, 'USD', ?, ?, ?, NULL, ?, ?) \
         ON CONFLICT (offering_id) DO UPDATE SET \
           input_per_m = excluded.input_per_m, \
           output_per_m = excluded.output_per_m, \
           cache_read_per_m = excluded.cache_read_per_m, \
           cache_write_per_m = excluded.cache_write_per_m, \
           reasoning_per_m = NULL, \
           request_fee = excluded.request_fee, \
           currency = 'USD', \
           source = excluded.source, \
           source_url = excluded.source_url, \
           fetched_at = excluded.fetched_at, \
           updated_by = NULL, \
           updated_at = excluded.updated_at \
         WHERE prices.source != 'manual'",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(offering_id)
    .bind(price.input_per_m)
    .bind(price.output_per_m)
    .bind(price.cache_read_per_m)
    .bind(price.cache_write_per_m)
    .bind(price.request_fee)
    .bind(source)
    .bind(source_url)
    .bind(at)
    .bind(at)
    .bind(at)
    .execute(&mut *conn)
    .await?;

    Ok(if changed {
        SyncPriceOutcome::Changed
    } else {
        SyncPriceOutcome::Unchanged
    })
}

// ---------------------------------------------------------------------------
// GET /v1/models
// ---------------------------------------------------------------------------

/// A joined row of one enabled offering with its model and price.
#[derive(Debug, sqlx::FromRow)]
struct V1Row {
    upstream_model_id: String,
    context_override: Option<i64>,
    max_output_override: Option<i64>,
    canonical_name: String,
    modalities_input: Option<String>,
    modalities_output: Option<String>,
    context_length: Option<i64>,
    max_output_tokens: Option<i64>,
    capabilities_json: Option<String>,
    input_per_m: Option<f64>,
    output_per_m: Option<f64>,
    cache_read_per_m: Option<f64>,
    cache_write_per_m: Option<f64>,
    reasoning_per_m: Option<f64>,
    request_fee: Option<f64>,
    currency: Option<String>,
}

/// The enabled offerings as advertised by `GET /v1/models`: one entry per
/// enabled offering of an enabled provider, with effective (override-aware)
/// limits and the offering's price row. Unknown prices stay null.
pub async fn list_v1_models(pool: &sqlx::SqlitePool) -> Result<Vec<V1Model>, CatalogError> {
    let rows = sqlx::query_as::<_, V1Row>(
        "SELECT o.upstream_model_id, o.context_override, o.max_output_override, \
                m.canonical_name, m.modalities_input, m.modalities_output, \
                m.context_length, m.max_output_tokens, m.capabilities_json, \
                p.input_per_m, p.output_per_m, p.cache_read_per_m, p.cache_write_per_m, \
                p.reasoning_per_m, p.request_fee, p.currency \
         FROM offerings o \
         JOIN models m ON m.id = o.model_id \
         JOIN providers pr ON pr.id = o.provider_id \
         LEFT JOIN prices p ON p.offering_id = o.id \
         WHERE o.enabled = 1 AND pr.enabled = 1 \
         ORDER BY m.canonical_name, o.upstream_model_id",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|row| {
            let price = row.currency.as_deref().map(|currency| V1Price {
                input_per_m: row.input_per_m,
                output_per_m: row.output_per_m,
                cache_read_per_m: row.cache_read_per_m,
                cache_write_per_m: row.cache_write_per_m,
                reasoning_per_m: row.reasoning_per_m,
                request_fee: row.request_fee,
                currency: currency.to_string(),
            });
            V1Model {
                id: row.upstream_model_id.clone(),
                name: types::display_name(row.capabilities_json.as_deref())
                    .unwrap_or_else(|| row.canonical_name.clone()),
                context_length: row.context_override.or(row.context_length),
                max_output_tokens: row.max_output_override.or(row.max_output_tokens),
                modalities: V1Modalities {
                    input: types::parse_modalities(row.modalities_input.as_deref())
                        .unwrap_or_default(),
                    output: types::parse_modalities(row.modalities_output.as_deref())
                        .unwrap_or_default(),
                },
                price,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_validation_keeps_whitespace_and_long_keys_but_caps_at_8kb() {
        // 300 characters (well past the old 200-char name cap) is a normal
        // JWT/OAuth client secret and must be accepted.
        let long = "sk-".to_string() + &"a".repeat(297);
        assert_eq!(validate_secret(&long).expect("300-char secret"), long);

        // Whitespace is key material: it is kept verbatim, never trimmed.
        assert_eq!(
            validate_secret(" sk-live ").expect("untrimmed secret"),
            " sk-live "
        );

        // 8 KB is the cap: exactly 8192 bytes pass, one byte more is a 400.
        let at_cap = "a".repeat(8192);
        assert_eq!(validate_secret(&at_cap).expect("8 KB secret"), at_cap);
        let over_cap = "a".repeat(8193);
        let error = validate_secret(&over_cap).expect_err("8 KB + 1 must be rejected");
        assert!(error.to_string().contains("8192"), "{error}");

        assert!(
            validate_secret("").is_err(),
            "an empty secret must not be stored"
        );
    }

    #[test]
    fn balance_source_is_stored_trimmed_or_rejected() {
        // `" manual"` normalizes to `manual` instead of violating the DB CHECK.
        assert_eq!(
            validate_balance_source(Some(" manual")).expect("trimmed"),
            Some("manual".to_string())
        );
        assert_eq!(
            validate_balance_source(Some("manual")).expect("plain"),
            Some("manual".to_string())
        );
        assert_eq!(validate_balance_source(None).expect("absent"), None);
        assert_eq!(validate_balance_source(Some("   ")).expect("blank"), None);

        let error = validate_balance_source(Some("by hand")).expect_err("unknown source");
        assert!(
            error.to_string().contains("balance_source"),
            "the error names the field: {error}"
        );
    }

    #[test]
    fn amount_validation_rejects_nan_infinity_and_negative_values() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = validate_amount(Some(bad), "input_per_m")
                .expect_err("non-finite amounts must be rejected");
            assert!(error.to_string().contains("finite"), "{error}");
        }
        assert_eq!(validate_amount(Some(0.0), "x").expect("zero"), Some(0.0));
        assert_eq!(validate_amount(None, "x").expect("absent"), None);

        // Negative prices: the amount passes `validate_amount` (it is finite)
        // and is rejected where prices are validated.
        assert_eq!(
            validate_amount(Some(-1.0), "x").expect("finite"),
            Some(-1.0)
        );
    }
}
