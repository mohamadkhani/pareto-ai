//! Chat data plane pipeline (M3+M5): `POST /v1/chat/completions`.
//!
//! One request runs validate → resolve offering → forward → stream → settle:
//!
//! * **Validate**: body size (handler-enforced, `GATEWAY_MAX_BODY_BYTES`,
//!   32 MiB by default; the message count is unbounded, see D62),
//!   `max_tokens` clamped to the offering's output limit, and
//!   `context_length_exceeded` before any upstream call when the estimated
//!   input plus `max_tokens` does not fit the offering's context.
//! * **Resolve**: enabled offerings of enabled providers and enabled models
//!   matching the requested model (`upstream_model_id` or `canonical_name`) in
//!   a fixed deterministic order (creation order, id tiebreak), or — for the
//!   M5 `combo:<name>` namespace — the combo's member offerings in the combo's
//!   strategy order (`ordered` = stored list, `round_robin` = stable rotation
//!   persisted in `combo_models.rr_cursor` — advanced once per request that
//!   actually dispatches upstream, never for a pre-dispatch refusal —
//!   `weighted` = [`crate::routing`] scoring with the combo's weights and
//!   per-member weight multipliers).
//!   Credentials in cooldown are demoted to the end of the order on the
//!   fixed-order paths; the weighted strategy demotes them through the
//!   recent-failure penalty instead. The M5 circuit breaker
//!   ([`crate::breaker`], per (credential, offering)) excludes an open pair
//!   entirely; see its module docs for how it layers with the M3 cooldown.
//!   Access grants (M4, decisions D9 and D59) then filter the candidates: the
//!   user's verdict always applies. A key without grants inherits it; a key
//!   with its own grants applies on top and can narrow that verdict but never
//!   widen it. Before forwarding, the request's estimate is reserved against
//!   the user's budgets and token caps (decision D7).
//! * **Failover**: only before the first byte reaches the client — before the
//!   first upstream chunk is forwarded for streams, before the response is
//!   returned for buffered calls. `400`/`422` never fail over. A failure
//!   attributable to the credential (`upstream_auth` 401/403, `upstream_quota`
//!   402, `upstream_rate_limited` 429) first retries the **same offering with
//!   the next eligible credential** (ordered: not cooled and not breaker-open,
//!   then cooled; a credential that just failed is never retried in the same
//!   request), then moves to the next offering. Inside a combo an
//!   `upstream_auth` failure also advances to the next member — the member
//!   list is the operator's explicit fallback chain. `429` fails over at most
//!   once per request, honouring `Retry-After` as a cooldown on that
//!   credential.
//! * **Settle**: exactly one `usage_events` and one `request_logs` row per
//!   request (never one per attempt — a failed over request is not double
//!   spent), plus one `telemetry_attempts` sample per classified attempt
//!   (decision D26): a failure rescued by failover still teaches the router
//!   about the offering that failed. Upstream `usage` is authoritative; when
//!   missing, tokens are estimated `chars/4` from **content text only**
//!   (`choices[].message.content` / `delta.content`, plus `reasoning`; input
//!   from the message text) and `tokens_estimated` is set — never from JSON
//!   scaffolding or raw bytes, and only when a request was actually sent
//!   upstream (review M1/M3). The same transaction prices the request (M4,
//!   decisions D7/D10), settles its reservation into the budgets and token
//!   caps of the period it started in, and books the `usage_daily` rollup.
//!   `decision_json` carries the M5 explanation: every candidate with its
//!   score breakdown and a one-line reason (why chosen, why not), plus the
//!   routing block (strategy, combo, weights, round-robin cursor).
//!
//! Cancellation: the response body (or the awaiting handler for buffered
//! calls) holds a guard; when the client disconnects the guard signals the
//! runner, which drops the upstream response within milliseconds and settles
//! the rows as `client_disconnected` (status 499).
//!
//! Mid-stream failures cannot change the already-committed `200` status: they
//! surface in-band as one SSE `data: {"error": {...}}` event in the uniform
//! error shape, preceded by a blank line (`\n\n`) so it can never concatenate
//! onto a partial upstream line, followed by `data: [DONE]`, then the stream
//! ends.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::StatusCode;
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use crate::AppState;
use crate::access;
use crate::benchmarks;
use crate::catalog;
use crate::combos;
use crate::mix;
use crate::money;
use crate::reports;
use crate::routing::{self, Strategy};
use crate::telemetry;
use crate::upstream::detail::ErrorDetail;
use crate::upstream::sse::{self, SseInspector, Usage};
use crate::upstream::{
    Budget, FailureClass, TimeoutKind, UpstreamCall, UpstreamError, UpstreamStream,
};

/// Largest buffered (non-streaming) response kept in memory.
const MAX_BUFFERED_RESPONSE: usize = 32 * 1024 * 1024;
/// Status recorded for a request whose client disconnected before completion.
const CLIENT_DISCONNECTED_STATUS: i64 = 499;

/// Everything the pipeline knows about the HTTP request around the body.
#[derive(Debug, Clone)]
pub struct RequestMeta {
    /// Server-generated ledger key (the request-id middleware's stamp or a
    /// fresh UUID); joins both accounting rows and is what error bodies show.
    pub request_id: String,
    /// The client's optional `x-request-id` echo, preserved in
    /// `request_logs.client_request_id` and never used as a key (review B1).
    pub client_request_id: Option<String>,
    /// The authenticated caller: an API key's owner or an admin session.
    pub user_id: String,
    /// The caller's role (access defaults, decision D9).
    pub role: crate::auth::Role,
    /// The API key behind the request, `None` for session callers (M4).
    pub api_key: Option<crate::api_keys::KeyContext>,
    /// The client's optional `X-Session-Id`, used by sticky combos (D35).
    pub session_hint: Option<String>,
    /// Wall-clock request start: the period a request settles into (D7) and
    /// the rollup day key.
    pub started_at: String,
    pub method: String,
    pub path: String,
}

/// A pipeline failure in the taxonomy shape, ready for the error response.
#[derive(Debug, Clone)]
pub struct ChatFailure {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    /// Seconds to honour as `Retry-After` (upstream rate limits).
    pub retry_after: Option<u64>,
    /// Bounded, redacted provider text for the operator (D49). Never serialized
    /// into the client response: `into_api_error` and the in-band error event
    /// read only `code`, `message`, and `retry_after`.
    pub detail: Option<String>,
}

impl ChatFailure {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
            detail: None,
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error",
        )
    }

    /// The uniform `{"error": {...}}` response for this failure.
    pub fn into_api_error(self, request_id: Option<String>) -> crate::error::ApiError {
        let mut error = crate::error::ApiError::custom(self.status, self.code, self.message)
            .with_request_id(request_id);
        if let Some(seconds) = self.retry_after {
            error = error.with_retry_after(seconds);
        }
        error
    }
}

/// What the handler does with a completed pipeline run.
pub enum ChatOutcome {
    /// Complete JSON response body (`stream: false`).
    Json(Bytes),
    /// SSE response body (`stream: true`); mid-stream failures arrive in-band.
    Sse(Body),
}

// ---------------------------------------------------------------------------
// Request parsing and validation
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct RawRequest {
    model: Option<Value>,
    messages: Option<Value>,
    #[serde(default)]
    stream: bool,
    max_tokens: Option<i64>,
    /// Every other OpenAI-compatible field, forwarded untouched.
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug)]
struct WireRequest {
    model: String,
    messages: Vec<Value>,
    stream: bool,
    max_tokens: Option<i64>,
    extra: serde_json::Map<String, Value>,
}

/// Output estimate for a request that does not set `max_tokens` (the M4
/// reservation formula needs one): bounded by the offering's output limit.
const DEFAULT_OUTPUT_ESTIMATE: i64 = 4096;

/// The reservation estimate, in tokens: message text over four (the content
/// estimator, review M1) plus the output budget — the request's clamped
/// `max_tokens`, or [`DEFAULT_OUTPUT_ESTIMATE`] clamped to the offering's
/// limit when the request sets none.
fn estimate_tokens(
    input_chars: usize,
    max_tokens: Option<i64>,
    offering_max_output: Option<i64>,
) -> (i64, i64) {
    let estimated_input = (input_chars / 4) as i64;
    let estimated_output = match max_tokens {
        Some(max) => max.max(0),
        None => offering_max_output
            .unwrap_or(DEFAULT_OUTPUT_ESTIMATE)
            .clamp(0, DEFAULT_OUTPUT_ESTIMATE),
    };
    (estimated_input, estimated_output)
}

/// Message-text characters of the request: the `content` of each message
/// (string or text parts). This is what the input estimate counts — never the
/// raw body bytes, which inflate UTF-8 text and count JSON scaffolding
/// (review M1).
fn message_text_chars(messages: &[Value]) -> usize {
    messages
        .iter()
        .filter_map(|message| message.get("content"))
        .map(sse::text_chars)
        .sum()
}

/// Parses and validates the request body. Failure mode: `400 invalid_request`.
fn parse_request(raw: &[u8]) -> Result<WireRequest, ChatFailure> {
    let raw: RawRequest =
        serde_json::from_slice(raw).map_err(|_| ChatFailure::invalid("invalid request body"))?;

    let model = raw
        .model
        .as_ref()
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ChatFailure::invalid("`model` must be a non-empty string"))?;

    let messages = raw.messages.and_then(|value| match value {
        Value::Array(items) => Some(items),
        _ => None,
    });
    let messages = match messages {
        Some(messages) if messages.is_empty() => {
            return Err(ChatFailure::invalid("`messages` must not be empty"));
        }
        Some(messages) if messages.iter().any(|message| !message.is_object()) => {
            return Err(ChatFailure::invalid("each message must be an object"));
        }
        Some(messages) => messages,
        None => return Err(ChatFailure::invalid("`messages` must be an array")),
    };
    if raw.max_tokens.is_some_and(|max| max <= 0) {
        return Err(ChatFailure::invalid(
            "`max_tokens` must be a positive integer",
        ));
    }

    Ok(WireRequest {
        model,
        messages,
        stream: raw.stream,
        max_tokens: raw.max_tokens,
        extra: raw.extra,
    })
}

// ---------------------------------------------------------------------------
// Offering resolution
// ---------------------------------------------------------------------------

/// One provider credential selected for an offering.
#[derive(Debug, Clone)]
struct Credential {
    id: String,
    secret_cipher: String,
    headers_json: Option<String>,
    cooled: bool,
    /// Manual balance (advisory in v1, decision D8): feeds the low-quota
    /// penalty on the weighted path, never a hard gate.
    balance_usd: Option<f64>,
    balance_low_threshold: Option<f64>,
    /// Stored currency of `balance_usd`; non-USD values are advisory only.
    balance_currency: Option<String>,
}

/// One offering with everything needed to serve a request from it.
#[derive(Debug)]
struct Candidate {
    offering_id: String,
    model_id: String,
    canonical_name: String,
    upstream_model_id: String,
    provider_id: String,
    provider_name: String,
    base_url: String,
    /// The offering and its provider are both enabled (D35 pools include
    /// disabled rows so the preview can explain them).
    enabled: bool,
    context_length: Option<i64>,
    max_output_tokens: Option<i64>,
    /// The model's declared input modalities (JSON array), for the capability
    /// filter. Unknown modalities pass every requirement.
    modalities_input: Option<String>,
    offering_headers_json: Option<String>,
    /// The credential that will serve the next attempt for this offering.
    /// Rotated in-request on a credential-attributable failure (see
    /// [`credential_attributable`]).
    credential: Option<Credential>,
    /// Every eligible credential of this offering's provider, in the same
    /// deterministic preference order as [`ordered_credentials`] (not cooled and
    /// not breaker-open first, then cooled, then breaker-open). The first
    /// entry is `credential` when it was selected from this list.
    credentials: Vec<Credential>,
}

#[derive(Debug, sqlx::FromRow)]
struct OfferingJoin {
    offering_id: String,
    model_id: String,
    upstream_model_id: String,
    context_override: Option<i64>,
    max_output_override: Option<i64>,
    offering_headers_json: Option<String>,
    canonical_name: String,
    context_length: Option<i64>,
    max_output_tokens: Option<i64>,
    modalities_input: Option<String>,
    provider_id: String,
    provider_name: String,
    base_url: String,
    offering_enabled: i64,
    provider_enabled: i64,
    model_enabled: i64,
}

const OFFERING_JOIN_COLUMNS: &str = "o.id AS offering_id, o.model_id, o.upstream_model_id, \
     o.context_override, o.max_output_override, o.headers_json AS offering_headers_json, \
     m.canonical_name, m.context_length, m.max_output_tokens, m.modalities_input, \
     pr.id AS provider_id, pr.name AS provider_name, pr.base_url, \
     o.enabled AS offering_enabled, pr.enabled AS provider_enabled, \
     m.enabled AS model_enabled \
     FROM offerings o \
     JOIN models m ON m.id = o.model_id \
     JOIN providers pr ON pr.id = o.provider_id";

const SELECT_OFFERING_JOIN: &str = "SELECT o.id AS offering_id, o.model_id, o.upstream_model_id, \
     o.context_override, o.max_output_override, o.headers_json AS offering_headers_json, \
     m.canonical_name, m.context_length, m.max_output_tokens, m.modalities_input, \
     pr.id AS provider_id, pr.name AS provider_name, pr.base_url, \
     o.enabled AS offering_enabled, pr.enabled AS provider_enabled, \
     m.enabled AS model_enabled \
     FROM offerings o \
     JOIN models m ON m.id = o.model_id \
     JOIN providers pr ON pr.id = o.provider_id \
     WHERE pr.enabled = 1 AND o.enabled = 1 AND m.enabled = 1";

/// Builds one candidate from its join row, picking the provider credential
/// (M3 cooldown demotion plus M5 breaker avoidance, see [`ordered_credentials`]).
fn build_candidate(
    breaker: &crate::breaker::BreakerRegistry,
    row: OfferingJoin,
    credentials: &mut HashMap<String, Vec<CredentialRow>>,
    now: &str,
) -> Candidate {
    let credentials = ordered_credentials(
        credentials
            .get(&row.provider_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        &row.offering_id,
        now,
        breaker,
    );
    let credential = credentials.first().cloned();
    Candidate {
        offering_id: row.offering_id,
        model_id: row.model_id,
        canonical_name: row.canonical_name,
        upstream_model_id: row.upstream_model_id,
        provider_id: row.provider_id,
        provider_name: row.provider_name,
        base_url: row.base_url,
        enabled: row.offering_enabled == 1 && row.provider_enabled == 1 && row.model_enabled == 1,
        context_length: row.context_override.or(row.context_length),
        max_output_tokens: row.max_output_override.or(row.max_output_tokens),
        modalities_input: row.modalities_input,
        offering_headers_json: row.offering_headers_json,
        credential,
        credentials,
    }
}

/// Loads the credentials of every candidate provider in **one** query
/// (`WHERE provider_id IN (...)`), grouped by provider with `ORDER BY id`
/// inside a provider — the same per-provider order the one-query-per-candidate
/// path used. Failure mode: `500 internal` on database errors.
async fn load_credentials(
    pool: &sqlx::SqlitePool,
    rows: &[OfferingJoin],
) -> Result<HashMap<String, Vec<CredentialRow>>, ChatFailure> {
    let mut provider_ids: Vec<&str> = rows.iter().map(|row| row.provider_id.as_str()).collect();
    provider_ids.sort_unstable();
    provider_ids.dedup();
    if provider_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; provider_ids.len()].join(", ");
    let sql = format!(
        "SELECT id, provider_id, secret_cipher, headers_json, cooldown_until, balance_usd, \
                balance_low_threshold, balance_gate, balance_currency \
         FROM provider_credentials WHERE provider_id IN ({placeholders}) \
         ORDER BY provider_id, id"
    );
    let mut query = sqlx::query_as::<_, CredentialRow>(&sql);
    for provider_id in &provider_ids {
        query = query.bind(provider_id);
    }
    let credentials = query.fetch_all(pool).await.map_err(|e| {
        tracing::error!(error = %e, "resolving credentials failed");
        ChatFailure::internal()
    })?;
    let mut grouped: HashMap<String, Vec<CredentialRow>> = HashMap::new();
    for credential in credentials {
        grouped
            .entry(credential.provider_id.clone())
            .or_default()
            .push(credential);
    }
    Ok(grouped)
}

/// Resolves the requested plain model to candidates in the fixed M3 order:
/// creation order with id tiebreak (cooldown demotion and the weighted
/// strategy are applied by the caller). Failure mode: `500 internal` on
/// database errors (the "no candidates" case is decided by the caller).
async fn resolve_candidates(
    pool: &sqlx::SqlitePool,
    breaker: &crate::breaker::BreakerRegistry,
    requested: &str,
    now: &str,
) -> Result<Vec<Candidate>, ChatFailure> {
    let rows = sqlx::query_as::<_, OfferingJoin>(&format!(
        "{SELECT_OFFERING_JOIN} \
         AND (o.upstream_model_id = ? OR m.canonical_name = ?) \
         ORDER BY o.created_at, o.id"
    ))
    .bind(requested)
    .bind(requested)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "resolving offerings failed");
        ChatFailure::internal()
    })?;

    let mut credentials = load_credentials(pool, &rows).await?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in rows {
        candidates.push(build_candidate(breaker, row, &mut credentials, now));
    }
    Ok(candidates)
}

/// Resolves a combo's member offerings in member order. Members pointing at
/// missing or disabled offerings are dropped with a warning (their provider or
/// offering row is gone or switched off). Failure mode: `500 internal` on
/// database errors.
async fn resolve_members(
    pool: &sqlx::SqlitePool,
    breaker: &crate::breaker::BreakerRegistry,
    combo: &combos::Combo,
    now: &str,
) -> Result<Vec<Candidate>, ChatFailure> {
    let ids: Vec<&str> = combo
        .members
        .iter()
        .map(|member| member.offering_id.as_str())
        .collect();
    let placeholders = vec!["?"; ids.len()].join(", ");
    let members_sql = format!("{SELECT_OFFERING_JOIN} AND o.id IN ({placeholders})");
    let mut query = sqlx::query_as::<_, OfferingJoin>(&members_sql);
    for id in &ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(pool).await.map_err(|e| {
        tracing::error!(error = %e, "resolving combo members failed");
        ChatFailure::internal()
    })?;

    let mut credentials = load_credentials(pool, &rows).await?;
    let mut by_id: HashMap<String, Candidate> = HashMap::with_capacity(rows.len());
    for row in rows {
        let candidate = build_candidate(breaker, row, &mut credentials, now);
        by_id.insert(candidate.offering_id.clone(), candidate);
    }
    let mut candidates = Vec::with_capacity(combo.members.len());
    for member in &combo.members {
        match by_id.remove(&member.offering_id) {
            Some(candidate) => candidates.push(candidate),
            None => tracing::warn!(
                combo = %combo.name,
                offering_id = %member.offering_id,
                "combo member is missing or disabled; skipped"
            ),
        }
    }
    Ok(candidates)
}

/// Resolves every offering in the catalog (enabled or not) for a `pool = all`
/// combo, in the same stable creation order as plain models. Disabled rows are
/// kept so [`combos::resolve_pool`] can report them; the preview shares this
/// loader. Failure mode: `500 internal` on database errors.
async fn resolve_pool_candidates(
    state: &AppState,
    now: &str,
) -> Result<Vec<Candidate>, ChatFailure> {
    let rows = sqlx::query_as::<_, OfferingJoin>(&format!(
        "SELECT {OFFERING_JOIN_COLUMNS} ORDER BY o.created_at, o.id"
    ))
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "resolving the catalog pool failed");
        ChatFailure::internal()
    })?;
    let mut credentials = load_credentials(&state.pool, &rows).await?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in rows {
        candidates.push(build_candidate(&state.breaker, row, &mut credentials, now));
    }
    Ok(candidates)
}

/// The message text of a content string or text parts (for the auto session
/// key). Non-text parts are ignored.
fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("content").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The conversation opening for the auto session key: the first `system`
/// message and the first `user` message (D35).
fn conversation_opening(messages: &[Value]) -> (Option<String>, Option<String>) {
    let role = |message: &Value| {
        message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let system = messages
        .iter()
        .find(|message| role(message) == "system")
        .map(message_text);
    let user = messages
        .iter()
        .find(|message| role(message) == "user")
        .map(message_text);
    (system, user)
}

/// The sticky session key of a request, when the combo is sticky and a key
/// can be derived (see [`combos::session_key`]).
fn sticky_session_key(
    combo: &combos::Combo,
    meta: &RequestMeta,
    messages: &[Value],
) -> Option<String> {
    if !combo.stickiness_enabled {
        return None;
    }
    let subject = meta
        .api_key
        .as_ref()
        .map(|key| key.key_id.as_str())
        .unwrap_or(meta.user_id.as_str());
    let (system, user) = conversation_opening(messages);
    combos::session_key(
        combo.stickiness_key_source,
        meta.session_hint.as_deref(),
        subject,
        system.as_deref(),
        user.as_deref(),
    )
}

/// Stable M3 demotion: candidates whose credential is in cooldown sink to the
/// end of the order, relative order kept.
fn demote_cooled(candidates: &mut [Candidate]) {
    candidates.sort_by_key(|candidate| {
        candidate
            .credential
            .as_ref()
            .is_some_and(|credential| credential.cooled)
    });
}

#[derive(Debug, sqlx::FromRow)]
struct CredentialRow {
    id: String,
    provider_id: String,
    secret_cipher: String,
    headers_json: Option<String>,
    cooldown_until: Option<String>,
    balance_usd: Option<f64>,
    balance_low_threshold: Option<f64>,
    balance_gate: String,
    /// Stored currency of `balance_usd` (D52): a non-USD balance is never
    /// gated on or used as a routing signal.
    balance_currency: Option<String>,
}

/// The provider's credentials for one offering in attempt order: not cooled
/// and not breaker-open first, then cooled ones (the M3 demotion), then
/// breaker-open ones (the caller excludes those). Id order inside each tier
/// keeps it deterministic. `rows` is one provider's slice from
/// [`load_credentials`], already in id order.
fn ordered_credentials(
    rows: &[CredentialRow],
    offering_id: &str,
    now: &str,
    breaker: &crate::breaker::BreakerRegistry,
) -> Vec<Credential> {
    let cooled = |row: &CredentialRow| {
        row.cooldown_until
            .as_deref()
            .is_some_and(|until| until > now)
    };
    let open = |row: &CredentialRow| breaker.is_open(&row.id, offering_id, Instant::now());
    let mut ordered: Vec<&CredentialRow> = rows.iter().filter(|row| !balance_blocks(row)).collect();
    ordered.sort_by_key(|row| (open(row), cooled(row)));
    ordered
        .into_iter()
        .map(|row| {
            warn_low_balance(row);
            Credential {
                id: row.id.clone(),
                secret_cipher: row.secret_cipher.clone(),
                headers_json: row.headers_json.clone(),
                cooled: cooled(row),
                balance_usd: row.balance_usd,
                balance_low_threshold: row.balance_low_threshold,
                balance_currency: row.balance_currency.clone(),
            }
        })
        .collect()
}

/// D42 balance gate: only a credential explicitly set to `block` whose stored
/// balance is at or below zero is skipped. The default `warn` gate never
/// changes routing, and an unavailable balance (`NULL`, `balance_available =
/// 0`) is unknown rather than zero, so it never blocks either. A balance stored
/// in another currency is not comparable to USD zero and is ignored (D52).
fn balance_blocks(row: &CredentialRow) -> bool {
    row.balance_gate == "block"
        && balance_is_usd(row)
        && row.balance_usd.is_some_and(|balance| balance <= 0.0)
}

/// A stored balance without a currency predates D52 and is USD.
fn balance_is_usd(row: &CredentialRow) -> bool {
    row.balance_currency
        .as_deref()
        .is_none_or(|currency| currency.eq_ignore_ascii_case("USD"))
}

/// The provider credential to use for one offering: the first entry of
/// [`ordered_credentials`] (not cooled and not breaker-open preferred,
/// then cooled). `None` when the provider has no credential at all.
#[cfg(test)]
fn pick_credential(
    rows: &[CredentialRow],
    offering_id: &str,
    now: &str,
    breaker: &crate::breaker::BreakerRegistry,
) -> Option<Credential> {
    ordered_credentials(rows, offering_id, now, breaker)
        .into_iter()
        .next()
}

/// The manual credential balance is **advisory** in v1 (decision D8): a
/// balance below its low threshold is warned about here, in the logs, and
/// never gates a request. A real balance sync later turns this into a gate.
fn warn_low_balance(row: &CredentialRow) {
    if !balance_is_usd(row) {
        return;
    }
    if let (Some(balance), Some(threshold)) = (row.balance_usd, row.balance_low_threshold)
        && balance < threshold
    {
        tracing::warn!(
            credential_id = %row.id,
            balance_usd = balance,
            low_threshold = threshold,
            "credential balance is below its low threshold (advisory only)"
        );
    }
}

/// Records a cooldown on the credential after an upstream 401/403/402/429.
/// Failures are logged and ignored: the cooldown is advisory, the response
/// must not depend on it.
async fn record_cooldown(
    pool: &sqlx::SqlitePool,
    credential_id: &str,
    cooldown: std::time::Duration,
) {
    let until = (chrono::Utc::now() + chrono::Duration::from_std(cooldown).unwrap_or_default())
        .format(crate::auth::TS_FMT)
        .to_string();
    let result = sqlx::query(
        "UPDATE provider_credentials SET cooldown_until = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&until)
    .bind(catalog::now_ts())
    .bind(credential_id)
    .execute(pool)
    .await;
    if let Err(e) = result {
        tracing::error!(error = %e, credential_id, "recording cooldown failed");
    }
}

// ---------------------------------------------------------------------------
// Routing decision bookkeeping
// ---------------------------------------------------------------------------

/// One row of `decision_json`: a candidate considered, its price when known,
/// its M5 score breakdown and one-line explanation, and what happened to it.
#[derive(Debug, Clone)]
struct CandidateRecord {
    id: String,
    provider: String,
    model: String,
    upstream_model_id: String,
    price: Option<Value>,
    credential_id: Option<String>,
    outcome: &'static str,
    error_class: Option<&'static str>,
    status: Option<i64>,
    /// The mix price used for this candidate, when computable (D34).
    mix_price: Option<mix::MixPrice>,
    /// `mix`, `component_sum`, or `unpriced` — how the routing price was
    /// obtained.
    price_basis: &'static str,
    /// The M5 score breakdown for weighted candidates (None elsewhere).
    score: Option<routing::Score>,
    /// Why this candidate sits where it sits (set by the router or the
    /// skip sites; synthesized from the outcome otherwise).
    explanation: Option<String>,
    /// Every credential this request attempted for the candidate, in order.
    /// Additive to `decision_json`: the field is always present as an array
    /// (empty when the candidate was skipped), so older readers keep working.
    credential_attempts: Vec<CredentialAttempt>,
}

/// One credential attempt inside a candidate, for `decision_json`.
#[derive(Debug, Clone)]
struct CredentialAttempt {
    credential_id: String,
    /// `failed` or `chosen`.
    outcome: &'static str,
    error_class: Option<&'static str>,
    status: Option<i64>,
}

/// The `decision_json` routing block: which strategy decided the order and
/// with what configuration — the "why this model" context for the admin UI.
#[derive(Debug, Clone)]
struct RoutingInfo {
    /// `fixed_order` (plain models) or the combo's strategy name.
    strategy: &'static str,
    /// The combo name behind `combo:<name>`, when this request used one.
    combo: Option<String>,
    /// The effective scoring weights (weighted combos only).
    weights: Option<Value>,
    /// The named weight preset, when `weights_json` named one.
    preset: Option<String>,
    /// The token-mix profile whose blend supplied the price component
    /// (decision D34).
    mix_profile: Option<mix::MixProfileRef>,
    /// The round-robin cursor consumed by this request.
    rr_cursor: Option<i64>,
}

impl Default for RoutingInfo {
    fn default() -> Self {
        Self {
            strategy: "fixed_order",
            combo: None,
            weights: None,
            preset: None,
            mix_profile: None,
            rr_cursor: None,
        }
    }
}

/// The `decision_json.sticky` block (D35): whether stickiness was enabled, the
/// key source, the pin in force for this session, its age, and why a previous
/// pin was replaced.
#[derive(Debug, Clone, Default)]
struct StickyInfo {
    enabled: bool,
    key_source: Option<&'static str>,
    pinned: Option<String>,
    age_s: Option<i64>,
    replaced_why: Option<String>,
}

impl StickyInfo {
    fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "key_source": self.key_source,
            "pinned": self.pinned,
            "age_s": self.age_s,
            "replaced_why": self.replaced_why,
        })
    }
}

#[derive(Debug, Clone, Default)]
struct Decision {
    candidates: Vec<CandidateRecord>,
    chosen_offering_id: Option<String>,
    reason: String,
    routing: RoutingInfo,
    sticky: StickyInfo,
    /// One telemetry sample per classified attempt (decision D26), written to
    /// `telemetry_attempts` with the settle transaction. Not part of
    /// `decision_json` — the candidate records already narrate the attempts.
    samples: Vec<telemetry::AttemptSample>,
}

/// The one-line explanation of a candidate that carries no router text yet,
/// composed from its outcome, position, and the terminal state.
fn explain_outcome(record: &CandidateRecord, chosen: Option<&str>, any_attempted: bool) -> String {
    match record.outcome {
        "chosen" => match record.error_class {
            Some(code) => format!("chosen: served the request, which ended with {code}"),
            None => "chosen: served the request".to_string(),
        },
        "failed" => format!(
            "tried and failed with {}{}",
            record.error_class.unwrap_or("unknown"),
            record
                .status
                .map(|status| format!(" (HTTP {status})"))
                .unwrap_or_default()
        ),
        "not_tried" => match chosen {
            Some(chosen) => format!("not reached: {chosen} served before this candidate"),
            // Review m5: candidates after exhausted failover were "not
            // reached" *because* attempts happened — never say none did.
            None => match any_attempted {
                true => "not reached: every attempted candidate failed before this one".to_string(),
                false => "not reached: no candidate was attempted".to_string(),
            },
        },
        "skipped_no_credential" => routing::Exclusion::NoCredential.explain(),
        "skipped_context" => routing::Exclusion::ContextExceeded.explain(),
        "skipped_breaker_open" => routing::Exclusion::BreakerOpen.explain(),
        _ => "not selected".to_string(),
    }
}

impl Decision {
    fn to_json(&self) -> Value {
        let any_attempted = self
            .candidates
            .iter()
            .any(|record| matches!(record.outcome, "failed" | "chosen"));
        json!({
            "candidates": self.candidates.iter().map(|record| {
                let explanation = record.explanation.clone().unwrap_or_else(|| {
                    explain_outcome(record, self.chosen_offering_id.as_deref(), any_attempted)
                });
                json!({
                    "id": record.id,
                    "provider": record.provider,
                    "model": record.model,
                    "upstream_model_id": record.upstream_model_id,
                    "price": record.price,
                    "mix_price": record.mix_price.as_ref().map(|mix| json!({
                        "per_m": mix.per_m,
                        "profile_id": self.routing.mix_profile.as_ref().map(|profile| &profile.profile_id),
                        "profile_name": self.routing.mix_profile.as_ref().map(|profile| &profile.profile_name),
                        "shares": {
                            "input": mix.effective_shares.input,
                            "output": mix.effective_shares.output,
                            "cache_read": mix.effective_shares.cache_read,
                            "cache_write": mix.effective_shares.cache_write,
                            "reasoning": mix.effective_shares.reasoning,
                        },
                    })),
                    "price_basis": record.price_basis,
                    "credential_id": record.credential_id,
                    "credential_attempts": record.credential_attempts.iter().map(|attempt| json!({
                        "credential_id": attempt.credential_id,
                        "outcome": attempt.outcome,
                        "error_class": attempt.error_class,
                        "status": attempt.status,
                    })).collect::<Vec<_>>(),
                    "outcome": record.outcome,
                    "error_class": record.error_class,
                    "status": record.status,
                    "score": record.score.as_ref().map(routing::Score::to_json),
                    "explanation": explanation,
                })
            }).collect::<Vec<_>>(),
            "chosen_offering_id": self.chosen_offering_id,
            "reason": self.reason,
            "routing": {
                "strategy": self.routing.strategy,
                "combo": self.routing.combo,
                "weights": self.routing.weights,
                "preset": self.routing.preset,
                "mix_profile": self.routing.mix_profile.as_ref().map(|profile| json!({
                    "profile_id": profile.profile_id,
                    "profile_name": profile.profile_name,
                })),
                "rr_cursor": self.routing.rr_cursor,
            },
            "sticky": self.sticky.to_json(),
        })
    }
}

/// The one-line reason plus the chosen candidate's mix-price provenance
/// (decision D34), e.g. `mix $0.578/M at code-agent (8.1/0.4/91.5)`.
fn decision_reason_with_mix(decision: &Decision) -> String {
    let mut reason = decision_reason(decision);
    let chosen = decision.chosen_offering_id.as_deref().and_then(|chosen| {
        decision
            .candidates
            .iter()
            .find(|record| record.id == chosen)
            .and_then(|record| record.mix_price.as_ref())
    });
    if let Some(mix) = chosen {
        let name = decision
            .routing
            .mix_profile
            .as_ref()
            .map(|profile| profile.profile_name.as_str())
            .unwrap_or("?");
        reason.push_str(&format!(
            "; mix ${:.3}/M at {} ({})",
            mix.per_m,
            name,
            mix.shares_label()
        ));
    }
    reason
}

/// The one-line reason for `decision_json`, composed from the attempt
/// outcomes, the strategy, and the terminal state.
fn decision_reason(decision: &Decision) -> String {
    let failed: Vec<String> = decision
        .candidates
        .iter()
        .filter(|record| record.outcome == "failed")
        .map(|record| {
            format!(
                "{} failed with {}",
                record.id,
                record.error_class.unwrap_or("unknown")
            )
        })
        .collect();
    let mut reason = String::new();
    if !failed.is_empty() {
        reason.push_str(&failed.join(", "));
        reason.push_str("; ");
    }
    match &decision.chosen_offering_id {
        Some(chosen) => {
            let chosen_record = decision
                .candidates
                .iter()
                .find(|record| &record.id == chosen);
            match chosen_record.and_then(|record| record.error_class) {
                Some(code) => reason.push_str(&format!("{chosen} chosen, ended with {code}")),
                None => match (
                    decision.routing.strategy,
                    chosen_record.and_then(|record| record.score.as_ref()),
                ) {
                    ("weighted", Some(score)) => {
                        let scored = decision
                            .candidates
                            .iter()
                            .filter(|record| record.score.is_some())
                            .count();
                        reason.push_str(&format!(
                            "{chosen} chosen (best weighted score {:.4} of {scored} scored candidates)",
                            score.total
                        ));
                    }
                    ("round_robin", _) => {
                        reason.push_str(&format!(
                            "{chosen} chosen (next in the round-robin rotation)"
                        ));
                    }
                    _ => reason.push_str(&format!(
                        "{chosen} chosen (first eligible candidate in fixed order)"
                    )),
                },
            }
        }
        None => {
            let skips: Vec<String> = decision
                .candidates
                .iter()
                .filter(|record| record.outcome.starts_with("skipped"))
                .map(|record| format!("{} {}", record.id, record.outcome))
                .collect();
            match (failed.is_empty(), skips.is_empty()) {
                (true, false) => {
                    reason.push_str(&format!("no eligible candidate ({})", skips.join(", ")))
                }
                _ => reason.push_str("no candidate left"),
            }
        }
    }
    reason
}

// ---------------------------------------------------------------------------
// The pipeline
// ---------------------------------------------------------------------------

/// One upstream attempt: which candidate, and the exact body to send it.
struct Attempt {
    decision_idx: usize,
    candidate: Candidate,
    body: Value,
    /// The offering's price row resolved during candidate resolution. The
    /// settlement reuses it when the request is served by this offering, so
    /// one request reads each price row once (docs/research/13-performance.md
    /// §2, §8.5). `None` means the offering is unpriced, not "not loaded".
    price: Option<catalog::PriceRow>,
    /// Index into `candidate.credentials` of the credential the next attempt
    /// uses. Starts at 0 and only advances on a credential-attributable
    /// failure, so no credential is retried in the same request.
    credential_cursor: usize,
}

/// The `decision_json` explanation fragment for the candidate's routing
/// price (D34): the mix, or the documented component-sum fallback.
fn price_note(
    mix_price: Option<&mix::MixPrice>,
    price: Option<&catalog::PriceRow>,
    profile: &mix::MixProfile,
) -> Option<String> {
    match mix_price {
        Some(mix) => Some(format!(
            "price from mix ${:.3}/M at {} ({})",
            mix.per_m,
            profile.name,
            mix.shares_label()
        )),
        None => scoring_price(price).map(|per_m| {
            format!("price from component sum ${per_m:.3}/M (input + output; no mix price)")
        }),
    }
}

/// The old per-component scoring price: input + output per million tokens,
/// used only when a mix price cannot be computed. A partial price is unknown,
/// never 0 (decision D10).
fn scoring_price(price: Option<&catalog::PriceRow>) -> Option<f64> {
    let price = price?;
    Some(price.input_per_m? + price.output_per_m?)
}

/// Whether the estimated input plus the output reservation fits the
/// offering's context window — the same content-only estimator as settlement
/// (review M1).
fn context_fits(candidate: &Candidate, input_chars: usize, max_tokens: Option<i64>) -> bool {
    match candidate.context_length {
        None => true,
        Some(context) => {
            let estimated_input = (input_chars / 4) as i64;
            estimated_input.saturating_add(max_tokens.unwrap_or(0)) <= context
        }
    }
}

/// Whether a candidate can actually serve this request right now: enabled,
/// with a usable credential that is neither cooled nor breaker-open, and
/// inside its context window. Sticky pins use the same test as the attempt
/// builder (D35).
fn candidate_ready(state: &AppState, candidate: &Candidate, parsed: &WireRequest) -> bool {
    if !candidate.enabled {
        return false;
    }
    let Some(credential) = &candidate.credential else {
        return false;
    };
    if credential.cooled {
        return false;
    }
    if state
        .breaker
        .is_open(&credential.id, &candidate.offering_id, Instant::now())
    {
        return false;
    }
    let max_tokens =
        crate::upstream::clamp_max_tokens(parsed.max_tokens, candidate.max_output_tokens);
    context_fits(candidate, message_text_chars(&parsed.messages), max_tokens)
}

/// Why a pinned offering is no longer eligible, if it is excluded by either
/// routing path.
fn sticky_excluded(
    offering_id: &str,
    pool_exclusions: &HashMap<String, combos::PoolExclusion>,
    weighted_exclusions: &HashMap<String, routing::Exclusion>,
) -> Option<String> {
    if let Some(excluded) = pool_exclusions.get(offering_id) {
        return Some(excluded.reason.as_str().to_string());
    }
    weighted_exclusions
        .get(offering_id)
        .map(|reason| reason.outcome().to_string())
}

/// The tokens this request wants from a candidate's context window (the same
/// estimate the context filter uses), for the near-context penalty.
fn requested_tokens(input_chars: usize, max_tokens: Option<i64>) -> f64 {
    ((input_chars / 4) as i64).saturating_add(max_tokens.unwrap_or(0)) as f64
}

/// Required model capabilities derived from the request: image content parts
/// need a vision-capable model. Per-task routing categories are a non-goal
/// (spec M5), so this is the only requirement today.
fn needs_image_input(messages: &[Value]) -> bool {
    messages.iter().any(|message| match message.get("content") {
        Some(Value::Array(parts)) => parts.iter().any(|part| {
            part.get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("image"))
        }),
        _ => false,
    })
}

/// Whether the model accepts image input: its declared input modalities
/// include `image`, or it declares none (unknown capabilities pass, like the
/// imputed score components).
fn modalities_allow_image(raw: Option<&str>) -> bool {
    let Some(modalities) = raw.and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok()) else {
        return true;
    };
    modalities.is_empty() || modalities.iter().any(|modality| modality == "image")
}

/// Builds the M5 routing signal of one candidate for the `weighted`
/// strategy. Missing metrics stay `None` — the router imputes them at 0.5
/// and marks them.
#[allow(clippy::too_many_arguments)]
fn signal_for(
    state: &AppState,
    candidate: &Candidate,
    price: Option<&catalog::PriceRow>,
    mix_price: Option<&mix::MixPrice>,
    quality: Option<routing::QualityScore>,
    member_weight: f64,
    requested_tokens: f64,
    needs_image: bool,
    context_fit: bool,
) -> routing::CandidateSignal {
    let stats = state.telemetry.stats(&candidate.offering_id);
    let credential = candidate.credential.as_ref();
    let breaker_open = credential.is_some_and(|credential| {
        state
            .breaker
            .is_open(&credential.id, &candidate.offering_id, Instant::now())
    });
    let balance_ratio = match (
        credential.and_then(|credential| credential.balance_usd),
        credential.and_then(|credential| credential.balance_low_threshold),
    ) {
        // A non-USD balance is not comparable to a USD threshold, so it never
        // feeds the low-quota penalty (D52).
        (Some(balance), Some(threshold))
            if threshold > 0.0
                && credential.is_some_and(|credential| {
                    credential
                        .balance_currency
                        .as_deref()
                        .is_none_or(|currency| currency.eq_ignore_ascii_case("USD"))
                }) =>
        {
            Some((balance / threshold).clamp(0.0, 1.0))
        }
        _ => None,
    };
    let mut signal = routing::CandidateSignal::neutral(candidate.offering_id.clone());
    // The price component is the mix price (D34); the old input + output
    // component sum is the fallback when no mix can be computed.
    signal.price_per_m_usd = mix_price
        .map(|mix| mix.per_m)
        .or_else(|| scoring_price(price));
    signal.tokens_per_sec = stats.as_ref().and_then(|stats| stats.tokens_per_sec);
    // A resolved-but-imputed quality (the neutral model default) is a
    // missing component, scored as 0.5 and marked like the others.
    signal.quality = quality
        .filter(|quality| !quality.imputed)
        .map(|quality| quality.score);
    signal.quality_source = quality.map(|quality| quality.source.as_str());
    signal.failure_rate = stats.as_ref().and_then(|stats| stats.failure_rate);
    signal.latency_p95_ms = stats.as_ref().and_then(|stats| stats.latency_p95_ms);
    signal.latency_cv = stats
        .as_ref()
        .and_then(telemetry::OfferingStats::latency_cv);
    // Thin telemetry windows shrink speed/reliability toward 0.5 (decision
    // D27).
    signal.telemetry_samples = stats.as_ref().map(|stats| stats.samples);
    signal.member_weight = member_weight;
    signal.breaker_open = breaker_open;
    signal.breaker_recovering = credential.is_some_and(|credential| {
        state
            .breaker
            .is_recovering(&credential.id, &candidate.offering_id, Instant::now())
    });
    signal.credential_cooled = credential.is_some_and(|credential| credential.cooled);
    signal.balance_ratio = balance_ratio;
    signal.context_pressure = routing::context_pressure(requested_tokens, candidate.context_length);
    signal.has_credential = credential.is_some();
    signal.capability_fit =
        !needs_image || modalities_allow_image(candidate.modalities_input.as_deref());
    signal.context_fit = context_fit;
    signal
}

/// Runs the full pipeline. Failure mode: every [`ChatFailure`] is ready for
/// the uniform error response; accounting rows are written before returning
/// (or before the first response byte, for streams).
pub async fn execute(
    state: &AppState,
    meta: &RequestMeta,
    raw_body: Bytes,
) -> Result<ChatOutcome, ChatFailure> {
    let started = Instant::now();
    let bytes_in = raw_body.len() as u64;

    let parsed = match parse_request(&raw_body) {
        Ok(parsed) => parsed,
        Err(failure) => {
            settle(
                state,
                &Settlement::rejected(meta, started, bytes_in, &failure),
            )
            .await;
            return Err(failure);
        }
    };
    let input_chars = message_text_chars(&parsed.messages);

    // M5: `combo:<name>` resolves to the combo's member offerings and the
    // combo's strategy; an unknown or disabled combo is `404 model_not_found`,
    // exactly like an unknown model.
    let now = catalog::now_ts();
    let combo = match parsed.model.strip_prefix(combos::NAMESPACE) {
        Some(name) => match combos::load_by_name(&state.pool, name).await {
            Ok(Some(combo)) if combo.enabled => Some(combo),
            Ok(_) => {
                let failure = ChatFailure::new(
                    StatusCode::NOT_FOUND,
                    "model_not_found",
                    format!("no eligible offering serves model {:?}", parsed.model),
                );
                settle(
                    state,
                    &Settlement::rejected_with_decision(
                        meta,
                        started,
                        bytes_in,
                        &failure,
                        Decision::default(),
                        &parsed.model,
                    ),
                )
                .await;
                return Err(failure);
            }
            Err(e) => {
                tracing::error!(error = %e, "loading the combo failed");
                return Err(ChatFailure::internal());
            }
        },
        None => None,
    };

    let mut candidates = match &combo {
        Some(combo) if combo.pool == combos::Pool::All => {
            resolve_pool_candidates(state, &now).await?
        }
        Some(combo) => resolve_members(&state.pool, &state.breaker, combo, &now).await?,
        None => resolve_candidates(&state.pool, &state.breaker, &parsed.model, &now).await?,
    };
    if candidates.is_empty() {
        let failure = ChatFailure::new(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("no eligible offering serves model {:?}", parsed.model),
        );
        settle(
            state,
            &Settlement::rejected_with_decision(
                meta,
                started,
                bytes_in,
                &failure,
                Decision::default(),
                &parsed.model,
            ),
        )
        .await;
        return Err(failure);
    }

    // Access control (decisions D9 and D59): a candidate survives only when
    // the user's verdict allows its model, offering, and credential, and the
    // key's verdict allows them too when the key has grants of its own; a key
    // without grants inherits the user's verdict. Admins are allowed by
    // default; a request whose candidates are all denied is a 403, never a 404.
    let api_key_id = meta.api_key.as_ref().map(|key| key.key_id.as_str());
    let grants = access::Grants::load(&state.pool, &meta.user_id, api_key_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "loading grants failed");
            ChatFailure::internal()
        })?;
    candidates.retain(|candidate| {
        grants.allows(
            meta.role,
            &meta.user_id,
            api_key_id,
            &candidate.model_id,
            &candidate.offering_id,
            candidate
                .credential
                .as_ref()
                .map(|credential| credential.id.as_str()),
        )
    });
    if candidates.is_empty() {
        let failure = ChatFailure::new(
            StatusCode::FORBIDDEN,
            "access_denied",
            "the caller may not use the model, offering, or credential behind this request",
        );
        settle(
            state,
            &Settlement::rejected_with_decision(
                meta,
                started,
                bytes_in,
                &failure,
                Decision::default(),
                &parsed.model,
            ),
        )
        .await;
        return Err(failure);
    }

    // The routing decision is recorded from here on: every candidate, its
    // price and M5 score when known, its one-line explanation, and what
    // happened to it.
    let mut prices: HashMap<String, Option<catalog::PriceRow>> = HashMap::new();
    // The effective token-mix profile (D34): a combo's `mix_profile`
    // overrides the global default. A resolution failure is logged and falls
    // back to the embedded `code-agent`, keeping routing deterministic.
    let default_mix = match state
        .mix_profiles
        .resolve_default(&state.pool, state.config.token_mix_profile.as_deref())
        .await
    {
        Ok(profile) => profile,
        Err(e) => {
            tracing::error!(error = %e, "resolving the default mix profile failed");
            mix::code_agent_profile()
        }
    };
    let mix_profile = match combo
        .as_ref()
        .and_then(|combo| combo.mix_profile.as_deref())
    {
        Some(selector) => match state
            .mix_profiles
            .resolve_selector(&state.pool, selector)
            .await
        {
            Ok(Some(profile)) => profile,
            Ok(None) => {
                tracing::warn!(
                    selector,
                    "the combo's mix profile is missing; using the default"
                );
                default_mix
            }
            Err(e) => {
                tracing::error!(error = %e, "resolving the combo's mix profile failed");
                default_mix
            }
        },
        None => default_mix,
    };
    let mut mix_prices: HashMap<String, Option<mix::MixPrice>> = HashMap::new();
    for candidate in &candidates {
        let price = match catalog::get_price(&state.pool, &candidate.offering_id).await {
            Ok(row) => row,
            Err(e) => {
                tracing::error!(error = %e, "reading price failed");
                None
            }
        };
        let mix_price = price
            .as_ref()
            .and_then(|price| mix::mix_price(price, &mix_profile));
        prices.insert(candidate.offering_id.clone(), price);
        mix_prices.insert(candidate.offering_id.clone(), mix_price);
    }

    let mut decision = Decision {
        candidates: Vec::with_capacity(candidates.len()),
        routing: RoutingInfo {
            combo: combo.as_ref().map(|combo| combo.name.clone()),
            mix_profile: Some(mix::MixProfileRef::from(&mix_profile)),
            ..RoutingInfo::default()
        },
        ..Decision::default()
    };
    // M5 strategy ordering: `ordered` and plain models keep their fixed
    // order, `round_robin` rotates over the members with the persisted
    // cursor, `weighted` scores the eligible pool and explains every
    // candidate. The M3 cooldown demotion applies on top of the fixed-order
    // paths; the weighted strategy demotes cooled credentials through the
    // recent-failure penalty instead (see `crate::routing`).
    let mut weighted_exclusions: HashMap<String, routing::Exclusion> = HashMap::new();
    let mut pool_exclusions: HashMap<String, combos::PoolExclusion> = HashMap::new();
    let mut weighted_scores: HashMap<String, routing::Score> = HashMap::new();
    let mut weighted_explanations: HashMap<String, String> = HashMap::new();
    if let Some(combo) = &combo {
        decision.routing.strategy = combo.strategy.as_str();
        match combo.strategy {
            Strategy::Ordered => {
                demote_cooled(&mut candidates);
            }
            Strategy::RoundRobin => {
                // Rotate from the persisted cursor. The cursor itself
                // advances only once the request actually dispatches (after
                // the attempts are built): a request refused before dispatch
                // must not consume the rotation (review nit).
                candidates = routing::rotate(candidates, combo.rr_cursor as usize);
                demote_cooled(&mut candidates);
            }
            Strategy::Weighted => {
                decision.routing.weights = Some(combo.weights.to_json());
                decision.routing.preset = combo.preset.map(str::to_owned);

                let mut model_ids: Vec<String> = candidates
                    .iter()
                    .map(|candidate| candidate.model_id.clone())
                    .collect();
                model_ids.sort();
                model_ids.dedup();
                let quality = match benchmarks::quality_for_models(&state.pool, &model_ids).await {
                    Ok(quality) => quality,
                    Err(e) => {
                        tracing::error!(error = %e, "loading model quality failed");
                        HashMap::new()
                    }
                };
                let needs_image = needs_image_input(&parsed.messages);
                let member_weights: HashMap<&str, f64> = combo
                    .members
                    .iter()
                    .map(|member| (member.offering_id.as_str(), member.weight))
                    .collect();
                if combo.pool == combos::Pool::All {
                    // D35: resolve the whole catalog with the combo's
                    // constraints through the same engine the preview uses.
                    let items: Vec<combos::PoolItem> = candidates
                        .iter()
                        .map(|candidate| {
                            let max_tokens = crate::upstream::clamp_max_tokens(
                                parsed.max_tokens,
                                candidate.max_output_tokens,
                            );
                            let mut signal = signal_for(
                                state,
                                candidate,
                                prices.get(&candidate.offering_id).and_then(Option::as_ref),
                                mix_prices
                                    .get(&candidate.offering_id)
                                    .and_then(Option::as_ref),
                                quality.get(&candidate.model_id).copied(),
                                1.0,
                                requested_tokens(input_chars, max_tokens),
                                needs_image,
                                context_fits(candidate, input_chars, max_tokens),
                            );
                            signal.enabled = candidate.enabled;
                            combos::PoolItem {
                                offering_id: candidate.offering_id.clone(),
                                model: candidate.canonical_name.clone(),
                                provider: candidate.provider_name.clone(),
                                quality: quality.get(&candidate.model_id).copied().unwrap_or(
                                    routing::QualityScore {
                                        score: 0.5,
                                        source: routing::QualitySource::ModelDefault,
                                        imputed: true,
                                    },
                                ),
                                mix_price: mix_prices
                                    .get(&candidate.offering_id)
                                    .and_then(Option::as_ref)
                                    .cloned(),
                                signal,
                            }
                        })
                        .collect();
                    let outcome = combos::resolve_pool(
                        items,
                        &combos::PoolConstraints::of(combo),
                        &combo.weights,
                    );
                    for scored in &outcome.ordered {
                        weighted_scores
                            .insert(scored.item.offering_id.clone(), scored.score.clone());
                        weighted_explanations
                            .insert(scored.item.offering_id.clone(), scored.explanation.clone());
                    }
                    for excluded in &outcome.excluded {
                        pool_exclusions.insert(excluded.offering_id.clone(), excluded.clone());
                    }
                    let mut by_id: HashMap<String, Candidate> = candidates
                        .into_iter()
                        .map(|candidate| (candidate.offering_id.clone(), candidate))
                        .collect();
                    let mut reordered = Vec::with_capacity(by_id.len());
                    for scored in &outcome.ordered {
                        if let Some(candidate) = by_id.remove(&scored.item.offering_id) {
                            reordered.push(candidate);
                        }
                    }
                    for excluded in &outcome.excluded {
                        if let Some(candidate) = by_id.remove(&excluded.offering_id) {
                            reordered.push(candidate);
                        }
                    }
                    reordered.extend(by_id.into_values());
                    candidates = reordered;
                } else {
                    let signals: Vec<routing::CandidateSignal> = candidates
                        .iter()
                        .map(|candidate| {
                            let max_tokens = crate::upstream::clamp_max_tokens(
                                parsed.max_tokens,
                                candidate.max_output_tokens,
                            );
                            signal_for(
                                state,
                                candidate,
                                prices.get(&candidate.offering_id).and_then(Option::as_ref),
                                mix_prices
                                    .get(&candidate.offering_id)
                                    .and_then(Option::as_ref),
                                quality.get(&candidate.model_id).copied(),
                                member_weights
                                    .get(candidate.offering_id.as_str())
                                    .copied()
                                    .unwrap_or(1.0),
                                requested_tokens(input_chars, max_tokens),
                                needs_image,
                                context_fits(candidate, input_chars, max_tokens),
                            )
                        })
                        .collect();
                    let outcome = routing::weighted_order(signals, &combo.weights);
                    for scored in &outcome.ordered {
                        weighted_scores.insert(scored.id.clone(), scored.score.clone());
                        weighted_explanations.insert(scored.id.clone(), scored.explanation.clone());
                    }
                    for (id, reason) in &outcome.excluded {
                        weighted_exclusions.insert(id.clone(), *reason);
                    }
                    // Rebuild the candidate order: scored candidates first
                    // (best first), then the excluded ones in their original
                    // order — their records explain why they are out.
                    let mut by_id: HashMap<String, Candidate> = candidates
                        .into_iter()
                        .map(|candidate| (candidate.offering_id.clone(), candidate))
                        .collect();
                    let mut reordered = Vec::with_capacity(by_id.len());
                    for scored in &outcome.ordered {
                        if let Some(candidate) = by_id.remove(&scored.id) {
                            reordered.push(candidate);
                        }
                    }
                    for (id, _) in &outcome.excluded {
                        if let Some(candidate) = by_id.remove(id) {
                            reordered.push(candidate);
                        }
                    }
                    reordered.extend(by_id.into_values());
                    candidates = reordered;
                }
            }
        }
    } else {
        // Plain models keep the fixed M3 order (creation order, id
        // tiebreak), cooled credentials demoted last.
        demote_cooled(&mut candidates);
    }

    // D35 session stickiness: a valid pin is put first when its offering is
    // still eligible; otherwise the resolved pool already re-ordered the
    // candidates and the first ready one becomes the new pin. The write
    // happens after the attempt list is built (a request that dispatches no
    // upstream call must not move the pin).
    let mut sticky_key: Option<String> = None;
    let mut sticky_pinned: Option<String> = None;
    let mut sticky_age: Option<i64> = None;
    let mut sticky_write: Option<String> = None;
    let mut sticky_ttl = 0i64;
    if let Some(combo) = &combo {
        decision.sticky.enabled = combo.stickiness_enabled;
        decision.sticky.key_source = combo
            .stickiness_enabled
            .then(|| combo.stickiness_key_source.as_str());
        if combo.stickiness_enabled {
            sticky_ttl = combo.stickiness_ttl_seconds;
            sticky_key = sticky_session_key(combo, meta, &parsed.messages);
            if let Some(key) = &sticky_key {
                let mut pin = match combos::load_pin_any(&state.pool, &combo.id, key).await {
                    Ok(pin) => pin,
                    Err(e) => {
                        tracing::error!(error = %e, "loading the sticky pin failed");
                        None
                    }
                };
                if let Some(existing) = &pin
                    && existing.expires_at <= now
                {
                    decision.sticky.replaced_why = Some("expired".to_string());
                    pin = None;
                }
                if let Some(existing) = pin {
                    let excluded = sticky_excluded(
                        &existing.offering_id,
                        &pool_exclusions,
                        &weighted_exclusions,
                    );
                    let ready = candidates
                        .iter()
                        .find(|candidate| candidate.offering_id == existing.offering_id)
                        .is_some_and(|candidate| candidate_ready(state, candidate, &parsed));
                    if let (None, true) = (&excluded, ready) {
                        if let Some(index) = candidates
                            .iter()
                            .position(|candidate| candidate.offering_id == existing.offering_id)
                        {
                            let candidate = candidates.remove(index);
                            candidates.insert(0, candidate);
                        }
                        sticky_age = combos::age_seconds(&existing.chosen_at, &now);
                        sticky_pinned = Some(existing.offering_id);
                    } else {
                        decision.sticky.replaced_why = Some(match excluded {
                            Some(reason) => reason,
                            None => "not_eligible".to_string(),
                        });
                    }
                }
                if sticky_pinned.is_none()
                    && let Some(candidate) = candidates
                        .iter()
                        .find(|candidate| candidate_ready(state, candidate, &parsed))
                {
                    sticky_write = Some(candidate.offering_id.clone());
                    sticky_pinned = Some(candidate.offering_id.clone());
                    sticky_age = Some(0);
                }
            }
        }
    }
    decision.sticky.pinned = sticky_pinned.clone();
    decision.sticky.age_s = sticky_age;

    let mut attempts: Vec<Attempt> = Vec::new();
    let mut first_output_estimate: Option<Option<i64>> = None;
    let mut context_rejected = false;
    for candidate in candidates {
        let price_row = prices.remove(&candidate.offering_id).flatten();
        let candidate_mix = mix_prices.remove(&candidate.offering_id).flatten();
        let price_basis = match (&candidate_mix, scoring_price(price_row.as_ref())) {
            (Some(_), _) => "mix",
            (None, Some(_)) => "component_sum",
            (None, None) => "unpriced",
        };
        let price = price_row.as_ref().map(|row| {
            json!({
                "input_per_m": row.input_per_m,
                "output_per_m": row.output_per_m,
                "cache_read_per_m": row.cache_read_per_m,
                "cache_write_per_m": row.cache_write_per_m,
                "reasoning_per_m": row.reasoning_per_m,
                "request_fee": row.request_fee,
                "currency": row.currency,
            })
        });
        let explanation = weighted_explanations
            .remove(&candidate.offering_id)
            .map(|text| {
                match price_note(candidate_mix.as_ref(), price_row.as_ref(), &mix_profile) {
                    Some(note) => format!("{text}; {note}"),
                    None => text,
                }
            });
        decision.candidates.push(CandidateRecord {
            id: candidate.offering_id.clone(),
            provider: candidate.provider_name.clone(),
            model: candidate.canonical_name.clone(),
            upstream_model_id: candidate.upstream_model_id.clone(),
            price,
            credential_id: candidate.credential.as_ref().map(|c| c.id.clone()),
            outcome: "not_tried",
            error_class: None,
            status: None,
            mix_price: candidate_mix,
            price_basis,
            score: weighted_scores.remove(&candidate.offering_id),
            explanation,
            credential_attempts: Vec::new(),
        });
        let decision_idx = decision.candidates.len() - 1;

        // A catalog pool already filtered this candidate out; record its
        // constraint/filter reason.
        if let Some(excluded) = pool_exclusions.remove(&candidate.offering_id) {
            decision.candidates[decision_idx].outcome = excluded.reason.as_str();
            decision.candidates[decision_idx].explanation = Some(excluded.detail);
            if excluded.reason == combos::PoolReason::ContextExceeded {
                context_rejected = true;
            }
            continue;
        }
        // The weighted strategy already filtered this candidate out; record
        // the router's reason instead of re-deriving it.
        if let Some(reason) = weighted_exclusions.remove(&candidate.offering_id) {
            decision.candidates[decision_idx].outcome = reason.outcome();
            decision.candidates[decision_idx].explanation = Some(reason.explain());
            if reason == routing::Exclusion::ContextExceeded {
                context_rejected = true;
            }
            continue;
        }
        if candidate.credential.is_none() {
            decision.candidates[decision_idx].outcome = "skipped_no_credential";
            continue;
        }
        // `max_tokens` clamped to this offering's output limit; the estimated
        // input must fit the context minus that reservation, or this offering
        // is out before any upstream call.
        let max_tokens =
            crate::upstream::clamp_max_tokens(parsed.max_tokens, candidate.max_output_tokens);
        if !context_fits(&candidate, input_chars, max_tokens) {
            decision.candidates[decision_idx].outcome = "skipped_context";
            context_rejected = true;
            continue;
        }
        // M5 circuit breaker: an open (credential, offering) pair is
        // excluded for its cooldown window, with the reason recorded.
        if let Some(credential) = &candidate.credential
            && state
                .breaker
                .is_open(&credential.id, &candidate.offering_id, Instant::now())
        {
            decision.candidates[decision_idx].outcome = "skipped_breaker_open";
            decision.candidates[decision_idx].explanation = Some(format!(
                "{} (credential {}; {} consecutive failures)",
                routing::Exclusion::BreakerOpen.explain(),
                credential.id,
                // The failure streak, not `trips()` (times opened) — review
                // M2: the honesty surface printed the wrong number.
                state
                    .breaker
                    .consecutive(&credential.id, &candidate.offering_id),
            ));
            continue;
        }
        let body = crate::upstream::build_body(
            &parsed.extra,
            &parsed.messages,
            &candidate.upstream_model_id,
            parsed.stream,
            max_tokens,
        );
        if attempts.is_empty() {
            // The output estimate (M4's `max_tokens` term; see
            // `estimate_tokens`) comes from the first attempt's clamped
            // output budget; the USD estimate is priced later on the first
            // *priced* candidate (review m4).
            first_output_estimate = Some(max_tokens);
        }
        attempts.push(Attempt {
            decision_idx,
            candidate,
            body,
            price: price_row,
            credential_cursor: 0,
        });
    }

    // Persist the new pin only when this request will actually dispatch: a
    // request refused before dispatch must not move the session's pin.
    let mut sticky_run: Option<StickyRun> = None;
    if !attempts.is_empty()
        && let (Some(combo), Some(key)) = (&combo, &sticky_key)
    {
        if let Some(offering_id) = &sticky_write {
            match combos::upsert_pin(&state.pool, &combo.id, key, offering_id, sticky_ttl, &now)
                .await
            {
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "writing the sticky pin failed"),
            }
        }
        sticky_run = Some(StickyRun {
            combo_id: combo.id.clone(),
            session_key: key.clone(),
            pinned: sticky_pinned.clone(),
            ttl_seconds: sticky_ttl,
        });
    }

    // The round-robin cursor advances once per request that actually
    // dispatches upstream (review nit): the refusals above consumed no
    // upstream call, so they must not consume the rotation either.
    if !attempts.is_empty()
        && let Some(combo) = &combo
        && combo.strategy == Strategy::RoundRobin
    {
        match combos::advance_cursor(&state.pool, &combo.id).await {
            Ok(next) => decision.routing.rr_cursor = Some(next),
            Err(e) => {
                tracing::error!(error = %e, "advancing the round-robin cursor failed");
                return Err(ChatFailure::internal());
            }
        }
    }

    if attempts.is_empty() {
        let failure = match context_rejected {
            true => ChatFailure::new(
                StatusCode::BAD_REQUEST,
                "context_length_exceeded",
                "estimated input plus max_tokens exceeds the model's context window",
            ),
            false => ChatFailure::new(
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("no eligible offering serves model {:?}", parsed.model),
            ),
        };
        decision.reason = decision_reason_with_mix(&decision);
        settle(
            state,
            &Settlement::rejected_with_decision(
                meta,
                started,
                bytes_in,
                &failure,
                decision,
                &parsed.model,
            ),
        )
        .await;
        return Err(failure);
    }

    // The hard money stop (M4, decisions D7/D9): the reservation opens in
    // one write-first transaction *before* anything is forwarded. Only after
    // the budget/cap checks pass is the key's TPM bucket charged (review m6):
    // a `402`/`429`-refused request must not burn rate-limit quota, and a
    // later TPM refusal releases the reservation through its rejection
    // settlement. Either refusal settles its ledger rows like any rejection.
    let first = &attempts[0].candidate;
    let (estimated_input, estimated_output) = estimate_tokens(
        input_chars,
        first_output_estimate.flatten(),
        first.max_output_tokens,
    );
    // The reservation's USD estimate is priced on the first *priced*
    // candidate (review m4) — an unpriced first candidate must not reserve
    // 0 USD against a priced fallback. Documented residual (see `money`):
    // with no priced candidate at all the reservation holds 0 USD and the
    // request is metered by token caps only, and the output estimate is
    // capped at 4096 tokens when the request sets no `max_tokens`, so an
    // overshooting output can exceed the reserved USD — settlement books the
    // true actual either way.
    let estimate_usage = Usage {
        input_tokens: Some(estimated_input),
        output_tokens: Some(estimated_output),
        ..Usage::default()
    };
    let estimated_usd = attempts
        .iter()
        .find_map(|attempt| {
            money::compute_cost(attempt.price.as_ref(), Some(&estimate_usage)).cost_usd
        })
        .unwrap_or(0.0);
    let reservation = money::ReservationInput {
        request_id: meta.request_id.clone(),
        user_id: meta.user_id.clone(),
        api_key_id: meta.api_key.as_ref().map(|key| key.key_id.clone()),
        offering_id: first.offering_id.clone(),
        model_id: first.model_id.clone(),
        estimated_tokens: estimated_input.saturating_add(estimated_output),
        estimated_usd,
    };
    if let Err(denied) = money::open_reservation(&state.pool, state.config.tz, &reservation).await {
        let failure = match &denied {
            money::ReservationDenied::Budget { period } => ChatFailure::new(
                StatusCode::PAYMENT_REQUIRED,
                "budget_exceeded",
                format!("the {period} spend limit is exhausted"),
            ),
            money::ReservationDenied::TokenCap { period, model_id } => ChatFailure::new(
                StatusCode::TOO_MANY_REQUESTS,
                "token_cap_exceeded",
                format!("the {period} token limit for model {model_id} is exhausted"),
            ),
            money::ReservationDenied::Db(e) => {
                tracing::error!(error = %e, "opening the reservation failed");
                ChatFailure::internal()
            }
        };
        settle(
            state,
            &Settlement::rejected_with_decision(
                meta,
                started,
                bytes_in,
                &failure,
                decision,
                &parsed.model,
            ),
        )
        .await;
        return Err(failure);
    }
    // The TPM bucket is charged with the same estimate the reservation
    // booked (review m6: after the budget/cap checks). A refusal answers
    // `429` and its settlement releases the reservation above.
    if let Some(key) = &meta.api_key
        && let Err(wait) = state.key_limiter.charge_tpm(
            &key.key_id,
            key.rate_limit_tpm,
            estimated_input.saturating_add(estimated_output),
            Instant::now(),
        )
    {
        let failure = ChatFailure {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limited",
            message: "the API key's token rate limit is exhausted".to_string(),
            retry_after: Some(wait.as_secs().max(1)),
            detail: None,
        };
        settle(
            state,
            &Settlement::rejected_with_decision(
                meta,
                started,
                bytes_in,
                &failure,
                decision,
                &parsed.model,
            ),
        )
        .await;
        return Err(failure);
    }

    let (tx, mut rx) = mpsc::channel::<Result<Bytes, ChatFailure>>(8);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let guard = CancelOnDrop(Some(cancel_tx));
    let total = tokio::time::Instant::now() + state.config.timeout_total;
    // The resolved price rows travel to settlement: the chosen offering's
    // row is reused when it is still the same offering, so settlement never
    // re-reads it. An offering missing from the map (or a failover to one
    // outside the resolution set) falls back to a database read.
    let prices = attempts
        .iter()
        .map(|attempt| (attempt.candidate.offering_id.clone(), attempt.price.clone()))
        .collect();
    let plan = Plan {
        parsed_stream: parsed.stream,
        attempts,
        decision,
        requested_model: parsed.model,
        bytes_in,
        input_chars,
        has_request_body: bytes_in > 0,
        sticky: sticky_run,
        prices,
    };
    let runner_state = state.clone();
    let runner_meta = meta.clone();
    tokio::spawn(async move {
        runner(
            runner_state,
            runner_meta,
            plan,
            tx,
            cancel_rx,
            started,
            total,
        )
        .await;
    });

    if !parsed.stream {
        // Buffered mode: nothing reaches the client before the full body is
        // in, so any attempt failure can still fail over inside the runner.
        let mut acc = Vec::new();
        while let Some(item) = rx.recv().await {
            match item {
                Ok(bytes) => acc.extend_from_slice(&bytes),
                Err(failure) => return Err(failure),
            }
        }
        drop(guard);
        return Ok(ChatOutcome::Json(Bytes::from(acc)));
    }

    // Streaming: the first item decides between an error response and the
    // committed SSE stream.
    match rx.recv().await {
        Some(Ok(first)) => {
            let rid = meta.request_id.clone();
            let rest = ReceiverStream::new(rx).map(move |item| match item {
                Ok(bytes) => Ok::<Bytes, std::convert::Infallible>(bytes),
                Err(failure) => Ok(in_band_error_bytes(&failure, &rid)),
            });
            let chained = stream::once(std::future::ready(Ok::<Bytes, std::convert::Infallible>(
                first,
            )))
            .chain(rest);
            // The guard rides inside the stream state: dropping the response
            // body drops it, which signals the runner to abort upstream.
            let body_stream = chained.map(move |item| {
                let _keep_alive = &guard;
                item
            });
            Ok(ChatOutcome::Sse(Body::from_stream(body_stream)))
        }
        Some(Err(failure)) => Err(failure),
        None => {
            tracing::error!(request_id = %meta.request_id, "runner ended without a result");
            Err(ChatFailure::internal())
        }
    }
}

/// Records rejection rows for failures that happen before the pipeline owns
/// the request (the handler's body-size limit).
pub async fn record_rejection(
    state: &AppState,
    meta: &RequestMeta,
    bytes_in: u64,
    failure: &ChatFailure,
) {
    settle(
        state,
        &Settlement::rejected(meta, Instant::now(), bytes_in, failure),
    )
    .await;
}

// ---------------------------------------------------------------------------
// The runner task
// ---------------------------------------------------------------------------

/// The pin this request resolved, so a failover that serves from another
/// offering moves the session's pin there (D35).
struct StickyRun {
    combo_id: String,
    session_key: String,
    pinned: Option<String>,
    ttl_seconds: i64,
}

struct Plan {
    parsed_stream: bool,
    attempts: Vec<Attempt>,
    decision: Decision,
    requested_model: String,
    bytes_in: u64,
    /// Message-text characters, the input estimator's base (review M1).
    input_chars: usize,
    has_request_body: bool,
    sticky: Option<StickyRun>,
    /// Price rows resolved before dispatch, by offering id.
    prices: HashMap<String, Option<catalog::PriceRow>>,
}

/// How a run ended, before settlement.
enum RunEnd {
    /// The response body was produced (complete JSON, or a stream that is
    /// still being consumed).
    Delivered(RunResult),
    /// No response body was produced (or the stream failed mid-flight).
    Failed {
        result: RunResult,
        failure: ChatFailure,
    },
    /// The client hung up before completion.
    Disconnected(RunResult),
}

struct RunResult {
    decision: Decision,
    offering_id: Option<String>,
    model_id: Option<String>,
    usage: Option<Usage>,
    /// Content characters produced (never scaffolding), for estimation.
    output_chars: usize,
    bytes_out: u64,
    ttft_ms: Option<i64>,
    /// An upstream call was actually opened (`open()` returned a stream);
    /// only then may tokens be estimated (review M3).
    sent: bool,
}

/// A [`RunResult`] for the attempt currently owning the chosen slot.
#[allow(clippy::too_many_arguments)]
fn chosen_result(
    decision: &mut Decision,
    offering_id: &str,
    model_id: &str,
    usage: Option<Usage>,
    output_chars: usize,
    bytes_out: u64,
    ttft_ms: Option<i64>,
    sent: bool,
) -> RunResult {
    RunResult {
        decision: std::mem::take(decision),
        offering_id: Some(offering_id.to_string()),
        model_id: Some(model_id.to_string()),
        usage,
        output_chars,
        bytes_out,
        ttft_ms,
        sent,
    }
}

/// A [`RunResult`] attributed to the last candidate tried (failure paths).
#[allow(clippy::too_many_arguments)]
fn last_result(
    decision: &mut Decision,
    last: &Option<(String, String)>,
    usage: Option<Usage>,
    output_chars: usize,
    bytes_out: u64,
    ttft_ms: Option<i64>,
    sent: bool,
) -> RunResult {
    RunResult {
        decision: std::mem::take(decision),
        offering_id: last.as_ref().map(|(id, _)| id.clone()),
        model_id: last.as_ref().map(|(_, id)| id.clone()),
        usage,
        output_chars,
        bytes_out,
        ttft_ms,
        sent,
    }
}

async fn runner(
    state: AppState,
    meta: RequestMeta,
    plan: Plan,
    tx: mpsc::Sender<Result<Bytes, ChatFailure>>,
    cancel: oneshot::Receiver<()>,
    started: Instant,
    total: tokio::time::Instant,
) {
    let Plan {
        parsed_stream,
        attempts,
        decision,
        requested_model,
        bytes_in,
        input_chars,
        has_request_body,
        sticky,
        prices,
    } = plan;
    let mut decision = decision;
    let end = match parsed_stream {
        true => run_streaming(&state, attempts, &mut decision, &tx, cancel, total).await,
        false => run_buffered(&state, attempts, &mut decision, &tx, cancel, total).await,
    };

    // Settlement runs before the terminal item is sent, so the rows exist
    // when the failure reaches the client.
    let (result, failure, status, error_class, error_detail) = match end {
        RunEnd::Delivered(result) => (result, None, 200i64, None, None),
        RunEnd::Failed { result, failure } => {
            // A stream that failed after its first byte was already committed
            // as 200; only failures before it can still change the status.
            let status = match result.bytes_out > 0 {
                true => 200i64,
                false => failure.status.as_u16() as i64,
            };
            let code: &'static str = failure.code;
            let detail = failure.detail.clone();
            (result, Some(failure), status, Some(code), detail)
        }
        RunEnd::Disconnected(result) => (
            result,
            None,
            CLIENT_DISCONNECTED_STATUS,
            Some("client_disconnected"),
            None,
        ),
    };
    let mut decision = result.decision;
    // D35: a failed pinned attempt that failed over moves the pin to the
    // offering that actually served the request. Failures before any
    // offering served leave the pin untouched.
    if let Some(sticky) = &sticky
        && decision.chosen_offering_id.is_some()
        && let Some(served) = &result.offering_id
        && sticky.pinned.as_deref() != Some(served.as_str())
    {
        let now = crate::catalog::now_ts();
        match combos::upsert_pin(
            &state.pool,
            &sticky.combo_id,
            &sticky.session_key,
            served,
            sticky.ttl_seconds,
            &now,
        )
        .await
        {
            Ok(_) => {
                decision.sticky.pinned = Some(served.clone());
                decision.sticky.age_s = Some(0);
            }
            Err(e) => tracing::error!(error = %e, "moving the sticky pin failed"),
        }
    }
    decision.reason = decision_reason_with_mix(&decision);
    settle(
        &state,
        &Settlement {
            meta,
            requested_model: Some(requested_model),
            decision,
            offering_id: result.offering_id,
            model_id: result.model_id,
            usage: result.usage,
            output_chars: result.output_chars,
            sent: result.sent,
            status,
            error_class,
            error_detail,
            started,
            ttft_ms: result.ttft_ms,
            bytes_out: result.bytes_out,
            bytes_in,
            input_chars,
            has_request_body,
            prices,
        },
    )
    .await;

    // A failure becomes the terminal channel item (an error response for
    // buffered calls, an in-band error event for streams). The send is raced
    // against the total deadline: a stalled reader must not pin the runner —
    // the rows are settled above either way (review m1).
    if let Some(failure) = failure {
        let _ = tokio::time::timeout_at(total, tx.send(Err(failure))).await;
    }
}

/// The outcome of one attempt: the open stream (first chunk read), or a
/// failure to classify before anything reached the client.
enum AttemptStart {
    Started {
        stream: UpstreamStream,
        first: Bytes,
        ttft_ms: i64,
        /// Redacts provider text taken from this stream (D49).
        redactor: Arc<crate::upstream::detail::Redactor>,
    },
    Failed(UpstreamError),
    Canceled,
}

/// Opens the call and reads the first body chunk under the first-byte budget.
/// Cancellation is honoured at every await so a disconnect aborts the
/// upstream within milliseconds.
async fn start_attempt(
    state: &AppState,
    attempt: &Attempt,
    cancel: &mut oneshot::Receiver<()>,
    total: tokio::time::Instant,
) -> AttemptStart {
    let Some(credential) = &attempt.candidate.credential else {
        // Filtered out before the attempt list was built.
        return AttemptStart::Failed(UpstreamError::Credential("no credential".to_string()));
    };
    let call = match UpstreamCall::build(
        &state.secrets,
        &crate::upstream::UpstreamTarget {
            base_url: &attempt.candidate.base_url,
            credential_id: &credential.id,
            secret_cipher: &credential.secret_cipher,
            credential_headers_json: credential.headers_json.as_deref(),
            offering_id: &attempt.candidate.offering_id,
            offering_headers_json: attempt.candidate.offering_headers_json.as_deref(),
        },
        attempt.body.clone(),
    ) {
        Ok(call) => call,
        Err(e) => return AttemptStart::Failed(e),
    };

    let budget = Budget {
        first_byte: tokio::time::Instant::now() + state.config.timeout_first_byte,
        idle: state.config.timeout_idle,
        total,
    };
    let attempt_started = Instant::now();
    let opened = match or_cancel(cancel, state.upstream.open(&call, budget)).await {
        Wait::Canceled => return AttemptStart::Canceled,
        Wait::Done(opened) => opened,
    };
    let mut stream = match opened {
        Ok(stream) => stream,
        Err(e) => return AttemptStart::Failed(e),
    };
    match or_cancel(cancel, stream.next_chunk()).await {
        Wait::Canceled => AttemptStart::Canceled,
        Wait::Done(Ok(Some(first))) => AttemptStart::Started {
            redactor: stream.redactor(),
            stream,
            first,
            ttft_ms: attempt_started.elapsed().as_millis() as i64,
        },
        // A clean but empty body is a stream failure: nothing was sent, so
        // failover is allowed.
        Wait::Done(Ok(None)) => AttemptStart::Failed(UpstreamError::Body("empty body".to_string())),
        Wait::Done(Err(e)) => AttemptStart::Failed(e),
    }
}

/// Streaming attempt loop: failover only before the first chunk is forwarded.
async fn run_streaming(
    state: &AppState,
    attempts: Vec<Attempt>,
    decision: &mut Decision,
    tx: &mpsc::Sender<Result<Bytes, ChatFailure>>,
    mut cancel: oneshot::Receiver<()>,
    total: tokio::time::Instant,
) -> RunEnd {
    let mut last_failure: Option<ChatFailure> = None;
    let mut last: Option<(String, String)> = None;
    let mut rate_budget = 1i32;
    let mut sent = false;
    let combo_member = decision.routing.combo.is_some();

    'candidates: for mut attempt in attempts {
        let decision_idx = attempt.decision_idx;
        // One offering may have several credentials. A credential-attributable
        // failure (401/403, 402, 429) advances to the next eligible credential
        // of the same offering before the request moves to another offering;
        // the failed credential is never retried in this request.
        let (mut stream, first, ttft_ms, attempt_started, redactor) = 'credentials: loop {
            let candidate = &attempt.candidate;
            let attempt_started = Instant::now();
            last = Some((candidate.offering_id.clone(), candidate.model_id.clone()));
            match start_attempt(state, &attempt, &mut cancel, total).await {
                AttemptStart::Started {
                    stream,
                    first,
                    ttft_ms,
                    redactor,
                } => {
                    sent = true;
                    if let Some(credential_id) = candidate.credential.as_ref().map(|c| c.id.clone())
                    {
                        decision.candidates[decision_idx].credential_id =
                            Some(credential_id.clone());
                        record_credential_attempt(
                            decision,
                            decision_idx,
                            &credential_id,
                            "chosen",
                            None,
                            None,
                        );
                    }
                    break 'credentials (stream, first, ttft_ms, attempt_started, redactor);
                }
                AttemptStart::Canceled => {
                    return RunEnd::Disconnected(last_result(
                        decision, &last, None, 0, 0, None, sent,
                    ));
                }
                AttemptStart::Failed(e) => {
                    let Some(credential) = candidate.credential.as_ref() else {
                        let failure = ChatFailure::internal();
                        return RunEnd::Failed {
                            result: last_result(decision, &last, None, 0, 0, None, sent),
                            failure,
                        };
                    };
                    let class = e.classify();
                    let status = upstream_status(&e);
                    let error_detail = e.error_detail();
                    if let Some(cooldown) = class.cooldown {
                        record_cooldown(&state.pool, &credential.id, cooldown).await;
                    }
                    let credential_id = credential.id.clone();
                    decision.candidates[decision_idx].credential_id = Some(credential_id.clone());
                    record_failure(decision, decision_idx, &class, status);
                    record_credential_attempt(
                        decision,
                        decision_idx,
                        &credential_id,
                        "failed",
                        Some(&class),
                        status,
                    );
                    record_breaker_failure(state, candidate, &class, error_detail.as_ref()).await;
                    push_attempt_sample(
                        &mut decision.samples,
                        candidate,
                        attempt_started,
                        Some(class.code),
                        None,
                        None,
                        status,
                        error_detail.as_ref(),
                    );
                    if credential_attributable(&class) {
                        let cursor = attempt.credential_cursor;
                        if let Some((next_index, next)) =
                            next_credential(state, &attempt.candidate, cursor)
                        {
                            let rate_ok = class.code != "upstream_rate_limited" || rate_budget > 0;
                            if rate_ok {
                                if class.code == "upstream_rate_limited" {
                                    rate_budget -= 1;
                                }
                                attempt.credential_cursor = next_index;
                                attempt.candidate.credential = Some(next);
                                continue 'credentials;
                            }
                        }
                    }
                    let failure = failure_from_class_with_detail(&class, error_detail.as_ref());
                    if can_fail_over(&class, &failure, &mut rate_budget, combo_member) {
                        last_failure = Some(failure);
                        continue 'candidates;
                    }
                    return RunEnd::Failed {
                        result: last_result(decision, &last, None, 0, 0, None, sent),
                        failure,
                    };
                }
            }
        };
        let candidate = &attempt.candidate;
        let offering_id = candidate.offering_id.clone();
        let model_id = candidate.model_id.clone();

        // The first chunk decides the boundary: inspect before forwarding, so
        // a protocol error on the very first chunk can still fail over.
        let mut inspector = SseInspector::new();
        inspector.push(&first);
        if inspector.overflowed {
            // Line-cap overflow is terminal even here: retrying could just
            // buffer the cap again per candidate (review M2).
            let class = UpstreamError::Body("response line too large".to_string()).classify();
            let detail = ErrorDetail {
                provider_code: None,
                detail: Some("the response line exceeded the size cap".to_string()),
            };
            record_failure(decision, decision_idx, &class, None);
            record_breaker_failure(state, candidate, &class, Some(&detail)).await;
            push_attempt_sample(
                &mut decision.samples,
                candidate,
                attempt_started,
                Some(class.code),
                Some(ttft_ms),
                attempt_output_tokens(None, inspector.output_chars),
                None,
                Some(&detail),
            );
            return RunEnd::Failed {
                result: last_result(decision, &last, None, 0, 0, None, sent),
                failure: failure_from_class_with_detail(&class, Some(&detail)),
            };
        }
        if inspector.protocol_error {
            let class = UpstreamError::Body("malformed SSE".to_string()).classify();
            let detail = inspector
                .provider_error
                .as_ref()
                .map(|error| error.detail(&redactor));
            record_failure(decision, decision_idx, &class, None);
            record_breaker_failure(state, candidate, &class, detail.as_ref()).await;
            push_attempt_sample(
                &mut decision.samples,
                candidate,
                attempt_started,
                Some(class.code),
                Some(ttft_ms),
                attempt_output_tokens(None, inspector.output_chars),
                None,
                detail.as_ref(),
            );
            let failure = failure_from_class_with_detail(&class, detail.as_ref());
            if can_fail_over(&class, &failure, &mut rate_budget, combo_member) {
                last_failure = Some(failure);
                continue;
            }
            return RunEnd::Failed {
                result: last_result(decision, &last, None, 0, 0, None, sent),
                failure,
            };
        }

        decision.candidates[decision_idx].outcome = "chosen";
        decision.candidates[decision_idx].error_class = None;
        decision.candidates[decision_idx].status = None;
        decision.chosen_offering_id = Some(offering_id.clone());

        // Forwarding starts here: from now on no failover is allowed.
        let first_len = first.len() as u64;
        let mut bytes_out = 0u64;
        match send_item(&mut cancel, tx, total, Ok(first)).await {
            Sent::Gone => {
                push_attempt_sample(
                    &mut decision.samples,
                    candidate,
                    attempt_started,
                    Some("client_disconnected"),
                    Some(ttft_ms),
                    attempt_output_tokens(None, inspector.output_chars),
                    None,
                    None,
                );
                return RunEnd::Disconnected(chosen_result(
                    decision,
                    &offering_id,
                    &model_id,
                    None,
                    0,
                    bytes_out,
                    Some(ttft_ms),
                    sent,
                ));
            }
            Sent::TimedOut => {
                let failure = total_timeout_failure();
                push_attempt_sample(
                    &mut decision.samples,
                    candidate,
                    attempt_started,
                    Some("upstream_timeout"),
                    Some(ttft_ms),
                    attempt_output_tokens(None, inspector.output_chars),
                    None,
                    None,
                );
                return RunEnd::Failed {
                    result: chosen_result(
                        decision,
                        &offering_id,
                        &model_id,
                        None,
                        0,
                        bytes_out,
                        Some(ttft_ms),
                        sent,
                    ),
                    failure,
                };
            }
            Sent::Ok => {}
        }
        bytes_out = first_len;

        let end = pump(
            &mut stream,
            &mut inspector,
            tx,
            &mut cancel,
            &mut bytes_out,
            total,
            &redactor,
        )
        .await;
        return match end {
            PumpEnd::Done => {
                record_breaker_success(state, candidate);
                push_attempt_sample(
                    &mut decision.samples,
                    candidate,
                    attempt_started,
                    None,
                    Some(ttft_ms),
                    attempt_output_tokens(inspector.usage.as_ref(), inspector.output_chars),
                    None,
                    None,
                );
                RunEnd::Delivered(chosen_result(
                    decision,
                    &offering_id,
                    &model_id,
                    inspector.usage.clone(),
                    inspector.output_chars,
                    bytes_out,
                    Some(ttft_ms),
                    sent,
                ))
            }
            PumpEnd::Failed {
                class,
                failure,
                detail,
            } => {
                // The chosen record keeps its outcome but records how the
                // stream ended; the response was already committed.
                decision.candidates[decision_idx].error_class = Some(failure.code);
                decision.candidates[decision_idx].status = Some(failure.status.as_u16() as i64);
                record_breaker_failure(state, candidate, &class, detail.as_ref()).await;
                push_attempt_sample(
                    &mut decision.samples,
                    candidate,
                    attempt_started,
                    Some(class.code),
                    Some(ttft_ms),
                    attempt_output_tokens(inspector.usage.as_ref(), inspector.output_chars),
                    None,
                    detail.as_ref(),
                );
                RunEnd::Failed {
                    result: chosen_result(
                        decision,
                        &offering_id,
                        &model_id,
                        inspector.usage.clone(),
                        inspector.output_chars,
                        bytes_out,
                        Some(ttft_ms),
                        sent,
                    ),
                    failure,
                }
            }
            PumpEnd::Disconnected => {
                push_attempt_sample(
                    &mut decision.samples,
                    candidate,
                    attempt_started,
                    Some("client_disconnected"),
                    Some(ttft_ms),
                    attempt_output_tokens(inspector.usage.as_ref(), inspector.output_chars),
                    None,
                    None,
                );
                RunEnd::Disconnected(chosen_result(
                    decision,
                    &offering_id,
                    &model_id,
                    inspector.usage.clone(),
                    inspector.output_chars,
                    bytes_out,
                    Some(ttft_ms),
                    sent,
                ))
            }
        };
    }

    // Every candidate failed before the first byte.
    RunEnd::Failed {
        result: last_result(decision, &last, None, 0, 0, None, sent),
        failure: last_failure.unwrap_or_else(ChatFailure::internal),
    }
}

/// Buffered attempt loop: the whole body is read per attempt and nothing
/// reaches the client before it is complete, so every failure may fail over
/// (subject to the never-retry classes and the 429-once budget).
async fn run_buffered(
    state: &AppState,
    attempts: Vec<Attempt>,
    decision: &mut Decision,
    tx: &mpsc::Sender<Result<Bytes, ChatFailure>>,
    mut cancel: oneshot::Receiver<()>,
    total: tokio::time::Instant,
) -> RunEnd {
    let mut last_failure: Option<ChatFailure> = None;
    let mut last: Option<(String, String)> = None;
    let mut rate_budget = 1i32;
    let mut sent = false;
    let combo_member = decision.routing.combo.is_some();

    'candidates: for mut attempt in attempts {
        let decision_idx = attempt.decision_idx;
        // Same credential rotation as the streaming loop: retry the offering
        // with its next eligible credential before moving on.
        let (mut stream, first, ttft_ms, attempt_started, redactor) = 'credentials: loop {
            let candidate = &attempt.candidate;
            let attempt_started = Instant::now();
            last = Some((candidate.offering_id.clone(), candidate.model_id.clone()));
            match start_attempt(state, &attempt, &mut cancel, total).await {
                AttemptStart::Started {
                    stream,
                    first,
                    ttft_ms,
                    redactor,
                } => {
                    sent = true;
                    if let Some(credential_id) = candidate.credential.as_ref().map(|c| c.id.clone())
                    {
                        decision.candidates[decision_idx].credential_id =
                            Some(credential_id.clone());
                        record_credential_attempt(
                            decision,
                            decision_idx,
                            &credential_id,
                            "chosen",
                            None,
                            None,
                        );
                    }
                    break 'credentials (stream, first, ttft_ms, attempt_started, redactor);
                }
                AttemptStart::Canceled => {
                    return RunEnd::Disconnected(last_result(
                        decision, &last, None, 0, 0, None, sent,
                    ));
                }
                AttemptStart::Failed(e) => {
                    let class = e.classify();
                    let status = upstream_status(&e);
                    let error_detail = e.error_detail();
                    let credential_id = candidate.credential.as_ref().map(|c| c.id.clone());
                    if let Some(credential) = &candidate.credential
                        && let Some(cooldown) = class.cooldown
                    {
                        record_cooldown(&state.pool, &credential.id, cooldown).await;
                    }
                    if let Some(credential_id) = &credential_id {
                        decision.candidates[decision_idx].credential_id =
                            Some(credential_id.clone());
                        record_credential_attempt(
                            decision,
                            decision_idx,
                            credential_id,
                            "failed",
                            Some(&class),
                            status,
                        );
                    }
                    record_failure(decision, decision_idx, &class, status);
                    record_breaker_failure(state, candidate, &class, error_detail.as_ref()).await;
                    push_attempt_sample(
                        &mut decision.samples,
                        candidate,
                        attempt_started,
                        Some(class.code),
                        None,
                        None,
                        status,
                        error_detail.as_ref(),
                    );
                    if credential_attributable(&class) {
                        let cursor = attempt.credential_cursor;
                        if let Some((next_index, next)) =
                            next_credential(state, &attempt.candidate, cursor)
                        {
                            let rate_ok = class.code != "upstream_rate_limited" || rate_budget > 0;
                            if rate_ok {
                                if class.code == "upstream_rate_limited" {
                                    rate_budget -= 1;
                                }
                                attempt.credential_cursor = next_index;
                                attempt.candidate.credential = Some(next);
                                continue 'credentials;
                            }
                        }
                    }
                    let failure = failure_from_class_with_detail(&class, error_detail.as_ref());
                    if can_fail_over(&class, &failure, &mut rate_budget, combo_member) {
                        last_failure = Some(failure);
                        continue 'candidates;
                    }
                    return RunEnd::Failed {
                        result: last_result(decision, &last, None, 0, 0, None, sent),
                        failure,
                    };
                }
            }
        };
        let candidate = &attempt.candidate;
        let offering_id = candidate.offering_id.clone();
        let model_id = candidate.model_id.clone();

        // Read the full body under the idle/total budgets. The client has
        // seen nothing yet, so any failure may fail over — except the cap
        // below, which is terminal (review m5).
        let mut buffer: Vec<u8> = Vec::with_capacity(first.len().min(MAX_BUFFERED_RESPONSE));
        let read_failure = if !push_bounded(&mut buffer, &first) {
            Some(UpstreamError::TooLarge)
        } else {
            loop {
                match or_cancel(&mut cancel, stream.next_chunk()).await {
                    Wait::Canceled => {
                        return RunEnd::Disconnected(last_result(
                            decision, &last, None, 0, 0, None, sent,
                        ));
                    }
                    Wait::Done(Ok(Some(chunk))) => {
                        if !push_bounded(&mut buffer, &chunk) {
                            break Some(UpstreamError::TooLarge);
                        }
                    }
                    Wait::Done(Ok(None)) => break None,
                    Wait::Done(Err(e)) => break Some(e),
                }
            }
        };
        let (usage, output_chars, failure, error_detail) = match read_failure {
            // A cap failure is terminal and the oversized buffer is never
            // parsed (review m5).
            Some(e) => {
                let detail = e.error_detail();
                (None, 0, Some(e), detail)
            }
            None => {
                let (usage, output_chars, provider_error) = sse::inspect_json_body(&buffer);
                let error_detail = provider_error.map(|error| error.detail(&redactor));
                let failure = error_detail
                    .as_ref()
                    .map(|_| UpstreamError::Body("invalid JSON body".to_string()));
                (usage, output_chars, failure, error_detail)
            }
        };
        let Some(e) = failure else {
            record_breaker_success(state, candidate);
            decision.candidates[decision_idx].outcome = "chosen";
            decision.candidates[decision_idx].error_class = None;
            decision.candidates[decision_idx].status = None;
            decision.chosen_offering_id = Some(offering_id.clone());
            let bytes_out = buffer.len() as u64;
            let output_tokens = attempt_output_tokens(usage.as_ref(), output_chars);
            return match send_item(&mut cancel, tx, total, Ok(Bytes::from(buffer))).await {
                Sent::Ok => {
                    push_attempt_sample(
                        &mut decision.samples,
                        candidate,
                        attempt_started,
                        None,
                        Some(ttft_ms),
                        output_tokens,
                        None,
                        None,
                    );
                    RunEnd::Delivered(chosen_result(
                        decision,
                        &offering_id,
                        &model_id,
                        usage,
                        output_chars,
                        bytes_out,
                        Some(ttft_ms),
                        sent,
                    ))
                }
                Sent::Gone => {
                    push_attempt_sample(
                        &mut decision.samples,
                        candidate,
                        attempt_started,
                        Some("client_disconnected"),
                        Some(ttft_ms),
                        output_tokens,
                        None,
                        None,
                    );
                    RunEnd::Disconnected(chosen_result(
                        decision,
                        &offering_id,
                        &model_id,
                        usage,
                        output_chars,
                        bytes_out,
                        Some(ttft_ms),
                        sent,
                    ))
                }
                Sent::TimedOut => {
                    push_attempt_sample(
                        &mut decision.samples,
                        candidate,
                        attempt_started,
                        Some("upstream_timeout"),
                        Some(ttft_ms),
                        output_tokens,
                        None,
                        None,
                    );
                    RunEnd::Failed {
                        result: chosen_result(
                            decision,
                            &offering_id,
                            &model_id,
                            usage,
                            output_chars,
                            bytes_out,
                            Some(ttft_ms),
                            sent,
                        ),
                        failure: total_timeout_failure(),
                    }
                }
            };
        };

        let class = e.classify();
        let status = upstream_status(&e);
        record_failure(decision, decision_idx, &class, status);
        record_breaker_failure(state, candidate, &class, error_detail.as_ref()).await;
        push_attempt_sample(
            &mut decision.samples,
            candidate,
            attempt_started,
            Some(class.code),
            Some(ttft_ms),
            attempt_output_tokens(usage.as_ref(), output_chars),
            status,
            error_detail.as_ref(),
        );
        let failure = failure_from_class_with_detail(&class, error_detail.as_ref());
        if can_fail_over(&class, &failure, &mut rate_budget, combo_member) {
            last_failure = Some(failure);
            continue;
        }
        return RunEnd::Failed {
            result: last_result(decision, &last, usage, output_chars, 0, Some(ttft_ms), sent),
            failure,
        };
    }

    RunEnd::Failed {
        result: last_result(decision, &last, None, 0, 0, None, sent),
        failure: last_failure.unwrap_or_else(ChatFailure::internal),
    }
}

enum PumpEnd {
    Done,
    /// The stream failed mid-flight: its failure class (for breaker
    /// accounting, review M1), the client-visible failure, and the bounded
    /// provider detail for the operator (D49).
    Failed {
        class: FailureClass,
        failure: ChatFailure,
        detail: Option<ErrorDetail>,
    },
    Disconnected,
}

/// Forwards chunks as they arrive; the offending chunk of a malformed stream
/// is never forwarded — the in-band error replaces it. A stalled reader that
/// stops consuming is bounded by the total deadline (review m1).
#[allow(clippy::too_many_arguments)]
async fn pump(
    stream: &mut UpstreamStream,
    inspector: &mut SseInspector,
    tx: &mpsc::Sender<Result<Bytes, ChatFailure>>,
    cancel: &mut oneshot::Receiver<()>,
    bytes_out: &mut u64,
    total: tokio::time::Instant,
    redactor: &crate::upstream::detail::Redactor,
) -> PumpEnd {
    loop {
        let chunk = match or_cancel(cancel, stream.next_chunk()).await {
            Wait::Canceled => return PumpEnd::Disconnected,
            Wait::Done(result) => result,
        };
        match chunk {
            Err(e) => {
                let class = e.classify();
                let detail = e.error_detail();
                let failure = failure_from_class_with_detail(&class, detail.as_ref());
                return PumpEnd::Failed {
                    class,
                    failure,
                    detail,
                };
            }
            Ok(None) => {
                return match inspector.truncated() {
                    false => PumpEnd::Done,
                    true => {
                        let class = UpstreamError::Body("truncated stream".to_string()).classify();
                        let detail = text_detail("the stream ended without [DONE]");
                        PumpEnd::Failed {
                            class: class.clone(),
                            failure: failure_from_class_with_detail(&class, Some(&detail)),
                            detail: Some(detail),
                        }
                    }
                };
            }
            Ok(Some(bytes)) => {
                inspector.push(&bytes);
                if inspector.failed() {
                    // A malformed payload or a line-buffer overflow (review
                    // M2): both end the stream in-band, never fail over.
                    let class =
                        UpstreamError::Body("malformed or oversized SSE".to_string()).classify();
                    let detail = inspector
                        .provider_error
                        .as_ref()
                        .map(|error| error.detail(redactor))
                        .or_else(|| {
                            inspector
                                .overflowed
                                .then(|| text_detail("the response line exceeded the size cap"))
                        });
                    return PumpEnd::Failed {
                        class: class.clone(),
                        failure: failure_from_class_with_detail(&class, detail.as_ref()),
                        detail,
                    };
                } else {
                    let len = bytes.len() as u64;
                    match send_item(cancel, tx, total, Ok(bytes)).await {
                        Sent::Ok => *bytes_out += len,
                        Sent::Gone => return PumpEnd::Disconnected,
                        Sent::TimedOut => {
                            let class = UpstreamError::Timeout {
                                kind: TimeoutKind::Total,
                            }
                            .classify();
                            let detail = text_detail("the request deadline elapsed");
                            return PumpEnd::Failed {
                                class: class.clone(),
                                failure: failure_from_class_with_detail(&class, Some(&detail)),
                                detail: Some(detail),
                            };
                        }
                    }
                }
            }
        }
    }
}

/// How a channel send ended.
enum Sent {
    /// The item reached the consumer.
    Ok,
    /// The client disconnected (cancel fired) or the consumer is gone.
    Gone,
    /// The total deadline passed while the consumer was not reading.
    TimedOut,
}

/// Sends one item to the consumer, racing client cancellation and the total
/// deadline. A reader that stops consuming must not pin the runner past the
/// budget — settlement must happen promptly (review m1).
async fn send_item(
    cancel: &mut oneshot::Receiver<()>,
    tx: &mpsc::Sender<Result<Bytes, ChatFailure>>,
    total: tokio::time::Instant,
    item: Result<Bytes, ChatFailure>,
) -> Sent {
    tokio::select! {
        _ = &mut *cancel => Sent::Gone,
        () = tokio::time::sleep_until(total) => Sent::TimedOut,
        result = tx.send(item) => match result {
            Ok(()) => Sent::Ok,
            Err(_) => Sent::Gone,
        },
    }
}

/// The failure for a run that ran out of its absolute total budget.
fn total_timeout_failure() -> ChatFailure {
    failure_from_class(
        &UpstreamError::Timeout {
            kind: TimeoutKind::Total,
        }
        .classify(),
    )
}

/// Appends `chunk` unless that would exceed the buffering cap. `false` means
/// the cap tripped: the buffer never overshoots by a chunk (review m5).
fn push_bounded(buffer: &mut Vec<u8>, chunk: &[u8]) -> bool {
    if buffer.len() + chunk.len() > MAX_BUFFERED_RESPONSE {
        return false;
    }
    buffer.extend_from_slice(chunk);
    true
}

/// Failover is allowed only before the first byte and never for 400/422.
/// A `429` advances at most once (the request-wide `rate_budget`); a
/// credential-attributable failure first rotates to the offering's next
/// credential (see [`credential_attributable`]). `upstream_auth` (401/403)
/// moves to another credential or offering because it is a credential fault,
/// never a caller fault; the failed credential itself is not retried.
fn can_fail_over(
    class: &FailureClass,
    failure: &ChatFailure,
    rate_budget: &mut i32,
    combo_member: bool,
) -> bool {
    let retryable =
        class.retryable_before_first_byte || (combo_member && class.code == "upstream_auth");
    if !retryable {
        return false;
    }
    if failure.code == "upstream_rate_limited" {
        if *rate_budget <= 0 {
            return false;
        }
        *rate_budget -= 1;
    }
    true
}

/// Whether a failure points at the credential rather than the offering:
/// authentication/authorization, quota, or rate limiting. These rotate to the
/// same offering's next credential first, then fall over to another offering.
fn credential_attributable(class: &FailureClass) -> bool {
    matches!(
        class.code,
        "upstream_auth" | "upstream_quota" | "upstream_rate_limited"
    )
}

/// The next credential of this offering after `from`, skipping credentials
/// whose (credential, offering) breaker is open. Deterministic: the ordered
/// list was built once per candidate. Never returns an index already tried.
fn next_credential(
    state: &AppState,
    candidate: &Candidate,
    from: usize,
) -> Option<(usize, Credential)> {
    candidate
        .credentials
        .iter()
        .enumerate()
        .skip(from + 1)
        .find(|(_, credential)| {
            !state
                .breaker
                .is_open(&credential.id, &candidate.offering_id, Instant::now())
        })
        .map(|(index, credential)| (index, credential.clone()))
}

/// Appends one credential attempt to the candidate's `decision_json` record.
fn record_credential_attempt(
    decision: &mut Decision,
    idx: usize,
    credential_id: &str,
    outcome: &'static str,
    class: Option<&FailureClass>,
    status: Option<i64>,
) {
    decision.candidates[idx]
        .credential_attempts
        .push(CredentialAttempt {
            credential_id: credential_id.to_string(),
            outcome,
            error_class: class.map(|class| class.code),
            status,
        });
}

/// The upstream's own HTTP status for the decision record, when it answered.
fn upstream_status(error: &UpstreamError) -> Option<i64> {
    match error {
        UpstreamError::Http { status, .. } => Some(i64::from(*status)),
        _ => None,
    }
}

fn record_failure(decision: &mut Decision, idx: usize, class: &FailureClass, status: Option<i64>) {
    decision.candidates[idx].outcome = "failed";
    decision.candidates[idx].error_class = Some(class.code);
    decision.candidates[idx].status = Some(status.unwrap_or(class.status.as_u16() as i64));
}

/// One failed attempt counts toward the (credential, offering) circuit
/// breaker (M5) — unless its class does not reflect the offering's service
/// (`invalid_request`, `internal`; review M1). No-op without a credential.
/// When the failure opens the pair, the trip is persisted with its cause (D50).
async fn record_breaker_failure(
    state: &AppState,
    candidate: &Candidate,
    class: &FailureClass,
    detail: Option<&ErrorDetail>,
) {
    if !class.counts_toward_breaker {
        return;
    }
    let Some(credential) = &candidate.credential else {
        return;
    };
    let sighting = state.breaker.record_failure(
        &credential.id,
        &candidate.offering_id,
        class.code,
        detail.and_then(|detail| detail.detail.as_deref()),
        Instant::now(),
    );
    if let Some(trip) = sighting.trip {
        persist_breaker_trip(state, candidate, &trip).await;
    }
}

/// Writes one trip row and prunes rows older than 30 days (D50). Failure mode:
/// a database error is logged and swallowed — breaker accounting must never
/// fail a request.
async fn persist_breaker_trip(
    state: &AppState,
    candidate: &Candidate,
    trip: &crate::breaker::Trip,
) {
    let occurred_at = crate::catalog::now_ts();
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(30))
        .format(crate::auth::TS_FMT)
        .to_string();
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!(error = %e, "breaker trip write failed");
            return;
        }
    };
    let result = async {
        sqlx::query("DELETE FROM breaker_trips WHERE occurred_at < ?")
            .bind(&cutoff)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO breaker_trips \
             (provider_id, credential_id, offering_id, occurred_at, consecutive_failures, \
              error_class, error_detail) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&candidate.provider_id)
        .bind(&trip.credential_id)
        .bind(&trip.offering_id)
        .bind(&occurred_at)
        .bind(i64::from(trip.consecutive_failures))
        .bind(trip.error_class)
        .bind(&trip.error_detail)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = result {
        tracing::error!(
            error = %e,
            offering_id = %candidate.offering_id,
            "breaker trip write failed"
        );
    }
}

/// A completed response resets the pair's breaker streak (M5).
fn record_breaker_success(state: &AppState, candidate: &Candidate) {
    if let Some(credential) = &candidate.credential {
        state
            .breaker
            .record_success(&credential.id, &candidate.offering_id);
    }
}

/// The output tokens of one attempt for its telemetry sample: the upstream
/// `usage` count when present, else the same `chars/4` content estimate
/// settlement uses.
fn attempt_output_tokens(usage: Option<&Usage>, output_chars: usize) -> Option<i64> {
    match usage {
        Some(usage) => usage.output_tokens.or(Some((output_chars / 4) as i64)),
        None => Some((output_chars / 4) as i64),
    }
}

/// Records one classified attempt's telemetry sample (decision D26): every
/// attempt teaches the router, including failures rescued by failover.
/// `outcome` is the failure taxonomy code, `None` for a completed attempt.
#[allow(clippy::too_many_arguments)]
fn push_attempt_sample(
    samples: &mut Vec<telemetry::AttemptSample>,
    candidate: &Candidate,
    started: Instant,
    outcome: Option<&'static str>,
    ttft_ms: Option<i64>,
    output_tokens: Option<i64>,
    upstream_status: Option<i64>,
    detail: Option<&ErrorDetail>,
) {
    samples.push(telemetry::AttemptSample {
        offering_id: candidate.offering_id.clone(),
        credential_id: candidate.credential.as_ref().map(|c| c.id.clone()),
        outcome,
        latency_ms: Some(started.elapsed().as_millis() as i64),
        ttft_ms,
        output_tokens,
        upstream_status,
        provider_error_code: detail.and_then(|detail| detail.provider_code.clone()),
        error_detail: detail.and_then(|detail| detail.detail.clone()),
    });
}

/// Fixed, secret-free messages per taxonomy code; details go to the log.
fn failure_from_class(class: &FailureClass) -> ChatFailure {
    let message = match class.code {
        "invalid_request" => "the upstream rejected the request as invalid",
        "upstream_auth" => "the upstream rejected the gateway's credential",
        "upstream_quota" => "the upstream credential is out of quota",
        "upstream_rate_limited" => "the upstream is rate limiting requests",
        "upstream_timeout" => "the upstream timed out",
        "upstream_stream_failed" => "the upstream response stream failed",
        "internal" => "internal error",
        _ => "the upstream request failed",
    };
    let mut failure = ChatFailure::new(class.status, class.code, message);
    failure.retry_after = class.cooldown.map(|cooldown| cooldown.as_secs());
    failure
}

/// The same client-visible failure plus the operator-only detail (D49).
fn failure_from_class_with_detail(
    class: &FailureClass,
    detail: Option<&ErrorDetail>,
) -> ChatFailure {
    let mut failure = failure_from_class(class);
    failure.detail = detail.and_then(|detail| detail.detail.clone());
    failure
}

/// A synthetic detail for a failure that has no provider body (a truncation,
/// an oversized line, a deadline).
fn text_detail(message: &str) -> ErrorDetail {
    ErrorDetail {
        provider_code: None,
        detail: Some(message.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// Signals the runner when the response body (or the awaiting handler) is
/// dropped — i.e. the client disconnected.
struct CancelOnDrop(Option<oneshot::Sender<()>>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

enum Wait<T> {
    Done(T),
    Canceled,
}

/// Awaits `future` until it completes or the client cancels.
async fn or_cancel<F: std::future::Future>(
    cancel: &mut oneshot::Receiver<()>,
    future: F,
) -> Wait<F::Output> {
    tokio::select! {
        _ = &mut *cancel => Wait::Canceled,
        output = future => Wait::Done(output),
    }
}

/// The in-band error event for a stream that failed after first byte: the
/// uniform error shape as one SSE `data:` event, then `[DONE]`. It is preceded
/// by a blank line so it can never concatenate onto a partial upstream line
/// (review m2).
fn in_band_error_bytes(failure: &ChatFailure, request_id: &str) -> Bytes {
    let payload = json!({
        "error": {
            "code": failure.code,
            "message": failure.message,
            "request_id": request_id,
        }
    });
    Bytes::from(format!("\n\ndata: {payload}\n\ndata: [DONE]\n\n"))
}

// ---------------------------------------------------------------------------
// Settlement
// ---------------------------------------------------------------------------

struct Settlement {
    meta: RequestMeta,
    requested_model: Option<String>,
    decision: Decision,
    offering_id: Option<String>,
    model_id: Option<String>,
    usage: Option<Usage>,
    /// Content characters produced (never scaffolding), for estimation.
    output_chars: usize,
    /// Message-text characters sent, for estimation (review M1).
    input_chars: usize,
    /// A request was actually sent upstream (an attempt opened its stream);
    /// only then may tokens be estimated (review M3).
    sent: bool,
    status: i64,
    error_class: Option<&'static str>,
    /// Bounded, redacted provider text for the final failure (D49).
    error_detail: Option<String>,
    started: Instant,
    ttft_ms: Option<i64>,
    bytes_out: u64,
    bytes_in: u64,
    has_request_body: bool,
    /// Price rows resolved before dispatch, by offering id. Settlement reuses
    /// the chosen offering's row instead of re-reading it; a missing entry is
    /// read from the database.
    prices: HashMap<String, Option<catalog::PriceRow>>,
}

impl Settlement {
    /// Rows for a request that never reached the routing stage.
    fn rejected(
        meta: &RequestMeta,
        started: Instant,
        bytes_in: u64,
        failure: &ChatFailure,
    ) -> Self {
        Self {
            meta: meta.clone(),
            requested_model: None,
            decision: Decision::default(),
            offering_id: None,
            model_id: None,
            usage: None,
            output_chars: 0,
            input_chars: 0,
            sent: false,
            status: failure.status.as_u16() as i64,
            error_class: Some(failure.code),
            error_detail: failure.detail.clone(),
            started,
            ttft_ms: None,
            bytes_out: 0,
            bytes_in,
            has_request_body: bytes_in > 0,
            prices: HashMap::new(),
        }
    }

    fn rejected_with_decision(
        meta: &RequestMeta,
        started: Instant,
        bytes_in: u64,
        failure: &ChatFailure,
        decision: Decision,
        requested_model: &str,
    ) -> Self {
        Self {
            requested_model: Some(requested_model.to_string()),
            decision,
            ..Self::rejected(meta, started, bytes_in, failure)
        }
    }
}

/// The token counts to store: upstream `usage` when present, else the `chars/4`
/// content estimate (flagged) — but only when a request was actually sent
/// upstream, never for pre-send failures (review M1/M3).
fn resolve_usage(settlement: &Settlement) -> (Option<Usage>, bool) {
    if let Some(usage) = &settlement.usage {
        return (Some(usage.clone()), false);
    }
    match settlement.sent {
        true => (
            Some(Usage {
                input_tokens: Some((settlement.input_chars / 4) as i64),
                output_tokens: Some((settlement.output_chars / 4) as i64),
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            }),
            true,
        ),
        false => (None, false),
    }
}

/// Writes the `usage_events` + `request_logs` pair, one `telemetry_attempts`
/// sample per classified attempt (decision D26), settles the request's
/// reservation (budgets and token caps, decision D7), and books the
/// `usage_daily` rollup — all in one transaction. Exactly one pair per
/// request (a failover never double-spends). Accounting failures are logged,
/// never surfaced: the response must not depend on the ledger.
///
/// The cost (M4) is priced from the *chosen* offering's `prices` row against
/// the settled usage (decision D10): an unknown price leaves `cost_usd` NULL
/// ("unpriced", never a silent 0) and a used component without price
/// contributes 0 and flags the row `priced = 0`. The reservation marks itself
/// `settled` (booking the actuals into the period the request started in) or
/// `released` when the request consumed nothing. A request that consumed
/// nothing (a pre-send refusal) is booked with `refused = 1` and cost 0.0:
/// reports count it in `refused_requests`, never in `priced_requests`
/// (migration 0006).
async fn settle(state: &AppState, settlement: &Settlement) {
    let (usage, tokens_estimated) = resolve_usage(settlement);
    let occurred_at = catalog::now_ts();
    let decision_json = settlement.decision.to_json().to_string();
    let latency_ms = settlement.started.elapsed().as_millis() as i64;
    let api_key_id = settlement
        .meta
        .api_key
        .as_ref()
        .map(|key| key.key_id.clone());

    // The routing price row is reused when the request was served by the
    // offering it was resolved for. Only an offering outside the resolved set
    // (an unexpected failover) pays for the read.
    let price = match &settlement.offering_id {
        Some(offering_id) => match settlement.prices.get(offering_id) {
            Some(price) => price.clone(),
            None => match catalog::get_price(&state.pool, offering_id).await {
                Ok(price) => price,
                Err(e) => {
                    tracing::error!(error = %e, "reading the settled price failed");
                    None
                }
            },
        },
        None => None,
    };
    let cost = money::compute_cost(price.as_ref(), usage.as_ref());
    let tokens = usage.as_ref().map(money::total_tokens).unwrap_or(0);
    let started_at = chrono::DateTime::parse_from_rfc3339(&settlement.meta.started_at)
        .map(|value| value.to_utc())
        .unwrap_or_else(|_| chrono::Utc::now());
    let day = money::day_key(state.config.tz, started_at);

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!(
                error = %e,
                request_id = %settlement.meta.request_id,
                "usage accounting unavailable"
            );
            return;
        }
    };

    let usage_result = sqlx::query(
        "INSERT INTO usage_events \
         (request_id, user_id, api_key_id, offering_id, model_id, input_tokens, output_tokens, \
          cache_read_tokens, cache_write_tokens, reasoning_tokens, tokens_estimated, cost_usd, \
          priced, refused, started_at, latency_ms, ttft_ms, status, error_class, occurred_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&settlement.meta.request_id)
    .bind(&settlement.meta.user_id)
    .bind(&api_key_id)
    .bind(&settlement.offering_id)
    .bind(&settlement.model_id)
    .bind(usage.as_ref().and_then(|usage| usage.input_tokens))
    .bind(usage.as_ref().and_then(|usage| usage.output_tokens))
    .bind(usage.as_ref().and_then(|usage| usage.cache_read_tokens))
    .bind(usage.as_ref().and_then(|usage| usage.cache_write_tokens))
    .bind(usage.as_ref().and_then(|usage| usage.reasoning_tokens))
    .bind(i64::from(tokens_estimated))
    .bind(cost.cost_usd)
    .bind(i64::from(cost.priced))
    // Refusal marker (migration 0006): `resolve_usage` books no usage
    // exactly when nothing was sent upstream — a pre-send refusal (402
    // budget, 429 token cap, 403 access, ...) that consumed nothing and was
    // never priced. Reports give it its own counter.
    .bind(i64::from(usage.is_none()))
    .bind(&settlement.meta.started_at)
    .bind(latency_ms)
    .bind(settlement.ttft_ms)
    .bind(settlement.status)
    .bind(settlement.error_class)
    .bind(&occurred_at)
    .execute(&mut *tx)
    .await;

    let log_result = sqlx::query(
        "INSERT INTO request_logs \
         (request_id, client_request_id, occurred_at, method, path, requested_model, \
          resolved_offering_id, decision_json, status, error_class, bytes_in, bytes_out, user_id, \
          api_key_id, has_request_body, error_detail) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&settlement.meta.request_id)
    .bind(&settlement.meta.client_request_id)
    .bind(&occurred_at)
    .bind(&settlement.meta.method)
    .bind(&settlement.meta.path)
    .bind(&settlement.requested_model)
    .bind(&settlement.offering_id)
    .bind(&decision_json)
    .bind(settlement.status)
    .bind(settlement.error_class)
    .bind(settlement.bytes_in as i64)
    .bind(settlement.bytes_out as i64)
    .bind(&settlement.meta.user_id)
    .bind(&api_key_id)
    .bind(i64::from(settlement.has_request_body))
    .bind(&settlement.error_detail)
    .execute(&mut *tx)
    .await;

    // The reservation, budgets, and token caps settle with the ledger row.
    let reserve_result = money::settle_reservation(
        &mut tx,
        state.config.tz,
        &money::SettleInput {
            request_id: settlement.meta.request_id.clone(),
            user_id: settlement.meta.user_id.clone(),
            model_id: settlement.model_id.clone(),
            started_at,
            billable: usage.is_some(),
            cost_usd: cost.cost_usd,
            tokens,
        },
    )
    .await;

    let rollup_result = reports::record_usage(
        &mut tx,
        &reports::UsageRow {
            day: &day,
            user_id: &settlement.meta.user_id,
            offering_id: settlement.offering_id.as_deref().unwrap_or(""),
            input_tokens: usage
                .as_ref()
                .and_then(|usage| usage.input_tokens)
                .unwrap_or(0),
            output_tokens: usage
                .as_ref()
                .and_then(|usage| usage.output_tokens)
                .unwrap_or(0),
            cost_usd: cost.cost_usd,
            priced: cost.priced,
            refused: usage.is_none(),
        },
    )
    .await;

    // Per-attempt telemetry samples (decision D26) commit with the ledger:
    // every classified attempt teaches the router — including failures
    // rescued by failover, which the single request row cannot show.
    let telemetry_result = telemetry::record_attempts(
        &mut tx,
        &settlement.meta.request_id,
        &settlement.decision.samples,
    )
    .await;

    let outcome = match (
        usage_result,
        log_result,
        reserve_result,
        rollup_result,
        telemetry_result,
    ) {
        (Ok(_), Ok(_), Ok(_), Ok(_), Ok(_)) => tx.commit().await.map(|_| ()),
        (Err(e), _, _, _, _)
        | (_, Err(e), _, _, _)
        | (_, _, Err(e), _, _)
        | (_, _, _, Err(e), _)
        | (_, _, _, _, Err(e)) => Err(e),
    };
    if let Err(e) = outcome {
        tracing::error!(
            error = %e,
            request_id = %settlement.meta.request_id,
            "usage accounting write failed"
        );
    }
}

/// Computes the D35 preview for a validated draft or saved combo. The
/// ranking half is exactly [`combos::resolve_pool`], the same function the
/// runtime `weighted` path calls; this function supplies the catalog, price,
/// quality, telemetry, and breaker inputs. It never writes anything (no pins,
/// no audit). Failure mode: `500 internal` on database errors.
pub(crate) async fn preview_combo(
    state: &AppState,
    combo: &combos::Combo,
    session_key: Option<&str>,
    limit: usize,
) -> Result<combos::PreviewResult, ChatFailure> {
    let now = crate::catalog::now_ts();
    let candidates = match combo.pool {
        combos::Pool::All => resolve_pool_candidates(state, &now).await?,
        combos::Pool::Members => resolve_members(&state.pool, &state.breaker, combo, &now).await?,
    };

    let default_mix = match state
        .mix_profiles
        .resolve_default(&state.pool, state.config.token_mix_profile.as_deref())
        .await
    {
        Ok(profile) => profile,
        Err(e) => {
            tracing::error!(error = %e, "resolving the default mix profile failed");
            mix::code_agent_profile()
        }
    };
    let mix_profile = match combo.mix_profile.as_deref() {
        Some(selector) => match state
            .mix_profiles
            .resolve_selector(&state.pool, selector)
            .await
        {
            Ok(Some(profile)) => profile,
            Ok(None) | Err(_) => default_mix,
        },
        None => default_mix,
    };

    let mut model_ids: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.model_id.clone())
        .collect();
    model_ids.sort();
    model_ids.dedup();
    let quality = match benchmarks::quality_for_models(&state.pool, &model_ids).await {
        Ok(quality) => quality,
        Err(e) => {
            tracing::error!(error = %e, "loading model quality failed");
            HashMap::new()
        }
    };
    let member_weights: HashMap<&str, f64> = combo
        .members
        .iter()
        .map(|member| (member.offering_id.as_str(), member.weight))
        .collect();

    let mut items = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let price = catalog::get_price(&state.pool, &candidate.offering_id)
            .await
            .ok()
            .flatten();
        let mix_price = price
            .as_ref()
            .and_then(|price| mix::mix_price(price, &mix_profile));
        let quality_score =
            quality
                .get(&candidate.model_id)
                .copied()
                .unwrap_or(routing::QualityScore {
                    score: 0.5,
                    source: routing::QualitySource::ModelDefault,
                    imputed: true,
                });
        let mut signal = signal_for(
            state,
            candidate,
            price.as_ref(),
            mix_price.as_ref(),
            Some(quality_score),
            member_weights
                .get(candidate.offering_id.as_str())
                .copied()
                .unwrap_or(1.0),
            0.0,
            false,
            true,
        );
        signal.enabled = candidate.enabled;
        items.push(combos::PoolItem {
            offering_id: candidate.offering_id.clone(),
            model: candidate.canonical_name.clone(),
            provider: candidate.provider_name.clone(),
            quality: quality_score,
            mix_price,
            signal,
        });
    }

    let constraints = match combo.pool {
        combos::Pool::All => combos::PoolConstraints::of(combo),
        combos::Pool::Members => combos::PoolConstraints::default(),
    };
    let outcome = combos::resolve_pool(items, &constraints, &combo.weights);
    let mut ordered = outcome.ordered;
    // For explicit members the strategy decides the order: ordered keeps the
    // stored member order, round_robin rotates it, weighted keeps the score
    // order above.
    if combo.pool == combos::Pool::Members {
        match combo.strategy {
            Strategy::Ordered => {
                let positions: HashMap<&str, usize> = combo
                    .members
                    .iter()
                    .enumerate()
                    .map(|(index, member)| (member.offering_id.as_str(), index))
                    .collect();
                ordered.sort_by_key(|scored| {
                    positions
                        .get(scored.item.offering_id.as_str())
                        .copied()
                        .unwrap_or(usize::MAX)
                });
            }
            Strategy::RoundRobin => {
                ordered = routing::rotate(ordered, combo.rr_cursor as usize);
            }
            Strategy::Weighted => {}
        }
    }
    // The score count is taken before truncation: the list shows `limit`
    // rows but every explanation says "#N of scored".
    let scored = ordered.len();
    ordered.truncate(limit.clamp(1, 100));

    let pinned = match (combo.stickiness_enabled, session_key) {
        (true, Some(key)) => combos::load_pin(&state.pool, &combo.id, key, &now)
            .await
            .ok()
            .flatten(),
        _ => None,
    };
    let sticky = combo.stickiness_enabled.then(|| combos::PreviewSticky {
        enabled: true,
        key_source: combo.stickiness_key_source.as_str().to_string(),
        session_key: session_key.map(str::to_owned),
        pinned: pinned.as_ref().map(|pin| pin.offering_id.clone()),
        age_s: pinned
            .as_ref()
            .and_then(|pin| combos::age_seconds(&pin.chosen_at, &now)),
    });

    Ok(combos::PreviewResult {
        profile: mix::MixProfileRef::from(&mix_profile),
        scored,
        candidates: ordered
            .into_iter()
            .map(|scored| combos::PreviewCandidate {
                offering_id: scored.item.offering_id,
                model: scored.item.model,
                provider: scored.item.provider,
                quality: scored.item.quality.score,
                quality_source: scored.item.quality.source.as_str().to_string(),
                mix_price: scored.item.mix_price.as_ref().map(|mix| mix.per_m),
                score: combos::PreviewScore::from(&scored.score),
                explanation: scored.explanation,
            })
            .collect(),
        excluded: outcome
            .excluded
            .into_iter()
            .map(|excluded| combos::PreviewExcluded {
                offering_id: excluded.offering_id,
                model: excluded.model,
                provider: excluded.provider,
                reason: excluded.reason.as_str().to_string(),
                detail: excluded.detail,
            })
            .collect(),
        sticky,
    })
}

#[cfg(test)]
pub(crate) fn parse_for_measurement(raw: &[u8]) -> bool {
    parse_request(raw).is_ok()
}

/// Test-only: runs the real settlement transaction for one synthesized
/// successful request (the performance harness times it directly). The caller
/// must have inserted the matching `reservations` row and a priced offering.
#[cfg(test)]
pub(crate) async fn settle_for_measurement(
    state: &AppState,
    request_id: &str,
    user_id: &str,
    offering_id: &str,
    model_id: &str,
) {
    let meta = RequestMeta {
        request_id: request_id.to_string(),
        client_request_id: None,
        user_id: user_id.to_string(),
        role: crate::auth::Role::Admin,
        api_key: None,
        session_hint: None,
        started_at: catalog::now_ts(),
        method: "POST".to_string(),
        path: "/v1/chat/completions".to_string(),
    };
    let decision = Decision {
        chosen_offering_id: Some(offering_id.to_string()),
        reason: "performance probe".to_string(),
        ..Decision::default()
    };
    let settlement = Settlement {
        meta,
        requested_model: Some("bench-model".to_string()),
        decision,
        offering_id: Some(offering_id.to_string()),
        model_id: Some(model_id.to_string()),
        usage: Some(Usage {
            input_tokens: Some(900),
            output_tokens: Some(300),
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
        }),
        output_chars: 1200,
        input_chars: 3600,
        sent: true,
        status: 200,
        error_class: None,
        error_detail: None,
        started: Instant::now(),
        ttft_ms: Some(25),
        bytes_out: 1500,
        bytes_in: 4000,
        has_request_body: true,
        prices: HashMap::new(),
    };
    settle(state, &settlement).await;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use axum::body::Body;
    use axum::http::{Method, StatusCode, header};
    use futures_util::StreamExt;
    use serde_json::json;

    use super::*;
    use crate::auth::Role;
    use crate::catalog::{
        self, Actor, CredentialInput, ModelInput, ModelPatch, OfferingInput, PriceInput,
        ProviderInput,
    };
    use crate::config::Config;
    use crate::test_support::{TestApp, api_request, body_text, login, send, send_streaming};
    use crate::upstream::stub::{Scenario, Stub};

    // -----------------------------------------------------------------------
    // Seeding and request helpers
    // -----------------------------------------------------------------------

    struct Seeded {
        app: TestApp,
        cookie: String,
        uid: String,
    }

    async fn seeded() -> Seeded {
        seeded_with(|_| {}).await
    }

    async fn seeded_with(mutate: impl FnOnce(&mut Config)) -> Seeded {
        let app = TestApp::with_config(mutate).await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let (cookie, _csrf) = login(&app.router(), "admin@example.test").await;
        Seeded { app, cookie, uid }
    }

    fn headers(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, serde_json::Value>> {
        match pairs.is_empty() {
            true => None,
            false => Some(
                pairs
                    .iter()
                    .map(|(name, value)| (name.to_string(), json!(value)))
                    .collect(),
            ),
        }
    }

    async fn provider_with_credential(
        app: &TestApp,
        uid: &str,
        name: &str,
        base_url: &str,
        credential_headers: Option<BTreeMap<String, serde_json::Value>>,
    ) -> (String, String) {
        let actor = Actor {
            user_id: uid,
            ip: "127.0.0.1",
        };
        let provider = catalog::create_provider(
            &app.state.pool,
            &actor,
            &ProviderInput {
                name: name.to_string(),
                base_url: base_url.to_string(),
                kind: None,
                enabled: None,
                preset: None,
                connectors: None,
            },
        )
        .await
        .expect("provider");
        let credential = catalog::create_credential(
            &app.state.pool,
            &app.state.secrets,
            &actor,
            &provider.id,
            &CredentialInput {
                label: format!("{name}-key"),
                secret: "sk-stub-secret".to_string(),
                headers: credential_headers,
                max_concurrency: None,
                balance_usd: None,
                balance_source: None,
                balance_low_threshold: None,
                balance_gate: None,
            },
        )
        .await
        .expect("credential");
        (provider.id, credential.id)
    }

    async fn add_credential(app: &TestApp, uid: &str, provider_id: &str, label: &str) -> String {
        let actor = Actor {
            user_id: uid,
            ip: "127.0.0.1",
        };
        catalog::create_credential(
            &app.state.pool,
            &app.state.secrets,
            &actor,
            provider_id,
            &CredentialInput {
                label: label.to_string(),
                secret: format!("sk-{label}"),
                headers: None,
                max_concurrency: None,
                balance_usd: None,
                balance_source: None,
                balance_low_threshold: None,
                balance_gate: None,
            },
        )
        .await
        .expect("credential")
        .id
    }

    async fn model(
        app: &TestApp,
        uid: &str,
        name: &str,
        context: Option<i64>,
        max_output: Option<i64>,
    ) -> String {
        let actor = Actor {
            user_id: uid,
            ip: "127.0.0.1",
        };
        catalog::create_model(
            &app.state.pool,
            &actor,
            &ModelInput {
                canonical_name: name.to_string(),
                family: None,
                modalities_input: None,
                modalities_output: None,
                context_length: context,
                max_output_tokens: max_output,
                capabilities: None,
            },
        )
        .await
        .expect("model")
        .id
    }

    /// Creates an offering pinned to `created_at` so the fixed candidate
    /// order (creation order, id tiebreak) is deterministic in tests.
    async fn offering(
        app: &TestApp,
        uid: &str,
        provider_id: &str,
        model_id: &str,
        upstream_model_id: &str,
        created_at: &str,
        offering_headers: Option<BTreeMap<String, serde_json::Value>>,
    ) -> String {
        let actor = Actor {
            user_id: uid,
            ip: "127.0.0.1",
        };
        let row = catalog::create_offering(
            &app.state.pool,
            &app.state.secrets,
            &actor,
            &OfferingInput {
                provider_id: provider_id.to_string(),
                model_id: model_id.to_string(),
                upstream_model_id: upstream_model_id.to_string(),
                context_override: None,
                max_output_override: None,
                enabled: None,
                headers: offering_headers,
            },
        )
        .await
        .expect("offering");
        sqlx::query("UPDATE offerings SET created_at = ? WHERE id = ?")
            .bind(created_at)
            .bind(&row.id)
            .execute(&app.state.pool)
            .await
            .expect("pin offering order");
        row.id
    }

    fn chat_request(cookie: &str, body: serde_json::Value) -> axum::http::Request<Body> {
        // Deliberately no X-CSRF-Token: /v1 is exempt from the CSRF guard
        // (asserted by every test here).
        api_request(
            Method::POST,
            "/v1/chat/completions",
            Some(body),
            Some(cookie),
            None,
        )
    }

    fn chat_body(model: &str, stream: bool) -> serde_json::Value {
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": stream,
            "max_tokens": 42,
        })
    }

    fn event(i: usize) -> String {
        format!(r#"{{"choices":[{{"delta":{{"content":"c{i}"}}}}]}}"#)
    }

    fn usage_json(prompt: i64, completion: i64) -> serde_json::Value {
        json!({"prompt_tokens": prompt, "completion_tokens": completion})
    }

    // -----------------------------------------------------------------------
    // Ledger access
    // -----------------------------------------------------------------------

    #[derive(sqlx::FromRow, Debug)]
    struct UsageRow {
        request_id: String,
        user_id: Option<String>,
        offering_id: Option<String>,
        model_id: Option<String>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        cache_read_tokens: Option<i64>,
        cache_write_tokens: Option<i64>,
        reasoning_tokens: Option<i64>,
        tokens_estimated: i64,
        cost_usd: Option<f64>,
        latency_ms: Option<i64>,
        ttft_ms: Option<i64>,
        status: Option<i64>,
        error_class: Option<String>,
    }

    #[derive(sqlx::FromRow, Debug)]
    struct LogRow {
        request_id: String,
        client_request_id: Option<String>,
        requested_model: Option<String>,
        resolved_offering_id: Option<String>,
        decision_json: Option<String>,
        status: Option<i64>,
        error_class: Option<String>,
        bytes_in: Option<i64>,
        bytes_out: Option<i64>,
        has_request_body: i64,
    }

    async fn usage_rows(app: &TestApp) -> Vec<UsageRow> {
        sqlx::query_as::<_, UsageRow>(
            "SELECT request_id, user_id, offering_id, model_id, input_tokens, output_tokens, \
             cache_read_tokens, cache_write_tokens, reasoning_tokens, tokens_estimated, \
             cost_usd, latency_ms, ttft_ms, status, error_class \
             FROM usage_events ORDER BY id",
        )
        .fetch_all(&app.state.pool)
        .await
        .expect("usage rows")
    }

    async fn log_rows(app: &TestApp) -> Vec<LogRow> {
        sqlx::query_as::<_, LogRow>(
            "SELECT request_id, client_request_id, requested_model, resolved_offering_id, \
             decision_json, status, error_class, bytes_in, bytes_out, has_request_body \
             FROM request_logs ORDER BY rowid",
        )
        .fetch_all(&app.state.pool)
        .await
        .expect("log rows")
    }

    fn decision_of(log: &LogRow) -> serde_json::Value {
        let raw = log.decision_json.as_deref().expect("decision_json");
        serde_json::from_str(raw).expect("decision parses")
    }

    async fn cooldown_of(app: &TestApp, credential_id: &str) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT cooldown_until FROM provider_credentials WHERE id = ?",
        )
        .bind(credential_id)
        .fetch_one(&app.state.pool)
        .await
        .expect("cooldown row")
    }

    // -----------------------------------------------------------------------
    // Streaming
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn stream_with_usage_is_forwarded_and_settled() {
        let stub = Stub::spawn(vec![Scenario::Stream {
            events: vec![event(0)],
            usage: Some(json!({
                "prompt_tokens": 11,
                "completion_tokens": 7,
                "prompt_tokens_details": {"cached_tokens": 3, "cache_creation_tokens": 2},
                "completion_tokens_details": {"reasoning_tokens": 4},
            })),
        }])
        .await;
        let s = seeded().await;
        let (provider_id, _credential_id) = provider_with_credential(
            &s.app,
            &s.uid,
            "p1",
            &stub.base_url,
            headers(&[("x-tenant", "credential-value")]),
        )
        .await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        let offering_id = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            headers(&[("x-tenant", "offering-value")]),
        )
        .await;

        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "max_tokens": 999999,
            "temperature": 0.7,
        });
        let (status, response_headers, response_body) =
            send_streaming(s.app.router(), chat_request(&s.cookie, body.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            response_headers[header::CONTENT_TYPE],
            "text/event-stream; charset=utf-8"
        );
        let text = body_text(response_body).await;
        assert!(text.contains("c0"), "{text}");
        assert!(text.contains("data: [DONE]"), "{text}");

        // The upstream request was rewritten, clamped, and header-merged.
        let requests = stub.requests();
        assert_eq!(requests.len(), 1);
        let upstream = &requests[0];
        assert_eq!(upstream.path, "/chat/completions");
        assert_eq!(upstream.body["model"], "test-model");
        assert_eq!(upstream.body["stream"], true);
        assert_eq!(upstream.body["stream_options"]["include_usage"], true);
        assert_eq!(
            upstream.body["max_tokens"], 128,
            "clamped to the offering's output limit"
        );
        assert_eq!(upstream.body["temperature"], 0.7, "extras pass through");
        assert_eq!(
            upstream.headers.get("authorization").map(String::as_str),
            Some("Bearer sk-stub-secret")
        );
        assert_eq!(
            upstream.headers.get("x-tenant").map(String::as_str),
            Some("offering-value"),
            "the offering header wins over the credential's"
        );

        // Exactly one row pair, settled from the upstream's usage.
        let usage = usage_rows(&s.app).await;
        assert_eq!(
            usage.len(),
            1,
            "one usage row per request, never per attempt"
        );
        let row = &usage[0];
        assert_eq!(row.user_id.as_deref(), Some(s.uid.as_str()));
        assert_eq!(row.offering_id.as_deref(), Some(offering_id.as_str()));
        assert_eq!(row.model_id.as_deref(), Some(model_id.as_str()));
        assert_eq!(row.input_tokens, Some(11));
        assert_eq!(row.output_tokens, Some(7));
        assert_eq!(row.cache_read_tokens, Some(3));
        assert_eq!(row.cache_write_tokens, Some(2));
        assert_eq!(row.reasoning_tokens, Some(4));
        assert_eq!(row.tokens_estimated, 0);
        assert!(row.cost_usd.is_none(), "cost accounting is M4's job");
        assert_eq!(row.status, Some(200));
        assert!(row.error_class.is_none());
        assert!(row.ttft_ms.is_some(), "ttft recorded from the first chunk");
        assert!(row.latency_ms.is_some());

        let logs = log_rows(&s.app).await;
        assert_eq!(logs.len(), 1, "one request log row per request");
        let log = &logs[0];
        assert_eq!(log.request_id, row.request_id);
        assert_eq!(log.requested_model.as_deref(), Some("m"));
        assert_eq!(
            log.resolved_offering_id.as_deref(),
            Some(offering_id.as_str())
        );
        assert_eq!(log.status, Some(200));
        assert!(log.error_class.is_none());
        assert_eq!(log.bytes_in, Some(body.to_string().len() as i64));
        assert!(log.bytes_out.unwrap() > 0);
        assert_eq!(log.has_request_body, 1);

        let decision = decision_of(log);
        let candidates = decision["candidates"].as_array().expect("candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["id"], offering_id);
        assert_eq!(candidates[0]["provider"], "p1");
        assert_eq!(candidates[0]["model"], "m");
        assert_eq!(candidates[0]["outcome"], "chosen");
        assert!(candidates[0]["price"].is_null(), "unknown price stays null");
        assert_eq!(decision["chosen_offering_id"], offering_id);
        let reason = decision["reason"].as_str().expect("reason");
        assert!(reason.contains(&offering_id), "{reason}");
    }

    #[tokio::test]
    async fn stream_without_usage_estimates_content_chars_over_four() {
        // 39 characters on the wire, one content character: the estimate is
        // content/4 = 0, never frame/4 = 9 (review M1).
        let frame = r#"{"choices":[{"delta":{"content":"x"}}]}"#.to_string();
        assert_eq!(frame.chars().count(), 39);
        let stub = Stub::spawn(vec![Scenario::Stream {
            events: vec![frame],
            usage: None,
        }])
        .await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true,
        });
        let (_, _, response_body) =
            send_streaming(s.app.router(), chat_request(&s.cookie, body)).await;
        let text = body_text(response_body).await;
        assert!(text.contains("data: [DONE]"), "{text}");

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.tokens_estimated, 1, "estimation must be flagged");
        assert_eq!(
            row.input_tokens,
            Some(1),
            "message text (\"hello\" = 5 chars) / 4, not raw body bytes"
        );
        assert_eq!(
            row.output_tokens,
            Some(0),
            "one content char / 4, not the 39-char frame"
        );
        assert!(row.cache_read_tokens.is_none(), "unknown is NULL, never 0");
    }

    #[tokio::test]
    async fn non_streaming_without_usage_estimates_content_chars() {
        let upstream_body = r#"{"id":"cmpl-1","object":"chat.completion","model":"test-model","choices":[{"message":{"role":"assistant","content":"hello"}}]}"#;
        let stub = Stub::spawn(vec![Scenario::Json {
            body: upstream_body.to_string(),
        }])
        .await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false,
        });
        let response = send(s.app.router(), chat_request(&s.cookie, body)).await;
        assert_eq!(response.status, StatusCode::OK);

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.tokens_estimated, 1);
        assert_eq!(row.input_tokens, Some(1), "message text / 4");
        assert_eq!(
            row.output_tokens,
            Some(1),
            "\"hello\" = 5 content chars / 4, not the ~130-char body"
        );
    }

    #[tokio::test]
    async fn malformed_sse_surfaces_a_clean_in_band_error() {
        let stub = Stub::spawn(vec![Scenario::Malformed { after: 1 }]).await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = body_text(response_body).await;
        assert!(text.contains("c0"), "{text}");
        assert!(
            !text.contains("this is not json"),
            "the offending chunk is never forwarded: {text}"
        );
        assert!(
            text.contains("\"code\":\"upstream_stream_failed\""),
            "{text}"
        );
        assert!(text.contains("data: [DONE]"), "{text}");

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("upstream_stream_failed"));
        assert_eq!(row.status, Some(200), "the stream was already committed");
    }

    #[tokio::test]
    async fn truncated_stream_after_first_byte_never_fails_over() {
        // The second scenario would serve a full stream; it must never run.
        let stub = Stub::spawn(vec![
            Scenario::Truncate { after: 2 },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = body_text(response_body).await;
        assert!(text.contains("c0") && text.contains("c1"), "{text}");
        assert!(
            text.contains("\"code\":\"upstream_stream_failed\""),
            "{text}"
        );
        assert!(text.contains("data: [DONE]"), "{text}");
        assert_eq!(
            stub.request_count(),
            1,
            "failover is only allowed before the first byte"
        );

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("upstream_stream_failed"));
        let log = &log_rows(&s.app).await[0];
        let decision = decision_of(log);
        assert_eq!(decision["candidates"][0]["outcome"], "chosen");
        assert_eq!(
            decision["candidates"][0]["error_class"],
            "upstream_stream_failed"
        );
        assert_eq!(decision["candidates"][1]["outcome"], "not_tried");
        assert_eq!(decision["chosen_offering_id"], off1);
    }

    // -----------------------------------------------------------------------
    // Timeouts
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn slow_first_byte_times_out_within_budget() {
        let stub = Stub::spawn(vec![Scenario::SlowStart {
            delay: Duration::from_secs(10),
            then: Box::new(Scenario::Stream {
                events: vec![],
                usage: None,
            }),
        }])
        .await;
        let s = seeded_with(|config| {
            config.timeout_first_byte = Duration::from_millis(300);
        })
        .await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let started = Instant::now();
        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the request must not outlive its budget: {:?}",
            started.elapsed()
        );
        assert_eq!(response.status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(response.body["error"]["code"], "upstream_timeout");
        assert!(response.body["error"]["request_id"].is_string());
        assert_eq!(stub.request_count(), 1);

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("upstream_timeout"));
        assert_eq!(row.status, Some(504));
    }

    #[tokio::test]
    async fn stall_mid_stream_times_out_in_band_without_failover() {
        let stub = Stub::spawn(vec![
            Scenario::Stall {
                after: 1,
                delay: Duration::from_secs(10),
            },
            Scenario::Stream {
                events: vec![],
                usage: None,
            },
        ])
        .await;
        let s = seeded_with(|config| {
            config.timeout_idle = Duration::from_millis(300);
        })
        .await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let started = Instant::now();
        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the stream was committed before the stall"
        );
        let text = body_text(response_body).await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the request must not outlive its budget: {:?}",
            started.elapsed()
        );
        assert!(text.contains("c0"), "{text}");
        assert!(text.contains("\"code\":\"upstream_timeout\""), "{text}");
        assert!(text.contains("data: [DONE]"), "{text}");
        assert_eq!(stub.request_count(), 1, "no failover after the first byte");

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("upstream_timeout"));
        assert_eq!(row.status, Some(200), "the client saw a committed 200");
    }

    #[tokio::test]
    async fn total_budget_bounds_the_whole_request() {
        let stub = Stub::spawn(vec![Scenario::SlowStart {
            delay: Duration::from_secs(10),
            then: Box::new(Scenario::Stream {
                events: vec![],
                usage: None,
            }),
        }])
        .await;
        let s = seeded_with(|config| {
            config.timeout_total = Duration::from_millis(400);
            config.timeout_first_byte = Duration::from_secs(30);
        })
        .await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let started = Instant::now();
        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the total budget is absolute: {:?}",
            started.elapsed()
        );
        assert_eq!(response.status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(response.body["error"]["code"], "upstream_timeout");
    }

    #[tokio::test]
    async fn client_disconnect_aborts_the_upstream_within_one_second() {
        let stub = Stub::spawn(vec![Scenario::Drip {
            interval: Duration::from_millis(50),
            chunk: format!("data: {}\n\n", event(0)),
        }])
        .await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let (_, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        let mut stream = response_body.into_data_stream();
        assert!(stream.next().await.is_some(), "first chunk arrives");
        assert!(stream.next().await.is_some(), "second chunk arrives");
        let dropped_at = Instant::now();
        drop(stream);

        let disconnected = stub
            .wait_for_disconnect(Duration::from_secs(1))
            .await
            .expect("the stub must see the upstream connection close");
        let after_drop = disconnected.saturating_duration_since(dropped_at);
        assert!(
            after_drop < Duration::from_secs(1),
            "upstream aborted within 1s of the disconnect: {after_drop:?}"
        );

        // The rows settle right after the abort.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let rows = usage_rows(&s.app).await;
            if !rows.is_empty() {
                assert_eq!(rows[0].error_class.as_deref(), Some("client_disconnected"));
                assert_eq!(rows[0].status, Some(499));
                break;
            }
            assert!(Instant::now() < deadline, "rows must settle: {rows:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // -----------------------------------------------------------------------
    // Failover
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn upstream_500_fails_over_to_the_next_candidate() {
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 500,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(5, 6)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, cred1) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let off2 = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = body_text(response_body).await;
        assert!(text.contains("c0"), "{text}");
        assert_eq!(stub.request_count(), 2, "failover before the first byte");

        let rows = usage_rows(&s.app).await;
        assert_eq!(rows.len(), 1, "one settled row despite two attempts");
        assert_eq!(rows[0].offering_id.as_deref(), Some(off2.as_str()));
        assert_eq!(
            rows[0].input_tokens,
            Some(5),
            "usage of the serving attempt"
        );
        assert_eq!(rows[0].output_tokens, Some(6));
        assert_eq!(rows[0].tokens_estimated, 0);

        let log = &log_rows(&s.app).await[0];
        assert_eq!(log.resolved_offering_id.as_deref(), Some(off2.as_str()));
        let decision = decision_of(log);
        assert_eq!(decision["candidates"][0]["id"], off1);
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(decision["candidates"][0]["error_class"], "upstream_error");
        assert_eq!(decision["candidates"][0]["status"], 500);
        assert_eq!(decision["candidates"][1]["id"], off2);
        assert_eq!(decision["candidates"][1]["outcome"], "chosen");
        assert_eq!(decision["chosen_offering_id"], off2);
        let reason = decision["reason"].as_str().expect("reason");
        assert!(reason.contains("upstream_error"), "{reason}");

        // A 500 is transient: no cooldown on the first credential.
        assert!(cooldown_of(&s.app, &cred1).await.is_none());
    }

    #[tokio::test]
    async fn rate_limited_fails_over_and_records_the_cooldown() {
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 429,
                headers: vec![("retry-after".to_string(), "7".to_string())],
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(3, 4)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, cred1) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let off2 = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = body_text(response_body).await;
        assert!(text.contains("c0"), "{text}");
        assert_eq!(stub.request_count(), 2);

        // Retry-After is honoured as a cooldown on that credential.
        let cooldown = cooldown_of(&s.app, &cred1).await.expect("cooldown set");
        let until = chrono::DateTime::parse_from_rfc3339(&cooldown).expect("rfc3339");
        let delta = until
            .signed_duration_since(chrono::Utc::now())
            .num_seconds();
        assert!(
            (3..=20).contains(&delta),
            "Retry-After 7 honoured as cooldown: {delta}s"
        );

        let rows = usage_rows(&s.app).await;
        assert_eq!(rows[0].offering_id.as_deref(), Some(off2.as_str()));
        let decision = decision_of(&log_rows(&s.app).await[0]);
        assert_eq!(
            decision["candidates"][0]["error_class"],
            "upstream_rate_limited"
        );
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(decision["candidates"][1]["outcome"], "chosen");
    }

    #[tokio::test]
    async fn auth_failure_falls_over_to_the_next_credential_of_the_same_offering() {
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 401,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (provider_id, cred_a) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let cred_b = add_credential(&s.app, &s.uid, &provider_id, "second").await;
        let (first, second) = if cred_a < cred_b {
            (cred_a.clone(), cred_b.clone())
        } else {
            (cred_b.clone(), cred_a.clone())
        };
        let offering_id = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = body_text(response_body).await;
        assert!(text.contains("c0"), "{text}");
        assert_eq!(
            stub.request_count(),
            2,
            "the same offering is retried with the second credential"
        );

        // One settlement row: a credential retry is not a second request.
        let rows = usage_rows(&s.app).await;
        assert_eq!(rows.len(), 1, "exactly one settled row");
        assert_eq!(rows[0].offering_id.as_deref(), Some(offering_id.as_str()));
        assert_eq!(rows[0].error_class, None);
        assert_eq!(log_rows(&s.app).await.len(), 1);

        let decision = decision_of(&log_rows(&s.app).await[0]);
        let attempts = decision["candidates"][0]["credential_attempts"]
            .as_array()
            .expect("credential_attempts array");
        assert_eq!(attempts.len(), 2, "{decision}");
        assert_eq!(attempts[0]["credential_id"], first);
        assert_eq!(attempts[0]["outcome"], "failed");
        assert_eq!(attempts[0]["error_class"], "upstream_auth");
        assert_eq!(attempts[0]["status"], 401);
        assert_eq!(attempts[1]["credential_id"], second);
        assert_eq!(attempts[1]["outcome"], "chosen");
        assert_eq!(decision["candidates"][0]["outcome"], "chosen");
        assert_eq!(decision["candidates"][0]["credential_id"], second);

        // The credential that failed is cooled down; the serving one is not.
        assert!(cooldown_of(&s.app, &first).await.is_some());
        assert!(cooldown_of(&s.app, &second).await.is_none());
    }

    #[tokio::test]
    async fn both_credentials_failing_auth_end_in_502_with_both_recorded() {
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 401,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Status {
                code: 403,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (provider_id, cred_a) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let cred_b = add_credential(&s.app, &s.uid, &provider_id, "second").await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_GATEWAY);
        assert_eq!(response.body["error"]["code"], "upstream_auth");
        assert_eq!(
            stub.request_count(),
            2,
            "both credentials are tried exactly once"
        );

        assert_eq!(usage_rows(&s.app).await.len(), 1, "one settlement row");
        let decision = decision_of(&log_rows(&s.app).await[0]);
        let attempts = decision["candidates"][0]["credential_attempts"]
            .as_array()
            .expect("credential_attempts array");
        assert_eq!(attempts.len(), 2, "{decision}");
        assert_eq!(attempts[0]["outcome"], "failed");
        assert_eq!(attempts[1]["outcome"], "failed");
        let recorded: Vec<&str> = attempts
            .iter()
            .filter_map(|attempt| attempt["credential_id"].as_str())
            .collect();
        assert!(recorded.contains(&cred_a.as_str()));
        assert!(recorded.contains(&cred_b.as_str()));
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(decision["candidates"][0]["error_class"], "upstream_auth");
    }

    #[tokio::test]
    async fn auth_failure_on_the_only_credential_still_cools_it() {
        let stub = Stub::spawn(vec![Scenario::Status {
            code: 401,
            headers: Vec::new(),
            body: "{}".to_string(),
        }])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, cred1) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_GATEWAY);
        assert_eq!(response.body["error"]["code"], "upstream_auth");
        assert_eq!(stub.request_count(), 1, "nothing else to try");

        // The broken credential is cooled down.
        let cooldown = cooldown_of(&s.app, &cred1).await.expect("cooldown set");
        assert!(!cooldown.is_empty());

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("upstream_auth"));
        assert_eq!(row.status, Some(502));
        let decision = decision_of(&log_rows(&s.app).await[0]);
        assert_eq!(decision["candidates"][0]["error_class"], "upstream_auth");
        let attempts = decision["candidates"][0]["credential_attempts"]
            .as_array()
            .expect("credential_attempts array");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["credential_id"], cred1);
    }

    #[tokio::test]
    async fn auth_failure_falls_over_to_the_next_offering() {
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 401,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, cred1) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let off2 = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            stub.request_count(),
            2,
            "a credential fault advances to the next offering"
        );

        assert_eq!(usage_rows(&s.app).await.len(), 1);
        assert!(cooldown_of(&s.app, &cred1).await.is_some());
        let decision = decision_of(&log_rows(&s.app).await[0]);
        assert_eq!(decision["candidates"][0]["id"], off1);
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(decision["candidates"][0]["error_class"], "upstream_auth");
        assert_eq!(decision["candidates"][1]["id"], off2);
        assert_eq!(decision["candidates"][1]["outcome"], "chosen");
    }

    #[tokio::test]
    async fn rate_limit_on_one_credential_falls_over_to_the_other() {
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 429,
                headers: vec![("retry-after".to_string(), "5".to_string())],
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(2, 3)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (provider_id, cred_a) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let cred_b = add_credential(&s.app, &s.uid, &provider_id, "second").await;
        let (first, second) = if cred_a < cred_b {
            (cred_a.clone(), cred_b.clone())
        } else {
            (cred_b.clone(), cred_a.clone())
        };
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(stub.request_count(), 2);
        assert_eq!(usage_rows(&s.app).await.len(), 1, "one settlement row");

        // Retry-After cools only the credential that was rate limited.
        assert!(cooldown_of(&s.app, &first).await.is_some());
        assert!(cooldown_of(&s.app, &second).await.is_none());
        let decision = decision_of(&log_rows(&s.app).await[0]);
        let attempts = decision["candidates"][0]["credential_attempts"]
            .as_array()
            .expect("credential_attempts array");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["error_class"], "upstream_rate_limited");
        assert_eq!(attempts[1]["outcome"], "chosen");
        assert_eq!(decision["candidates"][0]["outcome"], "chosen");
    }

    #[tokio::test]
    async fn rate_limits_fail_over_at_most_once() {
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 429,
                headers: vec![("retry-after".to_string(), "1".to_string())],
                body: "{}".to_string(),
            },
            Scenario::Status {
                code: 429,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        for (name, created) in [
            ("p1", "2026-01-01T00:00:01.000Z"),
            ("p2", "2026-01-01T00:00:02.000Z"),
            ("p3", "2026-01-01T00:00:03.000Z"),
        ] {
            let (provider_id, _) =
                provider_with_credential(&s.app, &s.uid, name, &stub.base_url, None).await;
            offering(
                &s.app,
                &s.uid,
                &provider_id,
                &model_id,
                &format!("{name}-model"),
                created,
                None,
            )
            .await;
        }

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.body["error"]["code"], "upstream_rate_limited");
        assert!(
            response.headers.get(header::RETRY_AFTER).is_some(),
            "the cooldown is passed on as Retry-After"
        );
        assert_eq!(
            stub.request_count(),
            2,
            "429 is retried once on the next candidate, then given up"
        );
        let decision = decision_of(&log_rows(&s.app).await[0]);
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(decision["candidates"][1]["outcome"], "failed");
        assert_eq!(decision["candidates"][2]["outcome"], "not_tried");
    }

    // -----------------------------------------------------------------------
    // Validation and routing errors
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn context_length_exceeded_never_calls_the_upstream() {
        let stub = Stub::spawn(vec![]).await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", Some(64), Some(200)).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "max_tokens": 100,
        });
        let response = send(s.app.router(), chat_request(&s.cookie, body)).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(response.body["error"]["code"], "context_length_exceeded");
        assert_eq!(stub.request_count(), 0, "checked before any upstream call");

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("context_length_exceeded"));
        assert_eq!(row.status, Some(400));
        assert!(row.input_tokens.is_none(), "no attempt, no token claim");
        assert_eq!(row.tokens_estimated, 0);
        assert_eq!(
            row.request_id,
            response.body["error"]["request_id"]
                .as_str()
                .expect("request_id"),
            "the error body and the rows share the request id"
        );

        let log = &log_rows(&s.app).await[0];
        let decision = decision_of(log);
        assert_eq!(decision["candidates"][0]["outcome"], "skipped_context");
        assert!(
            decision["reason"]
                .as_str()
                .expect("reason")
                .contains("skipped_context")
        );
    }

    #[tokio::test]
    async fn unknown_models_get_model_not_found() {
        let stub = Stub::spawn(vec![]).await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("nope", true)),
        )
        .await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert_eq!(response.body["error"]["code"], "model_not_found");
        assert_eq!(stub.request_count(), 0);

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("model_not_found"));
        assert!(row.input_tokens.is_none());
        let log = &log_rows(&s.app).await[0];
        assert_eq!(log.requested_model.as_deref(), Some("nope"));
        assert!(
            decision_of(log)["candidates"]
                .as_array()
                .expect("candidates")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_model_disabled_for_all_providers_cannot_route_through_any_offering() {
        let stub = Stub::spawn(vec![]).await;
        let s = seeded().await;
        let (first_provider, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (second_provider, _) =
            provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &first_provider,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        // Disable the model itself (D61), then add another provider's offering
        // after the switch was flipped: it must inherit the disabled state.
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        catalog::update_model(
            &s.app.state.pool,
            &actor,
            &model_id,
            &ModelPatch {
                enabled: Some(false),
                ..ModelPatch::default()
            },
        )
        .await
        .expect("disable model");
        offering(
            &s.app,
            &s.uid,
            &second_provider,
            &model_id,
            "test-model-copy",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert_eq!(response.body["error"]["code"], "model_not_found");
        assert_eq!(
            stub.request_count(),
            0,
            "a disabled model never reaches an upstream"
        );

        // Enabling the model restores both offerings without touching their
        // own flags.
        catalog::update_model(
            &s.app.state.pool,
            &actor,
            &model_id,
            &ModelPatch {
                enabled: Some(true),
                ..ModelPatch::default()
            },
        )
        .await
        .expect("enable model");
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT id FROM offerings WHERE enabled = 1 ORDER BY id")
                .fetch_all(&s.app.state.pool)
                .await
                .expect("offerings");
        assert_eq!(rows.len(), 2, "the offering flags were never rewritten");
    }

    #[tokio::test]
    async fn validation_failures_are_invalid_request_and_still_logged() {
        let stub = Stub::spawn(vec![]).await;
        let s = seeded().await;
        let router = s.app.router();

        let no_model = send(
            router.clone(),
            chat_request(&s.cookie, json!({"messages": [{"role": "user"}]})),
        )
        .await;
        assert_eq!(no_model.status, StatusCode::BAD_REQUEST);

        let bad_max = send(
            router.clone(),
            chat_request(
                &s.cookie,
                json!({"model": "m", "messages": [{"role": "user"}], "max_tokens": -1}),
            ),
        )
        .await;
        assert_eq!(bad_max.status, StatusCode::BAD_REQUEST);

        // A raw non-JSON body is still a validation failure.
        let raw = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/v1/chat/completions")
            .header(axum::http::header::COOKIE, s.cookie.clone())
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from("not json"))
            .expect("request");
        let not_json = send(router, raw).await;
        assert_eq!(not_json.status, StatusCode::BAD_REQUEST);

        assert_eq!(stub.request_count(), 0);
        let rows = usage_rows(&s.app).await;
        assert_eq!(rows.len(), 3, "every completion is accounted for");
        for row in &rows {
            assert_eq!(row.error_class.as_deref(), Some("invalid_request"));
            assert_eq!(row.status, Some(400));
        }
    }

    #[tokio::test]
    async fn v1_chat_completions_is_csrf_exempt() {
        // The session cookie is present, the X-CSRF-Token header is not: the
        // request must be routed (404 here), never rejected as csrf_failed.
        let stub = Stub::spawn(vec![]).await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("nope", true)),
        )
        .await;
        assert_ne!(response.status, StatusCode::FORBIDDEN);
        assert_eq!(response.body["error"]["code"], "model_not_found");
    }

    #[tokio::test]
    async fn oversized_bodies_are_rejected_and_logged() {
        let stub = Stub::spawn(vec![]).await;
        let s = seeded_with(|config| {
            config.max_body_bytes = 1024;
        })
        .await;

        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "x".repeat(4096)}],
            "stream": true,
        });
        let real_len = body.to_string().len() as i64;
        assert!(real_len > 1024);
        let response = send(s.app.router(), chat_request(&s.cookie, body)).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(response.body["error"]["code"], "invalid_request");
        assert!(
            response.body["error"]["message"]
                .as_str()
                .expect("message")
                .contains("exceeds")
        );
        assert_eq!(stub.request_count(), 0);

        let rows = usage_rows(&s.app).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].error_class.as_deref(), Some("invalid_request"));
        let log = &log_rows(&s.app).await[0];
        assert_eq!(
            log.bytes_in,
            Some(real_len),
            "bytes_in is the body size actually received, not the limit"
        );
        assert_eq!(log.has_request_body, 1);
    }

    // -----------------------------------------------------------------------
    // Non-streaming
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_long_conversation_is_forwarded_instead_of_capped() {
        // D62: there is no message-count limit, so a 1000-message history must
        // reach the upstream instead of being refused with a local 400.
        let upstream_body = json!({
            "id": "c1",
            "object": "chat.completion",
            "model": "test-model",
            "choices": [{"message": {"role": "assistant", "content": "hi"}}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 8},
        });
        let stub = Stub::spawn(vec![Scenario::Json {
            body: upstream_body.to_string(),
        }])
        .await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let history = (0..1000)
            .map(|i| json!({"role": "user", "content": format!("m{i}")}))
            .collect::<Vec<_>>();
        let response = send(
            s.app.router(),
            chat_request(
                &s.cookie,
                json!({"model": "m", "messages": history, "max_tokens": 42}),
            ),
        )
        .await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(stub.request_count(), 1, "the request reached the provider");
        assert_eq!(
            stub.requests()[0].body["messages"]
                .as_array()
                .expect("messages")
                .len(),
            1000
        );
    }

    #[tokio::test]
    async fn non_streaming_returns_the_json_body_and_settles() {
        let upstream_body = json!({
            "id": "cmpl-1",
            "object": "chat.completion",
            "model": "test-model",
            "choices": [{"message": {"role": "assistant", "content": "hi"}}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 8},
        });
        let stub = Stub::spawn(vec![Scenario::Json {
            body: upstream_body.to_string(),
        }])
        .await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, upstream_body, "passed through verbatim");
        assert_eq!(response.headers[header::CONTENT_TYPE], "application/json");

        let request = &stub.requests()[0];
        assert_eq!(request.body["stream"], false);
        assert!(
            request.body.get("stream_options").is_none(),
            "stream_options is only injected for streams"
        );

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.input_tokens, Some(4));
        assert_eq!(row.output_tokens, Some(8));
        assert_eq!(row.tokens_estimated, 0);
        assert_eq!(row.status, Some(200));
        let log = &log_rows(&s.app).await[0];
        assert_eq!(log.bytes_out, Some(upstream_body.to_string().len() as i64));
    }

    #[tokio::test]
    async fn non_streaming_failures_use_the_error_taxonomy() {
        let stub = Stub::spawn(vec![Scenario::Status {
            code: 500,
            headers: Vec::new(),
            body: "{}".to_string(),
        }])
        .await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_GATEWAY);
        assert_eq!(response.body["error"]["code"], "upstream_error");
        assert!(response.body["error"]["request_id"].is_string());
        assert_eq!(
            stub.request_count(),
            1,
            "a single candidate failure is not retried"
        );

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("upstream_error"));
        assert_eq!(row.status, Some(502));
    }

    // -----------------------------------------------------------------------
    // Deep-review regressions (2026-11-16)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn duplicate_client_request_ids_settle_two_independent_ledger_pairs() {
        // Review B1: a client-chosen `x-request-id` used to be the ledger key;
        // a duplicate made the second settlement roll back — zero rows for a
        // full 200. Both requests must settle their own pair now.
        let stub = Stub::spawn(vec![
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 2)),
            },
            Scenario::Stream {
                events: vec![event(1)],
                usage: Some(usage_json(3, 4)),
            },
        ])
        .await;
        let s = seeded().await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let mut first = chat_request(&s.cookie, chat_body("m", true));
        first
            .headers_mut()
            .insert("x-request-id", "shared-echo".parse().expect("header"));
        let mut second = chat_request(&s.cookie, chat_body("m", true));
        second
            .headers_mut()
            .insert("x-request-id", "shared-echo".parse().expect("header"));

        let (status, first_headers, first_body) = send_streaming(s.app.router(), first).await;
        assert_eq!(status, StatusCode::OK);
        body_text(first_body).await;
        let (status, second_headers, second_body) = send_streaming(s.app.router(), second).await;
        assert_eq!(status, StatusCode::OK);
        body_text(second_body).await;

        let logs = log_rows(&s.app).await;
        assert_eq!(logs.len(), 2, "no rollback: both requests settle a log row");
        assert_ne!(
            logs[0].request_id, logs[1].request_id,
            "the ledger key is server-generated, never the client's echo"
        );
        for log in &logs {
            assert_eq!(
                log.client_request_id.as_deref(),
                Some("shared-echo"),
                "the echo is preserved for the ledger"
            );
        }
        let usage = usage_rows(&s.app).await;
        assert_eq!(usage.len(), 2, "exactly one usage row per request");
        assert_eq!(usage[0].request_id, logs[0].request_id);
        assert_eq!(usage[1].request_id, logs[1].request_id);

        // Clients see the server id (response header), never their echo.
        let server_id = |headers: &axum::http::HeaderMap| {
            headers
                .get("x-request-id")
                .map(|value| value.to_str().expect("ascii").to_string())
                .expect("response x-request-id")
        };
        assert_ne!(server_id(&first_headers), "shared-echo");
        assert_eq!(server_id(&first_headers), logs[0].request_id);
        assert_eq!(server_id(&second_headers), logs[1].request_id);
    }

    #[tokio::test]
    async fn a_connect_timeout_attempt_books_no_token_estimate() {
        // Review M3: pre-send failures (decrypt errors, connect timeouts)
        // must book NULL tokens, not a fabricated estimate. The stub is a
        // black-hole address, so the connect never completes.
        let s = seeded_with(|config| {
            config.timeout_first_byte = Duration::from_millis(300);
        })
        .await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", "http://10.255.255.1:81", None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let started = Instant::now();
        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must not hang: {:?}",
            started.elapsed()
        );
        assert_ne!(response.status, StatusCode::OK);

        let rows = usage_rows(&s.app).await;
        assert_eq!(rows.len(), 1, "the failure is still settled");
        assert!(rows[0].error_class.is_some());
        assert!(
            rows[0].input_tokens.is_none() && rows[0].output_tokens.is_none(),
            "nothing was sent upstream, so nothing may be estimated: {:?}",
            rows[0]
        );
        assert_eq!(rows[0].tokens_estimated, 0);
    }

    #[tokio::test]
    async fn a_stalled_reader_cannot_pin_the_runner_past_the_total_budget() {
        // Review m1: `tx.send` must race the total deadline. The test reads
        // one chunk and then stops consuming (without disconnecting): the
        // runner's channel fills and its sends block until the deadline ends
        // the request and settles the rows.
        let stub = Stub::spawn(vec![Scenario::Drip {
            interval: Duration::from_millis(10),
            chunk: format!("data: {}\n\n", event(0)),
        }])
        .await;
        let s = seeded_with(|config| {
            config.timeout_total = Duration::from_millis(400);
        })
        .await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let (_, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        let mut stream = response_body.into_data_stream();
        assert!(stream.next().await.is_some(), "first chunk arrives");
        // Stop reading here — the response body (and its cancel guard) stays
        // alive, so this is a stalled reader, not a disconnect.

        let started = Instant::now();
        let deadline = started + Duration::from_secs(2);
        loop {
            let rows = usage_rows(&s.app).await;
            if !rows.is_empty() {
                assert_eq!(rows[0].status, Some(200), "the stream was committed");
                assert_eq!(rows[0].error_class.as_deref(), Some("upstream_timeout"));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the rows must settle at the total deadline: {rows:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        drop(stream);
    }

    #[tokio::test]
    async fn an_oversized_stream_line_ends_the_stream_in_band_without_failover() {
        // Review M2: a single line over the 1 MiB cap is an in-band
        // `upstream_stream_failed` error after the first byte; never failover.
        let huge = "x".repeat(sse::MAX_LINE_BYTES + 1);
        let stub = Stub::spawn(vec![
            Scenario::Stream {
                events: vec![event(0), huge],
                usage: None,
            },
            Scenario::Stream {
                events: vec![event(1)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the first event committed the stream"
        );
        let text = body_text(response_body).await;
        assert!(text.contains("c0"), "{text}");
        assert!(
            text.contains("\"code\":\"upstream_stream_failed\""),
            "{text}"
        );
        assert!(text.contains("data: [DONE]"), "{text}");
        assert_eq!(
            stub.request_count(),
            1,
            "line-cap overflow never fails over"
        );

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("upstream_stream_failed"));
        assert_eq!(row.status, Some(200));
    }

    #[tokio::test]
    async fn malformed_first_chunk_fails_over_before_forwarding() {
        // Review m6: a protocol error in the very first chunk is still before
        // the first byte, so the next candidate is tried.
        let stub = Stub::spawn(vec![
            Scenario::Malformed { after: 0 },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = body_text(response_body).await;
        assert!(text.contains("c0"), "{text}");
        assert!(text.contains("data: [DONE]"), "{text}");
        assert_eq!(stub.request_count(), 2, "the first chunk failed over");

        let log = &log_rows(&s.app).await[0];
        let decision = decision_of(log);
        assert_eq!(decision["candidates"][0]["id"], off1);
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(
            decision["candidates"][0]["error_class"],
            "upstream_stream_failed"
        );
        assert_eq!(decision["candidates"][1]["outcome"], "chosen");
    }

    #[tokio::test]
    async fn empty_upstream_body_fails_over() {
        // Review m6: a 200 with an empty body is a stream failure before the
        // first byte, so the next candidate is tried.
        let stub = Stub::spawn(vec![
            Scenario::Json {
                body: String::new(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = body_text(response_body).await;
        assert!(text.contains("c0"), "{text}");
        assert_eq!(
            stub.request_count(),
            2,
            "an empty body fails over before the first byte"
        );
        let decision = decision_of(&log_rows(&s.app).await[0]);
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(decision["candidates"][1]["outcome"], "chosen");
    }

    #[tokio::test]
    async fn the_total_budget_spans_both_attempts() {
        // Review m6: the total deadline is absolute across failover attempts.
        // Attempt 1 burns 250ms and fails; attempt 2 must hit the shared
        // deadline at ~400ms, not restart a fresh first-byte budget.
        let stub = Stub::spawn(vec![
            Scenario::SlowStart {
                delay: Duration::from_millis(250),
                then: Box::new(Scenario::Status {
                    code: 500,
                    headers: Vec::new(),
                    body: "{}".to_string(),
                }),
            },
            Scenario::SlowStart {
                delay: Duration::from_secs(10),
                then: Box::new(Scenario::Stream {
                    events: vec![],
                    usage: None,
                }),
            },
        ])
        .await;
        let s = seeded_with(|config| {
            config.timeout_total = Duration::from_millis(400);
            config.timeout_first_byte = Duration::from_secs(30);
        })
        .await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let started = Instant::now();
        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(350) && elapsed < Duration::from_secs(2),
            "the shared deadline bounds both attempts: {elapsed:?}"
        );
        assert_eq!(response.status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(response.body["error"]["code"], "upstream_timeout");
        assert_eq!(
            stub.request_count(),
            2,
            "attempt 2 was made, then timed out"
        );
    }

    #[tokio::test]
    async fn buffered_cap_overflow_is_terminal_and_exact() {
        // Review m5/m6: one over-cap buffered response must not fail over (the
        // next candidate could buffer 32 MiB again) and the buffer must not
        // overshoot the cap by a chunk.
        let oversized = json!({"data": "x".repeat(MAX_BUFFERED_RESPONSE + 1)}).to_string();
        assert!(oversized.len() > MAX_BUFFERED_RESPONSE);
        let stub = Stub::spawn(vec![
            Scenario::Json { body: oversized },
            Scenario::Json {
                body: "{}".to_string(),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_GATEWAY);
        assert_eq!(response.body["error"]["code"], "upstream_stream_failed");
        assert_eq!(
            stub.request_count(),
            1,
            "the cap is terminal: no second 32 MiB buffer on another candidate"
        );
        let decision = decision_of(&log_rows(&s.app).await[0]);
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(
            decision["candidates"][0]["error_class"],
            "upstream_stream_failed"
        );
        assert_eq!(decision["candidates"][1]["outcome"], "not_tried");
    }

    // -----------------------------------------------------------------------
    // Pure pipeline units
    // -----------------------------------------------------------------------

    #[test]
    fn parse_request_enforces_the_documented_limits() {
        let ok = parse_request(
            json!({"model": "m", "messages": [{"role": "user"}], "max_tokens": 5})
                .to_string()
                .as_bytes(),
        )
        .expect("valid");
        assert_eq!(ok.model, "m");
        assert_eq!(ok.messages.len(), 1);
        assert!(!ok.stream);
        assert_eq!(ok.max_tokens, Some(5));

        // D62: message count is not capped, so a long but small conversation
        // reaches the upstream instead of being refused locally.
        let long_history = (0..1000)
            .map(|i| json!({"role": "user", "content": format!("m{i}")}))
            .collect::<Vec<_>>();
        let parsed = parse_request(
            json!({"model": "m", "messages": long_history})
                .to_string()
                .as_bytes(),
        )
        .expect("a 1000-message conversation is accepted");
        assert_eq!(parsed.messages.len(), 1000);

        for (body, why) in [
            (json!({"messages": [{"role": "user"}]}), "missing model"),
            (
                json!({"model": "", "messages": [{"role": "user"}]}),
                "blank model",
            ),
            (json!({"model": "m"}), "missing messages"),
            (json!({"model": "m", "messages": []}), "empty messages"),
            (
                json!({"model": "m", "messages": ["not an object"]}),
                "non-object message",
            ),
            (
                json!({"model": "m", "messages": [{"role": "user"}], "max_tokens": 0}),
                "zero max_tokens",
            ),
            (
                json!({"model": "m", "messages": [{"role": "user"}], "stream": "yes"}),
                "non-bool stream",
            ),
        ] {
            assert!(
                parse_request(body.to_string().as_bytes()).is_err(),
                "{why} must be rejected"
            );
        }
        assert!(parse_request(b"not json").is_err());
    }

    #[test]
    fn failover_rules_match_the_spec() {
        let retryable = UpstreamError::Http {
            status: 500,
            retry_after: None,
            detail: None,
        }
        .classify();
        let rate_limited = UpstreamError::Http {
            status: 429,
            retry_after: None,
            detail: None,
        }
        .classify();
        let auth = UpstreamError::Http {
            status: 401,
            retry_after: None,
            detail: None,
        }
        .classify();

        let mut budget = 1;
        assert!(can_fail_over(
            &retryable,
            &failure_from_class(&retryable),
            &mut budget,
            false
        ));
        assert_eq!(budget, 1, "transient failures do not spend the 429 budget");
        assert!(can_fail_over(
            &rate_limited,
            &failure_from_class(&rate_limited),
            &mut budget,
            false
        ));
        assert_eq!(budget, 0);
        assert!(
            !can_fail_over(
                &rate_limited,
                &failure_from_class(&rate_limited),
                &mut budget,
                false
            ),
            "429 fails over at most once"
        );
        assert!(
            can_fail_over(&auth, &failure_from_class(&auth), &mut budget, false),
            "an auth failure advances to the next credential/offering"
        );
        // The combo flag is no longer needed for auth; it remains part of the
        // signature and the explicit member chain still advances.
        let mut budget = 1;
        assert!(can_fail_over(
            &auth,
            &failure_from_class(&auth),
            &mut budget,
            true
        ));
        assert!(credential_attributable(&auth));
        assert!(credential_attributable(&rate_limited));
        assert!(!credential_attributable(&retryable));
    }

    #[test]
    fn in_band_error_is_prefixed_with_a_blank_line() {
        // Review m2: the error frame must never concatenate onto a partial
        // upstream SSE line.
        let bytes = in_band_error_bytes(
            &ChatFailure::new(
                StatusCode::BAD_GATEWAY,
                "upstream_stream_failed",
                "the upstream response stream failed",
            ),
            "req-1",
        );
        let text = String::from_utf8(bytes.to_vec()).expect("utf8");
        assert!(text.starts_with("\n\ndata: {\"error\""), "{text}");
        assert!(text.contains("\"request_id\":\"req-1\""), "{text}");
        assert!(text.ends_with("data: [DONE]\n\n"), "{text}");
    }

    #[test]
    fn estimation_applies_only_without_upstream_usage_and_only_after_send() {
        let meta = RequestMeta {
            request_id: "req-1".to_string(),
            client_request_id: Some("echo-1".to_string()),
            user_id: "user-1".to_string(),
            role: crate::auth::Role::Admin,
            api_key: None,
            session_hint: None,
            started_at: "2026-11-16T10:00:00.000Z".to_string(),
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
        };
        let base = Settlement::rejected(&meta, Instant::now(), 400, &ChatFailure::invalid("x"));

        // No attempt: tokens stay NULL, nothing is estimated.
        let (usage, estimated) = resolve_usage(&base);
        assert!(usage.is_none());
        assert!(!estimated);

        // A request that never reached upstream (decrypt failure, connect
        // timeout, pre-send rejection) books NULL even when a candidate is
        // attributed to the failure — never a zero-row rollback and never a
        // fabricated estimate (review M3).
        let pre_send = Settlement {
            offering_id: Some("off-1".to_string()),
            input_chars: 404,
            output_chars: 400,
            sent: false,
            ..base
        };
        let (usage, estimated) = resolve_usage(&pre_send);
        assert!(usage.is_none(), "pre-send failures book no estimate");
        assert!(!estimated);

        // A request that was actually sent but got no usage: chars/4 from
        // content characters only, flagged (review M1).
        let sent = Settlement {
            sent: true,
            ..pre_send
        };
        let (usage, estimated) = resolve_usage(&sent);
        let usage = usage.expect("estimated");
        assert!(estimated);
        assert_eq!(usage.input_tokens, Some(101), "message-text chars / 4");
        assert_eq!(usage.output_tokens, Some(100), "content chars / 4");

        // Upstream usage is authoritative and never estimated.
        let exact = Settlement {
            usage: Some(Usage {
                input_tokens: Some(7),
                output_tokens: Some(8),
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            }),
            ..sent
        };
        let (usage, estimated) = resolve_usage(&exact);
        assert!(!estimated);
        assert_eq!(usage.expect("usage").input_tokens, Some(7));
    }

    // -----------------------------------------------------------------------
    // M4: money, keys, and access on the data plane
    // -----------------------------------------------------------------------

    /// A non-streaming completion carrying `usage`, for the money tests.
    fn completion(usage: serde_json::Value) -> Scenario {
        Scenario::Json {
            body: json!({
                "id": "cmpl-1",
                "object": "chat.completion",
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop",
                }],
                "usage": usage,
            })
            .to_string(),
        }
    }

    /// One priced, budget-capped scenario: `used + open + estimate > limit`
    /// rejects the second request with `402 budget_exceeded` and the report
    /// shows the spend of the first.
    #[tokio::test]
    async fn budget_exhaustion_returns_402_and_the_report_shows_the_spend() {
        let stub = Stub::spawn(vec![completion(usage_json(11, 7))]).await;
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        let offering_id = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        catalog::set_manual_price(
            &s.app.state.pool,
            &actor,
            &offering_id,
            &PriceInput {
                input_per_m: Some(2.5),
                output_per_m: Some(10.0),
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                request_fee: None,
                currency: None,
            },
        )
        .await
        .expect("price");
        // The reservation estimate of one request is 42 output tokens at
        // 10/M = 0.00042 USD; the actual settles at 11*2.5/M + 7*10/M.
        crate::money::set_budget(
            &s.app.state.pool,
            &actor,
            s.app.state.config.tz,
            &s.uid,
            crate::money::Period::Total,
            0.0005,
        )
        .await
        .expect("budget");

        let body = chat_body("m", false);
        let first = send(s.app.router(), chat_request(&s.cookie, body.clone())).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);

        let second = send(s.app.router(), chat_request(&s.cookie, body)).await;
        assert_eq!(
            second.status,
            StatusCode::PAYMENT_REQUIRED,
            "{}",
            second.body
        );
        assert_eq!(second.body["error"]["code"], "budget_exceeded");

        // The ledger books the actual cost of the successful request only.
        let usage = usage_rows(&s.app).await;
        assert_eq!(usage.len(), 2);
        let actual = 11.0 * 2.5 / 1_000_000.0 + 7.0 * 10.0 / 1_000_000.0;
        assert!((usage[0].cost_usd.expect("priced cost") - actual).abs() < 1e-12);
        assert_eq!(usage[1].cost_usd, Some(0.0), "a 402 consumes nothing");
        assert_eq!(usage[1].status, Some(402));

        let (used,): (f64,) = sqlx::query_as("SELECT used_usd FROM budgets WHERE user_id = ?")
            .bind(&s.uid)
            .fetch_one(&s.app.state.pool)
            .await
            .expect("budget row");
        assert!(
            (used - actual).abs() < 1e-12,
            "used_usd books the actual: {used}"
        );

        // And the report shows the spend (acceptance curl sequence).
        let report = send(
            s.app.router(),
            api_request(Method::GET, "/api/usage/me", None, Some(&s.cookie), None),
        )
        .await;
        assert_eq!(report.status, StatusCode::OK, "{}", report.body);
        assert_eq!(report.body["totals"]["requests"], 2);
        assert!((report.body["totals"]["cost_usd"].as_f64().expect("cost") - actual).abs() < 1e-12);
    }

    /// Per-model token caps stop the second request with `429
    /// token_cap_exceeded`.
    #[tokio::test]
    async fn token_cap_exhaustion_returns_429() {
        let stub = Stub::spawn(vec![completion(usage_json(11, 7))]).await;
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        // One request reserves 42 tokens and settles 18; the cap fits one
        // request but not two.
        crate::money::set_token_cap(
            &s.app.state.pool,
            &actor,
            s.app.state.config.tz,
            &s.uid,
            &model_id,
            crate::money::Period::Total,
            50,
        )
        .await
        .expect("cap");

        let body = chat_body("m", false);
        let first = send(s.app.router(), chat_request(&s.cookie, body.clone())).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);

        let second = send(s.app.router(), chat_request(&s.cookie, body)).await;
        assert_eq!(
            second.status,
            StatusCode::TOO_MANY_REQUESTS,
            "{}",
            second.body
        );
        assert_eq!(second.body["error"]["code"], "token_cap_exceeded");

        let (used,): (i64,) =
            sqlx::query_as("SELECT used_tokens FROM token_caps WHERE user_id = ?")
                .bind(&s.uid)
                .fetch_one(&s.app.state.pool)
                .await
                .expect("cap row");
        assert_eq!(used, 18);
    }

    /// A request that never reached the upstream releases its reservation and
    /// books nothing: no permanent lockout.
    #[tokio::test]
    async fn failed_requests_release_the_reservation() {
        let stub = Stub::spawn(vec![Scenario::Status {
            code: 401,
            headers: vec![],
            body: "{}".to_string(),
        }])
        .await;
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        // The budget fits exactly one reservation: a leaked one would lock
        // the account out for good.
        crate::money::set_budget(
            &s.app.state.pool,
            &actor,
            s.app.state.config.tz,
            &s.uid,
            crate::money::Period::Total,
            0.001,
        )
        .await
        .expect("budget");

        let failed = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(failed.status, StatusCode::BAD_GATEWAY, "{}", failed.body);

        let (status,): (String,) = sqlx::query_as("SELECT status FROM reservations")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("reservation row");
        assert_eq!(
            status, "released",
            "nothing was consumed: the estimate frees"
        );
        let (used,): (f64,) = sqlx::query_as("SELECT used_usd FROM budgets WHERE user_id = ?")
            .bind(&s.uid)
            .fetch_one(&s.app.state.pool)
            .await
            .expect("budget row");
        assert_eq!(used, 0.0);

        // The headroom is available for the next request.
        let retry = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(
            retry.status,
            StatusCode::BAD_GATEWAY,
            "still failing upstream"
        );
        let open: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM reservations WHERE status = 'open'")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("count");
        assert_eq!(open, 0, "no reservation outlives its request");
    }

    /// An API key authenticates `/v1`, is booked into the ledger, and its
    /// per-key RPM/TPM buckets answer `429` with `Retry-After`.
    #[tokio::test]
    async fn api_key_requests_book_the_key_and_rate_limits_trip() {
        let stub = Stub::spawn(vec![
            completion(usage_json(11, 7)),
            completion(usage_json(11, 7)),
        ])
        .await;
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let owner = s
            .app
            .create_user("owner@example.test", Role::User, true)
            .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        // A one-request-per-minute key for its owner, with the user and key
        // both granted the model.
        let created = crate::api_keys::create_key(
            &s.app.state.pool,
            &actor,
            &owner,
            &crate::api_keys::ApiKeyInput {
                name: "sdk".to_string(),
                expires_at: None,
                scopes: None,
                rate_limit_rpm: Some(1),
                rate_limit_tpm: Some(100_000),
            },
        )
        .await
        .expect("key");
        for subject_type in ["user", "api_key"] {
            let subject_id = match subject_type {
                "user" => owner.clone(),
                _ => created.view.id.clone(),
            };
            crate::access::create_grant(
                &s.app.state.pool,
                &actor,
                &crate::access::GrantInput {
                    subject_type: subject_type.to_string(),
                    subject_id,
                    resource_type: "model".to_string(),
                    resource_id: model_id.clone(),
                    mode: "allow".to_string(),
                },
            )
            .await
            .expect("grant");
        }

        let bearer = |key: &str, body: serde_json::Value| {
            axum::http::Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(body.to_string()))
                .expect("request")
        };

        let first = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);

        // The ledger knows which key spent the tokens.
        let (api_key_id,): (Option<String>,) =
            sqlx::query_as("SELECT api_key_id FROM usage_events ORDER BY id LIMIT 1")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("usage row");
        assert_eq!(api_key_id.as_deref(), Some(created.view.id.as_str()));
        let last_used: Option<String> =
            sqlx::query_scalar("SELECT last_used_at FROM api_keys WHERE id = ?")
                .bind(&created.view.id)
                .fetch_one(&s.app.state.pool)
                .await
                .expect("key row");
        assert!(last_used.is_some(), "last_used_at is stamped on use");

        // The second request within the minute trips the RPM bucket.
        let second = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(
            second.status,
            StatusCode::TOO_MANY_REQUESTS,
            "{}",
            second.body
        );
        assert_eq!(second.body["error"]["code"], "rate_limited");
        assert!(
            second.headers.get(header::RETRY_AFTER).is_some(),
            "the 429 carries Retry-After"
        );

        // A TPM limit smaller than one request's estimate refuses outright.
        let chatty = crate::api_keys::create_key(
            &s.app.state.pool,
            &actor,
            &owner,
            &crate::api_keys::ApiKeyInput {
                name: "chatty".to_string(),
                expires_at: None,
                scopes: None,
                rate_limit_rpm: Some(100),
                rate_limit_tpm: Some(5),
            },
        )
        .await
        .expect("key");
        crate::access::create_grant(
            &s.app.state.pool,
            &actor,
            &crate::access::GrantInput {
                subject_type: "api_key".to_string(),
                subject_id: chatty.view.id.clone(),
                resource_type: "model".to_string(),
                resource_id: model_id.clone(),
                mode: "allow".to_string(),
            },
        )
        .await
        .expect("grant");
        let over = send(s.app.router(), bearer(&chatty.key, chat_body("m", false))).await;
        assert_eq!(over.status, StatusCode::TOO_MANY_REQUESTS, "{}", over.body);
        assert_eq!(over.body["error"]["code"], "rate_limited");
    }

    /// The grant matrix on the data plane: default deny for users, a key
    /// without grants inheriting its owner (D59), explicit key scopes
    /// intersecting, and deny beating allow.
    #[tokio::test]
    async fn grants_gate_the_data_plane_with_deny_precedence() {
        let stub = Stub::spawn(vec![
            completion(usage_json(11, 7)),
            completion(usage_json(11, 7)),
        ])
        .await;
        let s = seeded().await;
        let (provider_id, credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let owner = s
            .app
            .create_user("owner@example.test", Role::User, true)
            .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let created = crate::api_keys::create_key(
            &s.app.state.pool,
            &actor,
            &owner,
            &crate::api_keys::ApiKeyInput {
                name: "sdk".to_string(),
                expires_at: None,
                scopes: None,
                rate_limit_rpm: None,
                rate_limit_tpm: None,
            },
        )
        .await
        .expect("key");

        let bearer = |key: &str, body: serde_json::Value| {
            axum::http::Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(body.to_string()))
                .expect("request")
        };
        let listing = |key: &str| {
            axum::http::Request::builder()
                .method(Method::GET)
                .uri("/v1/models")
                .header(axum::http::header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::empty())
                .expect("request")
        };

        // No grants at all: denied, and the listing is empty.
        let denied = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.body);
        assert_eq!(denied.body["error"]["code"], "access_denied");
        let empty = send(s.app.router(), listing(&created.key)).await;
        assert_eq!(empty.status, StatusCode::OK);
        assert_eq!(empty.body["data"].as_array().expect("data").len(), 0);

        // Granting the account is enough: a key with no grants of its own
        // inherits its owner's access (D59).
        crate::access::create_grant(
            &s.app.state.pool,
            &actor,
            &crate::access::GrantInput {
                subject_type: "user".to_string(),
                subject_id: owner.clone(),
                resource_type: "model".to_string(),
                resource_id: model_id.clone(),
                mode: "allow".to_string(),
            },
        )
        .await
        .expect("user grant");
        let inherited = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(inherited.status, StatusCode::OK, "{}", inherited.body);
        let visible = send(s.app.router(), listing(&created.key)).await;
        assert_eq!(visible.body["data"].as_array().expect("data").len(), 1);

        // An explicit key grant turns the key into a scope; owner and key both
        // allowing still passes.
        crate::access::create_grant(
            &s.app.state.pool,
            &actor,
            &crate::access::GrantInput {
                subject_type: "api_key".to_string(),
                subject_id: created.view.id.clone(),
                resource_type: "model".to_string(),
                resource_id: model_id.clone(),
                mode: "allow".to_string(),
            },
        )
        .await
        .expect("key grant");
        let allowed = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.body);

        // A deny on the credential beats the model allow.
        crate::access::create_grant(
            &s.app.state.pool,
            &actor,
            &crate::access::GrantInput {
                subject_type: "api_key".to_string(),
                subject_id: created.view.id.clone(),
                resource_type: "credential".to_string(),
                resource_id: credential_id.clone(),
                mode: "deny".to_string(),
            },
        )
        .await
        .expect("deny grant");
        let blocked = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(blocked.status, StatusCode::FORBIDDEN, "{}", blocked.body);
        assert_eq!(blocked.body["error"]["code"], "access_denied");
    }

    /// The reservation is settled with the cost of the *chosen* offering and
    /// partial prices flag the row `priced = 0` instead of pretending 0.
    #[tokio::test]
    async fn settlement_costs_the_chosen_offering_and_flags_partial_prices() {
        let stub = Stub::spawn(vec![
            completion(usage_json(1_000_000, 1_000_000)),
            completion(usage_json(1_000_000, 1_000_000)),
        ])
        .await;
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        let offering_id = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        // Only the input price is known: output contributes 0 and the row is
        // flagged incomplete (`priced = 0`).
        catalog::set_manual_price(
            &s.app.state.pool,
            &actor,
            &offering_id,
            &PriceInput {
                input_per_m: Some(2.0),
                output_per_m: None,
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                request_fee: None,
                currency: None,
            },
        )
        .await
        .expect("price");

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);

        let (cost, priced): (Option<f64>, i64) =
            sqlx::query_as("SELECT cost_usd, priced FROM usage_events")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("usage row");
        assert_eq!(
            cost,
            Some(2.0),
            "1M input at 2/M; unpriced output contributes 0"
        );
        assert_eq!(priced, 0, "a used component without price is flagged");

        // With no price row at all the cost stays NULL: unpriced, never 0.
        let unpriced_app = seeded().await;
        let (provider_id, _credential_id) = provider_with_credential(
            &unpriced_app.app,
            &unpriced_app.uid,
            "p2",
            &stub.base_url,
            None,
        )
        .await;
        let model_id = model(&unpriced_app.app, &unpriced_app.uid, "m2", None, Some(128)).await;
        offering(
            &unpriced_app.app,
            &unpriced_app.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let response = send(
            unpriced_app.app.router(),
            chat_request(&unpriced_app.cookie, chat_body("m2", false)),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        let (cost, priced): (Option<f64>, i64) =
            sqlx::query_as("SELECT cost_usd, priced FROM usage_events")
                .fetch_one(&unpriced_app.app.state.pool)
                .await
                .expect("usage row");
        assert_eq!(cost, None, "unpriced rows stay NULL");
        assert_eq!(priced, 0);
    }

    /// A pre-send refusal settles a `refused = 1` row and the reports count
    /// it in `refused_requests`, never in `priced_requests` (M4 UI-alignment
    /// finding 1) — through `settle()` and through a full rollup rebuild.
    #[tokio::test]
    async fn refused_requests_get_their_own_counter_through_settle_and_rebuild() {
        /// The counter split one report must show, however it was derived.
        async fn assert_counters(pool: &sqlx::SqlitePool) {
            let (requests, refused, priced, unpriced, partial): (i64, i64, i64, i64, i64) =
                sqlx::query_as(
                    "SELECT SUM(requests), SUM(refused_requests), SUM(priced_requests), \
                            SUM(unpriced_requests), SUM(partial_requests) FROM usage_daily",
                )
                .fetch_one(pool)
                .await
                .expect("counters");
            assert_eq!(requests, 2, "requests counts every row");
            assert_eq!(refused, 1, "the refusal has its own counter");
            assert_eq!(priced, 1, "only the billable request was priced");
            assert_eq!(unpriced, 0);
            assert_eq!(partial, 0);
        }

        let stub = Stub::spawn(vec![completion(usage_json(11, 7))]).await;
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        let offering_id = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        catalog::set_manual_price(
            &s.app.state.pool,
            &actor,
            &offering_id,
            &PriceInput {
                input_per_m: Some(2.5),
                output_per_m: Some(10.0),
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                request_fee: None,
                currency: None,
            },
        )
        .await
        .expect("price");

        // One billable request...
        let ok = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);

        // ...and one refused before anything is sent: a non-admin account with
        // no grants may not use the only offering (403 access_denied). Its key
        // has no grants either, so it inherits the account's deny (D59).
        let denied_user = s
            .app
            .create_user("denied@example.test", Role::User, true)
            .await;
        let created = crate::api_keys::create_key(
            &s.app.state.pool,
            &actor,
            &denied_user,
            &crate::api_keys::ApiKeyInput {
                name: "denied".to_string(),
                expires_at: None,
                scopes: None,
                rate_limit_rpm: None,
                rate_limit_tpm: None,
            },
        )
        .await
        .expect("key");
        let bearer = |key: &str, body: serde_json::Value| {
            axum::http::Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(body.to_string()))
                .expect("request")
        };
        let refused = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
        assert_eq!(refused.body["error"]["code"], "access_denied");

        // The refusal's ledger row booked nothing: NULL tokens, cost 0.0,
        // and the `refused` marker the rollup rebuild derives from.
        let rows = usage_rows(&s.app).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].status, Some(403));
        assert!(rows[1].input_tokens.is_none() && rows[1].output_tokens.is_none());
        assert_eq!(rows[1].cost_usd, Some(0.0), "a refusal costs exactly 0");
        let refused_marker: i64 =
            sqlx::query_scalar("SELECT refused FROM usage_events WHERE status = 403")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("refused marker");
        assert_eq!(refused_marker, 1);

        // The incremental settlement books the split...
        assert_counters(&s.app.state.pool).await;

        // ...and the rebuild derives the same split from `usage_events`.
        crate::reports::rebuild_rollups(&s.app.state.pool, s.app.state.config.tz)
            .await
            .expect("rebuild");
        assert_counters(&s.app.state.pool).await;
    }

    /// A `402`-refused request must not burn its key's TPM quota (review
    /// m6): the budget/cap checks run before the rate-limit charge, so the
    /// next request keeps its full token budget.
    #[tokio::test]
    async fn budget_rejection_does_not_burn_tpm_quota() {
        let stub = Stub::spawn(vec![
            completion(usage_json(11, 7)),
            completion(usage_json(11, 7)),
        ])
        .await;
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        let offering_id = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        catalog::set_manual_price(
            &s.app.state.pool,
            &actor,
            &offering_id,
            &PriceInput {
                input_per_m: Some(2.5),
                output_per_m: Some(10.0),
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                request_fee: None,
                currency: None,
            },
        )
        .await
        .expect("price");
        let owner = s
            .app
            .create_user("owner@example.test", Role::User, true)
            .await;
        // 50 TPM fits exactly one 42-token request: if the 402'd request
        // burned quota, the retry below would answer `429 rate_limited`.
        let created = crate::api_keys::create_key(
            &s.app.state.pool,
            &actor,
            &owner,
            &crate::api_keys::ApiKeyInput {
                name: "sdk".to_string(),
                expires_at: None,
                scopes: None,
                rate_limit_rpm: Some(100),
                rate_limit_tpm: Some(50),
            },
        )
        .await
        .expect("key");
        for subject_type in ["user", "api_key"] {
            let subject_id = match subject_type {
                "user" => owner.clone(),
                _ => created.view.id.clone(),
            };
            crate::access::create_grant(
                &s.app.state.pool,
                &actor,
                &crate::access::GrantInput {
                    subject_type: subject_type.to_string(),
                    subject_id,
                    resource_type: "model".to_string(),
                    resource_id: model_id.clone(),
                    mode: "allow".to_string(),
                },
            )
            .await
            .expect("grant");
        }
        let bearer = |key: &str, body: serde_json::Value| {
            axum::http::Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(body.to_string()))
                .expect("request")
        };

        // One request reserves 0.00042 USD (42 output tokens at 10/M); this
        // budget fits none.
        crate::money::set_budget(
            &s.app.state.pool,
            &actor,
            s.app.state.config.tz,
            &owner,
            crate::money::Period::Total,
            0.0001,
        )
        .await
        .expect("budget");

        let refused = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(
            refused.status,
            StatusCode::PAYMENT_REQUIRED,
            "{}",
            refused.body
        );
        assert_eq!(refused.body["error"]["code"], "budget_exceeded");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reservations")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("reservations");
        assert_eq!(rows, 0, "the denial leaves no reservation behind");

        // With headroom again the next request must fit its 42-token TPM
        // charge — the 402'd request burned nothing.
        crate::money::set_budget(
            &s.app.state.pool,
            &actor,
            s.app.state.config.tz,
            &owner,
            crate::money::Period::Total,
            1.0,
        )
        .await
        .expect("budget");
        let retry = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(
            retry.status,
            StatusCode::OK,
            "the budget rejection must not burn TPM quota: {}",
            retry.body
        );

        // A TPM refusal (now charged only after the money gate) still opens
        // and then releases its reservation through the rejection settlement.
        let over = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(over.status, StatusCode::TOO_MANY_REQUESTS, "{}", over.body);
        assert_eq!(over.body["error"]["code"], "rate_limited");
        let (released, settled): (i64, i64) = sqlx::query_as(
            "SELECT SUM(status = 'released'), SUM(status = 'settled') FROM reservations",
        )
        .fetch_one(&s.app.state.pool)
        .await
        .expect("reservations");
        assert_eq!(released, 1, "the TPM refusal releases its reservation");
        assert_eq!(settled, 1, "the successful request settled its own");
    }

    /// Hostile upstream usage can never credit the ledger (review M2):
    /// negative counts book 0 and absurd `i64::MAX` counts debit without
    /// overflowing — `used_usd`/`used_tokens` stay non-negative and finite.
    #[tokio::test]
    async fn hostile_upstream_usage_never_credits_the_ledger() {
        let negative = json!({
            "prompt_tokens": -5000000,
            "completion_tokens": -1,
            "prompt_tokens_details": {"cached_tokens": -3, "cache_creation_tokens": -4},
            "completion_tokens_details": {"reasoning_tokens": -9},
        });
        let extreme = json!({
            "prompt_tokens": i64::MAX,
            "completion_tokens": i64::MAX,
            "completion_tokens_details": {"reasoning_tokens": i64::MAX},
        });
        let stub = Stub::spawn(vec![completion(negative), completion(extreme)]).await;
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        let offering_id = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        catalog::set_manual_price(
            &s.app.state.pool,
            &actor,
            &offering_id,
            &PriceInput {
                input_per_m: Some(2.5),
                output_per_m: Some(10.0),
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                request_fee: None,
                currency: None,
            },
        )
        .await
        .expect("price");
        crate::money::set_budget(
            &s.app.state.pool,
            &actor,
            s.app.state.config.tz,
            &s.uid,
            crate::money::Period::Total,
            1.0,
        )
        .await
        .expect("budget");
        crate::money::set_token_cap(
            &s.app.state.pool,
            &actor,
            s.app.state.config.tz,
            &s.uid,
            &model_id,
            crate::money::Period::Total,
            10_000,
        )
        .await
        .expect("cap");

        let first = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        // Negative counts clamp to 0: the row is priced at 0 and the budget
        // is not credited.
        let zeroed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM usage_events WHERE cost_usd = 0.0 AND input_tokens = 0",
        )
        .fetch_one(&s.app.state.pool)
        .await
        .expect("usage row");
        assert_eq!(zeroed, 1, "clamped counts book 0, never a credit");
        let used: f64 = sqlx::query_scalar("SELECT used_usd FROM budgets WHERE user_id = ?")
            .bind(&s.uid)
            .fetch_one(&s.app.state.pool)
            .await
            .expect("budget row");
        assert_eq!(used, 0.0, "negative usage cannot raise remaining budget");

        let second = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(second.status, StatusCode::OK, "{}", second.body);
        let (used, tokens_used): (f64, i64) =
            sqlx::query_as("SELECT used_usd, used_tokens FROM budgets JOIN token_caps USING (user_id) WHERE budgets.user_id = ?")
                .bind(&s.uid)
                .fetch_one(&s.app.state.pool)
                .await
                .expect("money rows");
        assert!(
            used > 0.0 && used.is_finite(),
            "absurd counts debit without overflow: {used}"
        );
        assert!(tokens_used >= 0, "saturating sums stay non-negative");
    }

    /// Period straddling end-to-end (review M1): a slow request that started
    /// at 23:59 settles after the tiny 00:00:01 request opened the new day —
    /// its cost lands in the 23:59 day bucket (budget *and* rollup), and the
    /// new day's bucket is unaffected by it.
    #[tokio::test]
    async fn period_straddling_requests_book_into_their_start_buckets() {
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", "https://up.example/v1", None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        let offering_id = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        catalog::set_manual_price(
            &s.app.state.pool,
            &actor,
            &offering_id,
            &PriceInput {
                input_per_m: Some(2.5),
                output_per_m: Some(10.0),
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                request_fee: None,
                currency: None,
            },
        )
        .await
        .expect("price");
        crate::money::set_budget(
            &s.app.state.pool,
            &actor,
            s.app.state.config.tz,
            &s.uid,
            crate::money::Period::Day,
            5.0,
        )
        .await
        .expect("budget");

        // Both requests reserved while their buckets were current.
        for request_id in ["slow", "tiny"] {
            crate::money::open_reservation(
                &s.app.state.pool,
                s.app.state.config.tz,
                &crate::money::ReservationInput {
                    request_id: request_id.to_string(),
                    user_id: s.uid.clone(),
                    api_key_id: None,
                    offering_id: offering_id.clone(),
                    model_id: model_id.clone(),
                    estimated_tokens: 10,
                    estimated_usd: 0.1,
                },
            )
            .await
            .expect("reserve");
        }

        let settlement = |request_id: &str, started_at: &str, input_tokens: i64| Settlement {
            meta: RequestMeta {
                request_id: request_id.to_string(),
                client_request_id: None,
                user_id: s.uid.clone(),
                role: Role::Admin,
                api_key: None,
                session_hint: None,
                started_at: started_at.to_string(),
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
            },
            requested_model: Some("m".to_string()),
            decision: Decision::default(),
            offering_id: Some(offering_id.clone()),
            model_id: Some(model_id.clone()),
            usage: Some(Usage {
                input_tokens: Some(input_tokens),
                output_tokens: Some(0),
                ..Usage::default()
            }),
            output_chars: 0,
            input_chars: 4,
            sent: true,
            status: 200,
            error_class: None,
            error_detail: None,
            started: Instant::now(),
            ttft_ms: None,
            bytes_out: 0,
            bytes_in: 0,
            has_request_body: true,
            prices: HashMap::new(),
        };

        // The tiny request settles first (at 00:00:02) and opens the new
        // day; the slow one (started 23:59) settles after midnight.
        settle(
            &s.app.state,
            &settlement("tiny", "2020-01-06T00:00:01.000Z", 40_000),
        )
        .await;
        settle(
            &s.app.state,
            &settlement("slow", "2020-01-05T23:59:00.000Z", 1_600_000),
        )
        .await;

        // The budget charges each cost to the bucket derived from started_at.
        let buckets: Vec<(String, f64)> = sqlx::query_as(
            "SELECT period_start, used_usd FROM budgets \
             WHERE user_id = ? AND period = 'day' ORDER BY period_start",
        )
        .bind(&s.uid)
        .fetch_all(&s.app.state.pool)
        .await
        .expect("bucket rows");
        let find = |bucket: &str| {
            buckets
                .iter()
                .find(|(start, _)| start == bucket)
                .map(|(_, used)| *used)
        };
        assert_eq!(
            find("2020-01-05T00:00:00.000Z"),
            Some(4.0),
            "the big cost is charged to the day of 23:59"
        );
        assert_eq!(
            find("2020-01-06T00:00:00.000Z"),
            Some(0.1),
            "the new day keeps only the tiny request"
        );

        // And the rollup day keys follow started_at, end to end through the
        // settlement's report write.
        let days: Vec<(String, f64)> =
            sqlx::query_as("SELECT day, cost_usd FROM usage_daily WHERE user_id = ? ORDER BY day")
                .bind(&s.uid)
                .fetch_all(&s.app.state.pool)
                .await
                .expect("rollup rows");
        assert_eq!(
            days,
            [
                ("2020-01-05".to_string(), 4.0),
                ("2020-01-06".to_string(), 0.1)
            ]
        );
    }

    /// Settlement reuses the price row resolved during candidate resolution
    /// instead of reading `prices` a second time. The cost is identical to a
    /// fresh read, and an offering missing from the carried set (a failover
    /// outside resolution) still falls back to the database read.
    #[tokio::test]
    async fn settlement_reuses_the_routing_price_row_and_falls_back() {
        let s = seeded().await;
        let (provider_id, _credential_id) =
            provider_with_credential(&s.app, &s.uid, "p1", "https://up.example/v1", None).await;
        let model_id = model(&s.app, &s.uid, "m", None, Some(128)).await;
        let offering_id = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        catalog::set_manual_price(
            &s.app.state.pool,
            &actor,
            &offering_id,
            &PriceInput {
                input_per_m: Some(1.0),
                output_per_m: Some(2.0),
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                request_fee: None,
                currency: None,
            },
        )
        .await
        .expect("price");
        let resolved = catalog::get_price(&s.app.state.pool, &offering_id)
            .await
            .expect("read price")
            .expect("priced");

        // An admin edit after routing must not change what this request is
        // charged: settlement uses the row it already resolved.
        catalog::set_manual_price(
            &s.app.state.pool,
            &actor,
            &offering_id,
            &PriceInput {
                input_per_m: Some(100.0),
                output_per_m: Some(200.0),
                cache_read_per_m: None,
                cache_write_per_m: None,
                reasoning_per_m: None,
                request_fee: None,
                currency: None,
            },
        )
        .await
        .expect("new price");

        let usage = Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            ..Usage::default()
        };
        let settlement =
            |request_id: &str, prices: HashMap<String, Option<catalog::PriceRow>>| Settlement {
                meta: RequestMeta {
                    request_id: request_id.to_string(),
                    client_request_id: None,
                    user_id: s.uid.clone(),
                    role: Role::Admin,
                    api_key: None,
                    session_hint: None,
                    started_at: "2026-01-01T00:00:01.000Z".to_string(),
                    method: "POST".to_string(),
                    path: "/v1/chat/completions".to_string(),
                },
                requested_model: Some("m".to_string()),
                decision: Decision::default(),
                offering_id: Some(offering_id.clone()),
                model_id: Some(model_id.clone()),
                usage: Some(usage.clone()),
                output_chars: 0,
                input_chars: 4,
                sent: true,
                status: 200,
                error_class: None,
                error_detail: None,
                started: Instant::now(),
                ttft_ms: None,
                bytes_out: 0,
                bytes_in: 0,
                has_request_body: true,
                prices,
            };

        let carried = HashMap::from([(offering_id.clone(), Some(resolved))]);
        settle(&s.app.state, &settlement("carried-price", carried)).await;
        let carried_cost: Option<f64> =
            sqlx::query_scalar("SELECT cost_usd FROM usage_events WHERE request_id = ?")
                .bind("carried-price")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("usage row");
        assert_eq!(
            carried_cost,
            Some(3.0),
            "1M input at $1/M + 1M output at $2/M from the routing row"
        );

        // No carried row: the database fallback reads the current price.
        settle(&s.app.state, &settlement("fallback-price", HashMap::new())).await;
        let fallback_cost: Option<f64> =
            sqlx::query_scalar("SELECT cost_usd FROM usage_events WHERE request_id = ?")
                .bind("fallback-price")
                .fetch_one(&s.app.state.pool)
                .await
                .expect("usage row");
        assert_eq!(
            fallback_cost,
            Some(300.0),
            "the fallback read sees the current $100/$200 row"
        );
    }

    /// The batched credential load must keep the original per-provider
    /// preference and id ordering: first not breaker-open, then not cooled,
    /// then id order.
    #[test]
    fn credential_pick_prefers_healthy_then_cooled_then_breaker_open() {
        use crate::breaker::{BreakerConfig, BreakerRegistry};

        let breaker = BreakerRegistry::new(BreakerConfig {
            threshold: 1,
            cooldown: Duration::from_secs(60),
        });
        let row = |id: &str, cooled: bool| CredentialRow {
            id: id.to_string(),
            provider_id: "provider".to_string(),
            secret_cipher: "cipher".to_string(),
            headers_json: None,
            cooldown_until: cooled.then(|| "2999-01-01T00:00:00Z".to_string()),
            balance_usd: None,
            balance_low_threshold: None,
            balance_gate: "warn".to_string(),
            balance_currency: None,
        };
        let now = "2026-01-01T00:00:00Z";

        // A cooled credential with a lower id sinks below a healthy one.
        let rows = vec![row("a", true), row("b", false)];
        assert_eq!(
            pick_credential(&rows, "offering", now, &breaker)
                .expect("credential")
                .id,
            "b"
        );
        assert!(pick_credential(&[], "offering", now, &breaker).is_none());

        // When all are cooled, the first in id order wins.
        let rows = vec![row("a", true), row("b", true)];
        let picked = pick_credential(&rows, "offering", now, &breaker).expect("credential");
        assert_eq!(picked.id, "a");
        assert!(picked.cooled);

        // An open breaker pair sinks below a closed one.
        breaker.record_failure("a", "offering", "upstream_error", None, Instant::now());
        let rows = vec![row("a", false), row("b", false)];
        assert_eq!(
            pick_credential(&rows, "offering", now, &breaker)
                .expect("credential")
                .id,
            "b"
        );
    }

    /// D42: `warn` is advisory even at a zero balance; `block` skips only a
    /// credential whose stored balance is at or below zero. An unavailable
    /// balance (NULL) is unknown and never blocks.
    #[test]
    fn balance_gate_warns_by_default_and_blocks_only_when_opted_in() {
        use crate::breaker::{BreakerConfig, BreakerRegistry};

        let breaker = BreakerRegistry::new(BreakerConfig {
            threshold: 1,
            cooldown: Duration::from_secs(60),
        });
        let row = |id: &str, balance: Option<f64>, gate: &str| CredentialRow {
            id: id.to_string(),
            provider_id: "provider".to_string(),
            secret_cipher: "cipher".to_string(),
            headers_json: None,
            cooldown_until: None,
            balance_usd: balance,
            balance_low_threshold: None,
            balance_gate: gate.to_string(),
            balance_currency: None,
        };
        let now = "2026-01-01T00:00:00Z";

        // Warn (the default) keeps an exhausted credential in rotation.
        let rows = vec![row("a", Some(0.0), "warn")];
        assert_eq!(
            pick_credential(&rows, "offering", now, &breaker)
                .expect("warn never blocks")
                .id,
            "a"
        );

        // Block skips the exhausted credential and falls through to a healthy
        // one, even when the blocked id sorts first.
        let rows = vec![row("a", Some(-0.5), "block"), row("b", Some(4.0), "warn")];
        assert_eq!(
            pick_credential(&rows, "offering", now, &breaker)
                .expect("the healthy credential serves")
                .id,
            "b"
        );

        // All credentials blocked at zero: nothing is left to attempt.
        let rows = vec![row("a", Some(0.0), "block"), row("b", Some(0.0), "block")];
        assert!(pick_credential(&rows, "offering", now, &breaker).is_none());

        // An unavailable balance stores NULL; `block` must not read that as
        // zero and refuse traffic (D42).
        let rows = vec![row("a", None, "block")];
        assert_eq!(
            pick_credential(&rows, "offering", now, &breaker)
                .expect("unknown balance is not zero")
                .id,
            "a"
        );

        // A non-USD balance is not comparable to USD zero and is ignored by
        // both the gate and the low-quota warning (D52).
        let mut euro = row("a", Some(0.0), "block");
        euro.balance_currency = Some("EUR".to_string());
        let rows = vec![euro];
        assert_eq!(
            pick_credential(&rows, "offering", now, &breaker)
                .expect("a non-USD balance is ignored")
                .id,
            "a"
        );
    }

    // -----------------------------------------------------------------------
    // M5: combo models and the circuit breaker
    // -----------------------------------------------------------------------
    /// Creates a combo and returns its id.
    async fn combo(
        s: &Seeded,
        name: &str,
        strategy: &str,
        weights: Option<serde_json::Value>,
        members: &[(&str, Option<f64>)],
    ) -> String {
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        crate::combos::create(
            &s.app.state.pool,
            &actor,
            &crate::combos::ComboInput {
                name: name.to_string(),
                strategy: strategy.to_string(),
                weights,
                members: members
                    .iter()
                    .map(|(offering_id, weight)| crate::combos::MemberInput {
                        offering_id: (*offering_id).to_string(),
                        weight: *weight,
                    })
                    .collect(),
                enabled: None,
                pool: None,
                min_quality: None,
                max_mix_price_per_m: None,
                mix_profile: None,
                stickiness: None,
            },
        )
        .await
        .expect("combo")
        .id
    }

    #[tokio::test]
    async fn combo_ordered_fails_over_to_the_next_member_on_a_401() {
        // The member list is the operator's explicit fallback chain (M5):
        // a 401 on member 1 advances to member 2 — unlike a plain model,
        // where auth failures are never retried.
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 401,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(3, 4)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, cred1) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let off2 = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        combo(
            &s,
            "test",
            "ordered",
            None,
            &[(off1.as_str(), None), (off2.as_str(), None)],
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("combo:test", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = body_text(response_body).await;
        assert!(text.contains("c0"), "{text}");
        assert_eq!(
            stub.request_count(),
            2,
            "the fallback chain advances past the 401"
        );

        let log = &log_rows(&s.app).await[0];
        assert_eq!(log.requested_model.as_deref(), Some("combo:test"));
        assert_eq!(log.resolved_offering_id.as_deref(), Some(off2.as_str()));
        let decision = decision_of(log);
        assert_eq!(decision["routing"]["strategy"], "ordered");
        assert_eq!(decision["routing"]["combo"], "test");
        assert_eq!(decision["candidates"][0]["id"], off1);
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(decision["candidates"][0]["error_class"], "upstream_auth");
        assert_eq!(decision["candidates"][1]["outcome"], "chosen");
        assert_eq!(decision["chosen_offering_id"], off2);
        let explanation = decision["candidates"][0]["explanation"]
            .as_str()
            .expect("explanation");
        assert!(explanation.contains("upstream_auth"), "{explanation}");
        // The broken credential is cooled down (M3) and one failure counts
        // toward the pair's breaker (M5).
        assert!(cooldown_of(&s.app, &cred1).await.is_some());
    }

    #[tokio::test]
    async fn combo_round_robin_alternates_between_members_across_requests() {
        let stub = Stub::spawn(vec![
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
            Scenario::Stream {
                events: vec![event(1)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "rr-a",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let off2 = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "rr-b",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        combo(
            &s,
            "rr",
            "round_robin",
            None,
            &[(off1.as_str(), None), (off2.as_str(), None)],
        )
        .await;

        for _ in 0..2 {
            let (status, _, response_body) = send_streaming(
                s.app.router(),
                chat_request(&s.cookie, chat_body("combo:rr", true)),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            body_text(response_body).await;
        }

        let requests = stub.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body["model"], "rr-a");
        assert_eq!(
            requests[1].body["model"], "rr-b",
            "the rotation is stable and persisted across requests"
        );
        let logs = log_rows(&s.app).await;
        assert_eq!(decision_of(&logs[0])["routing"]["rr_cursor"], 1);
        assert_eq!(decision_of(&logs[1])["routing"]["rr_cursor"], 2);
        assert_eq!(decision_of(&logs[0])["routing"]["strategy"], "round_robin");
    }

    #[tokio::test]
    async fn unknown_and_disabled_combos_are_model_not_found() {
        let stub = Stub::spawn(vec![]).await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (provider_id, _) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &provider_id,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        let unknown = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("combo:nope", true)),
        )
        .await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
        assert_eq!(unknown.body["error"]["code"], "model_not_found");

        // A disabled combo is as good as unknown.
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        crate::combos::create(
            &s.app.state.pool,
            &actor,
            &crate::combos::ComboInput {
                name: "off".to_string(),
                strategy: "ordered".to_string(),
                weights: None,
                members: vec![crate::combos::MemberInput {
                    offering_id: off1.clone(),
                    weight: None,
                }],
                enabled: Some(false),
                pool: None,
                min_quality: None,
                max_mix_price_per_m: None,
                mix_profile: None,
                stickiness: None,
            },
        )
        .await
        .expect("disabled combo");
        let disabled = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("combo:off", true)),
        )
        .await;
        assert_eq!(disabled.status, StatusCode::NOT_FOUND);
        assert_eq!(disabled.body["error"]["code"], "model_not_found");
        assert_eq!(stub.request_count(), 0);
    }

    #[tokio::test]
    async fn combo_weighted_scores_the_pool_and_explains_every_candidate() {
        let stub = Stub::spawn(vec![Scenario::Stream {
            events: vec![event(0)],
            usage: Some(usage_json(2, 2)),
        }])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let expensive = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let cheap = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        for (offering_id, per_m) in [(&expensive, 10.0), (&cheap, 1.0)] {
            catalog::set_manual_price(
                &s.app.state.pool,
                &actor,
                offering_id,
                &PriceInput {
                    input_per_m: Some(per_m),
                    output_per_m: Some(per_m),
                    ..PriceInput::default()
                },
            )
            .await
            .expect("price");
        }
        combo(
            &s,
            "cheap",
            "weighted",
            Some(json!({"preset": "cheapest"})),
            &[(expensive.as_str(), None), (cheap.as_str(), None)],
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("combo:cheap", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body_text(response_body).await;

        let log = &log_rows(&s.app).await[0];
        assert_eq!(log.resolved_offering_id.as_deref(), Some(cheap.as_str()));
        let decision = decision_of(log);
        assert_eq!(decision["routing"]["strategy"], "weighted");
        assert_eq!(decision["routing"]["preset"], "cheapest");
        assert_eq!(decision["routing"]["weights"]["price"], 1.0);

        let candidates = decision["candidates"].as_array().expect("candidates");
        assert_eq!(candidates[0]["id"], cheap, "scored order, best first");
        assert_eq!(candidates[0]["outcome"], "chosen");
        assert_eq!(candidates[0]["score"]["price"], 1.0);
        assert_eq!(candidates[0]["score"]["total"], 1.0);
        assert_eq!(candidates[1]["id"], expensive);
        assert_eq!(candidates[1]["score"]["price"], 0.0);
        assert_eq!(candidates[1]["score"]["total"], 0.0);
        // No telemetry and no benchmarks in this test: the missing speed,
        // quality, and reliability components are imputed at 0.5 and marked
        // in the score and the explanation.
        let imputed = candidates[0]["score"]["imputed"]
            .as_array()
            .expect("imputed");
        let imputed: Vec<&str> = imputed.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(imputed, ["speed", "quality", "reliability"]);
        let explanation = candidates[0]["explanation"].as_str().expect("explanation");
        assert!(explanation.contains("best weighted score"), "{explanation}");
        assert!(explanation.contains("imputed at 0.5"), "{explanation}");
        let not_chosen = candidates[1]["explanation"].as_str().expect("explanation");
        assert!(not_chosen.contains("below the leader"), "{not_chosen}");
        let reason = decision["reason"].as_str().expect("reason");
        assert!(reason.contains("best weighted score"), "{reason}");
    }

    /// Prices one offering component-wise for the mix-routing tests.
    async fn set_price(
        app: &TestApp,
        uid: &str,
        offering_id: &str,
        input_per_m: f64,
        output_per_m: f64,
        cache_read_per_m: Option<f64>,
    ) {
        let actor = Actor {
            user_id: uid,
            ip: "127.0.0.1",
        };
        catalog::set_manual_price(
            &app.state.pool,
            &actor,
            offering_id,
            &PriceInput {
                input_per_m: Some(input_per_m),
                output_per_m: Some(output_per_m),
                cache_read_per_m,
                ..PriceInput::default()
            },
        )
        .await
        .expect("price");
    }

    /// The key D34 regression: a cache-priced offering costs *more* on the old
    /// input+output component sum but *less* at the default `code-agent` mix,
    /// so mix routing must pick it where the old sum would pick the other.
    ///
    /// cache:   $3 input / $15 output / $0.30 cache → sum 18, mix $0.5775/M
    /// nocache: $1 input / $2 output, no cache     → sum 3,  mix $1.004/M
    #[tokio::test]
    async fn mix_price_reorders_routing_where_the_component_sum_ranks_the_other_way() {
        let stub = Stub::spawn(vec![Scenario::Stream {
            events: vec![event(0)],
            usage: Some(json!({"prompt_tokens": 100, "completion_tokens": 10})),
        }])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let cache = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "cache-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let nocache = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "nocache-model",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        set_price(&s.app, &s.uid, &cache, 3.0, 15.0, Some(0.3)).await;
        set_price(&s.app, &s.uid, &nocache, 1.0, 2.0, None).await;
        // The old scoring price would rank nocache first (3 < 18).
        let cache_row = catalog::get_price(&s.app.state.pool, &cache)
            .await
            .expect("price")
            .expect("row");
        let nocache_row = catalog::get_price(&s.app.state.pool, &nocache)
            .await
            .expect("price")
            .expect("row");
        assert!(
            scoring_price(Some(&nocache_row)).expect("sum")
                < scoring_price(Some(&cache_row)).expect("sum"),
            "the fixture must invert the old component-sum order"
        );

        combo(
            &s,
            "mix-cheapest",
            "weighted",
            Some(json!({"preset": "cheapest"})),
            &[(cache.as_str(), None), (nocache.as_str(), None)],
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("combo:mix-cheapest", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body_text(response_body).await;

        let log = &log_rows(&s.app).await[0];
        assert_eq!(
            log.resolved_offering_id.as_deref(),
            Some(cache.as_str()),
            "the mix price, not the component sum, decides"
        );
        let decision = decision_of(log);
        assert_eq!(
            decision["routing"]["mix_profile"]["profile_name"],
            "code-agent"
        );
        assert_eq!(
            decision["routing"]["mix_profile"]["profile_id"],
            "mix-code-agent"
        );

        let candidates = decision["candidates"].as_array().expect("candidates");
        assert_eq!(candidates[0]["id"], cache);
        assert_eq!(candidates[0]["price_basis"], "mix");
        let mix = &candidates[0]["mix_price"];
        assert!(
            (mix["per_m"].as_f64().expect("per_m") - 0.5775).abs() < 1e-9,
            "{mix}"
        );
        assert_eq!(mix["profile_name"], "code-agent");
        assert_eq!(candidates[1]["id"], nocache);
        assert!(
            (candidates[1]["mix_price"]["per_m"].as_f64().expect("per_m") - 1.004).abs() < 1e-9,
            "cache-less falls back to the input price"
        );
        let explanation = candidates[0]["explanation"].as_str().expect("explanation");
        assert!(
            explanation.contains("price from mix $0.578/M at code-agent (8.1/0.4/91.5)"),
            "{explanation}"
        );
        let reason = decision["reason"].as_str().expect("reason");
        assert!(
            reason.contains("mix $0.578/M at code-agent (8.1/0.4/91.5)"),
            "{reason}"
        );

        // D34: billing stays component-based (D22) — the request's 100
        // prompt and 10 completion tokens bill at $3/$15, never at the mix.
        let usage = usage_rows(&s.app).await;
        assert_eq!(usage.len(), 1);
        let expected = 100.0 * 3.0 / 1_000_000.0 + 10.0 * 15.0 / 1_000_000.0;
        let cost = usage[0].cost_usd.expect("priced");
        assert!((cost - expected).abs() < 1e-12, "{cost} != {expected}");
        assert!((cost - 0.00045).abs() < 1e-12);
        assert!(
            (cost - 110.0 * 0.5775 / 1_000_000.0).abs() > 1e-9,
            "the mix price is not the billed cost"
        );
    }

    #[tokio::test]
    async fn a_combo_mix_profile_overrides_the_global_default() {
        // Same fixture as the regression test, but the combo selects `chat`
        // (74/26/0): the cache-heavy offering is expensive at that shape and
        // the cache-less one wins, proving the combo-level override applies.
        let stub = Stub::spawn(vec![Scenario::Stream {
            events: vec![event(0)],
            usage: Some(usage_json(10, 5)),
        }])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let cache = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "cache-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let nocache = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "nocache-model",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        set_price(&s.app, &s.uid, &cache, 3.0, 15.0, Some(0.3)).await;
        set_price(&s.app, &s.uid, &nocache, 1.0, 2.0, None).await;
        combo(
            &s,
            "chat-cheapest",
            "weighted",
            Some(json!({"preset": "cheapest", "mix_profile": "chat"})),
            &[(cache.as_str(), None), (nocache.as_str(), None)],
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("combo:chat-cheapest", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body_text(response_body).await;

        let log = &log_rows(&s.app).await[0];
        assert_eq!(log.resolved_offering_id.as_deref(), Some(nocache.as_str()));
        let decision = decision_of(log);
        assert_eq!(decision["routing"]["mix_profile"]["profile_name"], "chat");
        let candidates = decision["candidates"].as_array().expect("candidates");
        assert_eq!(candidates[0]["id"], nocache);
        assert!((candidates[0]["mix_price"]["per_m"].as_f64().expect("per_m") - 1.26).abs() < 1e-9);
        assert!((candidates[1]["mix_price"]["per_m"].as_f64().expect("per_m") - 6.12).abs() < 1e-9);
    }

    #[tokio::test]
    async fn the_breaker_excludes_a_failing_pair_with_a_reason_and_recovers() {
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 500,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
            Scenario::Status {
                code: 500,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(1)],
                usage: Some(usage_json(1, 1)),
            },
            Scenario::Stream {
                events: vec![event(2)],
                usage: Some(usage_json(1, 1)),
            },
            Scenario::Stream {
                events: vec![event(3)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded_with(|config| {
            config.breaker_threshold = 2;
            config.breaker_cooldown = Duration::from_millis(300);
        })
        .await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, cred1) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let off2 = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        // Two requests: member 1 fails with 500 each time (failover serves
        // from member 2), and after the second consecutive failure the
        // breaker opens.
        for _ in 0..2 {
            let (status, _, response_body) = send_streaming(
                s.app.router(),
                chat_request(&s.cookie, chat_body("m", true)),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            body_text(response_body).await;
        }

        // Third request: the pair is excluded with its reason recorded.
        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body_text(response_body).await;
        let logs = log_rows(&s.app).await;
        let decision = decision_of(&logs[2]);
        assert_eq!(decision["candidates"][0]["id"], off1);
        assert_eq!(
            decision["candidates"][0]["outcome"], "skipped_breaker_open",
            "the open pair is excluded, not just demoted"
        );
        let explanation = decision["candidates"][0]["explanation"]
            .as_str()
            .expect("explanation");
        assert!(explanation.contains("circuit breaker"), "{explanation}");
        assert!(explanation.contains(&cred1), "{explanation}");
        assert!(
            explanation.contains("2 consecutive failures"),
            "the explanation reports the failure streak, not the trip count: {explanation}"
        );
        assert_eq!(decision["candidates"][1]["outcome"], "chosen");
        assert_eq!(decision["chosen_offering_id"], off2);

        // After the cooldown window the pair half-opens and recovers.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body_text(response_body).await;
        let decision = decision_of(&log_rows(&s.app).await[3]);
        assert_eq!(
            decision["chosen_offering_id"], off1,
            "recovered after the window"
        );
        assert_eq!(stub.request_count(), 6);
    }

    #[tokio::test]
    async fn an_upstream_400_never_trips_the_breaker() {
        // Review M1: `invalid_request` is the caller's own broken request —
        // one bad client integration must not exclude a healthy offering for
        // everyone. `internal` failures are gated the same way.
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 400,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Status {
                code: 400,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Status {
                code: 400,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
        ])
        .await;
        let s = seeded_with(|config| {
            config.breaker_threshold = 2;
        })
        .await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;

        // Three failing requests in a row — past the threshold of 2 — and the
        // pair must still be attempted every time.
        for _ in 0..3 {
            let response = send(
                s.app.router(),
                chat_request(&s.cookie, chat_body("m", false)),
            )
            .await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST);
            assert_eq!(response.body["error"]["code"], "invalid_request");
        }
        assert_eq!(
            stub.request_count(),
            3,
            "the third request was still dispatched"
        );
        let decision = decision_of(&log_rows(&s.app).await[2]);
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(decision["candidates"][0]["error_class"], "invalid_request");
    }

    #[tokio::test]
    async fn a_failover_rescued_401_counts_against_the_failing_offering() {
        // Decision D26: the rescued attempt must still teach the router about
        // the offering that failed — the single request row cannot.
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 401,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let off2 = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        combo(
            &s,
            "test",
            "ordered",
            None,
            &[(off1.as_str(), None), (off2.as_str(), None)],
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("combo:test", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body_text(response_body).await;

        s.app
            .state
            .telemetry
            .refresh(&s.app.state.pool)
            .await
            .expect("refresh");
        let failed = s.app.state.telemetry.stats(&off1).expect("off1 stats");
        assert_eq!(failed.samples, 1);
        assert_eq!(
            failed.failure_rate,
            Some(1.0),
            "the rescued 401 raises the failing offering's failure rate"
        );
        let rescued = s.app.state.telemetry.stats(&off2).expect("off2 stats");
        assert_eq!(rescued.samples, 1);
        assert_eq!(rescued.failure_rate, Some(0.0));
    }

    #[tokio::test]
    async fn a_rescued_429_is_attributed_to_the_credential_not_the_offering() {
        // Decision D26: 429 is a credential-side limit. It must not touch the
        // offering's reliability — but stays recorded with its credential.
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 429,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, cred1) =
            provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let off2 = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        combo(
            &s,
            "test",
            "ordered",
            None,
            &[(off1.as_str(), None), (off2.as_str(), None)],
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("combo:test", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body_text(response_body).await;

        s.app
            .state
            .telemetry
            .refresh(&s.app.state.pool)
            .await
            .expect("refresh");
        assert!(
            s.app.state.telemetry.stats(&off1).is_none(),
            "a 429 leaves no reliability sample for the offering"
        );
        let row: (String, String) = sqlx::query_as(
            "SELECT credential_id, outcome FROM telemetry_attempts WHERE offering_id = ?",
        )
        .bind(&off1)
        .fetch_one(&s.app.state.pool)
        .await
        .expect("attempt row");
        assert_eq!(
            row,
            (cred1, "upstream_rate_limited".to_string()),
            "attributed to the credential that was limited"
        );
        assert_eq!(
            s.app
                .state
                .telemetry
                .stats(&off2)
                .expect("off2")
                .failure_rate,
            Some(0.0)
        );
    }

    #[tokio::test]
    async fn untried_candidates_never_claim_nothing_was_attempted() {
        // Review m5: a terminal failure (400, never retried) leaves the
        // remaining candidates "not reached" *because* an attempt happened.
        let stub = Stub::spawn(vec![Scenario::Status {
            code: 400,
            headers: Vec::new(),
            body: "{}".to_string(),
        }])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let response = send(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", false)),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);

        let decision = decision_of(&log_rows(&s.app).await[0]);
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        let untried = decision["candidates"][1]["explanation"]
            .as_str()
            .expect("explanation");
        assert!(
            untried.contains("every attempted candidate failed"),
            "{untried}"
        );
        assert!(!untried.contains("no candidate was attempted"), "{untried}");
    }

    #[tokio::test]
    async fn a_refused_request_does_not_consume_the_round_robin_rotation() {
        // Review nit: requests refused before dispatch (here: context) must
        // not advance the persisted cursor.
        let stub = Stub::spawn(vec![
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
            Scenario::Stream {
                events: vec![event(1)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", Some(128), None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let off1 = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "rr-a",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let off2 = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "rr-b",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        combo(
            &s,
            "rr",
            "round_robin",
            None,
            &[(off1.as_str(), None), (off2.as_str(), None)],
        )
        .await;

        // Refused before dispatch: max_tokens does not fit the context.
        let mut too_big = chat_body("combo:rr", true);
        too_big["max_tokens"] = json!(10_000);
        let refused = send(s.app.router(), chat_request(&s.cookie, too_big)).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        assert_eq!(refused.body["error"]["code"], "context_length_exceeded");
        let logs = log_rows(&s.app).await;
        assert!(
            decision_of(&logs[0])["routing"]["rr_cursor"].is_null(),
            "a refused request consumed no rotation"
        );
        let combo_row = crate::combos::load_by_name(&s.app.state.pool, "rr")
            .await
            .expect("load")
            .expect("combo");
        assert_eq!(combo_row.rr_cursor, 0, "the cursor did not move");

        // The next two dispatched requests rotate exactly as if the refusal
        // never happened.
        for _ in 0..2 {
            let (status, _, response_body) = send_streaming(
                s.app.router(),
                chat_request(&s.cookie, chat_body("combo:rr", true)),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            body_text(response_body).await;
        }
        let requests = stub.requests();
        assert_eq!(requests[0].body["model"], "rr-a");
        assert_eq!(requests[1].body["model"], "rr-b");
        let logs = log_rows(&s.app).await;
        assert_eq!(decision_of(&logs[1])["routing"]["rr_cursor"], 1);
        assert_eq!(decision_of(&logs[2])["routing"]["rr_cursor"], 2);
    }

    #[tokio::test]
    async fn every_candidate_carries_a_one_line_explanation() {
        let stub = Stub::spawn(vec![
            Scenario::Status {
                code: 500,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(1, 1)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;

        let (status, _, response_body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body_text(response_body).await;

        let decision = decision_of(&log_rows(&s.app).await[0]);
        assert_eq!(decision["routing"]["strategy"], "fixed_order");
        for candidate in decision["candidates"].as_array().expect("candidates") {
            let explanation = candidate["explanation"].as_str().expect("explanation");
            assert!(!explanation.is_empty(), "{candidate}");
        }
        assert!(
            decision["candidates"][0]["explanation"]
                .as_str()
                .expect("explanation")
                .contains("failed with upstream_error")
        );
        assert!(
            decision["candidates"][1]["explanation"]
                .as_str()
                .expect("explanation")
                .contains("served the request")
        );
    }

    // -----------------------------------------------------------------------
    // D35: catalog pools, preview parity, and stickiness
    // -----------------------------------------------------------------------

    async fn create_pool_combo(
        s: &Seeded,
        name: &str,
        max_mix_price_per_m: Option<f64>,
        sticky: bool,
    ) -> String {
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        crate::combos::create(
            &s.app.state.pool,
            &actor,
            &crate::combos::ComboInput {
                name: name.to_string(),
                strategy: "weighted".to_string(),
                weights: Some(json!({"preset": "cheapest"})),
                members: Vec::new(),
                enabled: None,
                pool: Some("all".to_string()),
                min_quality: None,
                max_mix_price_per_m,
                mix_profile: None,
                stickiness: sticky.then(|| crate::combos::StickinessInput {
                    enabled: Some(true),
                    ttl_seconds: Some(1800),
                    key_source: Some("auto".to_string()),
                }),
            },
        )
        .await
        .expect("pool combo")
        .id
    }

    async fn price_offering(s: &Seeded, offering_id: &str, per_m: f64) {
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        catalog::set_manual_price(
            &s.app.state.pool,
            &actor,
            offering_id,
            &PriceInput {
                input_per_m: Some(per_m),
                output_per_m: Some(per_m),
                ..PriceInput::default()
            },
        )
        .await
        .expect("price");
    }

    fn chat_request_with_session(
        cookie: &str,
        body: serde_json::Value,
        session: &str,
    ) -> axum::http::Request<Body> {
        let mut request = chat_request(cookie, body);
        request.headers_mut().insert(
            "x-session-id",
            axum::http::HeaderValue::from_str(session).expect("session header"),
        );
        request
    }

    #[tokio::test]
    async fn catalog_pool_preview_matches_the_runtime_choice() {
        let stub = Stub::spawn(vec![Scenario::Stream {
            events: vec![event(0)],
            usage: Some(usage_json(3, 4)),
        }])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let expensive = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let cheap = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        price_offering(&s, &expensive, 10.0).await;
        price_offering(&s, &cheap, 1.0).await;
        create_pool_combo(&s, "pool", Some(5.0), false).await;

        let preview = send(
            s.app.router(),
            api_request(
                Method::POST,
                "/api/combos/preview",
                Some(json!({
                    "name": "pool",
                    "strategy": "weighted",
                    "weights": {"preset": "cheapest"},
                    "members": [],
                    "pool": "all",
                    "max_mix_price_per_m": 5.0,
                    "limit": 5
                })),
                Some(&s.cookie),
                None,
            ),
        )
        .await;
        assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
        assert_eq!(
            preview.body["candidates"][0]["offering_id"],
            cheap.as_str(),
            "{}",
            preview.body
        );
        assert_eq!(preview.body["profile"]["profile_name"], "code-agent");
        assert_eq!(
            preview.body["scored"], 1,
            "the over-cap offering is excluded before scoring: {}",
            preview.body
        );
        assert!(
            preview.body["candidates"][0]["score"]["total"]
                .as_f64()
                .is_some()
        );

        let (status, _, body) = send_streaming(
            s.app.router(),
            chat_request(&s.cookie, chat_body("combo:pool", true)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = body_text(body).await;
        let log = &log_rows(&s.app).await[0];
        assert_eq!(log.resolved_offering_id.as_deref(), Some(cheap.as_str()));
        let decision = decision_of(log);
        assert_eq!(decision["candidates"][0]["id"], cheap.as_str());
        assert_eq!(decision["routing"]["strategy"], "weighted");
        assert_eq!(decision["sticky"]["enabled"], false);
    }

    #[tokio::test]
    async fn preview_scored_count_survives_the_candidate_limit() {
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) =
            provider_with_credential(&s.app, &s.uid, "p1", "http://127.0.0.1:1", None).await;
        let (p2, _) =
            provider_with_credential(&s.app, &s.uid, "p2", "http://127.0.0.1:1", None).await;
        let first = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "first",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let second = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "second",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        price_offering(&s, &first, 1.0).await;
        price_offering(&s, &second, 2.0).await;

        let preview = send(
            s.app.router(),
            api_request(
                Method::POST,
                "/api/combos/preview",
                Some(json!({
                    "name": "limited",
                    "strategy": "weighted",
                    "weights": {"preset": "cheapest"},
                    "members": [],
                    "pool": "all",
                    "max_mix_price_per_m": 100.0,
                    "limit": 1
                })),
                Some(&s.cookie),
                None,
            ),
        )
        .await;
        assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
        // Two offerings pass the filters; the list shows one because of
        // `limit`, and every explanation says "#1 of 2".
        assert_eq!(preview.body["scored"], 2, "{}", preview.body);
        assert_eq!(
            preview.body["candidates"]
                .as_array()
                .expect("candidates")
                .len(),
            1
        );
        assert!(
            preview.body["candidates"][0]["explanation"]
                .as_str()
                .is_some_and(|text| text.contains("of 2")),
            "{}",
            preview.body
        );
    }

    #[tokio::test]
    async fn sticky_pins_hold_per_session_across_a_price_change() {
        let stub = Stub::spawn(vec![
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(3, 4)),
            },
            Scenario::Stream {
                events: vec![event(1)],
                usage: Some(usage_json(3, 4)),
            },
            Scenario::Stream {
                events: vec![event(2)],
                usage: Some(usage_json(3, 4)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let first = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let second = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        price_offering(&s, &first, 1.0).await;
        price_offering(&s, &second, 10.0).await;
        create_pool_combo(&s, "sticky", Some(50.0), true).await;

        let (status, _, body) = send_streaming(
            s.app.router(),
            chat_request_with_session(&s.cookie, chat_body("combo:sticky", true), "sess-a"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = body_text(body).await;
        assert_eq!(
            log_rows(&s.app).await[0].resolved_offering_id.as_deref(),
            Some(first.as_str())
        );

        // Re-price: per-turn re-picking would switch to `second`, the pin
        // must keep the warm-cache offering for the same session.
        price_offering(&s, &first, 40.0).await;
        price_offering(&s, &second, 1.0).await;
        let (status, _, body) = send_streaming(
            s.app.router(),
            chat_request_with_session(&s.cookie, chat_body("combo:sticky", true), "sess-a"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = body_text(body).await;
        let pinned_rows = log_rows(&s.app).await;
        let pinned_log = pinned_rows.last().expect("second request log");
        assert_eq!(
            pinned_log.resolved_offering_id.as_deref(),
            Some(first.as_str()),
            "the pin holds across the price change"
        );
        let decision = decision_of(pinned_log);
        assert_eq!(decision["sticky"]["enabled"], true);
        assert_eq!(decision["sticky"]["key_source"], "auto");
        assert_eq!(decision["sticky"]["pinned"], first.as_str());

        // A different session has no pin and follows the new prices.
        let (status, _, body) = send_streaming(
            s.app.router(),
            chat_request_with_session(&s.cookie, chat_body("combo:sticky", true), "sess-b"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = body_text(body).await;
        let other_rows = log_rows(&s.app).await;
        let other_log = other_rows.last().expect("third request log");
        assert_eq!(
            other_log.resolved_offering_id.as_deref(),
            Some(second.as_str())
        );
        let pins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM combo_sticky_choices")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("pin count");
        assert_eq!(pins, 2, "one pin per session");
    }

    #[tokio::test]
    async fn a_failed_pinned_attempt_moves_the_pin_to_the_server_that_served() {
        let stub = Stub::spawn(vec![
            Scenario::Stream {
                events: vec![event(0)],
                usage: Some(usage_json(3, 4)),
            },
            Scenario::Status {
                code: 500,
                headers: Vec::new(),
                body: "{}".to_string(),
            },
            Scenario::Stream {
                events: vec![event(1)],
                usage: Some(usage_json(3, 4)),
            },
        ])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let pinned = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let fallback = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        price_offering(&s, &pinned, 1.0).await;
        price_offering(&s, &fallback, 10.0).await;
        let combo_id = create_pool_combo(&s, "failover", Some(50.0), true).await;

        let (status, _, body) = send_streaming(
            s.app.router(),
            chat_request_with_session(&s.cookie, chat_body("combo:failover", true), "sess-a"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = body_text(body).await;

        let (status, _, body) = send_streaming(
            s.app.router(),
            chat_request_with_session(&s.cookie, chat_body("combo:failover", true), "sess-a"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = body_text(body).await;
        let rows = log_rows(&s.app).await;
        let log = rows.last().expect("second request log");
        assert_eq!(log.resolved_offering_id.as_deref(), Some(fallback.as_str()));
        let decision = decision_of(log);
        assert_eq!(decision["candidates"][0]["id"], pinned.as_str());
        assert_eq!(decision["candidates"][0]["outcome"], "failed");
        assert_eq!(decision["sticky"]["pinned"], fallback.as_str());
        let stored = crate::combos::load_pin(
            &s.app.state.pool,
            &combo_id,
            "sess-a",
            &crate::catalog::now_ts(),
        )
        .await
        .expect("load pin")
        .expect("pin exists");
        assert_eq!(stored.offering_id, fallback);
    }

    #[tokio::test]
    async fn an_expired_pin_re_picks_and_records_why() {
        let stub = Stub::spawn(vec![Scenario::Stream {
            events: vec![event(0)],
            usage: Some(usage_json(3, 4)),
        }])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let stale = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let fresh = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        price_offering(&s, &stale, 10.0).await;
        price_offering(&s, &fresh, 1.0).await;
        let combo_id = create_pool_combo(&s, "expiry", Some(50.0), true).await;
        crate::combos::upsert_pin(
            &s.app.state.pool,
            &combo_id,
            "sess-old",
            &stale,
            60,
            "2020-01-01T00:00:00.000Z",
        )
        .await
        .expect("old pin");

        let (status, _, body) = send_streaming(
            s.app.router(),
            chat_request_with_session(&s.cookie, chat_body("combo:expiry", true), "sess-old"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = body_text(body).await;
        let log = &log_rows(&s.app).await[0];
        assert_eq!(log.resolved_offering_id.as_deref(), Some(fresh.as_str()));
        let decision = decision_of(log);
        assert_eq!(decision["sticky"]["replaced_why"], "expired");
        assert_eq!(decision["sticky"]["pinned"], fresh.as_str());
    }

    #[tokio::test]
    async fn a_pin_the_caller_cannot_access_is_ignored() {
        let stub = Stub::spawn(vec![Scenario::Stream {
            events: vec![event(0)],
            usage: Some(usage_json(3, 4)),
        }])
        .await;
        let s = seeded().await;
        let model_id = model(&s.app, &s.uid, "m", None, None).await;
        let (p1, _) = provider_with_credential(&s.app, &s.uid, "p1", &stub.base_url, None).await;
        let (p2, _) = provider_with_credential(&s.app, &s.uid, "p2", &stub.base_url, None).await;
        let denied = offering(
            &s.app,
            &s.uid,
            &p1,
            &model_id,
            "test-model",
            "2026-01-01T00:00:01.000Z",
            None,
        )
        .await;
        let allowed = offering(
            &s.app,
            &s.uid,
            &p2,
            &model_id,
            "test-model-2",
            "2026-01-01T00:00:02.000Z",
            None,
        )
        .await;
        price_offering(&s, &denied, 1.0).await;
        price_offering(&s, &allowed, 10.0).await;
        let combo_id = create_pool_combo(&s, "grants", Some(50.0), true).await;

        // The caller is denied the first offering (a deny row restricts even
        // an admin, decision D9).
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        crate::access::create_grant(
            &s.app.state.pool,
            &actor,
            &crate::access::GrantInput {
                subject_type: "user".to_string(),
                subject_id: s.uid.clone(),
                resource_type: "offering".to_string(),
                resource_id: denied.clone(),
                mode: "deny".to_string(),
            },
        )
        .await
        .expect("deny grant");

        // Pin the offering the caller may not use.
        crate::combos::upsert_pin(
            &s.app.state.pool,
            &combo_id,
            "sess-grant",
            &denied,
            1800,
            &crate::catalog::now_ts(),
        )
        .await
        .expect("pin");

        let (status, _, body) = send_streaming(
            s.app.router(),
            chat_request_with_session(&s.cookie, chat_body("combo:grants", true), "sess-grant"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let _ = body_text(body).await;
        let log = &log_rows(&s.app).await[0];
        assert_eq!(log.resolved_offering_id.as_deref(), Some(allowed.as_str()));
        let decision = decision_of(log);
        assert_eq!(decision["sticky"]["pinned"], allowed.as_str());
        assert_eq!(decision["sticky"]["replaced_why"], "not_eligible");
    }
}
