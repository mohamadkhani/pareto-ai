//! Rows, API views, and request inputs for the provider catalog.
//!
//! Row structs are `FromRow` only — they are never `Serialize`, so a row that
//! carries a secret cipher or sealed header values cannot be turned into a
//! response by accident. Responses are built from the `*View` structs, which
//! expose `has_secret` and header *names* only (redaction lives in the
//! conversions). Every type that carries secret material or header values
//! also has a hand-written `Debug` that prints a mask and header names, so a
//! stray `tracing::debug!(?…)` can never leak them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;

use super::headers;

// ---------------------------------------------------------------------------
// Rows (database shape; never serialized)
// ---------------------------------------------------------------------------

/// Prints header input as `Some(["x-api-key"])` / `None`: the names an
/// operator configured, never the values (D17).
fn header_names_debug(
    headers: Option<&BTreeMap<String, serde_json::Value>>,
) -> impl std::fmt::Debug + '_ {
    headers.map(|map| map.keys().collect::<Vec<_>>())
}

/// Prints sealed `headers_json` as `Some(["x-api-key"])` / `None`: the plain
/// names half of the envelope, never the sealed values (D17).
fn sealed_names_debug(raw: Option<&str>) -> impl std::fmt::Debug + '_ {
    let names = headers::names(raw);
    (!names.is_empty()).then_some(names)
}

#[derive(Debug, FromRow)]
pub struct ProviderRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub enabled: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(FromRow)]
pub struct CredentialRow {
    pub id: String,
    pub provider_id: String,
    pub label: String,
    /// AEAD ciphertext (`v1:<nonce>:<ct>`); write-only by contract.
    pub secret_cipher: String,
    /// Sealed `{"names", "values"}` envelope (D17): names are plain, values
    /// are AEAD ciphertext. Write-only by contract.
    pub headers_json: Option<String>,
    pub max_concurrency: Option<i64>,
    pub balance_usd: Option<f64>,
    pub balance_source: String,
    pub balance_updated_at: Option<String>,
    pub balance_updated_by: Option<String>,
    pub balance_low_threshold: Option<f64>,
    pub status: String,
    pub cooldown_until: Option<String>,
    pub last_tested_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Redacted: prints a mask for the secret cipher and header names only.
impl std::fmt::Debug for CredentialRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialRow")
            .field("id", &self.id)
            .field("provider_id", &self.provider_id)
            .field("label", &self.label)
            .field("secret_cipher", &"********")
            .field(
                "header_names",
                &sealed_names_debug(self.headers_json.as_deref()),
            )
            .field("max_concurrency", &self.max_concurrency)
            .field("balance_usd", &self.balance_usd)
            .field("balance_source", &self.balance_source)
            .field("balance_updated_at", &self.balance_updated_at)
            .field("balance_updated_by", &self.balance_updated_by)
            .field("balance_low_threshold", &self.balance_low_threshold)
            .field("status", &self.status)
            .field("cooldown_until", &self.cooldown_until)
            .field("last_tested_at", &self.last_tested_at)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Debug, FromRow)]
pub struct ModelRow {
    pub id: String,
    pub canonical_name: String,
    pub family: Option<String>,
    pub modalities_input: Option<String>,
    pub modalities_output: Option<String>,
    pub context_length: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub capabilities_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(FromRow)]
pub struct OfferingRow {
    pub id: String,
    pub provider_id: String,
    pub model_id: String,
    pub upstream_model_id: String,
    pub context_override: Option<i64>,
    pub max_output_override: Option<i64>,
    pub enabled: i64,
    /// Sealed `{"names", "values"}` envelope (D17): names are plain, values
    /// are AEAD ciphertext. Write-only by contract.
    pub headers_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Redacted: prints header names only, never the sealed values.
impl std::fmt::Debug for OfferingRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfferingRow")
            .field("id", &self.id)
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .field("upstream_model_id", &self.upstream_model_id)
            .field("context_override", &self.context_override)
            .field("max_output_override", &self.max_output_override)
            .field("enabled", &self.enabled)
            .field(
                "header_names",
                &sealed_names_debug(self.headers_json.as_deref()),
            )
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Debug, FromRow)]
pub struct PriceRow {
    pub offering_id: String,
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub reasoning_per_m: Option<f64>,
    pub request_fee: Option<f64>,
    pub currency: String,
    pub source: String,
    pub source_url: Option<String>,
    pub fetched_at: Option<String>,
    pub updated_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Views (responses; secret-free by construction)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    /// Currently always `openai_compatible`.
    pub kind: String,
    pub base_url: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&ProviderRow> for ProviderView {
    fn from(row: &ProviderRow) -> Self {
        Self {
            id: row.id.clone(),
            name: row.name.clone(),
            kind: row.kind.clone(),
            base_url: row.base_url.clone(),
            enabled: row.enabled != 0,
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CredentialView {
    pub id: String,
    pub provider_id: String,
    pub label: String,
    /// True when a secret is stored. The secret itself is write-only.
    pub has_secret: bool,
    /// Fixed mask shown instead of the secret; it reveals no secret bytes.
    pub secret_masked: String,
    /// Custom header names only; values are never returned or logged.
    pub header_names: Vec<String>,
    pub max_concurrency: Option<i64>,
    pub balance_usd: Option<f64>,
    /// `manual`, `provider_api`, or `unknown`.
    pub balance_source: String,
    pub balance_updated_at: Option<String>,
    pub balance_updated_by: Option<String>,
    pub balance_low_threshold: Option<f64>,
    /// Last health-probe result: `unknown`, `ok`, or `error`.
    pub status: String,
    pub cooldown_until: Option<String>,
    pub last_tested_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&CredentialRow> for CredentialView {
    fn from(row: &CredentialRow) -> Self {
        Self {
            id: row.id.clone(),
            provider_id: row.provider_id.clone(),
            label: row.label.clone(),
            has_secret: !row.secret_cipher.is_empty(),
            secret_masked: "********".to_string(),
            header_names: headers::names(row.headers_json.as_deref()),
            max_concurrency: row.max_concurrency,
            balance_usd: row.balance_usd,
            balance_source: row.balance_source.clone(),
            balance_updated_at: row.balance_updated_at.clone(),
            balance_updated_by: row.balance_updated_by.clone(),
            balance_low_threshold: row.balance_low_threshold,
            status: row.status.clone(),
            cooldown_until: row.cooldown_until.clone(),
            last_tested_at: row.last_tested_at.clone(),
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ModelView {
    pub id: String,
    pub canonical_name: String,
    pub family: Option<String>,
    pub modalities_input: Option<Vec<String>>,
    pub modalities_output: Option<Vec<String>>,
    pub context_length: Option<i64>,
    pub max_output_tokens: Option<i64>,
    /// Free-form capabilities JSON; synced models keep their display name
    /// under `display_name` (the spec schema has no display-name column).
    pub capabilities: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&ModelRow> for ModelView {
    fn from(row: &ModelRow) -> Self {
        Self {
            id: row.id.clone(),
            canonical_name: row.canonical_name.clone(),
            family: row.family.clone(),
            modalities_input: parse_modalities(row.modalities_input.as_deref()),
            modalities_output: parse_modalities(row.modalities_output.as_deref()),
            context_length: row.context_length,
            max_output_tokens: row.max_output_tokens,
            capabilities: row
                .capabilities_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OfferingView {
    pub id: String,
    pub provider_id: String,
    pub model_id: String,
    /// The model id sent upstream, as the client names it.
    pub upstream_model_id: String,
    pub context_override: Option<i64>,
    pub max_output_override: Option<i64>,
    pub enabled: bool,
    /// Custom header names only; values are never returned or logged.
    pub header_names: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&OfferingRow> for OfferingView {
    fn from(row: &OfferingRow) -> Self {
        Self {
            id: row.id.clone(),
            provider_id: row.provider_id.clone(),
            model_id: row.model_id.clone(),
            upstream_model_id: row.upstream_model_id.clone(),
            context_override: row.context_override,
            max_output_override: row.max_output_override,
            enabled: row.enabled != 0,
            header_names: headers::names(row.headers_json.as_deref()),
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PriceView {
    pub offering_id: String,
    /// USD per 1M tokens; unknown components stay null and are never 0.
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub reasoning_per_m: Option<f64>,
    /// Flat USD fee per request, when the source prices per request.
    pub request_fee: Option<f64>,
    pub currency: String,
    /// `manual`, `openrouter`, `models_dev`, or `custom_api`.
    pub source: String,
    pub source_url: Option<String>,
    pub fetched_at: Option<String>,
    pub updated_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&PriceRow> for PriceView {
    fn from(row: &PriceRow) -> Self {
        Self {
            offering_id: row.offering_id.clone(),
            input_per_m: row.input_per_m,
            output_per_m: row.output_per_m,
            cache_read_per_m: row.cache_read_per_m,
            cache_write_per_m: row.cache_write_per_m,
            reasoning_per_m: row.reasoning_per_m,
            request_fee: row.request_fee,
            currency: row.currency.clone(),
            source: row.source.clone(),
            source_url: row.source_url.clone(),
            fetched_at: row.fetched_at.clone(),
            updated_by: row.updated_by.clone(),
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
        }
    }
}

/// One enabled offering as advertised by `GET /v1/models`.
#[derive(Debug, Serialize, ToSchema)]
pub struct V1Model {
    /// The upstream model id a client sends as `model`.
    pub id: String,
    /// Display name (or the canonical name when the model has none).
    pub name: String,
    /// Effective context: the offering's override wins over the model value.
    pub context_length: Option<i64>,
    /// Effective max output: the offering's override wins over the model value.
    pub max_output_tokens: Option<i64>,
    pub modalities: V1Modalities,
    /// Absent when the offering has no priced row yet; unknown components
    /// inside are null and never 0.
    pub price: Option<V1Price>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct V1Modalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct V1Price {
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub reasoning_per_m: Option<f64>,
    pub request_fee: Option<f64>,
    pub currency: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct V1ModelList {
    pub data: Vec<V1Model>,
}

// ---------------------------------------------------------------------------
// Request inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProviderInput {
    /// Unique provider name (e.g. `openrouter`).
    pub name: String,
    /// OpenAI-compatible base URL (e.g. `https://api.example.com/v1`).
    pub base_url: String,
    /// Omit; only `openai_compatible` exists today.
    pub kind: Option<String>,
    /// Defaults to enabled.
    pub enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ProviderPatch {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
pub struct CredentialInput {
    /// Human label for the credential (shown in the admin UI).
    pub label: String,
    /// The provider API key. Write-only: it is encrypted at rest and never
    /// returned by any endpoint.
    pub secret: String,
    /// Extra request headers (name -> value). Values are write-only and
    /// returned as names only.
    pub headers: Option<BTreeMap<String, serde_json::Value>>,
    pub max_concurrency: Option<i64>,
    /// Manual balance in USD (decision D8).
    pub balance_usd: Option<f64>,
    /// `manual` (default), `provider_api`, or `unknown`.
    pub balance_source: Option<String>,
    pub balance_low_threshold: Option<f64>,
}

/// Redacted: the secret is a mask and headers print names only.
impl std::fmt::Debug for CredentialInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialInput")
            .field("label", &self.label)
            .field("secret", &"********")
            .field("headers", &header_names_debug(self.headers.as_ref()))
            .field("max_concurrency", &self.max_concurrency)
            .field("balance_usd", &self.balance_usd)
            .field("balance_source", &self.balance_source)
            .field("balance_low_threshold", &self.balance_low_threshold)
            .finish()
    }
}

#[derive(Default, Deserialize, ToSchema)]
pub struct CredentialPatch {
    pub label: Option<String>,
    /// Replaces the stored secret (audited as `credential.rotate`).
    pub secret: Option<String>,
    /// Replaces the stored headers wholesale; `{}` clears them.
    pub headers: Option<BTreeMap<String, serde_json::Value>>,
    pub max_concurrency: Option<i64>,
    pub balance_usd: Option<f64>,
    pub balance_source: Option<String>,
    pub balance_low_threshold: Option<f64>,
}

/// Redacted: the secret is a mask and headers print names only.
impl std::fmt::Debug for CredentialPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialPatch")
            .field("label", &self.label)
            .field("secret", &self.secret.as_ref().map(|_| "********"))
            .field("headers", &header_names_debug(self.headers.as_ref()))
            .field("max_concurrency", &self.max_concurrency)
            .field("balance_usd", &self.balance_usd)
            .field("balance_source", &self.balance_source)
            .field("balance_low_threshold", &self.balance_low_threshold)
            .finish()
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ModelInput {
    /// Canonical model name shared across providers (e.g. `openai/gpt-4o`).
    pub canonical_name: String,
    pub family: Option<String>,
    pub modalities_input: Option<Vec<String>>,
    pub modalities_output: Option<Vec<String>>,
    pub context_length: Option<i64>,
    pub max_output_tokens: Option<i64>,
    /// Free-form capabilities JSON (e.g. `{"supported_parameters": [...]}`).
    pub capabilities: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ModelPatch {
    pub canonical_name: Option<String>,
    pub family: Option<String>,
    pub modalities_input: Option<Vec<String>>,
    pub modalities_output: Option<Vec<String>>,
    pub context_length: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub capabilities: Option<serde_json::Value>,
}

#[derive(Deserialize, ToSchema)]
pub struct OfferingInput {
    pub provider_id: String,
    pub model_id: String,
    /// The model id sent upstream (e.g. `anthropic/claude-3.5-sonnet`).
    pub upstream_model_id: String,
    pub context_override: Option<i64>,
    pub max_output_override: Option<i64>,
    /// Defaults to enabled.
    pub enabled: Option<bool>,
    /// Per-offering headers; they override the credential's at request time.
    pub headers: Option<BTreeMap<String, serde_json::Value>>,
}

/// Redacted: headers print names only, never values.
impl std::fmt::Debug for OfferingInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfferingInput")
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .field("upstream_model_id", &self.upstream_model_id)
            .field("context_override", &self.context_override)
            .field("max_output_override", &self.max_output_override)
            .field("enabled", &self.enabled)
            .field("headers", &header_names_debug(self.headers.as_ref()))
            .finish()
    }
}

#[derive(Default, Deserialize, ToSchema)]
pub struct OfferingPatch {
    pub upstream_model_id: Option<String>,
    pub context_override: Option<i64>,
    pub max_output_override: Option<i64>,
    pub enabled: Option<bool>,
    /// Replaces the stored headers wholesale; `{}` clears them.
    pub headers: Option<BTreeMap<String, serde_json::Value>>,
    /// Not patchable: an offering's identity is fixed at creation. Accepted
    /// only to be rejected with a clear `400` instead of silently dropped
    /// (alignment finding B3).
    pub provider_id: Option<serde_json::Value>,
    /// Not patchable: see `provider_id`.
    pub model_id: Option<serde_json::Value>,
}

/// Redacted: headers print names only, never values.
impl std::fmt::Debug for OfferingPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfferingPatch")
            .field("upstream_model_id", &self.upstream_model_id)
            .field("context_override", &self.context_override)
            .field("max_output_override", &self.max_output_override)
            .field("enabled", &self.enabled)
            .field("headers", &header_names_debug(self.headers.as_ref()))
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .finish()
    }
}

/// Manual price override (`PUT`, source becomes `manual` and is never
/// overwritten by a sync). Missing components are stored as NULL — unknown is
/// unknown, never 0. At least one price field must be set: an all-NULL body
/// is rejected with `400` instead of storing an all-NULL price row.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct PriceInput {
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub reasoning_per_m: Option<f64>,
    pub request_fee: Option<f64>,
    /// Defaults to `USD`.
    pub currency: Option<String>,
}

/// Response of `POST /api/credentials/{id}/test`.
#[derive(Debug, Serialize, ToSchema)]
pub struct CredentialTestView {
    /// `ok` or `error`; also recorded on the credential row.
    pub status: String,
    pub ok: bool,
    /// Upstream HTTP status, when a response arrived.
    pub http_status: Option<i64>,
    /// Probe round-trip time in milliseconds.
    pub latency_ms: u64,
    /// Transport-level failure summary (never contains secret material).
    pub error: Option<String>,
    pub tested_at: String,
}

/// Outcome of `POST /api/sync/openrouter`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SyncOutcomeView {
    pub run_id: String,
    /// `success`, `stale-if-error`, or `error`.
    pub status: String,
    /// Models applied (success) or served from the previous snapshot
    /// (stale-if-error); 0 when no snapshot exists.
    pub model_count: usize,
    /// True when the previous snapshot is being served because the fetch
    /// failed (stale-if-error).
    pub stale: bool,
    pub started_at: String,
    pub finished_at: String,
    pub error: Option<String>,
}

/// One recorded sync run, as returned by the status endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct SyncRunView {
    pub id: String,
    pub source: String,
    pub started_at: String,
    pub finished_at: String,
    /// `success`, `error`, or `stale-if-error`.
    pub status: String,
    pub model_count: i64,
    /// Run detail (raw source prices, error summary). Never secret material.
    pub detail: Option<serde_json::Value>,
}

/// `GET /api/sync/openrouter/status`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SyncStatusView {
    /// The most recent run, when one exists.
    pub last_run: Option<SyncRunView>,
    /// True when the last successful snapshot is older than the 24h TTL or
    /// does not exist.
    pub snapshot_stale: bool,
    pub ttl_hours: i64,
}

/// Parses a stored JSON array of strings; malformed storage degrades to
/// `None` rather than failing a response.
pub(crate) fn parse_modalities(raw: Option<&str>) -> Option<Vec<String>> {
    raw.and_then(|text| serde_json::from_str::<Vec<String>>(text).ok())
}

/// The display name a synced model keeps in `capabilities_json`, if any.
pub(crate) fn display_name(capabilities_json: Option<&str>) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(capabilities_json?).ok()?;
    value
        .get("display_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-live-super-secret";
    const HEADER_VALUE: &str = "header-value-that-is-secret";

    fn header_input() -> BTreeMap<String, serde_json::Value> {
        [(
            "x-api-key".to_string(),
            serde_json::Value::String(HEADER_VALUE.to_string()),
        )]
        .into_iter()
        .collect()
    }

    /// A sealed-looking envelope: plain names, opaque values ciphertext.
    fn sealed_headers_json() -> String {
        serde_json::json!({
            "names": ["x-api-key"],
            "values": "v1:AAAAAAAAAAAAAAAA:BBBBBBBB",
        })
        .to_string()
    }

    #[test]
    fn credential_debug_prints_no_secret_or_header_values() {
        let input = CredentialInput {
            label: "primary".to_string(),
            secret: SECRET.to_string(),
            headers: Some(header_input()),
            max_concurrency: None,
            balance_usd: None,
            balance_source: None,
            balance_low_threshold: None,
        };
        let printed = format!("{input:?}");
        assert!(!printed.contains(SECRET), "{printed}");
        assert!(!printed.contains(HEADER_VALUE), "{printed}");
        assert!(
            printed.contains("x-api-key"),
            "names stay visible: {printed}"
        );

        let patch = CredentialPatch {
            secret: Some(SECRET.to_string()),
            headers: Some(header_input()),
            ..CredentialPatch::default()
        };
        let printed = format!("{patch:?}");
        assert!(!printed.contains(SECRET), "{printed}");
        assert!(!printed.contains(HEADER_VALUE), "{printed}");

        let row = CredentialRow {
            id: "cred-1".to_string(),
            provider_id: "prov-1".to_string(),
            label: "primary".to_string(),
            secret_cipher: "v1:AAAAAAAAAAAAAAAA:BBBBBBBB".to_string(),
            headers_json: Some(sealed_headers_json()),
            max_concurrency: None,
            balance_usd: None,
            balance_source: "manual".to_string(),
            balance_updated_at: None,
            balance_updated_by: None,
            balance_low_threshold: None,
            status: "unknown".to_string(),
            cooldown_until: None,
            last_tested_at: None,
            created_at: "2026-11-16T00:00:00.000Z".to_string(),
            updated_at: "2026-11-16T00:00:00.000Z".to_string(),
        };
        let printed = format!("{row:?}");
        assert!(
            !printed.contains("BBBBBBBB"),
            "no cipher material: {printed}"
        );
        assert!(!printed.contains(HEADER_VALUE), "{printed}");
        assert!(
            printed.contains("x-api-key"),
            "names stay visible: {printed}"
        );
    }

    #[test]
    fn offering_debug_prints_no_header_values() {
        let input = OfferingInput {
            provider_id: "prov-1".to_string(),
            model_id: "model-1".to_string(),
            upstream_model_id: "brain-1".to_string(),
            context_override: None,
            max_output_override: None,
            enabled: None,
            headers: Some(header_input()),
        };
        let printed = format!("{input:?}");
        assert!(!printed.contains(HEADER_VALUE), "{printed}");
        assert!(
            printed.contains("x-api-key"),
            "names stay visible: {printed}"
        );

        let patch = OfferingPatch {
            headers: Some(header_input()),
            ..OfferingPatch::default()
        };
        let printed = format!("{patch:?}");
        assert!(!printed.contains(HEADER_VALUE), "{printed}");

        let row = OfferingRow {
            id: "off-1".to_string(),
            provider_id: "prov-1".to_string(),
            model_id: "model-1".to_string(),
            upstream_model_id: "brain-1".to_string(),
            context_override: None,
            max_output_override: None,
            enabled: 1,
            headers_json: Some(sealed_headers_json()),
            created_at: "2026-11-16T00:00:00.000Z".to_string(),
            updated_at: "2026-11-16T00:00:00.000Z".to_string(),
        };
        let printed = format!("{row:?}");
        assert!(
            !printed.contains("BBBBBBBB"),
            "no cipher material: {printed}"
        );
        assert!(!printed.contains(HEADER_VALUE), "{printed}");
    }
}
