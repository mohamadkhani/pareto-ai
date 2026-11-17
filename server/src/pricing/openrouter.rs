//! OpenRouter catalog and price sync (decision D10).
//!
//! `GET https://openrouter.ai/api/v1/models` is public; the sync maps its
//! entries onto `models`, `offerings`, and `prices` rows of the `openrouter`
//! provider and records one `price_sync_runs` row per attempt.
//!
//! Semantics, as pinned by the M2 spec:
//! * Prices arrive as USD-per-token strings and are stored as USD per 1M
//!   tokens in `*_per_m`; `pricing.request` is a flat USD fee per request and
//!   is stored in `request_fee` as-is. The raw source values live only in the
//!   run's `detail_json`, never in the price columns — and only for models
//!   whose price changed or was skipped (D19), with run rows pruned to the
//!   newest [`MAX_RUNS`].
//! * A `prices` row with `source = 'manual'` is never overwritten (D10);
//!   unknown components stay NULL and are never written as 0.
//! * The sync applies discovery's id filter
//!   ([`catalog::is_discoverable_model_id`]): it never creates a model or
//!   offering for an id discovery would skip (`:batch`, reserved names). A
//!   filtered id may still refresh the price of an offering that already
//!   exists, because that row belongs to the operator.
//! * Snapshot TTL is 24 hours ([`SNAPSHOT_TTL`]): the previous successful
//!   sync's result is the snapshot, [`snapshot_is_fresh`] reports its age, and
//!   a failed refresh keeps serving it (the run is marked `stale-if-error`).
//!   An explicit `POST /api/sync/openrouter` always fetches; the TTL governs
//!   freshness reporting and future automatic refreshes (M5).
//!
//! Live fetch is unreachable from the build network (see
//! `docs/research/09-environment.md`), so every test drives this module
//! through the fixture `server/tests/fixtures/openrouter_models.json` or a
//! failing fetcher; live verification is a documented follow-up.

use std::future::Future;
use std::time::Duration;

use sqlx::SqlitePool;

use crate::catalog::{self, Actor, SyncPrice};

/// Source name recorded on synced prices and sync runs.
pub const SOURCE: &str = "openrouter";
/// The catalog endpoint the sync reads.
pub const MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
/// Provider row managed by the sync.
pub const PROVIDER_NAME: &str = "openrouter";
/// Base URL of the managed provider row (OpenAI-compatible).
pub const PROVIDER_BASE_URL: &str = "https://openrouter.ai/api/v1";
/// Snapshot freshness window: 24 hours.
pub const SNAPSHOT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How many `price_sync_runs` rows are kept per source (decision D19).
const MAX_RUNS: i64 = 30;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("fetching the OpenRouter catalog failed: {0}")]
    Fetch(String),
    #[error("the OpenRouter catalog response is not valid JSON: {0}")]
    Json(String),
    #[error("the OpenRouter catalog response has no data array")]
    Shape,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("catalog error: {0}")]
    Catalog(#[from] catalog::CatalogError),
}

/// How a sync run ended. Serialized to `price_sync_runs.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    /// The catalog was fetched and applied.
    Success,
    /// The fetch failed and the previous snapshot is being served.
    StaleIfError,
    /// The fetch failed and no previous snapshot exists.
    Error,
}

impl SyncStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::StaleIfError => "stale-if-error",
            Self::Error => "error",
        }
    }
}

/// Result of one [`run_sync`] attempt.
#[derive(Debug)]
pub struct SyncOutcome {
    pub run_id: String,
    pub status: SyncStatus,
    /// Models applied (success) or served by the previous snapshot
    /// (stale-if-error); 0 when no snapshot exists.
    pub model_count: usize,
    /// True when the fetch failed and the previous snapshot is served.
    pub stale: bool,
    pub started_at: String,
    pub finished_at: String,
    pub error: Option<String>,
}

/// One row of `price_sync_runs`.
#[derive(Debug, sqlx::FromRow)]
pub struct SyncRunRow {
    pub id: String,
    pub source: String,
    pub started_at: String,
    pub finished_at: String,
    pub status: String,
    pub model_count: i64,
    pub detail_json: Option<String>,
}

/// A parsed catalog entry.
#[derive(Debug, Clone, PartialEq)]
pub struct OrModel {
    /// OpenRouter model id, e.g. `openai/gpt-4o` (also the canonical name).
    pub id: String,
    pub display_name: Option<String>,
    pub context_length: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub supported_parameters: Option<serde_json::Value>,
    pub price: OrPrice,
    /// The raw `pricing` object, kept for the run's `detail_json` only.
    pub raw_pricing: Option<serde_json::Value>,
    /// `catalog::is_discoverable_model_id` passed for this id. Discoverable
    /// entries are created/updated; filtered ones (`:batch`, reserved names)
    /// may only update a price on an offering that already exists — the sync
    /// never creates catalog rows discovery would skip.
    pub managed: bool,
}

/// Mapped price components (USD per 1M tokens, plus the flat request fee).
/// `None` means unknown and is stored as NULL — never as 0.
///
/// `variable` records that the source published a negative component (the
/// `"-1"` sentinel OpenRouter uses when it cannot state a fixed price, e.g.
/// `openrouter/auto`). Every negative component is dropped to `None` at
/// parse, so a sentinel can never reach a price column, an aggregate, or the
/// routing price score; the flag is what the UI shows instead of a number.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrPrice {
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub request_fee: Option<f64>,
    pub variable: bool,
}

/// A parsed catalog plus the entries that could not be mapped.
#[derive(Debug, Default)]
pub struct Catalog {
    pub models: Vec<OrModel>,
    /// Entries skipped because they had no usable id.
    pub skipped: usize,
}

/// Fetches the OpenRouter catalog body.
///
/// Hardened against the live failure this build saw (a 15 s timeout mid-body
/// on the IPv6 path, while `curl -4` answered in 1–2 s):
///
/// * the client has a 30 s response timeout and a 5 s connect timeout, and
///   prefers IPv4 addresses when the host resolves to both families;
/// * the request sends `Accept-Encoding: identity` and the body is read as
///   raw bytes (capped at [`MAX_BODY_BYTES`]) and decoded lossily, so a
///   charset/encoding disagreement cannot look like a JSON error;
/// * a connect or body failure is retried once over HTTP/1.1, the layer that
///   failed is named in the error and every attempt is logged at debug level
///   with status, content length and elapsed time.
///
/// A non-2xx answer is not retried: it is a definite upstream answer.
///
/// Failure mode: [`SyncError::Fetch`] with the failing layer (`connect`,
/// `status`, `body`) named; the caller turns that into a stale-if-error
/// outcome.
pub async fn fetch_models() -> Result<String, SyncError> {
    fetch_catalog_from(MODELS_URL).await
}

/// [`fetch_models`] against an arbitrary URL, so tests can drive a local
/// server. Failure mode: [`SyncError::Fetch`].
pub async fn fetch_catalog_from(url: &str) -> Result<String, SyncError> {
    let client = fetch_client()?;
    let mut last: Option<SyncError> = None;
    for attempt in 0..2 {
        match fetch_once(&client, url, attempt).await {
            Ok(body) => return Ok(body),
            Err(failure) => {
                tracing::debug!(
                    url = %url,
                    attempt = attempt + 1,
                    layer = failure.layer,
                    retryable = failure.retryable,
                    error = %failure.detail,
                    "openrouter catalog fetch attempt failed"
                );
                let error = SyncError::Fetch(format!(
                    "{}: {} (attempt {})",
                    failure.layer,
                    failure.detail,
                    attempt + 1
                ));
                if !failure.retryable {
                    return Err(error);
                }
                last = Some(error);
            }
        }
    }
    Err(last.unwrap_or_else(|| SyncError::Fetch("the catalog body could not be read".to_string())))
}

/// The response timeout: the live endpoint needs 1–2 s, so 30 s leaves room
/// for a slow TLS/DNS path without hanging a sync trigger.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// A connection that has not been established in 5 s is not going to be.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on the catalog body; the live document is ≈2 MB.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// One failed attempt, with the layer that failed.
struct FetchFailure {
    layer: &'static str,
    detail: String,
    retryable: bool,
}

impl FetchFailure {
    fn new(layer: &'static str, detail: impl Into<String>, retryable: bool) -> Self {
        Self {
            layer,
            detail: detail.into(),
            retryable,
        }
    }
}

/// Builds the catalog client. The custom resolver sorts IPv4 addresses ahead
/// of IPv6 ones: the live failure was IPv6-only (`curl -6` timed out while
/// `curl -4` answered), and reqwest has no per-client IP-family option.
fn fetch_client() -> Result<reqwest::Client, SyncError> {
    reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent("pareto-ai-gateway/0.1 openrouter-sync")
        .dns_resolver(Ipv4FirstResolver)
        .build()
        .map_err(|e| SyncError::Fetch(format!("client: {e}")))
}

/// A DNS resolver that keeps every address but lists IPv4 first.
#[derive(Debug, Clone, Copy)]
struct Ipv4FirstResolver;

impl reqwest::dns::Resolve for Ipv4FirstResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let (v4, v6): (Vec<_>, Vec<_>) = addrs.into_iter().partition(|addr| addr.is_ipv4());
            let ordered: Vec<std::net::SocketAddr> = v4.into_iter().chain(v6).collect();
            if ordered.is_empty() {
                return Err(format!("{host} resolved to no addresses").into());
            }
            Ok(Box::new(ordered.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// One attempt: send, check the status, then read the body. Attempt 1 forces
/// HTTP/1.1 for a fresh connection/protocol.
async fn fetch_once(
    client: &reqwest::Client,
    url: &str,
    attempt: usize,
) -> Result<String, FetchFailure> {
    let started = std::time::Instant::now();
    let mut request = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity");
    if attempt == 1 {
        request = request.version(reqwest::Version::HTTP_11);
    }
    let mut response = request
        .send()
        .await
        .map_err(|e| FetchFailure::new("connect", e.to_string(), true))?;
    let status = response.status();
    let content_length = response.content_length();
    if !status.is_success() {
        return Err(FetchFailure::new(
            "status",
            format!("HTTP {status} from {url}"),
            false,
        ));
    }

    let mut body: Vec<u8> = Vec::with_capacity(
        content_length
            .map(|len| len.min(MAX_BODY_BYTES as u64) as usize)
            .unwrap_or(0),
    );
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(FetchFailure::new(
                        "body",
                        format!("the catalog body exceeds {MAX_BODY_BYTES} bytes"),
                        false,
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                tracing::debug!(
                    url = %url,
                    attempt = attempt + 1,
                    status = %status,
                    content_length = content_length.unwrap_or(0),
                    elapsed_ms = started.elapsed().as_millis(),
                    read_bytes = body.len(),
                    layer = "body",
                    error = %error,
                    "openrouter catalog body failed mid-read"
                );
                return Err(FetchFailure::new("body", error.to_string(), true));
            }
        }
    }
    if let Some(expected) = content_length
        && (body.len() as u64) < expected
    {
        // A short body that hyper did not surface as an error: treat it as a
        // truncated transfer and retry instead of failing JSON parsing.
        return Err(FetchFailure::new(
            "body",
            format!(
                "truncated response: read {} of {expected} bytes",
                body.len()
            ),
            true,
        ));
    }
    tracing::debug!(
        url = %url,
        attempt = attempt + 1,
        status = %status,
        content_length = content_length.unwrap_or(0),
        bytes = body.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "openrouter catalog fetched"
    );
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Parses the OpenRouter `/models` body into [`Catalog`].
///
/// Fails only when the body is not JSON or has no `data` array; individual
/// unusable entries are counted in [`Catalog::skipped`] instead of failing
/// the whole import.
pub fn parse_catalog(raw: &str) -> Result<Catalog, SyncError> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| SyncError::Json(e.to_string()))?;
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or(SyncError::Shape)?;

    let mut catalog = Catalog::default();
    for entry in data {
        match parse_entry(entry) {
            Some(model) => catalog.models.push(model),
            None => catalog.skipped += 1,
        }
    }
    Ok(catalog)
}

fn parse_entry(entry: &serde_json::Value) -> Option<OrModel> {
    let id = entry.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }

    let pricing = entry.get("pricing").filter(|p| p.is_object());
    let capabilities = entry.get("supported_parameters").and_then(|p| match p {
        serde_json::Value::Array(items) if !items.is_empty() => Some(p.clone()),
        _ => None,
    });

    Some(OrModel {
        id: id.to_string(),
        display_name: entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        context_length: entry
            .get("context_length")
            .and_then(serde_json::Value::as_i64),
        max_output_tokens: entry
            .get("top_provider")
            .and_then(|p| p.get("max_completion_tokens"))
            .and_then(serde_json::Value::as_i64),
        input_modalities: modalities(entry).0,
        output_modalities: modalities(entry).1,
        supported_parameters: capabilities,
        price: OrPrice {
            input_per_m: per_million_field(pricing, "prompt"),
            output_per_m: per_million_field(pricing, "completion"),
            cache_read_per_m: per_million_field(pricing, "input_cache_read"),
            cache_write_per_m: per_million_field(pricing, "input_cache_write"),
            request_fee: usd_amount_field(pricing, "request"),
            variable: has_negative_component(pricing),
        },
        raw_pricing: pricing.cloned(),
        managed: crate::catalog::is_discoverable_model_id(id),
    })
}

/// The price keys that carry a per-token or per-request amount. A negative
/// value in any of them is the "variable price" sentinel, never a price.
const PRICE_KEYS: [&str; 5] = [
    "prompt",
    "completion",
    "input_cache_read",
    "input_cache_write",
    "request",
];

/// True when the `pricing` object carries a negative amount. OpenRouter emits
/// `"-1"` when it cannot state a price for a router model; the value is a
/// sentinel, so the component is stored as NULL and the offering is flagged.
fn has_negative_component(pricing: Option<&serde_json::Value>) -> bool {
    let Some(pricing) = pricing else {
        return false;
    };
    PRICE_KEYS.iter().any(|key| {
        let Some(field) = pricing.get(*key) else {
            return false;
        };
        let value = match field {
            serde_json::Value::String(raw) => raw.trim().parse::<f64>().ok(),
            serde_json::Value::Number(number) => number.as_f64(),
            _ => None,
        };
        value.is_some_and(|value| value < 0.0)
    })
}

/// Reads a USD-per-token price field (`pricing.prompt` etc.) and converts it
/// to USD per 1M tokens. Missing, unparseable, non-finite, or negative values
/// are `None` (unknown), and a plain `"0"` is a known zero price.
fn per_million_field(pricing: Option<&serde_json::Value>, key: &str) -> Option<f64> {
    let field = pricing?.get(key)?;
    match field {
        serde_json::Value::String(raw) => usd_per_token_to_per_million(raw),
        serde_json::Value::Number(n) => n
            .as_f64()
            .map(|v| v * 1_000_000.0)
            .filter(|v| v.is_finite() && *v >= 0.0),
        _ => None,
    }
}

/// Reads a plain USD amount (`pricing.request`, a flat per-request fee).
/// Negative values are the variable-price sentinel and stay `None`.
fn usd_amount_field(pricing: Option<&serde_json::Value>, key: &str) -> Option<f64> {
    let field = pricing?.get(key)?;
    match field {
        serde_json::Value::String(raw) => raw
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0),
        serde_json::Value::Number(n) => n.as_f64().filter(|v| v.is_finite() && *v >= 0.0),
        _ => None,
    }
}

/// Converts a USD-per-token decimal string to USD per 1M tokens by shifting
/// the decimal point six places as text, so `"0.0000025"` becomes exactly
/// `2.5` instead of `2.499999...`. Returns `None` for unparseable input and
/// for negative input (`"-1"` is OpenRouter's variable-price sentinel, not a
/// price).
pub fn usd_per_token_to_per_million(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }
    // Exponent notation is not what OpenRouter emits; fall back to plain
    // float math for it (precision there is best-effort anyway).
    if trimmed.contains(['e', 'E']) {
        return trimmed
            .parse::<f64>()
            .ok()
            .map(|v| v * 1_000_000.0)
            .filter(|v| v.is_finite() && *v >= 0.0);
    }

    let (int_part, frac_part) = match trimmed.split_once('.') {
        Some((int, frac)) => (int, frac),
        None => (trimmed, ""),
    };
    if int_part.is_empty()
        || !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }

    // Move the decimal point six places right: int + frac digits, then a
    // point after `int_len + 6` digits (padding with zeros as needed).
    let mut digits = String::from(int_part);
    digits.push_str(frac_part);
    let point_at = int_part.len() + 6;
    while digits.len() < point_at {
        digits.push('0');
    }
    let (whole, tail) = digits.split_at(point_at);
    let shifted = match tail.is_empty() {
        true => format!("{whole}.0"),
        false => format!("{whole}.{tail}"),
    };
    let value: f64 = shifted.parse().ok()?;
    value.is_finite().then_some(value)
}

/// Input/output modalities of an entry: the explicit arrays when present,
/// else derived from `architecture.modality` (`"text+image->text"`).
fn modalities(entry: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let architecture = entry.get("architecture");
    let read = |key: &str| -> Vec<String> {
        architecture
            .and_then(|a| a.get(key))
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let input = read("input_modalities");
    let output = read("output_modalities");
    if !input.is_empty() || !output.is_empty() {
        return (input, output);
    }

    let modality = architecture
        .and_then(|a| a.get("modality"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match modality.split_once("->") {
        Some((inputs, outputs)) => (
            inputs
                .split('+')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            outputs
                .split('+')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        None => (Vec::new(), Vec::new()),
    }
}

/// Runs one sync attempt: fetch, parse, and apply the catalog inside one
/// transaction that also records the `price_sync_runs` row and exactly one
/// `sync.openrouter` audit row.
///
/// A fetch or parse failure is not an `Err`: the previous snapshot keeps
/// serving (outcome `stale-if-error`, or `error` when none exists) and the
/// failed run is still recorded. `Err` is reserved for database failures.
/// `fetch` is injected so tests drive it from a fixture instead of the live
/// endpoint.
pub async fn run_sync<F, Fut>(
    pool: &SqlitePool,
    fetch: F,
    actor: &Actor<'_>,
) -> Result<SyncOutcome, SyncError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, SyncError>>,
{
    let started_at = catalog::now_ts();
    let attempt = fetch().await.and_then(|raw| parse_catalog(&raw));

    match attempt {
        Ok(parsed) => apply_success(pool, actor, &started_at, parsed).await,
        Err(error) => apply_failure(pool, actor, &started_at, &error.to_string()).await,
    }
}

/// The successful path: upsert everything, record the run and the audit row,
/// all in one transaction.
async fn apply_success(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    started_at: &str,
    parsed: Catalog,
) -> Result<SyncOutcome, SyncError> {
    let finished_at = catalog::now_ts();
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;

    let provider_id = ensure_provider(&mut tx).await?;
    let mut skipped_manual = 0usize;
    let mut filtered_not_created = 0usize;
    let mut raw_prices = serde_json::Map::new();
    let mut variable_prices: Vec<&str> = Vec::new();
    for model in &parsed.models {
        // Discovery's id filter is the single source of truth: a filtered id
        // never creates a model or offering. It may still refresh the price of
        // an offering that already exists (for example one the operator
        // created by hand), because that row is not ours to hide.
        let offering_id = if model.managed {
            let model_id = upsert_model(&mut tx, model).await?;
            Some(upsert_offering(&mut tx, &provider_id, &model_id, &model.id).await?)
        } else {
            match existing_offering(&mut tx, &provider_id, &model.id).await? {
                Some(offering_id) => Some(offering_id),
                None => {
                    filtered_not_created += 1;
                    None
                }
            }
        };
        let Some(offering_id) = offering_id else {
            continue;
        };
        let outcome = catalog::apply_sync_price(
            &mut tx,
            &offering_id,
            &SyncPrice {
                input_per_m: model.price.input_per_m,
                output_per_m: model.price.output_per_m,
                cache_read_per_m: model.price.cache_read_per_m,
                cache_write_per_m: model.price.cache_write_per_m,
                request_fee: model.price.request_fee,
                variable_price: model.price.variable,
            },
            SOURCE,
            MODELS_URL,
            &finished_at,
        )
        .await?;

        // Raw source values (per-token strings, image pricing, ...) live only
        // in the run detail — never in the price columns (spec: sync section).
        // D19 keeps them for models whose price changed or was skipped
        // (manual), not for repeats that changed nothing.
        match outcome {
            catalog::SyncPriceOutcome::Changed | catalog::SyncPriceOutcome::SkippedManual => {
                raw_prices.insert(
                    model.id.clone(),
                    model.raw_pricing.clone().unwrap_or(serde_json::Value::Null),
                );
            }
            catalog::SyncPriceOutcome::Unchanged => {}
        }
        if outcome == catalog::SyncPriceOutcome::SkippedManual {
            skipped_manual += 1;
        }
        if model.price.variable {
            variable_prices.push(model.id.as_str());
        }
    }

    let detail = serde_json::json!({
        "raw_prices": serde_json::Value::Object(raw_prices),
        "skipped_entries": parsed.skipped,
        "filtered_not_created": filtered_not_created,
        "prices_skipped_manual": skipped_manual,
        // Ids whose source price was a negative sentinel: the components are
        // stored NULL and `prices.variable_price = 1`.
        "variable_prices": variable_prices,
    });

    let model_count = parsed.models.iter().filter(|model| model.managed).count();
    sqlx::query(
        "INSERT INTO price_sync_runs \
           (id, source, started_at, finished_at, status, model_count, detail_json) \
         VALUES (?, ?, ?, ?, 'success', ?, ?)",
    )
    .bind(&run_id)
    .bind(SOURCE)
    .bind(started_at)
    .bind(&finished_at)
    .bind(model_count as i64)
    .bind(detail.to_string())
    .execute(&mut *tx)
    .await?;

    catalog::audit(
        &mut tx,
        actor,
        "sync.openrouter",
        "price_sync_run",
        &run_id,
        &serde_json::json!({
            "status": SyncStatus::Success.as_str(),
            "model_count": model_count,
            "skipped_entries": parsed.skipped,
            "prices_skipped_manual": skipped_manual,
        }),
    )
    .await?;
    prune_runs(&mut tx).await?;
    tx.commit().await?;

    Ok(SyncOutcome {
        run_id,
        status: SyncStatus::Success,
        model_count,
        stale: false,
        started_at: started_at.to_string(),
        finished_at,
        error: None,
    })
}

/// The failure path: serve the previous snapshot, record the failed run and
/// the audit row in one transaction.
async fn apply_failure(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    started_at: &str,
    error: &str,
) -> Result<SyncOutcome, SyncError> {
    let finished_at = catalog::now_ts();
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;

    let previous = last_success(&mut tx).await?;
    let (status, model_count, served) = match &previous {
        Some(run) => (
            SyncStatus::StaleIfError,
            run.model_count as usize,
            Some(run.id.clone()),
        ),
        None => (SyncStatus::Error, 0, None),
    };

    let detail = serde_json::json!({
        "error": error,
        "served_snapshot_run_id": served,
    });
    sqlx::query(
        "INSERT INTO price_sync_runs \
           (id, source, started_at, finished_at, status, model_count, detail_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&run_id)
    .bind(SOURCE)
    .bind(started_at)
    .bind(&finished_at)
    .bind(status.as_str())
    .bind(model_count as i64)
    .bind(detail.to_string())
    .execute(&mut *tx)
    .await?;

    catalog::audit(
        &mut tx,
        actor,
        "sync.openrouter",
        "price_sync_run",
        &run_id,
        &serde_json::json!({
            "status": status.as_str(),
            "model_count": model_count,
            "error": error,
        }),
    )
    .await?;
    prune_runs(&mut tx).await?;
    tx.commit().await?;

    Ok(SyncOutcome {
        run_id,
        status,
        model_count,
        stale: status == SyncStatus::StaleIfError,
        started_at: started_at.to_string(),
        finished_at,
        error: Some(error.to_string()),
    })
}

/// The most recent sync run of any status, or `None` before the first sync.
pub async fn last_run(pool: &SqlitePool) -> Result<Option<SyncRunRow>, SyncError> {
    Ok(sqlx::query_as::<_, SyncRunRow>(
        "SELECT id, source, started_at, finished_at, status, model_count, detail_json \
         FROM price_sync_runs WHERE source = ? ORDER BY finished_at DESC, id DESC LIMIT 1",
    )
    .bind(SOURCE)
    .fetch_optional(pool)
    .await?)
}

/// When the serving snapshot was taken (the last successful run), if any.
pub async fn snapshot_finished_at(pool: &SqlitePool) -> Result<Option<String>, SyncError> {
    Ok(sqlx::query_scalar(
        "SELECT finished_at FROM price_sync_runs \
         WHERE source = ? AND status = 'success' \
         ORDER BY finished_at DESC, id DESC LIMIT 1",
    )
    .bind(SOURCE)
    .fetch_optional(pool)
    .await?)
}

/// Whether the snapshot taken at `finished_at` is still inside the 24h TTL.
pub fn snapshot_is_fresh(finished_at: Option<&str>, now: &chrono::DateTime<chrono::Utc>) -> bool {
    finished_at
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .map(|ts| {
            let age = now.signed_duration_since(ts.to_utc());
            age >= chrono::Duration::zero() && age.num_seconds() < SNAPSHOT_TTL.as_secs() as i64
        })
        .unwrap_or(false)
}

/// The most recent *successful* run inside a transaction (the snapshot).
async fn last_success(conn: &mut sqlx::SqliteConnection) -> Result<Option<SyncRunRow>, SyncError> {
    Ok(sqlx::query_as::<_, SyncRunRow>(
        "SELECT id, source, started_at, finished_at, status, model_count, detail_json \
         FROM price_sync_runs WHERE source = ? AND status = 'success' \
         ORDER BY finished_at DESC, id DESC LIMIT 1",
    )
    .bind(SOURCE)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Deletes run rows beyond the newest [`MAX_RUNS`] of this source (D19), so
/// `detail_json` cannot grow without bound. Runs inside the caller's
/// transaction, right after the new run row is written.
async fn prune_runs(conn: &mut sqlx::SqliteConnection) -> Result<(), SyncError> {
    sqlx::query(
        "DELETE FROM price_sync_runs \
         WHERE source = ? AND id NOT IN ( \
           SELECT id FROM price_sync_runs WHERE source = ? \
           ORDER BY finished_at DESC, id DESC LIMIT ? \
         )",
    )
    .bind(SOURCE)
    .bind(SOURCE)
    .bind(MAX_RUNS)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Finds (or creates, on first sync) the `openrouter` provider row. The
/// creation is reported to the caller and folded into the sync's audit row.
async fn ensure_provider(conn: &mut sqlx::SqliteConnection) -> Result<String, SyncError> {
    let existing: Option<String> = sqlx::query_scalar("SELECT id FROM providers WHERE name = ?")
        .bind(PROVIDER_NAME)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(id) = existing {
        return Ok(id);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = catalog::now_ts();
    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, enabled, created_at, updated_at) \
         VALUES (?, ?, 'openai_compatible', ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(PROVIDER_NAME)
    .bind(PROVIDER_BASE_URL)
    .bind(&now)
    .bind(&now)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// Upserts the model row of a catalog entry by canonical name. The lookup is
/// case-insensitive (`COLLATE NOCASE`), so a provider that spells an existing
/// model with different case (`moonshotai/Kimi-K3.1-Pro` vs
/// `moonshotai/kimi-k3.1-pro`) reuses that row and links its offering to it;
/// the stored spelling is never rewritten. Synced fields are refreshed (that
/// is what the sync is for); `family` is admin-maintained and never touched
/// here.
async fn upsert_model(
    conn: &mut sqlx::SqliteConnection,
    model: &OrModel,
) -> Result<String, SyncError> {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM models WHERE canonical_name = ? COLLATE NOCASE")
            .bind(&model.id)
            .fetch_optional(&mut *conn)
            .await?;

    let capabilities = capabilities_json(model);
    let modalities_input = serde_json::to_string(&model.input_modalities).ok();
    let modalities_output = serde_json::to_string(&model.output_modalities).ok();
    let now = catalog::now_ts();

    match existing {
        Some(id) => {
            sqlx::query(
                "UPDATE models SET modalities_input = ?, modalities_output = ?, \
                   context_length = ?, max_output_tokens = ?, capabilities_json = ?, \
                   updated_at = ? \
                 WHERE id = ?",
            )
            .bind(&modalities_input)
            .bind(&modalities_output)
            .bind(model.context_length)
            .bind(model.max_output_tokens)
            .bind(&capabilities)
            .bind(&now)
            .bind(&id)
            .execute(&mut *conn)
            .await?;
            Ok(id)
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO models \
                   (id, canonical_name, modalities_input, modalities_output, \
                    context_length, max_output_tokens, capabilities_json, \
                    created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&model.id)
            .bind(&modalities_input)
            .bind(&modalities_output)
            .bind(model.context_length)
            .bind(model.max_output_tokens)
            .bind(&capabilities)
            .bind(&now)
            .bind(&now)
            .execute(&mut *conn)
            .await?;
            Ok(id)
        }
    }
}

/// Upserts the offering row keyed on `(provider_id, upstream_model_id)`.
/// Admin-set overrides, `enabled`, and headers survive the sync.
async fn upsert_offering(
    conn: &mut sqlx::SqliteConnection,
    provider_id: &str,
    model_id: &str,
    upstream_model_id: &str,
) -> Result<String, SyncError> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM offerings WHERE provider_id = ? AND upstream_model_id = ?",
    )
    .bind(provider_id)
    .bind(upstream_model_id)
    .fetch_optional(&mut *conn)
    .await?;

    let now = catalog::now_ts();
    match existing {
        Some(id) => {
            sqlx::query("UPDATE offerings SET model_id = ?, updated_at = ? WHERE id = ?")
                .bind(model_id)
                .bind(&now)
                .bind(&id)
                .execute(&mut *conn)
                .await?;
            Ok(id)
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO offerings \
                   (id, provider_id, model_id, upstream_model_id, enabled, \
                    created_at, updated_at) \
                 VALUES (?, ?, ?, ?, 1, ?, ?)",
            )
            .bind(&id)
            .bind(provider_id)
            .bind(model_id)
            .bind(upstream_model_id)
            .bind(&now)
            .bind(&now)
            .execute(&mut *conn)
            .await?;
            Ok(id)
        }
    }
}

/// An existing offering of this provider for `upstream_model_id`, if any.
/// Used by filtered ids, which may refresh a price but never create a row.
async fn existing_offering(
    conn: &mut sqlx::SqliteConnection,
    provider_id: &str,
    upstream_model_id: &str,
) -> Result<Option<String>, SyncError> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM offerings WHERE provider_id = ? AND upstream_model_id = ?",
    )
    .bind(provider_id)
    .bind(upstream_model_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// `capabilities_json` for a synced model: the display name (the spec schema
/// has no display-name column) and `supported_parameters`, when present.
fn capabilities_json(model: &OrModel) -> Option<String> {
    let mut object = serde_json::Map::new();
    if let Some(name) = &model.display_name {
        object.insert(
            "display_name".to_string(),
            serde_json::Value::String(name.clone()),
        );
    }
    if let Some(parameters) = &model.supported_parameters {
        object.insert("supported_parameters".to_string(), parameters.clone());
    }
    if object.is_empty() {
        return None;
    }
    Some(serde_json::Value::Object(object).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::test_support::TestApp;

    const FIXTURE: &str = include_str!("../../tests/fixtures/openrouter_models.json");
    /// A recording of the `-1` variable-price sentinel (live, 2026-11-17).
    const VARIABLE_FIXTURE: &str =
        include_str!("../../tests/fixtures/openrouter_models_variable.json");

    /// Creates the admin whose id the audit rows attribute the sync to
    /// (`audit_log.actor_user_id` is a foreign key onto `users`).
    async fn admin_id(app: &TestApp) -> String {
        app.create_user("admin@example.com", Role::Admin, true)
            .await
    }

    /// The stored price row of the offering for `canonical_name`.
    async fn price_by_model(app: &TestApp, canonical_name: &str) -> catalog::PriceRow {
        let offering_id: String = sqlx::query_scalar(
            "SELECT o.id FROM offerings o JOIN models m ON m.id = o.model_id \
             WHERE m.canonical_name = ?",
        )
        .bind(canonical_name)
        .fetch_one(&app.state.pool)
        .await
        .expect("offering id");
        catalog::get_price(&app.state.pool, &offering_id)
            .await
            .expect("price read")
            .expect("price row")
    }

    fn erroring_fetch() -> impl Future<Output = Result<String, SyncError>> {
        std::future::ready(Err(SyncError::Fetch("connection timed out".to_string())))
    }

    fn fixture_fetch() -> impl Future<Output = Result<String, SyncError>> {
        std::future::ready(Ok(FIXTURE.to_string()))
    }

    // -----------------------------------------------------------------------
    // Parsing
    // -----------------------------------------------------------------------

    #[test]
    fn per_token_strings_convert_exactly_to_per_million() {
        assert_eq!(usd_per_token_to_per_million("0.0000025"), Some(2.5));
        assert_eq!(usd_per_token_to_per_million("0.00001"), Some(10.0));
        assert_eq!(usd_per_token_to_per_million("0"), Some(0.0));
        assert_eq!(usd_per_token_to_per_million("1.5"), Some(1_500_000.0));
        assert_eq!(usd_per_token_to_per_million("0.00000055"), Some(0.55));

        // Unknown is None — never a silent zero.
        assert_eq!(usd_per_token_to_per_million(""), None);
        assert_eq!(usd_per_token_to_per_million("free"), None);
        assert_eq!(usd_per_token_to_per_million("1,5"), None);
        assert_eq!(usd_per_token_to_per_million("NaN"), None);
    }

    #[test]
    fn fixture_parses_into_the_expected_models_and_prices() {
        let catalog = parse_catalog(FIXTURE).expect("fixture parses");
        assert_eq!(catalog.skipped, 0);
        assert_eq!(catalog.models.len(), 5);

        let by_id = |id: &str| {
            catalog
                .models
                .iter()
                .find(|m| m.id == id)
                .unwrap_or_else(|| panic!("fixture must contain {id}"))
        };

        // String per-token prices land as exact USD per 1M tokens.
        let gpt4o = by_id("openai/gpt-4o");
        assert_eq!(gpt4o.price.input_per_m, Some(2.5));
        assert_eq!(gpt4o.price.output_per_m, Some(10.0));
        assert_eq!(gpt4o.price.request_fee, Some(0.0), "a literal 0 is known");
        // Cache pricing maps `pricing.input_cache_read`/`input_cache_write`.
        assert_eq!(gpt4o.price.cache_read_per_m, Some(1.25));
        assert_eq!(gpt4o.price.cache_write_per_m, Some(6.25));
        assert_eq!(gpt4o.context_length, Some(128_000));
        assert_eq!(gpt4o.max_output_tokens, Some(16_384));
        assert_eq!(gpt4o.display_name.as_deref(), Some("OpenAI: GPT-4o"));
        assert_eq!(gpt4o.input_modalities, vec!["text", "image"]);
        assert!(gpt4o.supported_parameters.is_some());
        // The raw pricing object (incl. per-image pricing) is kept for the
        // run detail only — the price columns have no image component.
        let raw = gpt4o.raw_pricing.as_ref().expect("raw pricing");
        assert_eq!(raw["image"], "0.003825");
        assert_eq!(raw["prompt"], "0.0000025");

        // A per-request fee is stored as USD, not per million tokens.
        let claude = by_id("anthropic/claude-3.5-sonnet");
        assert_eq!(claude.price.request_fee, Some(0.0004));
        assert_eq!(claude.price.input_per_m, Some(3.0));
        assert_eq!(claude.price.cache_read_per_m, Some(0.3));
        assert_eq!(claude.price.cache_write_per_m, Some(3.75));

        // An image-modality model maps its modalities; image pricing itself
        // has no column and stays in the raw detail. A listing without cache
        // pricing maps to unknown, never 0.
        let image_model = by_id("google/gemini-2.5-flash-image-preview");
        assert_eq!(image_model.output_modalities, vec!["image"]);
        assert_eq!(image_model.price.input_per_m, Some(0.0));
        assert_eq!(image_model.price.cache_read_per_m, None);
        assert_eq!(image_model.price.cache_write_per_m, None);

        // A model without pricing is all unknown: None, never zero.
        let llama = by_id("meta-llama/llama-3.3-70b-instruct");
        assert_eq!(llama.price, OrPrice::default());
        assert!(llama.raw_pricing.is_none());

        // Missing optional top_provider/supported_parameters degrade cleanly.
        let deepseek = by_id("deepseek/deepseek-r1");
        assert_eq!(deepseek.max_output_tokens, None);
        assert!(deepseek.supported_parameters.is_none());
    }

    #[test]
    fn a_negative_price_sentinel_is_variable_not_a_price() {
        let catalog = parse_catalog(VARIABLE_FIXTURE).expect("fixture parses");
        let by_id = |id: &str| {
            catalog
                .models
                .iter()
                .find(|model| model.id == id)
                .unwrap_or_else(|| panic!("fixture must contain {id}"))
        };

        // Every component of `openrouter/auto` is the "-1" sentinel: stored
        // as unknown, flagged variable, never a minus one million price.
        let auto = by_id("openrouter/auto");
        assert!(auto.price.variable, "the sentinel is recorded");
        assert_eq!(auto.price.input_per_m, None);
        assert_eq!(auto.price.output_per_m, None);
        assert_eq!(auto.price.request_fee, None);

        // A partial sentinel keeps the components that are real numbers.
        let half = by_id("vendor/half-variable");
        assert!(half.price.variable);
        assert_eq!(half.price.input_per_m, None);
        assert_eq!(half.price.output_per_m, Some(10.0));

        // An ordinary model is unaffected.
        let priced = by_id("openai/priced-model");
        assert!(!priced.price.variable);
        assert_eq!(priced.price.input_per_m, Some(1.0));
        assert_eq!(priced.price.output_per_m, Some(2.0));

        // The converter itself refuses the sentinel, so no caller can turn it
        // into -$1,000,000/M by accident.
        assert_eq!(usd_per_token_to_per_million("-1"), None);
        assert_eq!(usd_per_token_to_per_million("-0.5"), None);
    }

    #[test]
    fn entries_without_an_id_are_skipped_not_fatal() {
        let raw = r#"{"data": [{"name": "no id"}, {"id": "x/y", "pricing": {}}]}"#;
        let catalog = parse_catalog(raw).expect("parses");
        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.skipped, 1);
    }

    #[test]
    fn malformed_bodies_fail_parse() {
        assert!(matches!(parse_catalog("not json"), Err(SyncError::Json(_))));
        assert!(matches!(parse_catalog("{}"), Err(SyncError::Shape)));
        assert!(matches!(
            parse_catalog(r#"{"data": 3}"#),
            Err(SyncError::Shape)
        ));
    }

    #[test]
    fn modality_falls_back_to_the_modality_string() {
        let raw =
            r#"{"data": [{"id": "a/b", "architecture": {"modality": "text+image->text+audio"}}]}"#;
        let catalog = parse_catalog(raw).expect("parses");
        assert_eq!(catalog.models[0].input_modalities, vec!["text", "image"]);
        assert_eq!(catalog.models[0].output_modalities, vec!["text", "audio"]);
    }

    #[test]
    fn filtered_variants_are_parsed_but_not_managed() {
        let raw = r#"{"data": [
            {"id": "a/base", "pricing": {"prompt": "0.000001"}},
            {"id": "a/base:batch", "pricing": {"prompt": "0.000002"}},
            {"id": "a/base:free", "pricing": {"prompt": "0"}}
        ]}"#;
        let catalog = parse_catalog(raw).expect("parses");
        assert_eq!(catalog.models.len(), 3);
        let managed = |id: &str| {
            catalog
                .models
                .iter()
                .find(|model| model.id == id)
                .expect("entry")
                .managed
        };
        assert!(managed("a/base"));
        assert!(managed("a/base:free"), ":free is a distinct offering");
        assert!(!managed("a/base:batch"), ":batch is a billing tier");
    }

    #[tokio::test]
    async fn filtered_variants_are_not_created_but_existing_offerings_get_prices() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };
        let raw = r#"{"data": [
            {"id": "a/base", "pricing": {"prompt": "0.000001"}},
            {"id": "a/base:batch", "pricing": {"prompt": "0.000002"}},
            {"id": "a/base:free", "pricing": {"prompt": "0"}}
        ]}"#
        .to_string();

        let body = raw.clone();
        let outcome = run_sync(
            &app.state.pool,
            move || std::future::ready(Ok::<_, SyncError>(body.clone())),
            &actor,
        )
        .await
        .expect("sync");
        assert_eq!(outcome.status, SyncStatus::Success);
        assert_eq!(outcome.model_count, 2, "only managed ids count");

        let (models, offerings): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM models), (SELECT COUNT(*) FROM offerings)",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("counts");
        assert_eq!((models, offerings), (2, 2));
        let batch_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM models WHERE canonical_name = 'a/base:batch'")
                .fetch_one(&app.state.pool)
                .await
                .expect("batch count");
        assert_eq!(batch_rows, 0, "the price sync must not create :batch");

        // An existing offering for a filtered id still gets its price.
        let provider_id: String = sqlx::query_scalar("SELECT id FROM providers WHERE name = ?")
            .bind(PROVIDER_NAME)
            .fetch_one(&app.state.pool)
            .await
            .expect("provider");
        let now = catalog::now_ts();
        sqlx::query(
            "INSERT INTO models (id, canonical_name, created_at, updated_at) VALUES (?, ?, ?, ?)",
        )
        .bind("model-batch")
        .bind("a/base:batch")
        .bind(&now)
        .bind(&now)
        .execute(&app.state.pool)
        .await
        .expect("manual model");
        sqlx::query(
            "INSERT INTO offerings \
               (id, provider_id, model_id, upstream_model_id, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 1, ?, ?)",
        )
        .bind("offering-batch")
        .bind(&provider_id)
        .bind("model-batch")
        .bind("a/base:batch")
        .bind(&now)
        .bind(&now)
        .execute(&app.state.pool)
        .await
        .expect("manual offering");

        let body = raw.clone();
        let outcome = run_sync(
            &app.state.pool,
            move || std::future::ready(Ok::<_, SyncError>(body.clone())),
            &actor,
        )
        .await
        .expect("second sync");
        assert_eq!(outcome.status, SyncStatus::Success);
        let batch_price: Option<f64> = sqlx::query_scalar(
            "SELECT input_per_m FROM prices WHERE offering_id = 'offering-batch'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("batch price");
        assert_eq!(
            batch_price,
            Some(2.0),
            "prices may still be recorded for existing offerings"
        );
        let models: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM models")
            .fetch_one(&app.state.pool)
            .await
            .expect("models");
        assert_eq!(models, 3, "no model row was created by the sync");
    }

    // -----------------------------------------------------------------------
    // Sync behaviour
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn success_sync_applies_models_offerings_prices_and_one_run() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };

        let outcome = run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("sync");
        assert_eq!(outcome.status, SyncStatus::Success);
        assert_eq!(outcome.model_count, 5);
        assert!(!outcome.stale);
        assert!(outcome.error.is_none());

        let counts: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM providers), \
                    (SELECT COUNT(*) FROM models), \
                    (SELECT COUNT(*) FROM offerings), \
                    (SELECT COUNT(*) FROM prices)",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("counts");
        assert_eq!(counts, (1, 5, 5, 5));

        // The provider row is the managed openrouter one.
        let (name, base_url): (String, String) =
            sqlx::query_as("SELECT name, base_url FROM providers")
                .fetch_one(&app.state.pool)
                .await
                .expect("provider row");
        assert_eq!(name, "openrouter");
        assert_eq!(base_url, PROVIDER_BASE_URL);

        // The run is recorded with model count and raw detail.
        let (status, model_count, detail): (String, i64, String) =
            sqlx::query_as("SELECT status, model_count, detail_json FROM price_sync_runs")
                .fetch_one(&app.state.pool)
                .await
                .expect("run row");
        assert_eq!(status, "success");
        assert_eq!(model_count, 5);
        assert!(
            detail.contains("0.0000025"),
            "raw values live in the detail"
        );

        // Synced prices carry the mapped cache components end to end.
        let (cache_read, cache_write): (Option<f64>, Option<f64>) = sqlx::query_as(
            "SELECT p.cache_read_per_m, p.cache_write_per_m \
             FROM prices p JOIN offerings o ON o.id = p.offering_id \
             JOIN models m ON m.id = o.model_id WHERE m.canonical_name = 'openai/gpt-4o'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("gpt-4o price row");
        assert_eq!(cache_read, Some(1.25), "input_cache_read is mapped");
        assert_eq!(cache_write, Some(6.25), "input_cache_write is mapped");

        // Exactly one audit row names the sync.
        let audits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'sync.openrouter'")
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
        assert_eq!(audits, 1);
    }

    #[tokio::test]
    async fn variable_prices_are_stored_null_and_never_rank_as_cheapest() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };

        let outcome = run_sync(
            &app.state.pool,
            || std::future::ready(Ok::<_, SyncError>(VARIABLE_FIXTURE.to_string())),
            &actor,
        )
        .await
        .expect("sync");
        assert_eq!(outcome.status, SyncStatus::Success);
        assert_eq!(outcome.model_count, 3);

        // Storage: never -1e6, the components are NULL and the row is flagged.
        let auto: (Option<f64>, Option<f64>, i64) = sqlx::query_as(
            "SELECT p.input_per_m, p.output_per_m, p.variable_price \
             FROM prices p JOIN offerings o ON o.id = p.offering_id \
             JOIN models m ON m.id = o.model_id WHERE m.canonical_name = 'openrouter/auto'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("auto price");
        assert_eq!(auto, (None, None, 1), "the sentinel is not a price");
        let priced: (Option<f64>, i64) = sqlx::query_as(
            "SELECT p.input_per_m, p.variable_price \
             FROM prices p JOIN offerings o ON o.id = p.offering_id \
             JOIN models m ON m.id = o.model_id WHERE m.canonical_name = 'openai/priced-model'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("priced row");
        assert_eq!(priced, (Some(1.0), 0));

        // The run detail names the variable ids for operators.
        let detail: String = sqlx::query_scalar("SELECT detail_json FROM price_sync_runs")
            .fetch_one(&app.state.pool)
            .await
            .expect("detail");
        assert!(detail.contains("openrouter/auto"), "{detail}");
        assert!(detail.contains("variable_prices"), "{detail}");

        // Regression: routing a normal model against the sentinel model. The
        // stored rows feed the same blended price the router uses, and the
        // variable one has none, so it can never win on price.
        let auto_price = price_by_model(&app, "openrouter/auto").await;
        let normal_price = price_by_model(&app, "openai/priced-model").await;
        let blended = |price: &catalog::PriceRow| match (price.input_per_m, price.output_per_m) {
            (Some(input), Some(output)) => Some(input + output),
            _ => None,
        };
        assert_eq!(blended(&auto_price), None);
        assert_eq!(blended(&normal_price), Some(3.0));
        assert!(
            crate::mix::mix_price(&auto_price, &crate::mix::code_agent_profile()).is_none(),
            "an all-variable offering has no mix price"
        );

        let signal = |id: &str, price: Option<f64>| {
            let mut signal = crate::routing::CandidateSignal::neutral(id);
            signal.price_per_m_usd = price;
            signal
        };
        let pool = || {
            vec![
                signal("openrouter/auto", blended(&auto_price)),
                signal("openai/priced-model", blended(&normal_price)),
            ]
        };
        for preset in ["cheapest", "balanced"] {
            let (_, weights) = crate::routing::Weights::preset(preset).expect("preset");
            let order = crate::routing::weighted_order(pool(), &weights);
            let ids: Vec<&str> = order
                .ordered
                .iter()
                .map(|entry| entry.id.as_str())
                .collect();
            assert_eq!(
                ids,
                ["openai/priced-model", "openrouter/auto"],
                "{preset} must not rank the variable price first"
            );
            let auto = order
                .ordered
                .iter()
                .find(|entry| entry.id == "openrouter/auto")
                .expect("auto scored");
            assert!(
                auto.score.imputed.contains(&"price"),
                "the variable price is imputed, not measured"
            );
        }
    }

    #[tokio::test]
    async fn sync_is_idempotent_across_runs() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };

        for _ in 0..2 {
            run_sync(&app.state.pool, fixture_fetch, &actor)
                .await
                .expect("sync");
        }

        let counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM models), \
                    (SELECT COUNT(*) FROM offerings), \
                    (SELECT COUNT(*) FROM prices)",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("counts");
        assert_eq!(counts, (5, 5, 5), "rows are upserted, never duplicated");

        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM price_sync_runs")
            .fetch_one(&app.state.pool)
            .await
            .expect("runs");
        assert_eq!(runs, 2, "each trigger records its own run");
    }

    #[tokio::test]
    async fn run_detail_keeps_raw_prices_only_for_changed_and_skipped_models() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };

        // First sync: every price is new (changed), so every raw value is kept.
        run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("first sync");
        let first: String =
            sqlx::query_scalar("SELECT detail_json FROM price_sync_runs ORDER BY finished_at, id")
                .fetch_one(&app.state.pool)
                .await
                .expect("first run detail");
        let first: serde_json::Value = serde_json::from_str(&first).expect("detail json");
        assert_eq!(
            first["raw_prices"].as_object().expect("raw_prices").len(),
            5,
            "a first run changes every price: {first}"
        );

        // Second sync: nothing changed, so no raw values are kept (D19).
        run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("second sync");
        let mut details: Vec<String> =
            sqlx::query_scalar("SELECT detail_json FROM price_sync_runs ORDER BY rowid")
                .fetch_all(&app.state.pool)
                .await
                .expect("run details");
        let second: serde_json::Value =
            serde_json::from_str(&details.pop().expect("second run detail")).expect("detail json");
        assert_eq!(
            second["raw_prices"].as_object().expect("raw_prices").len(),
            0,
            "an unchanged run keeps no raw values: {second}"
        );

        // Third sync: one row went manual in between, so only that model's
        // raw price is kept — for comparison with the skipped override.
        sqlx::query(
            "UPDATE prices SET source = 'manual' \
             WHERE offering_id = (SELECT o.id FROM offerings o JOIN models m ON m.id = o.model_id \
                                  WHERE m.canonical_name = 'openai/gpt-4o')",
        )
        .execute(&app.state.pool)
        .await
        .expect("mark manual");
        run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("third sync");
        let mut details: Vec<String> =
            sqlx::query_scalar("SELECT detail_json FROM price_sync_runs ORDER BY rowid")
                .fetch_all(&app.state.pool)
                .await
                .expect("run details");
        let third: serde_json::Value =
            serde_json::from_str(&details.pop().expect("third run detail")).expect("detail json");
        let raw = third["raw_prices"].as_object().expect("raw_prices");
        assert_eq!(
            raw.len(),
            1,
            "only the skipped model keeps raw values: {third}"
        );
        assert!(raw.contains_key("openai/gpt-4o"), "{third}");
        assert_eq!(third["prices_skipped_manual"], 1);
    }

    #[tokio::test]
    async fn sync_runs_are_pruned_to_the_newest_30() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };

        // 35 older runs of the same source, one per second.
        for i in 0..35 {
            let ts = format!("2026-01-01T00:{:02}:{:02}.000Z", i / 60, i % 60);
            sqlx::query(
                "INSERT INTO price_sync_runs \
                   (id, source, started_at, finished_at, status, model_count, detail_json) \
                 VALUES (?, 'openrouter', ?, ?, 'error', 0, '{}')",
            )
            .bind(format!("syn-{i:02}"))
            .bind(&ts)
            .bind(&ts)
            .execute(&app.state.pool)
            .await
            .expect("synthetic run");
        }

        run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("sync");

        // The newest 30 survive: the new run plus the 29 newest synthetics.
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM price_sync_runs")
            .fetch_one(&app.state.pool)
            .await
            .expect("run count");
        assert_eq!(total, 30, "runs are pruned to the newest 30 (D19)");
        let kept: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM price_sync_runs WHERE id IN ('syn-00', 'syn-04', 'syn-05')",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("kept count");
        assert_eq!(kept, 0, "the oldest synthetic runs are gone");
        let newest_synthetic: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM price_sync_runs WHERE id = 'syn-34'")
                .fetch_one(&app.state.pool)
                .await
                .expect("newest synthetic");
        assert_eq!(newest_synthetic, 1, "the newest synthetic run survives");
    }

    #[tokio::test]
    async fn manual_prices_are_never_overwritten() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };

        run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("first sync");

        // The admin overrides one offering's price by hand.
        let offering_id: String = sqlx::query_scalar(
            "SELECT o.id FROM offerings o JOIN models m ON m.id = o.model_id \
             WHERE m.canonical_name = 'openai/gpt-4o'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("offering id");
        sqlx::query(
            "UPDATE prices SET input_per_m = 99.0, source = 'manual', updated_by = ? \
             WHERE offering_id = ?",
        )
        .bind(&user_id)
        .bind(&offering_id)
        .execute(&app.state.pool)
        .await
        .expect("mark manual");

        let outcome = run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("second sync");
        assert_eq!(outcome.status, SyncStatus::Success);

        let (source, input): (String, Option<f64>) =
            sqlx::query_as("SELECT source, input_per_m FROM prices WHERE offering_id = ?")
                .bind(&offering_id)
                .fetch_one(&app.state.pool)
                .await
                .expect("price row");
        assert_eq!(source, "manual");
        assert_eq!(input, Some(99.0), "the manual value survives the sync");

        // Non-manual rows are still refreshed by the sync.
        let openrouter_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM prices WHERE source = 'openrouter'")
                .fetch_one(&app.state.pool)
                .await
                .expect("openrouter rows");
        assert_eq!(openrouter_rows, 4);
    }

    #[tokio::test]
    async fn failed_fetch_serves_the_previous_snapshot_and_records_the_run() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };

        run_sync(&app.state.pool, fixture_fetch, &actor)
            .await
            .expect("good sync");

        let outcome = run_sync(&app.state.pool, erroring_fetch, &actor)
            .await
            .expect("failed sync is still an outcome");
        assert_eq!(outcome.status, SyncStatus::StaleIfError);
        assert!(outcome.stale);
        assert_eq!(outcome.model_count, 5, "the snapshot keeps serving");
        assert!(
            outcome
                .error
                .as_deref()
                .is_some_and(|e| e.contains("timed out"))
        );

        // The catalog is untouched and both runs are recorded.
        let models: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM models")
            .fetch_one(&app.state.pool)
            .await
            .expect("models");
        assert_eq!(models, 5);
        let statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM price_sync_runs ORDER BY finished_at, id")
                .fetch_all(&app.state.pool)
                .await
                .expect("runs");
        assert_eq!(statuses, vec!["success", "stale-if-error"]);
    }

    #[tokio::test]
    async fn failed_first_sync_reports_error_because_no_snapshot_exists() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };

        let outcome = run_sync(&app.state.pool, erroring_fetch, &actor)
            .await
            .expect("failed sync is still an outcome");
        assert_eq!(outcome.status, SyncStatus::Error);
        assert!(!outcome.stale);
        assert_eq!(outcome.model_count, 0);

        let (status, detail): (String, String) =
            sqlx::query_as("SELECT status, detail_json FROM price_sync_runs")
                .fetch_one(&app.state.pool)
                .await
                .expect("run row");
        assert_eq!(status, "error");
        assert!(detail.contains("connection timed out"));
    }

    #[tokio::test]
    async fn a_failed_sync_still_writes_one_audit_row() {
        let app = TestApp::new().await;
        let user_id = admin_id(&app).await;
        let actor = Actor {
            user_id: &user_id,
            ip: "127.0.0.1",
        };

        run_sync(&app.state.pool, erroring_fetch, &actor)
            .await
            .expect("outcome");
        let audits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'sync.openrouter'")
                .fetch_one(&app.state.pool)
                .await
                .expect("audit count");
        assert_eq!(audits, 1);
    }

    // -----------------------------------------------------------------------
    // Live-fetch hardening (2026-11-17)
    // -----------------------------------------------------------------------

    /// Serves `full_body` on every connection, but advertises the full length
    /// and writes only half of it on the first one, then closes: a truncated
    /// transfer, the live failure's shape.
    async fn spawn_truncating_server(full_body: &'static str) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind truncating listener");
        let addr = listener.local_addr().expect("listener address");
        tokio::spawn(async move {
            let mut first = true;
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let body = match first {
                    true => {
                        first = false;
                        &full_body[..full_body.len() / 2]
                    }
                    false => full_body,
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    full_body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_truncated_body_is_retried_and_succeeds() {
        let addr = spawn_truncating_server(VARIABLE_FIXTURE).await;
        let body = fetch_catalog_from(&format!("http://{addr}/models"))
            .await
            .expect("the second attempt serves the whole body");
        assert_eq!(body, VARIABLE_FIXTURE);
        assert_eq!(parse_catalog(&body).expect("parses").models.len(), 3);
    }

    #[tokio::test]
    async fn a_failing_host_names_the_layer_and_the_attempt() {
        // Bind then drop: the port is free, so every connection is refused.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind probe listener");
        let addr = listener.local_addr().expect("probe address");
        drop(listener);

        let error = fetch_catalog_from(&format!("http://{addr}/models"))
            .await
            .expect_err("no server listens");
        let message = error.to_string();
        assert!(message.contains("connect"), "the layer is named: {message}");
        assert!(
            message.contains("attempt 2"),
            "the last attempt is named: {message}"
        );
    }

    #[tokio::test]
    async fn a_non_2xx_answer_is_not_retried() {
        let (addr, seen) =
            crate::test_support::spawn_static_server("500 Internal Server Error", "{}").await;
        let error = fetch_catalog_from(&format!("http://{addr}/models"))
            .await
            .expect_err("500 is an error");
        let message = error.to_string();
        assert!(message.contains("status"), "the layer is named: {message}");
        assert!(message.contains("HTTP 500"), "{message}");
        assert_eq!(
            seen.lock().expect("seen lock").len(),
            1,
            "a definite upstream answer is never retried"
        );
    }

    #[test]
    fn snapshot_freshness_follows_the_24h_ttl() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-16T12:00:00.000Z")
            .expect("ts")
            .to_utc();
        assert!(!snapshot_is_fresh(None, &now), "no snapshot is not fresh");
        assert!(snapshot_is_fresh(Some("2026-11-16T11:00:00.000Z"), &now));
        assert!(
            snapshot_is_fresh(Some("2026-11-15T12:00:01.000Z"), &now),
            "just inside the 24h window"
        );
        assert!(
            !snapshot_is_fresh(Some("2026-11-15T11:59:59.000Z"), &now),
            "just outside the 24h window is stale"
        );
        assert!(
            !snapshot_is_fresh(Some("2026-11-17T12:00:00.000Z"), &now),
            "a timestamp in the future is not fresh"
        );
    }
}
