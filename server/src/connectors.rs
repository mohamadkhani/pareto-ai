//! Provider connectors (decision D42): one declarative profile per provider
//! and kind.
//!
//! * `catalog` — fetch a URL (usually `{base}/models`) and map its entries to
//!   prices (D41) and discovery metadata.
//! * `balance` — fetch a provider-specific usage/balance URL and store the
//!   result **per credential**. Balance is advisory (`warn`, the default) with
//!   an opt-in hard gate (`block`).
//! * `health` — the endpoint the credential "Test" action calls instead of
//!   `{base}/models`.
//!
//! # Configuration
//!
//! One JSON document is stored per `(provider, kind)` in
//! `provider_connectors.config_json`. The top level carries `url`, `headers`
//! (values may reference the credential secret as `{{secret}}`),
//! `allow_private`, and the kind-specific section (`catalog`, `balance`, or
//! `health`). Header *values* are configuration, not a secret store: the
//! secret stays in `provider_credentials.secret_cipher`. The one exception is
//! the literal `{{secret}}` placeholder, which is substituted at fetch time
//! and never written anywhere else.
//!
//! # Safety rules (D42)
//!
//! * Only `http`/`https`; redirects are not followed.
//! * Loopback (`127.0.0.0/8`, `::1`), private (`10/8`, `172.16/12`,
//!   `192.168/16`, `fc00::/7`, CGNAT), and IPv4-mapped addresses are rejected
//!   unless the connector sets `allow_private: true`.
//! * Link-local (`169.254/16`, `fe80::/10`, including the `169.254.169.254`
//!   and `fd00:ec2::254` metadata endpoints), unspecified, broadcast, and
//!   multicast addresses are always rejected.
//! * 15 s timeout, 1 MiB response cap, `User-Agent: pareto-ai/<version>`,
//!   `Accept-Encoding: identity`.
//! * Response bodies are never logged or persisted. Errors are layer-named
//!   (`url`, `ssrf`, `resolve`, `connect`, `status`, `body`) and never carry a
//!   header value or the credential secret.
//! * A connector failure never changes prices or routing: catalog/balance work
//!   runs in its own transaction and a failed fetch writes only a
//!   `connector_runs` row.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};
use utoipa::ToSchema;

use crate::catalog::{self, Actor, CatalogError, SyncPrice, SyncPriceOutcome, headers};
use crate::secrets::SecretBox;

/// One fetch round trip, from connect to body.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Hard cap on a connector response body.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// The sample endpoint returns at most this much of the raw document.
pub const SAMPLE_LIMIT_BYTES: usize = 200 * 1024;
/// Balance refresh interval when the connector does not set one.
pub const DEFAULT_REFRESH_SECONDS: u64 = 900;
pub const MIN_REFRESH_SECONDS: u64 = 60;
pub const MAX_REFRESH_SECONDS: u64 = 86_400;
/// Rows kept per `(provider, kind)` in `connector_runs`.
pub const MAX_RUNS: i64 = 30;
/// The background balance job's wake-up cadence.
pub const BALANCE_TICK: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Kinds
// ---------------------------------------------------------------------------

/// The three connector families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorKind {
    Catalog,
    Balance,
    Health,
}

impl ConnectorKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "catalog" => Some(Self::Catalog),
            "balance" => Some(Self::Balance),
            "health" => Some(Self::Health),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Balance => "balance",
            Self::Health => "health",
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration shapes
// ---------------------------------------------------------------------------

/// The document stored in `provider_connectors.config_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConnectorConfig {
    /// Absolute `http(s)` URL, or a path beginning with `/` resolved against
    /// the provider's `base_url` origin.
    pub url: String,
    /// Extra headers merged over the credential's headers. Values may contain
    /// `{{secret}}`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Allow loopback and private destinations (local test providers only).
    #[serde(default)]
    pub allow_private: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CatalogConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<BalanceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthConfig>,
}

/// Price mapping plus discovery metadata (D41).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CatalogConfig {
    /// JSON Pointer to the array of model entries.
    pub entries: String,
    /// Pointer to the upstream model id inside each entry.
    pub model_id: String,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub cache_read: Option<String>,
    #[serde(default)]
    pub cache_write: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    /// How the published numbers are expressed (D44). One unit object applies
    /// to every price component of this connector.
    #[serde(default)]
    pub unit: PriceUnit,
    #[serde(default)]
    pub modalities_input: Option<String>,
    #[serde(default)]
    pub modalities_output: Option<String>,
    #[serde(default)]
    pub context_length: Option<String>,
    #[serde(default)]
    pub max_output: Option<String>,
    /// Run the price refresh after a successful discovery **apply**.
    #[serde(default)]
    pub apply_on_discovery: bool,
    /// Optional catalog refresh cadence; absent means on demand only.
    #[serde(default)]
    pub refresh_seconds: Option<u64>,
}

/// How a provider expresses its numbers, and what the UI should show (D44).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PriceUnit {
    /// `per_million_tokens`, `per_token`, or `custom`.
    #[serde(default)]
    pub source: UnitSource,
    /// Applied only when `source = custom`; e.g. `0.01` for prices quoted in
    /// cents. A non-positive or non-finite value makes the entry unpriced and
    /// raises a preview warning.
    #[serde(default = "one")]
    pub multiplier: f64,
    /// ISO currency of the raw numbers. Non-USD values only feed routing when
    /// `fx_to_usd` is supplied.
    #[serde(default = "usd")]
    pub currency: String,
    /// Static conversion to USD; no live FX feed exists.
    #[serde(default)]
    pub fx_to_usd: Option<f64>,
    /// Symbol and decimals for the price table.
    #[serde(default)]
    pub display: PriceDisplay,
    /// Free-text reminder shown by the UI (e.g. "quoted in cents — verify
    /// against an invoice").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Default for PriceUnit {
    fn default() -> Self {
        Self {
            source: UnitSource::PerMillionTokens,
            multiplier: 1.0,
            currency: "USD".to_string(),
            fx_to_usd: None,
            display: PriceDisplay::default(),
            note: None,
        }
    }
}

/// The base unit before the optional multiplier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnitSource {
    PerToken,
    #[default]
    PerMillionTokens,
    Custom,
}

/// Display hints for the price table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PriceDisplay {
    #[serde(default = "dollar")]
    pub symbol: String,
    #[serde(default = "four")]
    pub decimals: u8,
}

impl Default for PriceDisplay {
    fn default() -> Self {
        Self {
            symbol: "$".to_string(),
            decimals: 4,
        }
    }
}

fn one() -> f64 {
    1.0
}

/// `multiplier` only means something for `custom`; saying so beats silently
/// ignoring an operator's number.
fn non_custom_multiplier_warning(multiplier: f64) -> Option<String> {
    (multiplier != 1.0)
        .then(|| format!("unit.multiplier {multiplier} is ignored unless unit.source is custom"))
}

fn usd() -> String {
    "USD".to_string()
}

fn dollar() -> String {
    "$".to_string()
}

fn four() -> u8 {
    4
}

/// Balance document pointers (all relative to the response root).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BalanceConfig {
    /// Pointer to the remaining balance; required and numeric.
    pub balance: String,
    #[serde(default)]
    pub available: Option<String>,
    /// Literal currency or a pointer to a currency string. Default `USD`.
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub period_spend: Option<String>,
    #[serde(default)]
    pub burn_rate: Option<String>,
    #[serde(default)]
    pub days_remaining: Option<String>,
    /// Refresh cadence; default 900, bounded to 60…86 400.
    #[serde(default)]
    pub refresh_seconds: Option<u64>,
}

/// Health connector options.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HealthConfig {
    /// Optional pointer whose truthy value means healthy. A 2xx response is
    /// healthy when the pointer is absent.
    #[serde(default)]
    pub ok_pointer: Option<String>,
}

impl ConnectorConfig {
    /// Validates the document for its kind and normalizes nothing else.
    /// Failure mode: [`ConnectorError::Invalid`] naming the offending field.
    pub fn validate(&self, kind: ConnectorKind) -> Result<(), ConnectorError> {
        if self.url.trim().is_empty() {
            return Err(ConnectorError::Invalid("url must not be empty".to_string()));
        }
        for (name, value) in &self.headers {
            let map = BTreeMap::from([(name.clone(), Value::String(value.clone()))]);
            headers::normalize(&map).map_err(|error| ConnectorError::Invalid(error.to_string()))?;
        }
        match kind {
            ConnectorKind::Catalog => {
                let catalog = self.catalog.as_ref().ok_or_else(|| {
                    ConnectorError::Invalid("a catalog connector needs a catalog section".into())
                })?;
                validate_pointer(&catalog.entries, "catalog.entries")?;
                validate_pointer(&catalog.model_id, "catalog.model_id")?;
                for (value, field) in [
                    (&catalog.input, "catalog.input"),
                    (&catalog.output, "catalog.output"),
                    (&catalog.cache_read, "catalog.cache_read"),
                    (&catalog.cache_write, "catalog.cache_write"),
                    (&catalog.reasoning, "catalog.reasoning"),
                    (&catalog.modalities_input, "catalog.modalities_input"),
                    (&catalog.modalities_output, "catalog.modalities_output"),
                    (&catalog.context_length, "catalog.context_length"),
                    (&catalog.max_output, "catalog.max_output"),
                ] {
                    if let Some(pointer) = value {
                        validate_pointer(pointer, field)?;
                    }
                }
                if catalog.input.is_none() && catalog.output.is_none() {
                    return Err(ConnectorError::Invalid(
                        "catalog mapping needs at least one of input or output".to_string(),
                    ));
                }
                if catalog.unit.currency.trim().is_empty() {
                    return Err(ConnectorError::Invalid(
                        "catalog.unit.currency must not be empty".to_string(),
                    ));
                }
                if catalog.unit.display.symbol.trim().is_empty() {
                    return Err(ConnectorError::Invalid(
                        "catalog.unit.display.symbol must not be empty".to_string(),
                    ));
                }
                if catalog.unit.display.decimals > 8 {
                    return Err(ConnectorError::Invalid(
                        "catalog.unit.display.decimals must be at most 8".to_string(),
                    ));
                }
                if let Some(seconds) = catalog.refresh_seconds {
                    validate_refresh_seconds(seconds, "catalog.refresh_seconds")?;
                }
            }
            ConnectorKind::Balance => {
                let balance = self.balance.as_ref().ok_or_else(|| {
                    ConnectorError::Invalid("a balance connector needs a balance section".into())
                })?;
                validate_pointer(&balance.balance, "balance.balance")?;
                for (value, field) in [
                    (&balance.available, "balance.available"),
                    (&balance.period_spend, "balance.period_spend"),
                    (&balance.burn_rate, "balance.burn_rate"),
                    (&balance.days_remaining, "balance.days_remaining"),
                ] {
                    if let Some(pointer) = value {
                        validate_pointer(pointer, field)?;
                    }
                }
                if let Some(pointer) = balance.currency.as_deref()
                    && pointer.starts_with('/')
                {
                    validate_pointer(pointer, "balance.currency")?;
                }
                if let Some(seconds) = balance.refresh_seconds {
                    validate_refresh_seconds(seconds, "balance.refresh_seconds")?;
                }
            }
            ConnectorKind::Health => {
                if let Some(pointer) = self
                    .health
                    .as_ref()
                    .and_then(|health| health.ok_pointer.as_deref())
                {
                    validate_pointer(pointer, "health.ok_pointer")?;
                }
            }
        }
        Ok(())
    }

    /// The configured refresh cadence for a kind (balance default 900 s;
    /// catalog is disabled unless configured).
    pub fn refresh_seconds(&self, kind: ConnectorKind) -> Option<u64> {
        match kind {
            ConnectorKind::Balance => Some(
                self.balance
                    .as_ref()
                    .and_then(|balance| balance.refresh_seconds)
                    .unwrap_or(DEFAULT_REFRESH_SECONDS),
            ),
            ConnectorKind::Catalog => self.catalog.as_ref().and_then(|c| c.refresh_seconds),
            ConnectorKind::Health => None,
        }
    }
}

fn validate_refresh_seconds(seconds: u64, field: &str) -> Result<(), ConnectorError> {
    if (MIN_REFRESH_SECONDS..=MAX_REFRESH_SECONDS).contains(&seconds) {
        Ok(())
    } else {
        Err(ConnectorError::Invalid(format!(
            "{field} must be between {MIN_REFRESH_SECONDS} and {MAX_REFRESH_SECONDS} seconds (got {seconds})"
        )))
    }
}

/// Rejects a pointer that cannot be an RFC 6901 pointer.
fn validate_pointer(pointer: &str, field: &str) -> Result<(), ConnectorError> {
    if !pointer.starts_with('/') {
        return Err(ConnectorError::Invalid(format!(
            "{field} must be a JSON Pointer starting with / (got {pointer:?})"
        )));
    }
    let bytes = pointer.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'~' && !matches!(bytes.get(index + 1), Some(b'0' | b'1')) {
            return Err(ConnectorError::Invalid(format!(
                "{field} has an invalid ~ escape (use ~0 for ~ and ~1 for /)"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

/// One selectable connector template: plain configuration, fully editable.
/// A template carries one config per kind because the catalog and balance
/// endpoints usually differ (SailResearch: `/v1/models` vs
/// `/v2/usage/summary?range=30d`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectorTemplate {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub configs: ConnectorTemplateConfigs,
}

/// The per-kind documents a template prefills.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct ConnectorTemplateConfigs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<ConnectorConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance: Option<ConnectorConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<ConnectorConfig>,
}

/// The authorization header every shipped template uses. The value is a
/// template, never a secret: `{{secret}}` is substituted at fetch time.
fn bearer_headers() -> BTreeMap<String, String> {
    BTreeMap::from([("authorization".to_string(), "Bearer {{secret}}".to_string())])
}

fn catalog_only(config: ConnectorConfig) -> ConnectorTemplateConfigs {
    ConnectorTemplateConfigs {
        catalog: Some(config),
        balance: None,
        health: None,
    }
}

/// The shipped templates. They are configuration documents, never code.
pub fn templates() -> Vec<ConnectorTemplate> {
    let sail_catalog = ConnectorConfig {
        url: "https://api.sailresearch.com/v1/models".to_string(),
        headers: bearer_headers(),
        allow_private: false,
        catalog: Some(CatalogConfig {
            entries: "/data".to_string(),
            model_id: "/id".to_string(),
            input: Some("/pricing_usd_per_million_tokens/balanced/input".to_string()),
            output: Some("/pricing_usd_per_million_tokens/balanced/output".to_string()),
            cache_read: Some("/pricing_usd_per_million_tokens/balanced/cached_input".to_string()),
            cache_write: None,
            reasoning: None,
            unit: PriceUnit {
                source: UnitSource::Custom,
                multiplier: 0.01,
                currency: "USD".to_string(),
                fx_to_usd: None,
                display: PriceDisplay {
                    symbol: "$".to_string(),
                    decimals: 4,
                },
                note: Some("quoted in cents — verify against an invoice".to_string()),
            },
            modalities_input: Some("/input_modalities".to_string()),
            modalities_output: Some("/output_modalities".to_string()),
            context_length: Some("/context_length".to_string()),
            max_output: Some("/max_output_tokens".to_string()),
            apply_on_discovery: true,
            refresh_seconds: None,
        }),
        balance: None,
        health: None,
    };
    let sail_balance = ConnectorConfig {
        url: "/v2/usage/summary?range=30d".to_string(),
        headers: bearer_headers(),
        allow_private: false,
        catalog: None,
        balance: Some(BalanceConfig {
            balance: "/balance".to_string(),
            available: Some("/available".to_string()),
            currency: Some("/currency".to_string()),
            period_spend: Some("/period_spend".to_string()),
            burn_rate: Some("/burn_rate".to_string()),
            days_remaining: Some("/days_remaining".to_string()),
            refresh_seconds: Some(DEFAULT_REFRESH_SECONDS),
        }),
        health: None,
    };
    let sail_health = ConnectorConfig {
        url: "/models".to_string(),
        headers: bearer_headers(),
        allow_private: false,
        catalog: None,
        balance: None,
        health: Some(HealthConfig { ok_pointer: None }),
    };
    let openrouter = ConnectorConfig {
        url: "https://openrouter.ai/api/v1/models".to_string(),
        headers: BTreeMap::new(),
        allow_private: false,
        catalog: Some(CatalogConfig {
            entries: "/data".to_string(),
            model_id: "/id".to_string(),
            input: Some("/pricing/prompt".to_string()),
            output: Some("/pricing/completion".to_string()),
            cache_read: Some("/pricing/input_cache_read".to_string()),
            cache_write: Some("/pricing/input_cache_write".to_string()),
            reasoning: Some("/pricing/internal_reasoning".to_string()),
            unit: PriceUnit {
                source: UnitSource::PerToken,
                multiplier: 1.0,
                currency: "USD".to_string(),
                fx_to_usd: None,
                display: PriceDisplay {
                    symbol: "$".to_string(),
                    decimals: 4,
                },
                note: None,
            },
            modalities_input: Some("/architecture/input_modalities".to_string()),
            modalities_output: Some("/architecture/output_modalities".to_string()),
            context_length: Some("/context_length".to_string()),
            max_output: Some("/top_provider/max_completion_tokens".to_string()),
            apply_on_discovery: false,
            refresh_seconds: None,
        }),
        balance: None,
        health: None,
    };
    let generic = ConnectorConfig {
        url: "/models".to_string(),
        headers: BTreeMap::new(),
        allow_private: false,
        catalog: Some(CatalogConfig {
            entries: "/data".to_string(),
            model_id: "/id".to_string(),
            input: Some("/pricing/input".to_string()),
            output: Some("/pricing/output".to_string()),
            cache_read: None,
            cache_write: None,
            reasoning: None,
            unit: PriceUnit::default(),
            modalities_input: None,
            modalities_output: None,
            context_length: None,
            max_output: None,
            apply_on_discovery: false,
            refresh_seconds: None,
        }),
        balance: None,
        health: None,
    };
    vec![
        ConnectorTemplate {
            id: "sailresearch",
            label: "SailResearch",
            description: "Catalog prices under the balanced tier (cents, custom ×0.01), plus the /v2 usage balance.",
            configs: ConnectorTemplateConfigs {
                catalog: Some(sail_catalog),
                balance: Some(sail_balance),
                health: Some(sail_health),
            },
        },
        ConnectorTemplate {
            id: "openrouter_per_token",
            label: "OpenRouter (per token)",
            description: "OpenRouter publishes USD strings per token; the engine multiplies by 1e6.",
            configs: catalog_only(openrouter),
        },
        ConnectorTemplate {
            id: "generic_pricing_object",
            label: "Generic pricing object",
            description: "Best-effort default for `/data[].pricing.{input,output}` in USD per million tokens.",
            configs: catalog_only(generic),
        },
    ]
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Upstream(String),
}

impl ConnectorError {
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        use crate::error::ApiError;
        use axum::http::StatusCode;
        match self {
            Self::Catalog(error) => error.into_api_error(rid),
            Self::NotFound(what) => ApiError::not_found(what).with_request_id(rid),
            Self::Invalid(message) => ApiError::bad_request(message).with_request_id(rid),
            Self::Upstream(message) => {
                ApiError::custom(StatusCode::BAD_GATEWAY, "upstream_error", message)
                    .with_request_id(rid)
            }
            Self::Db(error) => ApiError::internal("provider connector", error).with_request_id(rid),
        }
    }
}

/// Why a fetch failed. Every message names only the layer.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FetchError {
    #[error("url: {0}")]
    InvalidUrl(String),
    #[error("ssrf: {0}")]
    Blocked(String),
    #[error("resolve: the host could not be resolved")]
    Resolve,
    #[error("connect: the provider could not be reached")]
    Connect,
    #[error("timeout: the provider did not answer within 15 seconds")]
    Timeout,
    #[error("status: the provider returned HTTP {0}")]
    Status(u16),
    #[error("body: the response exceeds the 1 MiB cap")]
    TooLarge,
    #[error("body: the response could not be read")]
    Body,
    #[error("header: {0}")]
    Header(String),
}

// ---------------------------------------------------------------------------
// Rows and views
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConnectorRow {
    pub kind: String,
    pub config_json: String,
    pub enabled: i64,
    pub updated_by: Option<String>,
    pub updated_at: String,
}

impl ConnectorRow {
    pub fn config(&self) -> Result<ConnectorConfig, ConnectorError> {
        serde_json::from_str(&self.config_json).map_err(|_| {
            ConnectorError::Invalid(format!("stored {} connector config is corrupt", self.kind))
        })
    }
}

/// A compact connector entry on `GET /api/providers`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectorSummary {
    pub kind: String,
    pub enabled: bool,
    pub updated_at: String,
}

/// `GET /api/providers/{id}/connectors/{kind}`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectorView {
    pub provider_id: String,
    pub kind: String,
    pub configured: bool,
    pub enabled: bool,
    pub config: Option<ConnectorConfig>,
    pub updated_at: Option<String>,
    pub updated_by: Option<String>,
}

/// Input of `PUT /api/providers/{id}/connectors/{kind}`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ConnectorPut {
    pub config: ConnectorConfig,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `POST .../sample` and `.../test` accept an optional draft config.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ConnectorDraft {
    #[serde(default)]
    pub config: Option<ConnectorConfig>,
    #[serde(default)]
    pub credential_id: Option<String>,
}

/// `POST .../sample` response: the raw document, truncated for the UI tree.
#[derive(Debug, Serialize, ToSchema)]
pub struct SampleView {
    pub kind: String,
    pub url: String,
    pub status: u16,
    pub bytes: usize,
    pub truncated: bool,
    pub body: String,
    pub fetched_at: String,
}

/// One resolved catalog entry in the test preview.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CatalogSample {
    pub model_id: Option<String>,
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub reasoning_per_m: Option<f64>,
    /// The provider's own numbers, before unit conversion (D44).
    pub raw: PriceSet,
    /// The numbers after the unit conversion, in the unit's currency.
    pub converted_per_m: PriceSet,
    /// Per-entry conversion warnings ("check the unit", non-USD without FX).
    pub warnings: Vec<String>,
    /// The stored currency: `USD` when routable, otherwise the source code.
    pub currency: String,
    /// False for a non-USD currency without a usable `fx_to_usd`.
    pub routable: bool,
    pub variable_price: bool,
    pub error: Option<String>,
}

/// `POST .../test` response. Writes nothing.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectorTestView {
    pub kind: String,
    pub ok: bool,
    pub url: String,
    pub entries_seen: Option<usize>,
    pub matched: Option<usize>,
    pub skipped: Option<usize>,
    pub samples: Vec<CatalogSample>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// The unit's (or balance's) currency, echoed for the UI (D44).
    pub currency: Option<String>,
    pub balance: Option<ExtractedBalance>,
    pub health: Option<ExtractedHealth>,
}

/// `POST /api/providers/{id}/prices/refresh`.
#[derive(Debug, Serialize, ToSchema)]
pub struct PriceRefreshView {
    pub ok: bool,
    pub provider_id: String,
    pub run_id: Option<String>,
    pub entries_seen: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped_manual: usize,
    pub skipped_unresolved: usize,
    pub source_url: String,
    pub fetched_at: String,
    pub error: Option<String>,
}

/// `POST .../balance/refresh`.
#[derive(Debug, Serialize, ToSchema)]
pub struct BalanceRefreshView {
    pub ok: bool,
    pub credential_id: String,
    pub available: bool,
    pub balance_usd: Option<f64>,
    pub currency: String,
    pub period_spend: Option<f64>,
    pub burn_rate: Option<f64>,
    pub days_remaining: Option<f64>,
    pub error: Option<String>,
    pub fetched_at: String,
}

/// `POST /api/providers/{id}/balances/refresh`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderBalancesView {
    pub provider_id: String,
    pub refreshed: usize,
    pub failed: usize,
    pub results: Vec<BalanceRefreshView>,
}

// ---------------------------------------------------------------------------
// JSON Pointer and value resolution
// ---------------------------------------------------------------------------

/// Resolves an RFC 6901 pointer against `document`; `None` when a step is
/// missing or an array index is malformed (never a panic).
pub fn resolve_pointer<'a>(document: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() {
        return Some(document);
    }
    if !pointer.starts_with('/') {
        return None;
    }
    let mut current = document;
    for token in pointer.split('/').skip(1) {
        let token = token.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Object(map) => map.get(&token)?,
            Value::Array(items) => items.get(array_index(&token)?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// RFC 6901 array index: decimal, no leading zeros, no `-`.
fn array_index(token: &str) -> Option<usize> {
    if token.is_empty() || (token.len() > 1 && token.starts_with('0')) {
        return None;
    }
    if !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
}

/// A JSON number or numeric string. Booleans are not numbers.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64().filter(|value| value.is_finite()),
        Value::String(text) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite()),
        _ => None,
    }
}

fn integer(value: &Value) -> Option<i64> {
    let number = number(value)?;
    if number.fract() != 0.0 || !(i64::MIN as f64..=i64::MAX as f64).contains(&number) {
        return None;
    }
    Some(number as i64)
}

fn boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Number(number) => number.as_f64().map(|value| value != 0.0),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" | "available" => Some(true),
            "false" | "no" | "0" | "unavailable" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn string_at(document: &Value, pointer: &str) -> Option<String> {
    let value = resolve_pointer(document, pointer)?;
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn modalities_at(document: &Value, pointer: &str) -> Option<Vec<String>> {
    let value = resolve_pointer(document, pointer)?;
    match value {
        Value::Array(items) => {
            let list: Vec<String> = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect();
            (!list.is_empty()).then_some(list)
        }
        Value::String(text) if !text.trim().is_empty() => Some(vec![text.trim().to_string()]),
        _ => None,
    }
}

/// One resolved price component, before unit conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PriceField {
    NotConfigured,
    Missing,
    NonNumeric,
    Number(f64),
    Variable,
}

fn price_field(entry: &Value, pointer: Option<&str>) -> PriceField {
    let Some(pointer) = pointer else {
        return PriceField::NotConfigured;
    };
    let Some(value) = resolve_pointer(entry, pointer) else {
        return PriceField::Missing;
    };
    match number(value) {
        None => PriceField::NonNumeric,
        Some(value) if value < 0.0 => PriceField::Variable,
        Some(value) => PriceField::Number(value),
    }
}

/// The prices a connector published, in the provider's own numbers and unit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, ToSchema)]
pub struct PriceSet {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

/// The conversion a `PriceUnit` declares, resolved once per document.
struct UnitConversion {
    /// Multiplier applied after the base unit (`1e6`, `1`, or `custom`).
    factor: f64,
    /// Source currency, upper-cased.
    currency: String,
    /// True when the converted number is USD and may feed routing.
    routable: bool,
    warnings: Vec<String>,
}

impl UnitConversion {
    fn new(unit: &PriceUnit) -> Self {
        // D44: per-token × 1e6, per-million × 1, custom × multiplier. The
        // multiplier is what expresses a per-million feed quoted in cents
        // (`custom` + 0.01); it is ignored for the two standard sources.
        let currency = unit.currency.trim().to_ascii_uppercase();
        let (base, multiplier_warning) = match unit.source {
            UnitSource::PerToken => (1_000_000.0, non_custom_multiplier_warning(unit.multiplier)),
            UnitSource::PerMillionTokens => (1.0, non_custom_multiplier_warning(unit.multiplier)),
            UnitSource::Custom => {
                if unit.multiplier.is_finite() && unit.multiplier > 0.0 {
                    (unit.multiplier, None)
                } else {
                    return Self {
                        factor: 0.0,
                        currency,
                        routable: false,
                        warnings: vec![format!(
                            "unit.multiplier {} is not a positive number; prices are unavailable",
                            unit.multiplier
                        )],
                    };
                }
            }
        };
        let mut warnings = Vec::new();
        if let Some(warning) = multiplier_warning {
            warnings.push(warning);
        }
        let (factor, routable) = if currency == "USD" {
            (base, true)
        } else {
            match unit.fx_to_usd {
                Some(fx) if fx.is_finite() && fx > 0.0 => (base * fx, true),
                Some(fx) => {
                    warnings.push(format!(
                        "currency {currency} has an invalid fx_to_usd {fx}; prices are excluded from routing"
                    ));
                    (base, false)
                }
                None => {
                    warnings.push(format!(
                        "currency {currency} has no fx_to_usd; prices are stored but excluded from routing"
                    ));
                    (base, false)
                }
            }
        };
        Self {
            factor,
            currency,
            routable,
            warnings,
        }
    }

    fn convert(&self, field: PriceField) -> PriceField {
        match field {
            PriceField::Number(value) if self.factor > 0.0 => {
                PriceField::Number(value * self.factor)
            }
            PriceField::Number(_) => PriceField::Variable,
            other => other,
        }
    }
}

fn raw_of(field: PriceField) -> Option<f64> {
    match field {
        PriceField::Number(value) => Some(value),
        _ => None,
    }
}

fn number_of(field: PriceField) -> Option<f64> {
    match field {
        PriceField::Number(value) => Some(value),
        _ => None,
    }
}

/// A resolved catalog entry ready to be applied.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub model_id: String,
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub reasoning_per_m: Option<f64>,
    pub variable_price: bool,
    /// `USD` when the values are routable; otherwise the source currency,
    /// stored for display only (D44: non-USD without FX is unpriced).
    pub currency: String,
}

/// The result of applying a catalog mapping to one document.
#[derive(Debug, Clone, Default)]
pub struct CatalogExtraction {
    pub entries_seen: usize,
    pub matched: usize,
    pub skipped: usize,
    pub entries: Vec<CatalogEntry>,
    pub samples: Vec<CatalogSample>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Applies a catalog mapping to a parsed document.
///
/// A missing or non-numeric required price pointer skips **that entry only**
/// and is counted; a negative component becomes `variable` (D37) with a NULL
/// value. A missing optional component is just absent. Unit conversion follows
/// D44: `per_token` × 1e6, `per_million` × 1, `custom` × multiplier, then
/// × `fx_to_usd` only for a non-USD currency with a static rate.
pub fn extract_catalog(
    document: &Value,
    config: &CatalogConfig,
) -> Result<CatalogExtraction, ConnectorError> {
    config_pointer_guard(&config.entries, "catalog.entries", document)?;
    let entries = resolve_pointer(document, &config.entries)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ConnectorError::Invalid(format!(
                "catalog.entries pointer {} did not resolve to an array",
                config.entries
            ))
        })?;

    let conversion = UnitConversion::new(&config.unit);
    let unit_warnings = conversion.warnings.clone();
    let mut out = CatalogExtraction::default();
    out.warnings.extend(unit_warnings.iter().cloned());

    for (index, entry) in entries.iter().enumerate() {
        out.entries_seen += 1;
        let model_id = resolve_pointer(entry, &config.model_id)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        let Some(model_id) = model_id else {
            out.skipped += 1;
            let error = format!("entry {index}: model_id pointer did not resolve");
            out.errors.push(error.clone());
            out.samples.push(CatalogSample {
                model_id: None,
                input_per_m: None,
                output_per_m: None,
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                raw: PriceSet::default(),
                converted_per_m: PriceSet::default(),
                warnings: unit_warnings.clone(),
                currency: conversion.currency.clone(),
                routable: conversion.routable,
                variable_price: false,
                error: Some(error),
            });
            continue;
        };

        let raw_input = price_field(entry, config.input.as_deref());
        let raw_output = price_field(entry, config.output.as_deref());
        let mut required_error: Option<String> = None;
        for (field, value, pointer) in [
            ("input", raw_input, config.input.as_deref()),
            ("output", raw_output, config.output.as_deref()),
        ] {
            match value {
                PriceField::Missing => {
                    required_error = Some(format!(
                        "{model_id}: {field} pointer {} is missing",
                        pointer.unwrap_or("")
                    ));
                }
                PriceField::NonNumeric => {
                    required_error = Some(format!(
                        "{model_id}: {field} pointer {} is not numeric",
                        pointer.unwrap_or("")
                    ));
                }
                _ => {}
            }
            if required_error.is_some() {
                break;
            }
        }
        if let Some(error) = required_error {
            out.skipped += 1;
            out.errors.push(error.clone());
            out.samples.push(CatalogSample {
                model_id: Some(model_id),
                input_per_m: None,
                output_per_m: None,
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                raw: PriceSet {
                    input: raw_of(raw_input),
                    output: raw_of(raw_output),
                    cache_read: None,
                    cache_write: None,
                },
                converted_per_m: PriceSet::default(),
                warnings: unit_warnings.clone(),
                currency: conversion.currency.clone(),
                routable: conversion.routable,
                variable_price: false,
                error: Some(error),
            });
            continue;
        }

        let raw_cache_read = price_field(entry, config.cache_read.as_deref());
        let raw_cache_write = price_field(entry, config.cache_write.as_deref());
        let raw_reasoning = price_field(entry, config.reasoning.as_deref());
        let input = conversion.convert(raw_input);
        let output = conversion.convert(raw_output);
        let cache_read = conversion.convert(raw_cache_read);
        let cache_write = conversion.convert(raw_cache_write);
        let reasoning = conversion.convert(raw_reasoning);

        let mut entry_warnings = unit_warnings.clone();
        for (field, value, pointer) in [
            ("cache_read", raw_cache_read, config.cache_read.as_deref()),
            (
                "cache_write",
                raw_cache_write,
                config.cache_write.as_deref(),
            ),
            ("reasoning", raw_reasoning, config.reasoning.as_deref()),
        ] {
            if matches!(value, PriceField::Missing | PriceField::NonNumeric) && pointer.is_some() {
                entry_warnings.push(format!(
                    "{model_id}: optional {field} pointer {} did not resolve to a number",
                    pointer.unwrap_or("")
                ));
            }
        }

        // D44: the converted input price is the unit sanity check.
        if conversion.routable
            && let Some(value) = number_of(input)
            && !(0.001..=1000.0).contains(&value)
        {
            entry_warnings.push(format!(
                "{model_id}: input converts to {}{}/M (outside 0.001–1000 USD/M); check the unit",
                config.unit.display.symbol, value
            ));
        }

        let raw_variable = [
            raw_input,
            raw_output,
            raw_cache_read,
            raw_cache_write,
            raw_reasoning,
        ]
        .iter()
        .any(|field| matches!(field, PriceField::Variable));
        let variable_price = raw_variable
            || !conversion.routable
            || [input, output, cache_read, cache_write, reasoning]
                .iter()
                .any(|field| matches!(field, PriceField::Variable));

        // Non-routable (non-USD without FX) keeps no numbers: NULL means
        // unpriced, never a fake USD amount.
        let (input_per_m, output_per_m, cache_read_per_m, cache_write_per_m, reasoning_per_m) =
            if conversion.routable {
                (
                    number_of(input),
                    number_of(output),
                    number_of(cache_read),
                    number_of(cache_write),
                    number_of(reasoning),
                )
            } else {
                (None, None, None, None, None)
            };

        let stored_currency = if conversion.routable {
            "USD".to_string()
        } else {
            conversion.currency.clone()
        };
        out.matched += 1;
        let resolved = CatalogEntry {
            model_id: model_id.clone(),
            input_per_m,
            output_per_m,
            cache_read_per_m,
            cache_write_per_m,
            reasoning_per_m,
            variable_price,
            currency: stored_currency.clone(),
        };
        out.samples.push(CatalogSample {
            model_id: Some(model_id),
            input_per_m,
            output_per_m,
            cache_read_per_m,
            cache_write_per_m,
            reasoning_per_m,
            raw: PriceSet {
                input: raw_of(raw_input),
                output: raw_of(raw_output),
                cache_read: raw_of(raw_cache_read),
                cache_write: raw_of(raw_cache_write),
            },
            converted_per_m: PriceSet {
                input: number_of(input),
                output: number_of(output),
                cache_read: number_of(cache_read),
                cache_write: number_of(cache_write),
            },
            warnings: entry_warnings,
            currency: stored_currency,
            routable: conversion.routable,
            variable_price,
            error: None,
        });
        out.entries.push(resolved);
    }
    Ok(out)
}

/// A `document`-only pointer guard: missing top-level structure is a mapping
/// error naming the pointer instead of a panic.
fn config_pointer_guard(
    pointer: &str,
    field: &str,
    document: &Value,
) -> Result<(), ConnectorError> {
    if pointer.is_empty() {
        return Err(ConnectorError::Invalid(format!(
            "{field} must not be empty"
        )));
    }
    if resolve_pointer(document, pointer).is_none() {
        return Err(ConnectorError::Invalid(format!(
            "{field} pointer {pointer} is missing from the response"
        )));
    }
    Ok(())
}

/// A catalog entry as discovery needs it: the upstream id plus optional
/// metadata. Prices are resolved separately by [`extract_catalog`], so a
/// mapping whose price pointer is missing still discovers the model.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryEntry {
    pub model_id: String,
    pub modalities_input: Option<Vec<String>>,
    pub modalities_output: Option<Vec<String>>,
    pub context_length: Option<i64>,
    pub max_output_tokens: Option<i64>,
}

/// Reads the catalog entries discovery should create, ignoring prices.
pub fn extract_discovery_entries(
    document: &Value,
    config: &CatalogConfig,
) -> Result<Vec<DiscoveryEntry>, ConnectorError> {
    let entries = resolve_pointer(document, &config.entries)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ConnectorError::Invalid(format!(
                "catalog.entries pointer {} did not resolve to an array",
                config.entries
            ))
        })?;
    let mut out = Vec::new();
    for entry in entries {
        let Some(model_id) = resolve_pointer(entry, &config.model_id)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
        else {
            continue;
        };
        if !catalog::is_discoverable_model_id(&model_id) {
            continue;
        }
        out.push(DiscoveryEntry {
            model_id,
            modalities_input: config
                .modalities_input
                .as_deref()
                .and_then(|pointer| modalities_at(entry, pointer)),
            modalities_output: config
                .modalities_output
                .as_deref()
                .and_then(|pointer| modalities_at(entry, pointer)),
            context_length: config
                .context_length
                .as_deref()
                .and_then(|pointer| resolve_pointer(entry, pointer))
                .and_then(integer),
            max_output_tokens: config
                .max_output
                .as_deref()
                .and_then(|pointer| resolve_pointer(entry, pointer))
                .and_then(integer),
        });
    }
    Ok(out)
}

/// A resolved provider balance.
#[derive(Debug, Clone, Serialize, ToSchema, PartialEq)]
pub struct ExtractedBalance {
    /// The provider said the balance is usable.
    pub available: bool,
    /// Remaining balance; `None` when unavailable. Never 0 as a stand-in.
    pub balance_usd: Option<f64>,
    pub currency: String,
    pub period_spend: Option<f64>,
    pub burn_rate: Option<f64>,
    pub days_remaining: Option<f64>,
}

/// Reads the balance pointers from a document.
///
/// Failure modes: [`ConnectorError::Invalid`] when the required balance
/// pointer is missing or not numeric, or when `available` is present but not
/// a boolean. A negative balance is unrepresentable and treated the same as a
/// missing number.
pub fn extract_balance(
    document: &Value,
    config: &BalanceConfig,
) -> Result<ExtractedBalance, ConnectorError> {
    config_pointer_guard(&config.balance, "balance.balance", document)?;
    let balance = resolve_pointer(document, &config.balance).and_then(number);
    let Some(balance) = balance.filter(|value| *value >= 0.0) else {
        return Err(ConnectorError::Invalid(format!(
            "balance.balance pointer {} did not resolve to a non-negative number",
            config.balance
        )));
    };
    let available = match config.available.as_deref() {
        None => true,
        Some(pointer) => {
            let value = resolve_pointer(document, pointer).ok_or_else(|| {
                ConnectorError::Invalid(format!(
                    "balance.available pointer {pointer} is missing from the response"
                ))
            })?;
            boolean(value).ok_or_else(|| {
                ConnectorError::Invalid(format!(
                    "balance.available pointer {pointer} is not a boolean"
                ))
            })?
        }
    };
    let currency = match config.currency.as_deref() {
        None | Some("") => "USD".to_string(),
        Some(pointer) if pointer.starts_with('/') => {
            string_at(document, pointer).unwrap_or_else(|| "USD".to_string())
        }
        Some(literal) => literal.to_string(),
    };
    let optional = |pointer: Option<&str>| {
        pointer
            .and_then(|pointer| resolve_pointer(document, pointer))
            .and_then(number)
            .filter(|value| *value >= 0.0)
    };
    Ok(ExtractedBalance {
        available,
        balance_usd: available.then_some(balance),
        currency,
        period_spend: optional(config.period_spend.as_deref()),
        burn_rate: optional(config.burn_rate.as_deref()),
        days_remaining: optional(config.days_remaining.as_deref()),
    })
}

/// A resolved health-check answer.
#[derive(Debug, Clone, Serialize, ToSchema, PartialEq)]
pub struct ExtractedHealth {
    pub ok: bool,
    pub detail: Option<String>,
}

/// Reads the health answer: a 2xx response is healthy unless `ok_pointer`
/// names a boolean/truthy field that says otherwise.
pub fn extract_health(document: Option<&Value>, config: &HealthConfig) -> ExtractedHealth {
    let Some(pointer) = config.ok_pointer.as_deref() else {
        return ExtractedHealth {
            ok: true,
            detail: None,
        };
    };
    let Some(document) = document else {
        return ExtractedHealth {
            ok: false,
            detail: Some(format!("health.ok_pointer {pointer} needs a JSON body")),
        };
    };
    match resolve_pointer(document, pointer).and_then(boolean) {
        Some(true) => ExtractedHealth {
            ok: true,
            detail: None,
        },
        Some(false) => ExtractedHealth {
            ok: false,
            detail: Some(format!("health.ok_pointer {pointer} is false")),
        },
        None => ExtractedHealth {
            ok: false,
            detail: Some(format!(
                "health.ok_pointer {pointer} is missing or not a boolean"
            )),
        },
    }
}

// ---------------------------------------------------------------------------
// URL resolution and SSRF guards
// ---------------------------------------------------------------------------

/// Resolves a connector URL: absolute `http(s)` stays as written, a path
/// starting with `/` is joined to the provider base URL's origin.
pub fn resolve_url(base_url: &str, configured: &str) -> Result<String, FetchError> {
    let raw = configured.trim();
    if raw.is_empty() {
        return Err(FetchError::InvalidUrl("url must not be empty".to_string()));
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        reqwest::Url::parse(raw)
            .map_err(|_| FetchError::InvalidUrl("url is not a valid URL".to_string()))?;
        return Ok(raw.to_string());
    }
    if !raw.starts_with('/') {
        return Err(FetchError::InvalidUrl(
            "url must be an http(s) URL or a path starting with /".to_string(),
        ));
    }
    let base = reqwest::Url::parse(base_url).map_err(|_| {
        FetchError::InvalidUrl("the provider base_url is not a valid URL".to_string())
    })?;
    let host = base
        .host_str()
        .ok_or_else(|| FetchError::InvalidUrl("the provider base_url has no host".to_string()))?;
    let origin = match base.port() {
        Some(port) => format!("{}://{}:{}", base.scheme(), host, port),
        None => format!("{}://{}", base.scheme(), host),
    };
    Ok(format!("{origin}{raw}"))
}

/// An address class the SSRF guard refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    Metadata,
    LinkLocal,
    Unspecified,
    Broadcast,
    Multicast,
    Loopback,
    Private,
}

impl BlockReason {
    pub fn message(self) -> &'static str {
        match self {
            Self::Metadata => "the address is a cloud metadata endpoint",
            Self::LinkLocal => "link-local addresses are not allowed",
            Self::Unspecified => "the unspecified address is not allowed",
            Self::Broadcast => "broadcast addresses are not allowed",
            Self::Multicast => "multicast addresses are not allowed",
            Self::Loopback => "loopback addresses need allow_private: true",
            Self::Private => "private addresses need allow_private: true",
        }
    }
}

/// Classifies one address. Loopback and private ranges pass only when
/// `allow_private` is set; metadata, link-local, unspecified, broadcast, and
/// multicast never pass.
pub fn classify_ip(ip: IpAddr, allow_private: bool) -> Result<(), BlockReason> {
    let ip = crate::ip::unmap_ipv4(ip);
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            if v4 == std::net::Ipv4Addr::new(169, 254, 169, 254) {
                return Err(BlockReason::Metadata);
            }
            if v4.is_link_local() {
                return Err(BlockReason::LinkLocal);
            }
            if v4.is_unspecified() {
                return Err(BlockReason::Unspecified);
            }
            if v4.is_broadcast() {
                return Err(BlockReason::Broadcast);
            }
            if v4.is_multicast() {
                return Err(BlockReason::Multicast);
            }
            if v4.is_loopback() {
                return match allow_private {
                    true => Ok(()),
                    false => Err(BlockReason::Loopback),
                };
            }
            // Private, CGNAT (100.64/10), and 192.0.0.0/24.
            let private = v4.is_private()
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0);
            if private {
                return match allow_private {
                    true => Ok(()),
                    false => Err(BlockReason::Private),
                };
            }
            Ok(())
        }
        IpAddr::V6(v6) => {
            if v6
                == "fd00:ec2::254"
                    .parse::<std::net::Ipv6Addr>()
                    .expect("static IPv6")
            {
                return Err(BlockReason::Metadata);
            }
            if v6.is_unicast_link_local() {
                return Err(BlockReason::LinkLocal);
            }
            if v6.is_unspecified() {
                return Err(BlockReason::Unspecified);
            }
            if v6.is_multicast() {
                return Err(BlockReason::Multicast);
            }
            if v6.is_loopback() {
                return match allow_private {
                    true => Ok(()),
                    false => Err(BlockReason::Loopback),
                };
            }
            if v6.is_unique_local() {
                return match allow_private {
                    true => Ok(()),
                    false => Err(BlockReason::Private),
                };
            }
            Ok(())
        }
    }
}

/// Validates a URL's scheme and destination, returning an address to pin the
/// request to when the host is a domain. Failure mode: [`FetchError`].
async fn guard_url(url: &str, allow_private: bool) -> Result<Option<SocketAddr>, FetchError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| FetchError::InvalidUrl("url is not a valid URL".to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(FetchError::InvalidUrl(
            "only http and https URLs are allowed".to_string(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| FetchError::InvalidUrl("url has no host".to_string()))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    if let Ok(ip) = host.parse::<IpAddr>() {
        classify_ip(ip, allow_private)
            .map_err(|reason| FetchError::Blocked(reason.message().to_string()))?;
        return Ok(None);
    }
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| FetchError::InvalidUrl("url has no port".to_string()))?;
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| FetchError::Resolve)?
        .collect();
    if addresses.is_empty() {
        return Err(FetchError::Resolve);
    }
    for address in &addresses {
        classify_ip(address.ip(), allow_private)
            .map_err(|reason| FetchError::Blocked(reason.message().to_string()))?;
    }
    Ok(addresses.first().copied())
}

// ---------------------------------------------------------------------------
// HTTP fetch
// ---------------------------------------------------------------------------

/// One successfully fetched document.
#[derive(Debug, Clone)]
pub struct FetchedDocument {
    pub url: String,
    pub status: u16,
    pub body: String,
}

/// Fetches a connector URL with the merged credential headers, the configured
/// header templates, and the credential secret.
///
/// The response is capped at [`MAX_RESPONSE_BYTES`]; the body is returned to
/// the caller but never logged. Failure mode: [`FetchError`] naming the
/// failing layer.
pub async fn fetch_document(
    base_url: &str,
    config: &ConnectorConfig,
    credential_headers: &BTreeMap<String, String>,
    secret: Option<&str>,
    timeout: Duration,
) -> Result<FetchedDocument, FetchError> {
    let url = resolve_url(base_url, &config.url)?;
    let pinned = guard_url(&url, config.allow_private).await?;

    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .user_agent(format!("pareto-ai/{}", env!("CARGO_PKG_VERSION")))
        // A redirect could point at a blocked address after the guard ran.
        .redirect(reqwest::redirect::Policy::none());
    if let Some(address) = pinned {
        let host = reqwest::Url::parse(&url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string))
            .ok_or_else(|| FetchError::InvalidUrl("url has no host".to_string()))?;
        builder = builder.resolve(&host, address);
    }
    let client = builder.build().map_err(|_| FetchError::Connect)?;

    let mut request = client
        .get(&url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity");
    let mut merged = credential_headers.clone();
    for (name, value) in &config.headers {
        merged.insert(name.clone(), substitute_secret(value, secret)?);
    }
    if !merged
        .keys()
        .any(|name| name.eq_ignore_ascii_case("authorization"))
        && let Some(secret) = secret
    {
        merged.insert("authorization".to_string(), format!("Bearer {secret}"));
    }
    for (name, value) in &merged {
        let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| FetchError::Header(format!("header {name} is not a valid name")))?;
        let header_value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| FetchError::Header(format!("header {name} has an invalid value")))?;
        request = request.header(header_name, header_value);
    }

    let mut response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            FetchError::Timeout
        } else if error.is_connect() {
            FetchError::Connect
        } else {
            FetchError::Body
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| FetchError::Body)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(FetchedDocument {
        url,
        status: status.as_u16(),
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn substitute_secret(value: &str, secret: Option<&str>) -> Result<String, FetchError> {
    const PLACEHOLDER: &str = "{{secret}}";
    if !value.contains(PLACEHOLDER) {
        return Ok(value.to_string());
    }
    let secret = secret.ok_or_else(|| {
        FetchError::Header(
            "a header references {{secret}} but the credential has no secret".to_string(),
        )
    })?;
    Ok(value.replace(PLACEHOLDER, secret))
}

/// Truncates a body to [`SAMPLE_LIMIT_BYTES`] at a UTF-8 boundary.
pub fn truncate_sample(body: &str) -> (String, bool) {
    if body.len() <= SAMPLE_LIMIT_BYTES {
        return (body.to_string(), false);
    }
    let mut end = SAMPLE_LIMIT_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    (body[..end].to_string(), true)
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Fetches one stored connector, when configured.
pub async fn get_connector(
    pool: &SqlitePool,
    provider_id: &str,
    kind: ConnectorKind,
) -> Result<Option<ConnectorRow>, ConnectorError> {
    let row = sqlx::query_as::<_, ConnectorRow>(
        "SELECT provider_id, kind, config_json, enabled, updated_by, updated_at \
         FROM provider_connectors WHERE provider_id = ? AND kind = ?",
    )
    .bind(provider_id)
    .bind(kind.as_str())
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Fetches an enabled connector's parsed configuration.
pub async fn enabled_config(
    pool: &SqlitePool,
    provider_id: &str,
    kind: ConnectorKind,
) -> Result<Option<ConnectorConfig>, ConnectorError> {
    let Some(row) = get_connector(pool, provider_id, kind).await? else {
        return Ok(None);
    };
    if row.enabled == 0 {
        return Ok(None);
    }
    Ok(Some(row.config()?))
}

/// The connector view returned by `GET`.
pub async fn connector_view(
    pool: &SqlitePool,
    provider_id: &str,
    kind: ConnectorKind,
) -> Result<ConnectorView, ConnectorError> {
    let row = get_connector(pool, provider_id, kind).await?;
    Ok(match row {
        Some(row) => ConnectorView {
            provider_id: provider_id.to_string(),
            kind: kind.as_str().to_string(),
            configured: true,
            enabled: row.enabled != 0,
            config: Some(row.config()?),
            updated_at: Some(row.updated_at),
            updated_by: row.updated_by,
        },
        None => ConnectorView {
            provider_id: provider_id.to_string(),
            kind: kind.as_str().to_string(),
            configured: false,
            enabled: false,
            config: None,
            updated_at: None,
            updated_by: None,
        },
    })
}

/// Upserts a connector, audited as `connector.update`. Failure mode:
/// [`ConnectorError::Invalid`] for an unknown provider or invalid config.
pub async fn put_connector(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    provider_id: &str,
    kind: ConnectorKind,
    put: &ConnectorPut,
) -> Result<ConnectorView, ConnectorError> {
    put.config.validate(kind)?;
    if catalog::get_provider(pool, provider_id).await?.is_none() {
        return Err(ConnectorError::NotFound("provider"));
    }
    let enabled = put.enabled.unwrap_or(true);
    let config_json = serde_json::to_string(&put.config)
        .map_err(|_| ConnectorError::Invalid("config could not be serialized".to_string()))?;
    let now = catalog::now_ts();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO provider_connectors \
           (provider_id, kind, config_json, enabled, updated_by, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (provider_id, kind) DO UPDATE SET \
           config_json = excluded.config_json, \
           enabled = excluded.enabled, \
           updated_by = excluded.updated_by, \
           updated_at = excluded.updated_at",
    )
    .bind(provider_id)
    .bind(kind.as_str())
    .bind(&config_json)
    .bind(i64::from(enabled))
    .bind(actor.user_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    catalog::audit(
        &mut tx,
        actor,
        "connector.update",
        "provider",
        provider_id,
        &serde_json::json!({
            "kind": kind.as_str(),
            "enabled": enabled,
            "url": put.config.url,
            "header_names": put.config.headers.keys().collect::<Vec<_>>(),
        }),
    )
    .await?;
    tx.commit().await?;
    connector_view(pool, provider_id, kind).await
}

/// Connector summaries for every provider, grouped by provider id.
pub async fn summaries_by_provider(
    pool: &SqlitePool,
) -> Result<std::collections::HashMap<String, Vec<ConnectorSummary>>, ConnectorError> {
    let rows = sqlx::query_as::<_, (String, String, i64, String)>(
        "SELECT provider_id, kind, enabled, updated_at FROM provider_connectors \
         ORDER BY provider_id, kind",
    )
    .fetch_all(pool)
    .await?;
    let mut grouped: std::collections::HashMap<String, Vec<ConnectorSummary>> =
        std::collections::HashMap::new();
    for (provider_id, kind, enabled, updated_at) in rows {
        grouped
            .entry(provider_id)
            .or_default()
            .push(ConnectorSummary {
                kind,
                enabled: enabled != 0,
                updated_at,
            });
    }
    Ok(grouped)
}

struct RunRecord<'a> {
    provider_id: &'a str,
    credential_id: Option<&'a str>,
    kind: ConnectorKind,
    started_at: &'a str,
    finished_at: &'a str,
    status: &'a str,
    detail: Option<&'a Value>,
    error: Option<&'a str>,
}

/// Inserts one run row and prunes the kind's history to [`MAX_RUNS`]. Runs
/// inside the caller's transaction so a price write and its run commit
/// together.
async fn record_run(
    conn: &mut SqliteConnection,
    run: RunRecord<'_>,
) -> Result<String, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let detail = run.detail.map(Value::to_string);
    sqlx::query(
        "INSERT INTO connector_runs \
           (id, provider_id, credential_id, kind, status, started_at, finished_at, \
            detail_json, error) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(run.provider_id)
    .bind(run.credential_id)
    .bind(run.kind.as_str())
    .bind(run.status)
    .bind(run.started_at)
    .bind(run.finished_at)
    .bind(detail)
    .bind(run.error)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "DELETE FROM connector_runs WHERE provider_id = ? AND kind = ? AND id NOT IN (\
           SELECT id FROM connector_runs WHERE provider_id = ? AND kind = ? \
           ORDER BY finished_at DESC, rowid DESC LIMIT ?)",
    )
    .bind(run.provider_id)
    .bind(run.kind.as_str())
    .bind(run.provider_id)
    .bind(run.kind.as_str())
    .bind(MAX_RUNS)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Credential material
// ---------------------------------------------------------------------------

/// A credential's merged headers and decrypted secret for one fetch.
struct Material {
    credential: catalog::CredentialRow,
    headers: BTreeMap<String, String>,
    secret: zeroize::Zeroizing<String>,
}

/// Loads a credential and its decrypted material. Without `requested` the
/// provider's oldest credential wins; without any credential the caller gets
/// `None` (the connector still fetches, unauthenticated).
async fn credential_material(
    pool: &SqlitePool,
    secrets: &SecretBox,
    provider_id: &str,
    requested: Option<&str>,
) -> Result<Option<Material>, ConnectorError> {
    let credential = match requested {
        Some(id) => catalog::get_credential(pool, id)
            .await?
            .filter(|row| row.provider_id == provider_id),
        None => catalog::list_credentials(pool, provider_id)
            .await?
            .into_iter()
            .next(),
    };
    let Some(credential) = credential else {
        if requested.is_some() {
            return Err(ConnectorError::NotFound("credential"));
        }
        return Ok(None);
    };
    let headers = headers::merge_headers(
        secrets,
        credential
            .headers_json
            .as_deref()
            .map(|raw| (raw, credential.id.as_str())),
        None,
    )?;
    let secret = zeroize::Zeroizing::new(
        secrets
            .decrypt(&credential.secret_cipher, &credential.id)
            .map_err(CatalogError::from)?,
    );
    Ok(Some(Material {
        credential,
        headers,
        secret,
    }))
}

async fn provider_base_url(pool: &SqlitePool, provider_id: &str) -> Result<String, ConnectorError> {
    catalog::get_provider(pool, provider_id)
        .await?
        .map(|provider| provider.base_url)
        .ok_or(ConnectorError::NotFound("provider"))
}

// ---------------------------------------------------------------------------
// Sample and test
// ---------------------------------------------------------------------------

/// Fetches a connector document and returns a truncated raw body. A draft
/// config may be tested before it is saved.
pub async fn sample(
    pool: &SqlitePool,
    secrets: &SecretBox,
    provider_id: &str,
    kind: ConnectorKind,
    draft: Option<&ConnectorConfig>,
    credential_id: Option<&str>,
) -> Result<SampleView, ConnectorError> {
    let config = match draft {
        Some(config) => config.clone(),
        None => enabled_config(pool, provider_id, kind)
            .await?
            .ok_or_else(|| {
                ConnectorError::Invalid(format!("no {} connector is configured", kind.as_str()))
            })?,
    };
    config.validate(kind)?;
    let base_url = provider_base_url(pool, provider_id).await?;
    let material = credential_material(pool, secrets, provider_id, credential_id).await?;
    let empty = BTreeMap::new();
    let (headers, secret) = match material.as_ref() {
        Some(material) => (&material.headers, Some(material.secret.as_str())),
        None => (&empty, None),
    };
    let fetched = fetch_document(&base_url, &config, headers, secret, FETCH_TIMEOUT)
        .await
        .map_err(|error| ConnectorError::Upstream(error.to_string()))?;
    let (body, truncated) = truncate_sample(&fetched.body);
    Ok(SampleView {
        kind: kind.as_str().to_string(),
        url: fetched.url,
        status: fetched.status,
        bytes: fetched.body.len(),
        truncated,
        body,
        fetched_at: catalog::now_ts(),
    })
}

/// Applies a draft or stored connector to a fresh fetch and returns what it
/// extracted. Writes nothing — no run row, no price, no balance.
pub async fn test_connector(
    pool: &SqlitePool,
    secrets: &SecretBox,
    provider_id: &str,
    kind: ConnectorKind,
    draft: Option<&ConnectorConfig>,
    credential_id: Option<&str>,
) -> Result<ConnectorTestView, ConnectorError> {
    let config = match draft {
        Some(config) => config.clone(),
        None => enabled_config(pool, provider_id, kind)
            .await?
            .ok_or_else(|| {
                ConnectorError::Invalid(format!("no {} connector is configured", kind.as_str()))
            })?,
    };
    config.validate(kind)?;
    let base_url = provider_base_url(pool, provider_id).await?;
    let material = credential_material(pool, secrets, provider_id, credential_id).await?;
    let empty = BTreeMap::new();
    let (headers, secret) = match material.as_ref() {
        Some(material) => (&material.headers, Some(material.secret.as_str())),
        None => (&empty, None),
    };
    let fetched = fetch_document(&base_url, &config, headers, secret, FETCH_TIMEOUT)
        .await
        .map_err(|error| ConnectorError::Upstream(error.to_string()))?;
    let document: Value = serde_json::from_str(&fetched.body)
        .map_err(|_| ConnectorError::Invalid("the response is not valid JSON".to_string()))?;

    let mut view = ConnectorTestView {
        kind: kind.as_str().to_string(),
        ok: true,
        url: fetched.url,
        entries_seen: None,
        matched: None,
        skipped: None,
        samples: Vec::new(),
        errors: Vec::new(),
        warnings: Vec::new(),
        currency: None,
        balance: None,
        health: None,
    };
    match kind {
        ConnectorKind::Catalog => {
            let catalog = config.catalog.as_ref().expect("validated");
            match extract_catalog(&document, catalog) {
                Ok(extraction) => {
                    view.entries_seen = Some(extraction.entries_seen);
                    view.matched = Some(extraction.matched);
                    view.skipped = Some(extraction.skipped);
                    view.samples = extraction.samples;
                    view.errors = extraction.errors;
                    view.warnings = extraction.warnings;
                    view.currency = Some(catalog.unit.currency.to_ascii_uppercase());
                    view.ok = extraction.matched > 0;
                }
                Err(error) => {
                    view.ok = false;
                    view.errors.push(error.to_string());
                }
            }
        }
        ConnectorKind::Balance => {
            let balance = config.balance.as_ref().expect("validated");
            match extract_balance(&document, balance) {
                Ok(balance) => {
                    view.currency = Some(balance.currency.clone());
                    view.balance = Some(balance);
                }
                Err(error) => {
                    view.ok = false;
                    view.errors.push(error.to_string());
                }
            }
        }
        ConnectorKind::Health => {
            let health = config.health.clone().unwrap_or_default();
            let extracted = extract_health(Some(&document), &health);
            view.ok = extracted.ok;
            if let Some(detail) = extracted.detail.clone() {
                view.errors.push(detail);
            }
            view.health = Some(extracted);
        }
    }
    Ok(view)
}

// ---------------------------------------------------------------------------
// Price refresh
// ---------------------------------------------------------------------------

/// Applies the provider's catalog connector to the catalog: upserts
/// `source = 'provider_api'` prices and records one run row.
///
/// Manual prices are never overwritten (D10). Offerings are never created
/// here — an entry with no matching offering is counted `skipped_unresolved`
/// (discovery owns the catalog). Failure mode: [`ConnectorError`]; a fetch
/// failure writes an error run and returns an error view only through
/// [`refresh_prices_for_discovery`].
pub async fn refresh_prices(
    pool: &SqlitePool,
    secrets: &SecretBox,
    actor: Option<&Actor<'_>>,
    provider_id: &str,
    credential_id: Option<&str>,
) -> Result<PriceRefreshView, ConnectorError> {
    let config = enabled_config(pool, provider_id, ConnectorKind::Catalog)
        .await?
        .ok_or_else(|| ConnectorError::Invalid("no catalog connector is configured".to_string()))?;
    config.validate(ConnectorKind::Catalog)?;
    let catalog_config = config.catalog.clone().expect("validated");
    let base_url = provider_base_url(pool, provider_id).await?;
    let material = credential_material(pool, secrets, provider_id, credential_id).await?;
    let (headers, secret) = match material.as_ref() {
        Some(material) => (&material.headers, Some(material.secret.as_str())),
        None => (&BTreeMap::new(), None),
    };

    let started_at = catalog::now_ts();
    let fetched = match fetch_document(&base_url, &config, headers, secret, FETCH_TIMEOUT).await {
        Ok(fetched) => fetched,
        Err(error) => {
            let finished_at = catalog::now_ts();
            let mut tx = pool.begin().await?;
            record_run(
                &mut tx,
                RunRecord {
                    provider_id,
                    credential_id: material.as_ref().map(|m| m.credential.id.as_str()),
                    kind: ConnectorKind::Catalog,
                    started_at: &started_at,
                    finished_at: &finished_at,
                    status: "error",
                    detail: None,
                    error: Some(&error.to_string()),
                },
            )
            .await?;
            tx.commit().await?;
            return Err(ConnectorError::Upstream(error.to_string()));
        }
    };
    let document: Value = serde_json::from_str(&fetched.body)
        .map_err(|_| ConnectorError::Invalid("the response is not valid JSON".to_string()))?;
    let extraction = extract_catalog(&document, &catalog_config)?;

    let finished_at = catalog::now_ts();
    let mut tx = pool.begin().await?;
    let mut created = 0usize;
    let mut updated = 0usize;
    let mut unchanged = 0usize;
    let mut skipped_manual = 0usize;
    // Entries the mapping itself could not resolve count as unresolved; the
    // loop below adds entries with no matching offering.
    let mut skipped_unresolved = extraction.skipped;
    for entry in &extraction.entries {
        let offering_id: Option<String> = sqlx::query_scalar(
            "SELECT o.id FROM offerings o JOIN models m ON m.id = o.model_id \
             WHERE o.provider_id = ? AND o.upstream_model_id = ? COLLATE NOCASE",
        )
        .bind(provider_id)
        .bind(&entry.model_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(offering_id) = offering_id else {
            skipped_unresolved += 1;
            continue;
        };
        let existing: Option<String> =
            sqlx::query_scalar("SELECT source FROM prices WHERE offering_id = ?")
                .bind(&offering_id)
                .fetch_optional(&mut *tx)
                .await?;
        let outcome = catalog::apply_sync_price(
            &mut tx,
            &offering_id,
            &SyncPrice {
                input_per_m: entry.input_per_m,
                output_per_m: entry.output_per_m,
                cache_read_per_m: entry.cache_read_per_m,
                cache_write_per_m: entry.cache_write_per_m,
                reasoning_per_m: entry.reasoning_per_m,
                request_fee: None,
                currency: entry.currency.clone(),
                variable_price: entry.variable_price,
            },
            "provider_api",
            &fetched.url,
            &finished_at,
        )
        .await?;
        match outcome {
            SyncPriceOutcome::Changed if existing.is_some() => updated += 1,
            SyncPriceOutcome::Changed => created += 1,
            SyncPriceOutcome::Unchanged => unchanged += 1,
            SyncPriceOutcome::SkippedManual => skipped_manual += 1,
        }
    }

    let detail = serde_json::json!({
        "entries_seen": extraction.entries_seen,
        "matched": extraction.matched,
        "skipped": extraction.skipped,
        "created": created,
        "updated": updated,
        "unchanged": unchanged,
        "skipped_manual": skipped_manual,
        "skipped_unresolved": skipped_unresolved,
        "source_url": fetched.url,
    });
    let run_id = record_run(
        &mut tx,
        RunRecord {
            provider_id,
            credential_id: material.as_ref().map(|m| m.credential.id.as_str()),
            kind: ConnectorKind::Catalog,
            started_at: &started_at,
            finished_at: &finished_at,
            status: "success",
            detail: Some(&detail),
            error: None,
        },
    )
    .await?;
    if let Some(actor) = actor {
        catalog::audit(
            &mut tx,
            actor,
            "provider.prices_refresh",
            "provider",
            provider_id,
            &detail,
        )
        .await?;
    }
    tx.commit().await?;

    Ok(PriceRefreshView {
        ok: true,
        provider_id: provider_id.to_string(),
        run_id: Some(run_id),
        entries_seen: extraction.entries_seen,
        created,
        updated,
        unchanged,
        skipped_manual,
        skipped_unresolved,
        source_url: fetched.url,
        fetched_at: finished_at,
        error: None,
    })
}

/// The discovery integration hook: refresh after an apply when the catalog
/// connector asked for it. Never fails discovery — a connector problem is
/// reported inside the merged `prices` object.
pub async fn refresh_prices_for_discovery(
    pool: &SqlitePool,
    secrets: &SecretBox,
    actor: &Actor<'_>,
    provider_id: &str,
    credential_id: &str,
) -> Option<PriceRefreshView> {
    let config = match enabled_config(pool, provider_id, ConnectorKind::Catalog).await {
        Ok(Some(config)) => config,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(provider_id, error = %error, "catalog connector could not be read");
            return None;
        }
    };
    if !config
        .catalog
        .as_ref()
        .is_some_and(|c| c.apply_on_discovery)
    {
        return None;
    }
    match refresh_prices(pool, secrets, Some(actor), provider_id, Some(credential_id)).await {
        Ok(view) => Some(view),
        Err(error) => Some(PriceRefreshView {
            ok: false,
            provider_id: provider_id.to_string(),
            run_id: None,
            entries_seen: 0,
            created: 0,
            updated: 0,
            unchanged: 0,
            skipped_manual: 0,
            skipped_unresolved: 0,
            source_url: String::new(),
            fetched_at: catalog::now_ts(),
            error: Some(error.to_string()),
        }),
    }
}

// ---------------------------------------------------------------------------
// Balance refresh
// ---------------------------------------------------------------------------

/// Fetches and stores one credential's balance. A provider report of
/// `available: false` stores `balance_available = 0` and a NULL value — never
/// zero. A fetch failure records `balance_error` and leaves the last value.
pub async fn refresh_balance(
    pool: &SqlitePool,
    secrets: &SecretBox,
    provider_id: &str,
    credential_id: &str,
) -> Result<BalanceRefreshView, ConnectorError> {
    let config = enabled_config(pool, provider_id, ConnectorKind::Balance)
        .await?
        .ok_or_else(|| ConnectorError::Invalid("no balance connector is configured".to_string()))?;
    config.validate(ConnectorKind::Balance)?;
    let balance_config = config.balance.clone().expect("validated");
    let base_url = provider_base_url(pool, provider_id).await?;
    let material = credential_material(pool, secrets, provider_id, Some(credential_id))
        .await?
        .ok_or(ConnectorError::NotFound("credential"))?;

    let started_at = catalog::now_ts();
    let fetched = fetch_document(
        &base_url,
        &config,
        &material.headers,
        Some(material.secret.as_str()),
        FETCH_TIMEOUT,
    )
    .await;
    let outcome = match fetched {
        Ok(fetched) => match serde_json::from_str::<Value>(&fetched.body) {
            Ok(document) => extract_balance(&document, &balance_config),
            Err(_) => Err(ConnectorError::Invalid(
                "the response is not valid JSON".to_string(),
            )),
        },
        Err(error) => Err(ConnectorError::Upstream(error.to_string())),
    };
    let finished_at = catalog::now_ts();
    let mut tx = pool.begin().await?;
    match outcome {
        Ok(balance) => {
            sqlx::query(
                "UPDATE provider_credentials SET \
                   balance_usd = ?, \
                   balance_source = 'provider_api', \
                   balance_updated_at = ?, \
                   balance_updated_by = NULL, \
                   balance_available = ?, \
                   balance_currency = ?, \
                   balance_error = NULL, \
                   balance_fetched_at = ?, \
                   updated_at = ? \
                 WHERE id = ?",
            )
            .bind(balance.balance_usd)
            .bind(&finished_at)
            .bind(i64::from(balance.available))
            .bind(&balance.currency)
            .bind(&finished_at)
            .bind(&finished_at)
            .bind(credential_id)
            .execute(&mut *tx)
            .await?;
            let detail = serde_json::json!({
                "available": balance.available,
                "balance_usd": balance.balance_usd,
                "currency": balance.currency,
                "period_spend": balance.period_spend,
                "burn_rate": balance.burn_rate,
                "days_remaining": balance.days_remaining,
            });
            record_run(
                &mut tx,
                RunRecord {
                    provider_id,
                    credential_id: Some(credential_id),
                    kind: ConnectorKind::Balance,
                    started_at: &started_at,
                    finished_at: &finished_at,
                    status: "success",
                    detail: Some(&detail),
                    error: None,
                },
            )
            .await?;
            tx.commit().await?;
            Ok(BalanceRefreshView {
                ok: true,
                credential_id: credential_id.to_string(),
                available: balance.available,
                balance_usd: balance.balance_usd,
                currency: balance.currency,
                period_spend: balance.period_spend,
                burn_rate: balance.burn_rate,
                days_remaining: balance.days_remaining,
                error: None,
                fetched_at: finished_at,
            })
        }
        Err(error) => {
            // Keep the last known value; mark it unavailable because this
            // fetch could not confirm it. Routing with the default warn gate
            // is unaffected either way.
            let message = error.to_string();
            sqlx::query(
                "UPDATE provider_credentials SET \
                   balance_available = 0, \
                   balance_error = ?, \
                   balance_fetched_at = ?, \
                   updated_at = ? \
                 WHERE id = ?",
            )
            .bind(&message)
            .bind(&finished_at)
            .bind(&finished_at)
            .bind(credential_id)
            .execute(&mut *tx)
            .await?;
            record_run(
                &mut tx,
                RunRecord {
                    provider_id,
                    credential_id: Some(credential_id),
                    kind: ConnectorKind::Balance,
                    started_at: &started_at,
                    finished_at: &finished_at,
                    status: "error",
                    detail: None,
                    error: Some(&message),
                },
            )
            .await?;
            tx.commit().await?;
            Ok(BalanceRefreshView {
                ok: false,
                credential_id: credential_id.to_string(),
                available: false,
                balance_usd: None,
                currency: "USD".to_string(),
                period_spend: None,
                burn_rate: None,
                days_remaining: None,
                error: Some(message),
                fetched_at: finished_at,
            })
        }
    }
}

/// Refreshes every credential of a provider, isolating failures per
/// credential.
pub async fn refresh_provider_balances(
    pool: &SqlitePool,
    secrets: &SecretBox,
    provider_id: &str,
) -> Result<ProviderBalancesView, ConnectorError> {
    if catalog::get_provider(pool, provider_id).await?.is_none() {
        return Err(ConnectorError::NotFound("provider"));
    }
    let credentials = catalog::list_credentials(pool, provider_id).await?;
    let mut results = Vec::with_capacity(credentials.len());
    let mut failed = 0usize;
    for credential in credentials {
        match refresh_balance(pool, secrets, provider_id, &credential.id).await {
            Ok(view) => {
                if !view.ok {
                    failed += 1;
                }
                results.push(view);
            }
            Err(error) => {
                failed += 1;
                results.push(BalanceRefreshView {
                    ok: false,
                    credential_id: credential.id,
                    available: false,
                    balance_usd: None,
                    currency: "USD".to_string(),
                    period_spend: None,
                    burn_rate: None,
                    days_remaining: None,
                    error: Some(error.to_string()),
                    fetched_at: catalog::now_ts(),
                });
            }
        }
    }
    Ok(ProviderBalancesView {
        provider_id: provider_id.to_string(),
        refreshed: results.len().saturating_sub(failed),
        failed,
        results,
    })
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

/// Probes the provider's health connector, or `None` when none is configured.
/// Returns a [`crate::probe::ProbeResult`]-shaped answer so the credential
/// Test action records it exactly like the `/models` probe.
pub async fn health_probe(
    pool: &SqlitePool,
    secrets: &SecretBox,
    provider_id: &str,
    credential_id: &str,
) -> Result<Option<crate::probe::ProbeResult>, ConnectorError> {
    let Some(config) = enabled_config(pool, provider_id, ConnectorKind::Health).await? else {
        return Ok(None);
    };
    let health = config.health.clone().unwrap_or_default();
    let base_url = provider_base_url(pool, provider_id).await?;
    let material = credential_material(pool, secrets, provider_id, Some(credential_id))
        .await?
        .ok_or(ConnectorError::NotFound("credential"))?;
    let started = std::time::Instant::now();
    let fetched = fetch_document(
        &base_url,
        &config,
        &material.headers,
        Some(material.secret.as_str()),
        FETCH_TIMEOUT,
    )
    .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let (ok, http_status, error) = match fetched {
        Ok(fetched) => {
            let document: Option<Value> = serde_json::from_str(&fetched.body).ok();
            let extracted = extract_health(document.as_ref(), &health);
            (
                extracted.ok,
                Some(i64::from(fetched.status)),
                extracted.detail,
            )
        }
        Err(FetchError::Status(status)) => (false, Some(i64::from(status)), None),
        Err(error) => (false, None, Some(error.to_string())),
    };
    Ok(Some(crate::probe::ProbeResult {
        ok,
        http_status,
        latency_ms,
        error,
    }))
}

// ---------------------------------------------------------------------------
// Scheduling
// ---------------------------------------------------------------------------

/// Consecutive error runs raise the retry delay from 1 minute to the 1-hour
/// cap: 1, 2, 4, … 60 minutes.
pub fn retry_delay(consecutive_errors: u32) -> Duration {
    let exponent = consecutive_errors.saturating_sub(1).min(6);
    Duration::from_secs(60 * 2u64.pow(exponent)).min(Duration::from_secs(3600))
}

/// True when a credential's balance is due for a refresh. `last_finished_at`
/// is the newest balance run (any status); `consecutive_errors` counts the
/// newest runs with status `error`. Backoff and jitter are applied on top of
/// the connector cadence.
pub fn refresh_due(
    last_finished_at: Option<&str>,
    consecutive_errors: u32,
    interval_seconds: u64,
    jitter_seconds: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(last) = last_finished_at else {
        return true;
    };
    let Ok(last) = chrono::DateTime::parse_from_rfc3339(last) else {
        return true;
    };
    let base = if consecutive_errors == 0 {
        Duration::from_secs(interval_seconds)
    } else {
        // Errors only ever slow the schedule down; the normal cadence is the
        // floor and the 1-hour cap is the ceiling.
        retry_delay(consecutive_errors).max(Duration::from_secs(interval_seconds))
    };
    let wait = base + Duration::from_secs(jitter_seconds);
    now.signed_duration_since(last.with_timezone(&chrono::Utc))
        .to_std()
        .map(|elapsed| elapsed >= wait)
        .unwrap_or(true)
}

/// The background balance job: every [`BALANCE_TICK`] refreshes the
/// credentials whose connector cadence (with backoff and jitter) says they are
/// due. Failures are logged and retried; the job never exits.
pub fn spawn_balance_job(
    pool: SqlitePool,
    secrets: std::sync::Arc<SecretBox>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(error) = run_balance_sweep(&pool, &secrets).await {
                tracing::warn!(error = %error, "balance sweep failed");
            }
            tokio::time::sleep(BALANCE_TICK).await;
        }
    })
}

/// One sweep over every enabled balance connector. Exposed for tests.
pub async fn run_balance_sweep(
    pool: &SqlitePool,
    secrets: &SecretBox,
) -> Result<usize, ConnectorError> {
    use rand::Rng;
    let connectors = sqlx::query_as::<_, (String, String)>(
        "SELECT provider_id, config_json FROM provider_connectors \
         WHERE kind = 'balance' AND enabled = 1",
    )
    .fetch_all(pool)
    .await?;
    let mut refreshed = 0usize;
    for (provider_id, config_json) in connectors {
        let Ok(config) = serde_json::from_str::<ConnectorConfig>(&config_json) else {
            tracing::warn!(provider_id, "balance connector config is corrupt");
            continue;
        };
        let interval = config
            .refresh_seconds(ConnectorKind::Balance)
            .unwrap_or(DEFAULT_REFRESH_SECONDS);
        let credentials = catalog::list_credentials(pool, &provider_id).await?;
        for credential in credentials {
            let runs: Vec<(String, String)> = sqlx::query_as(
                "SELECT status, finished_at FROM connector_runs \
                 WHERE provider_id = ? AND kind = 'balance' AND credential_id = ? \
                 ORDER BY finished_at DESC, rowid DESC LIMIT 20",
            )
            .bind(&provider_id)
            .bind(&credential.id)
            .fetch_all(pool)
            .await?;
            let last = runs.first().map(|(_, finished)| finished.as_str());
            let errors = runs
                .iter()
                .take_while(|(status, _)| status == "error")
                .count() as u32;
            let jitter = rand::rng().random_range(0..=60);
            if !refresh_due(last, errors, interval, jitter, chrono::Utc::now()) {
                continue;
            }
            match refresh_balance(pool, secrets, &provider_id, &credential.id).await {
                Ok(view) if view.ok => refreshed += 1,
                Ok(view) => tracing::warn!(
                    provider_id,
                    credential_id = %credential.id,
                    error = view.error.as_deref().unwrap_or("balance unavailable"),
                    "balance refresh failed"
                ),
                Err(error) => tracing::warn!(
                    provider_id,
                    credential_id = %credential.id,
                    error = %error,
                    "balance refresh failed"
                ),
            }
        }
    }
    Ok(refreshed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAIL_MODELS: &str = include_str!("../tests/fixtures/sailresearch_models.json");
    const SAIL_USAGE: &str = include_str!("../tests/fixtures/sailresearch_usage.json");
    const OPENROUTER_MODELS: &str = include_str!("../tests/fixtures/openrouter_models.json");

    fn sail_template() -> ConnectorConfig {
        templates()
            .into_iter()
            .find(|template| template.id == "sailresearch")
            .expect("sailresearch template")
            .configs
            .catalog
            .expect("catalog config")
    }

    fn sail_balance_template() -> ConnectorConfig {
        templates()
            .into_iter()
            .find(|template| template.id == "sailresearch")
            .expect("sailresearch template")
            .configs
            .balance
            .expect("balance config")
    }

    #[test]
    fn pointers_resolve_objects_arrays_and_escapes() {
        let document = serde_json::json!({
            "data": [
                {"id": "a", "price": {"in": 1.5, "out": "2.5"}},
                {"id": "b", "price": {"in": -1, "out": 4}}
            ],
            "a/b": {"~key~": "escaped"}
        });
        assert_eq!(
            resolve_pointer(&document, "/data/1/id").and_then(Value::as_str),
            Some("b")
        );
        assert_eq!(
            resolve_pointer(&document, "/data/0/price/out").and_then(number),
            Some(2.5),
            "numeric strings are accepted"
        );
        assert_eq!(
            resolve_pointer(&document, "/a~1b/~0key~0").and_then(Value::as_str),
            Some("escaped"),
            "RFC 6901 escapes"
        );
        assert!(resolve_pointer(&document, "/data/2").is_none());
        assert!(
            resolve_pointer(&document, "/data/01").is_none(),
            "leading zero"
        );
        assert!(resolve_pointer(&document, "/data/-").is_none());
        assert!(resolve_pointer(&document, "/missing").is_none());
        assert!(
            resolve_pointer(&document, "data").is_none(),
            "must start with /"
        );
        assert!(
            resolve_pointer(&document, "").is_some(),
            "the empty pointer is the document"
        );
    }

    #[test]
    fn sail_template_resolves_every_fixture_model_at_the_balanced_tier() {
        let document: Value = serde_json::from_str(SAIL_MODELS).expect("fixture parses");
        let config = sail_template();
        let catalog = config.catalog.expect("catalog section");
        let extraction = extract_catalog(&document, &catalog).expect("mapping applies");

        assert_eq!(extraction.entries_seen, 10);
        assert_eq!(extraction.matched, 10);
        assert_eq!(extraction.skipped, 0);
        assert!(extraction.errors.is_empty(), "{:?}", extraction.errors);

        let deepseek = extraction
            .entries
            .iter()
            .find(|entry| entry.model_id == "deepseek-ai/DeepSeek-V4.1-Flash-1017")
            .expect("deepseek entry");
        // The Sail template declares `multiplier: 0.01` (prices quoted in
        // cents, D44): each raw number is scaled once, with no provider branch.
        assert!(close(deepseek.input_per_m, 0.0035));
        assert!(close(deepseek.cache_read_per_m, 0.0007));
        assert!(close(deepseek.output_per_m, 0.0105));
        assert!(!deepseek.variable_price);
        assert_eq!(deepseek.currency, "USD");

        // Discovery reads the same document's metadata without prices.
        let discovered = extract_discovery_entries(&document, &catalog).expect("entries");
        let deepseek = discovered
            .iter()
            .find(|entry| entry.model_id == "deepseek-ai/DeepSeek-V4.1-Flash-1017")
            .expect("deepseek discovery entry");
        assert_eq!(deepseek.modalities_input, Some(vec!["text".to_string()]));
        assert_eq!(deepseek.context_length, Some(163_840));
        assert_eq!(deepseek.max_output_tokens, Some(65_536));
    }

    #[test]
    fn openrouter_template_scales_per_token_strings_to_per_million() {
        let document: Value = serde_json::from_str(OPENROUTER_MODELS).expect("fixture parses");
        let template = templates()
            .into_iter()
            .find(|template| template.id == "openrouter_per_token")
            .expect("template");
        let extraction = extract_catalog(
            &document,
            template
                .configs
                .catalog
                .as_ref()
                .expect("catalog config")
                .catalog
                .as_ref()
                .expect("catalog section"),
        )
        .expect("mapping applies");
        assert!(extraction.matched > 0);
        let entry = extraction
            .entries
            .iter()
            .find(|entry| entry.input_per_m.is_some_and(|value| value > 1.0))
            .expect("a per-token string scaled above 1 USD/M");
        assert!(entry.input_per_m.expect("input") > 0.0);
    }

    #[test]
    fn missing_and_non_numeric_prices_skip_that_entry_only() {
        let document = serde_json::json!({
            "data": [
                {"id": "missing", "price": {"input": 1}},
                {"id": "text", "price": {"input": "n/a", "output": 2}},
                {"id": "ok", "price": {"input": "0.5", "output": "2"}}
            ]
        });
        let config = CatalogConfig {
            entries: "/data".to_string(),
            model_id: "/id".to_string(),
            input: Some("/price/input".to_string()),
            output: Some("/price/output".to_string()),
            cache_read: None,
            cache_write: None,
            reasoning: None,
            unit: PriceUnit::default(),
            modalities_input: None,
            modalities_output: None,
            context_length: None,
            max_output: None,
            apply_on_discovery: false,
            refresh_seconds: None,
        };
        let extraction = extract_catalog(&document, &config).expect("mapping");
        assert_eq!(extraction.entries_seen, 3);
        assert_eq!(extraction.matched, 1, "only the complete entry survives");
        assert_eq!(extraction.skipped, 2);
        assert_eq!(extraction.entries[0].model_id, "ok");
        assert_eq!(extraction.entries[0].input_per_m, Some(0.5));
        assert!(
            extraction
                .errors
                .iter()
                .any(|error| error.contains("missing"))
        );
        assert!(
            extraction
                .errors
                .iter()
                .any(|error| error.contains("not numeric"))
        );
    }

    #[test]
    fn negative_components_follow_the_variable_rule() {
        let document = serde_json::json!({
            "data": [{"id": "auto", "price": {"input": -1, "output": "-1"}}]
        });
        let config = CatalogConfig {
            entries: "/data".to_string(),
            model_id: "/id".to_string(),
            input: Some("/price/input".to_string()),
            output: Some("/price/output".to_string()),
            cache_read: None,
            cache_write: None,
            reasoning: None,
            unit: PriceUnit {
                source: UnitSource::PerToken,
                ..PriceUnit::default()
            },
            modalities_input: None,
            modalities_output: None,
            context_length: None,
            max_output: None,
            apply_on_discovery: false,
            refresh_seconds: None,
        };
        let extraction = extract_catalog(&document, &config).expect("mapping");
        assert_eq!(extraction.matched, 1, "a variable entry is still mapped");
        let entry = &extraction.entries[0];
        assert!(entry.variable_price);
        assert_eq!(entry.input_per_m, None, "never a fake number");
        assert_eq!(entry.output_per_m, None);
    }

    #[test]
    fn unit_sources_parse_and_validate() {
        assert_eq!(
            serde_json::from_str::<UnitSource>("\"per_token\"").unwrap(),
            UnitSource::PerToken
        );
        assert_eq!(
            serde_json::from_str::<UnitSource>("\"per_million_tokens\"").unwrap(),
            UnitSource::PerMillionTokens
        );
        assert_eq!(
            serde_json::from_str::<UnitSource>("\"custom\"").unwrap(),
            UnitSource::Custom
        );
        assert!(serde_json::from_str::<UnitSource>("\"per_request\"").is_err());
        assert!(validate_refresh_seconds(60, "x").is_ok());
        assert!(validate_refresh_seconds(900, "x").is_ok());
        assert!(validate_refresh_seconds(86_400, "x").is_ok());
        assert!(validate_refresh_seconds(59, "x").is_err());
        assert!(validate_refresh_seconds(86_401, "x").is_err());
    }

    fn close(actual: Option<f64>, expected: f64) -> bool {
        actual.is_some_and(|value| (value - expected).abs() < 1e-12)
    }

    fn price_config(unit: PriceUnit) -> CatalogConfig {
        CatalogConfig {
            entries: "/data".to_string(),
            model_id: "/id".to_string(),
            input: Some("/price/input".to_string()),
            output: Some("/price/output".to_string()),
            cache_read: Some("/price/cache".to_string()),
            cache_write: None,
            reasoning: None,
            unit,
            modalities_input: None,
            modalities_output: None,
            context_length: None,
            max_output: None,
            apply_on_discovery: false,
            refresh_seconds: None,
        }
    }

    fn three_prices() -> Value {
        serde_json::json!({
            "data": [{"id": "m", "price": {"input": 1, "output": 2, "cache": 0.07}}]
        })
    }

    #[test]
    fn unit_conversion_table_covers_every_source_and_fx() {
        let cases: [(UnitSource, f64, &str, Option<f64>, f64); 4] = [
            (UnitSource::PerToken, 1.0, "USD", None, 1_000_000.0),
            (UnitSource::PerMillionTokens, 1.0, "USD", None, 1.0),
            (UnitSource::Custom, 0.01, "USD", None, 0.01),
            (UnitSource::Custom, 1.0, "EUR", Some(1.1), 1.1),
        ];
        for (source, multiplier, currency, fx, expected) in cases {
            let unit = PriceUnit {
                source,
                multiplier,
                currency: currency.to_string(),
                fx_to_usd: fx,
                ..PriceUnit::default()
            };
            let extraction =
                extract_catalog(&three_prices(), &price_config(unit)).expect("mapping");
            let entry = &extraction.entries[0];
            assert_eq!(
                entry.input_per_m.map(|v| (v * 1e6).round() / 1e6),
                Some((expected * 1e6).round() / 1e6),
                "{source:?} {multiplier} {currency} {fx:?}"
            );
            assert_eq!(entry.currency, "USD", "fx or USD makes it routable");
            assert!(!entry.variable_price);
        }
    }

    #[test]
    fn custom_multiplier_uses_the_shipped_sail_value_without_a_provider_branch() {
        let extraction = extract_catalog(
            &three_prices(),
            &price_config(PriceUnit {
                source: UnitSource::Custom,
                multiplier: 0.01,
                ..PriceUnit::default()
            }),
        )
        .expect("mapping");
        assert!(close(extraction.entries[0].input_per_m, 0.01));
        assert!(close(extraction.entries[0].cache_read_per_m, 0.0007));
    }

    #[test]
    fn non_usd_without_fx_is_stored_unpriced_and_warned() {
        let extraction = extract_catalog(
            &three_prices(),
            &price_config(PriceUnit {
                source: UnitSource::PerMillionTokens,
                currency: "EUR".to_string(),
                ..PriceUnit::default()
            }),
        )
        .expect("mapping");
        let entry = &extraction.entries[0];
        assert_eq!(entry.currency, "EUR");
        assert!(entry.variable_price, "non-USD without FX is unpriced");
        assert_eq!(entry.input_per_m, None);
        assert_eq!(entry.output_per_m, None);
        let sample = &extraction.samples[0];
        assert_eq!(sample.raw.input, Some(1.0), "the raw number is still shown");
        assert_eq!(sample.converted_per_m.input, Some(1.0));
        assert!(
            sample
                .warnings
                .iter()
                .any(|warning| warning.contains("no fx_to_usd")),
            "{:?}",
            sample.warnings
        );
    }

    #[test]
    fn implausible_input_prices_warn_but_still_extract() {
        let low = extract_catalog(
            &three_prices(),
            &price_config(PriceUnit {
                source: UnitSource::Custom,
                multiplier: 0.001,
                ..PriceUnit::default()
            }),
        )
        .expect("mapping");
        assert_eq!(low.entries[0].input_per_m, Some(0.001));
        assert_eq!(low.samples[0].raw.input, Some(1.0));
        assert_eq!(low.samples[0].converted_per_m.input, Some(0.001));
        assert!(
            low.samples[0].warnings.is_empty(),
            "0.001 is inside the accepted band"
        );

        let too_low = extract_catalog(
            &three_prices(),
            &price_config(PriceUnit {
                source: UnitSource::Custom,
                multiplier: 0.0001,
                ..PriceUnit::default()
            }),
        )
        .expect("mapping");
        assert!(
            too_low.samples[0]
                .warnings
                .iter()
                .any(|warning| warning.contains("check the unit")),
            "{:?}",
            too_low.samples[0].warnings
        );

        let too_high = extract_catalog(
            &serde_json::json!({"data": [{"id": "m", "price": {"input": 1001, "output": 2}}]}),
            &price_config(PriceUnit::default()),
        )
        .expect("mapping");
        assert!(
            too_high.samples[0]
                .warnings
                .iter()
                .any(|warning| warning.contains("check the unit"))
        );
    }

    #[test]
    fn a_zero_custom_multiplier_is_unpriced_and_warned() {
        let extraction = extract_catalog(
            &three_prices(),
            &price_config(PriceUnit {
                source: UnitSource::Custom,
                multiplier: 0.0,
                ..PriceUnit::default()
            }),
        )
        .expect("mapping");
        assert!(extraction.entries[0].variable_price);
        assert_eq!(extraction.entries[0].input_per_m, None);
        assert!(
            extraction
                .warnings
                .iter()
                .any(|warning| warning.contains("not a positive number"))
        );
    }

    #[test]
    fn sail_fixture_raw_seven_hundredths_becomes_00007_with_the_warning() {
        let document: Value = serde_json::from_str(SAIL_MODELS).expect("fixture parses");
        let extraction = extract_catalog(
            &document,
            sail_template().catalog.as_ref().expect("catalog"),
        )
        .expect("mapping");
        let mistral = extraction
            .entries
            .iter()
            .find(|entry| entry.model_id == "mistralai/Mistral-Large-3")
            .expect("mistral");
        assert!(close(mistral.input_per_m, 0.0007));
        let mistral = extraction
            .samples
            .iter()
            .find(|sample| sample.model_id.as_deref() == Some("mistralai/Mistral-Large-3"))
            .expect("mistral sample");
        assert!(close(mistral.raw.input, 0.07));
        assert!(close(mistral.converted_per_m.input, 0.0007));
        assert!(
            mistral
                .warnings
                .iter()
                .any(|warning| warning.contains("check the unit")),
            "{:?}",
            mistral.warnings
        );
    }

    #[test]
    fn config_validation_names_the_failing_field() {
        let mut config = sail_template();
        config.url.clear();
        assert!(config.validate(ConnectorKind::Catalog).is_err());

        let mut config = sail_template();
        config.catalog.as_mut().unwrap().entries = "data".to_string();
        let error = config
            .validate(ConnectorKind::Catalog)
            .expect_err("bad pointer");
        assert!(error.to_string().contains("catalog.entries"), "{error}");

        let mut config = sail_template();
        config.catalog.as_mut().unwrap().input = None;
        config.catalog.as_mut().unwrap().output = None;
        assert!(config.validate(ConnectorKind::Catalog).is_err());

        let mut config = sail_balance_template();
        config.balance.as_mut().unwrap().refresh_seconds = Some(5);
        assert!(config.validate(ConnectorKind::Balance).is_err());

        let mut config = sail_template();
        config
            .headers
            .insert("host".to_string(), "evil".to_string());
        assert!(config.validate(ConnectorKind::Catalog).is_err());
    }

    #[test]
    fn sail_balance_fixture_resolves() {
        let document: Value = serde_json::from_str(SAIL_USAGE).expect("fixture parses");
        let config = sail_balance_template().balance.expect("balance section");
        let balance = extract_balance(&document, &config).expect("balance resolves");
        assert_eq!(balance.balance_usd, Some(12.34));
        assert!(balance.available);
        assert_eq!(balance.currency, "USD");
        assert_eq!(balance.period_spend, Some(5.14));
        assert_eq!(balance.burn_rate, Some(0.42));
        assert_eq!(balance.days_remaining, Some(29.4));
    }

    #[test]
    fn available_false_is_null_and_never_zero() {
        let document = serde_json::json!({
            "balance": 0,
            "available": false,
            "currency": "USD"
        });
        let config = BalanceConfig {
            balance: "/balance".to_string(),
            available: Some("/available".to_string()),
            currency: Some("/currency".to_string()),
            period_spend: None,
            burn_rate: None,
            days_remaining: None,
            refresh_seconds: None,
        };
        let balance = extract_balance(&document, &config).expect("extract");
        assert!(!balance.available);
        assert_eq!(balance.balance_usd, None, "unavailable is never 0");
    }

    #[test]
    fn balance_requires_a_non_negative_number() {
        let config = BalanceConfig {
            balance: "/balance".to_string(),
            available: None,
            currency: None,
            period_spend: None,
            burn_rate: None,
            days_remaining: None,
            refresh_seconds: None,
        };
        for document in [
            serde_json::json!({}),
            serde_json::json!({"balance": "n/a"}),
            serde_json::json!({"balance": -3}),
        ] {
            assert!(extract_balance(&document, &config).is_err(), "{document}");
        }
        let balance = extract_balance(&serde_json::json!({"balance": "3.5"}), &config)
            .expect("numeric string");
        assert_eq!(balance.balance_usd, Some(3.5));
    }

    #[test]
    fn health_pointer_decides_the_answer() {
        let config = HealthConfig {
            ok_pointer: Some("/status/ok".to_string()),
        };
        assert!(extract_health(Some(&serde_json::json!({"status": {"ok": true}})), &config).ok);
        assert!(!extract_health(Some(&serde_json::json!({"status": {"ok": false}})), &config).ok);
        assert!(!extract_health(Some(&serde_json::json!({})), &config).ok);
        assert!(!extract_health(None, &config).ok);
        assert!(
            extract_health(None, &HealthConfig::default()).ok,
            "2xx is enough"
        );
    }

    #[test]
    fn ssrf_guards_block_metadata_link_local_and_special_ranges() {
        for ip in [
            "169.254.169.254",
            "169.254.0.1",
            "fd00:ec2::254",
            "fe80::1",
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "ff02::1",
            "255.255.255.255",
        ] {
            let ip: IpAddr = ip.parse().expect("ip");
            assert_eq!(
                classify_ip(ip, true).expect_err("must be blocked even with allow_private"),
                match ip {
                    IpAddr::V4(v4) if v4 == std::net::Ipv4Addr::new(169, 254, 169, 254) => {
                        BlockReason::Metadata
                    }
                    IpAddr::V4(v4) if v4.is_link_local() => BlockReason::LinkLocal,
                    IpAddr::V4(v4) if v4.is_broadcast() => BlockReason::Broadcast,
                    IpAddr::V4(v4) if v4.is_multicast() => BlockReason::Multicast,
                    IpAddr::V6(v6)
                        if v6 == "fd00:ec2::254".parse::<std::net::Ipv6Addr>().unwrap() =>
                    {
                        BlockReason::Metadata
                    }
                    IpAddr::V6(v6) if v6.is_unicast_link_local() => BlockReason::LinkLocal,
                    IpAddr::V6(v6) if v6.is_multicast() => BlockReason::Multicast,
                    _ => BlockReason::Unspecified,
                }
            );
        }
    }

    #[test]
    fn ssrf_guards_gate_loopback_and_private_on_allow_private() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "::1",
            "fd12::1",
        ] {
            let ip: IpAddr = ip.parse().expect("ip");
            assert!(
                classify_ip(ip, true).is_ok(),
                "{ip} allowed for local testing"
            );
            assert!(classify_ip(ip, false).is_err(), "{ip} blocked by default");
        }
        for ip in ["8.8.8.8", "2001:4860:4860::8888"] {
            let ip: IpAddr = ip.parse().expect("ip");
            assert!(classify_ip(ip, false).is_ok(), "public {ip} is allowed");
        }
        // An IPv4-mapped private address is judged as its IPv4 address.
        let mapped: IpAddr = "::ffff:10.0.0.1".parse().expect("ip");
        assert_eq!(classify_ip(mapped, false), Err(BlockReason::Private));
    }

    #[test]
    fn urls_resolve_relative_paths_against_the_provider_origin() {
        assert_eq!(
            resolve_url(
                "https://api.sailresearch.com/v1",
                "/v2/usage/summary?range=30d"
            )
            .unwrap(),
            "https://api.sailresearch.com/v2/usage/summary?range=30d"
        );
        assert_eq!(
            resolve_url("http://127.0.0.1:9000/v1", "/models").unwrap(),
            "http://127.0.0.1:9000/models"
        );
        assert_eq!(
            resolve_url("https://api.example.com/v1", "https://other.example/models").unwrap(),
            "https://other.example/models"
        );
        assert!(resolve_url("https://api.example.com/v1", "models").is_err());
        assert!(resolve_url("https://api.example.com/v1", "").is_err());
    }

    #[test]
    fn sample_truncation_keeps_utf8_boundaries() {
        let (body, truncated) = truncate_sample("short");
        assert!(!truncated);
        assert_eq!(body, "short");
        let long = "é".repeat(SAMPLE_LIMIT_BYTES);
        let (body, truncated) = truncate_sample(&long);
        assert!(truncated);
        assert!(body.len() <= SAMPLE_LIMIT_BYTES);
        assert!(body.is_char_boundary(body.len()));
    }

    #[test]
    fn retry_backoff_doubles_to_one_hour_and_refresh_due_applies_it() {
        assert_eq!(retry_delay(1), Duration::from_secs(60));
        assert_eq!(retry_delay(2), Duration::from_secs(120));
        assert_eq!(retry_delay(6), Duration::from_secs(1920));
        assert_eq!(retry_delay(7), Duration::from_secs(3600));
        assert_eq!(retry_delay(50), Duration::from_secs(3600));

        let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T01:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(refresh_due(None, 0, 900, 0, now));
        assert!(refresh_due(Some("2026-01-01T00:00:00Z"), 0, 900, 0, now));
        assert!(!refresh_due(Some("2026-01-01T00:50:00Z"), 0, 900, 0, now));
        // Two consecutive errors wait 2 minutes on a 60 s cadence.
        assert_eq!(retry_delay(2), Duration::from_secs(120));
        assert!(!refresh_due(Some("2026-01-01T00:59:30Z"), 2, 60, 0, now));
        assert!(refresh_due(Some("2026-01-01T00:57:00Z"), 2, 60, 0, now));
        // Backoff never shortens a slower cadence.
        assert!(!refresh_due(Some("2026-01-01T00:59:00Z"), 2, 900, 0, now));
        // Many errors cap at one hour.
        assert!(!refresh_due(Some("2026-01-01T00:00:30Z"), 50, 60, 0, now));
        assert!(refresh_due(Some("2026-01-01T00:00:00Z"), 50, 60, 0, now));
        // Clock skew and corrupt timestamps do not wedge the schedule.
        assert!(refresh_due(Some("not-a-date"), 0, 900, 0, now));
        assert!(refresh_due(Some("2026-01-02T00:00:00Z"), 0, 900, 0, now));
    }

    #[test]
    fn templates_are_valid_json_documents() {
        let shipped = templates();
        assert_eq!(shipped.len(), 3);
        for template in shipped {
            for (kind, config) in [
                (ConnectorKind::Catalog, template.configs.catalog.as_ref()),
                (ConnectorKind::Balance, template.configs.balance.as_ref()),
                (ConnectorKind::Health, template.configs.health.as_ref()),
            ] {
                let Some(config) = config else {
                    continue;
                };
                let json = serde_json::to_value(config).expect("serializes");
                let parsed: ConnectorConfig = serde_json::from_value(json).expect("round trips");
                assert_eq!(&parsed, config);
                config.validate(kind).expect("template validates");
                // The document is plain JSON, not code.
                assert!(!serde_json::to_string(config).unwrap().contains("=>"));
            }
        }
    }

    #[test]
    fn secret_placeholder_is_substituted_only_at_fetch_time() {
        assert_eq!(
            substitute_secret("Bearer {{secret}}", Some("sk-1")).unwrap(),
            "Bearer sk-1"
        );
        assert_eq!(substitute_secret("plain", None).unwrap(), "plain");
        assert!(substitute_secret("Bearer {{secret}}", None).is_err());
    }

    #[test]
    fn connector_config_round_trips_through_storage_shape() {
        let config = sail_template();
        let raw = serde_json::to_string(&config).expect("serialize");
        let parsed: ConnectorConfig = serde_json::from_str(&raw).expect("parse");
        assert_eq!(parsed, config);
        assert!(raw.contains("pricing_usd_per_million_tokens/balanced/input"));
    }
}
