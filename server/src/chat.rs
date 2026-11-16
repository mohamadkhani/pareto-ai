//! Chat data plane pipeline (M3): `POST /v1/chat/completions`.
//!
//! One request runs validate → resolve offering → forward → stream → settle:
//!
//! * **Validate**: body size (handler-enforced, 4 MiB), ≤200 messages,
//!   `max_tokens` clamped to the offering's output limit, and
//!   `context_length_exceeded` before any upstream call when the estimated
//!   input plus `max_tokens` does not fit the offering's context.
//! * **Resolve**: enabled offerings of enabled providers matching the
//!   requested model (`upstream_model_id` or `canonical_name`), in a fixed
//!   deterministic order (creation order, id tiebreak); scoring arrives in
//!   M5. Credentials in cooldown are demoted to the end of the order. Access
//!   grants (M4, decision D9) then filter the candidates: every subject (the
//!   user, and the key when present) must allow the model, the offering, and
//!   the credential. Before forwarding, the request's estimate is reserved
//!   against the user's budgets and token caps (decision D7).
//! * **Failover**: only before the first byte reaches the client — before the
//!   first upstream chunk is forwarded for streams, before the response is
//!   returned for buffered calls. `400`/`401`/`403` never fail over; `429`
//!   fails over at most once, honouring `Retry-After` as a cooldown on that
//!   credential.
//! * **Settle**: exactly one `usage_events` and one `request_logs` row per
//!   request (never one per attempt — a failed over request is not double
//!   spent). Upstream `usage` is authoritative; when missing, tokens are
//!   estimated `chars/4` from **content text only** (`choices[].message.content`
//!   / `delta.content`, plus `reasoning`; input from the message text) and
//!   `tokens_estimated` is set — never from JSON scaffolding or raw bytes, and
//!   only when a request was actually sent upstream (review M1/M3). The same
//!   transaction prices the request (M4, decisions D7/D10), settles its
//!   reservation into the budgets and token caps of the period it started in,
//!   and books the `usage_daily` rollup.
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
use crate::catalog;
use crate::money;
use crate::reports;
use crate::upstream::sse::{self, SseInspector, Usage};
use crate::upstream::{
    Budget, FailureClass, TimeoutKind, UpstreamCall, UpstreamError, UpstreamStream,
};

/// Maximum number of chat messages in one request.
pub const MAX_MESSAGES: usize = 200;
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
}

impl ChatFailure {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
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
        Some(messages) if messages.len() > MAX_MESSAGES => {
            return Err(ChatFailure::invalid(format!(
                "`messages` must contain at most {MAX_MESSAGES} messages"
            )));
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
#[derive(Debug)]
struct Credential {
    id: String,
    secret_cipher: String,
    headers_json: Option<String>,
    cooled: bool,
}

/// One offering with everything needed to serve a request from it.
#[derive(Debug)]
struct Candidate {
    offering_id: String,
    model_id: String,
    canonical_name: String,
    upstream_model_id: String,
    provider_name: String,
    base_url: String,
    context_length: Option<i64>,
    max_output_tokens: Option<i64>,
    offering_headers_json: Option<String>,
    credential: Option<Credential>,
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
    provider_id: String,
    provider_name: String,
    base_url: String,
}

/// Resolves the requested model to candidates in a fixed deterministic order:
/// creation order with id tiebreak, credentials in cooldown demoted last.
/// Failure mode: `500 internal` on database errors (the "no candidates" case
/// is decided by the caller, not here).
async fn resolve_candidates(
    pool: &sqlx::SqlitePool,
    requested: &str,
    now: &str,
) -> Result<Vec<Candidate>, ChatFailure> {
    let rows = sqlx::query_as::<_, OfferingJoin>(
        "SELECT o.id AS offering_id, o.model_id, o.upstream_model_id, \
                o.context_override, o.max_output_override, o.headers_json AS offering_headers_json, \
                m.canonical_name, m.context_length, m.max_output_tokens, \
                pr.id AS provider_id, pr.name AS provider_name, pr.base_url \
         FROM offerings o \
         JOIN models m ON m.id = o.model_id \
         JOIN providers pr ON pr.id = o.provider_id \
         WHERE pr.enabled = 1 AND o.enabled = 1 \
           AND (o.upstream_model_id = ? OR m.canonical_name = ?) \
         ORDER BY o.created_at, o.id",
    )
    .bind(requested)
    .bind(requested)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "resolving offerings failed");
        ChatFailure::internal()
    })?;

    let mut candidates = Vec::with_capacity(rows.len());
    for row in rows {
        let credential = pick_credential(pool, &row.provider_id, now).await?;
        candidates.push(Candidate {
            offering_id: row.offering_id,
            model_id: row.model_id,
            canonical_name: row.canonical_name,
            upstream_model_id: row.upstream_model_id,
            provider_name: row.provider_name,
            base_url: row.base_url,
            context_length: row.context_override.or(row.context_length),
            max_output_tokens: row.max_output_override.or(row.max_output_tokens),
            offering_headers_json: row.offering_headers_json,
            credential,
        });
    }
    // Deterministic and stable: cooled credentials last, relative order kept.
    candidates.sort_by_key(|candidate| {
        candidate
            .credential
            .as_ref()
            .is_some_and(|credential| credential.cooled)
    });
    Ok(candidates)
}

#[derive(Debug, sqlx::FromRow)]
struct CredentialRow {
    id: String,
    secret_cipher: String,
    headers_json: Option<String>,
    cooldown_until: Option<String>,
    balance_usd: Option<f64>,
    balance_low_threshold: Option<f64>,
}

/// The provider credential to use: the first one not in cooldown (id order),
/// else the first one. `None` when the provider has no credential at all.
async fn pick_credential(
    pool: &sqlx::SqlitePool,
    provider_id: &str,
    now: &str,
) -> Result<Option<Credential>, ChatFailure> {
    let rows = sqlx::query_as::<_, CredentialRow>(
        "SELECT id, secret_cipher, headers_json, cooldown_until, balance_usd, \
                balance_low_threshold \
         FROM provider_credentials WHERE provider_id = ? ORDER BY id",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "resolving credentials failed");
        ChatFailure::internal()
    })?;

    let to_credential = |row: &CredentialRow, cooled: bool| {
        warn_low_balance(row);
        Credential {
            id: row.id.clone(),
            secret_cipher: row.secret_cipher.clone(),
            headers_json: row.headers_json.clone(),
            cooled,
        }
    };
    let cooled = |row: &CredentialRow| {
        row.cooldown_until
            .as_deref()
            .is_some_and(|until| until > now)
    };
    let first_free = rows.iter().find(|row| !cooled(row));
    Ok(match first_free {
        Some(row) => Some(to_credential(row, false)),
        None => rows.first().map(|row| to_credential(row, true)),
    })
}

/// The manual credential balance is **advisory** in v1 (decision D8): a
/// balance below its low threshold is warned about here, in the logs, and
/// never gates a request. A real balance sync later turns this into a gate.
fn warn_low_balance(row: &CredentialRow) {
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
/// and what happened to it. The M5 explanation grows out of this.
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
}

#[derive(Debug, Clone, Default)]
struct Decision {
    candidates: Vec<CandidateRecord>,
    chosen_offering_id: Option<String>,
    reason: String,
}

impl Decision {
    fn to_json(&self) -> Value {
        json!({
            "candidates": self.candidates.iter().map(|record| json!({
                "id": record.id,
                "provider": record.provider,
                "model": record.model,
                "upstream_model_id": record.upstream_model_id,
                "price": record.price,
                "credential_id": record.credential_id,
                "outcome": record.outcome,
                "error_class": record.error_class,
                "status": record.status,
            })).collect::<Vec<_>>(),
            "chosen_offering_id": self.chosen_offering_id,
            "reason": self.reason,
        })
    }
}

/// The one-line reason for `decision_json`, composed from the attempt
/// outcomes and the terminal state.
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
                None => reason.push_str(&format!(
                    "{chosen} chosen (first eligible candidate in fixed order)"
                )),
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

    let mut candidates = resolve_candidates(&state.pool, &parsed.model, &catalog::now_ts()).await?;
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

    // Access control (decision D9): a candidate survives only when every
    // subject (the user, and the key when present) allows its model, its
    // offering, and its credential. Admins are allowed by default; a request
    // whose candidates are all denied is a 403, never a 404.
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
    // price when known, and what happened to it.
    let mut decision = Decision {
        candidates: Vec::with_capacity(candidates.len()),
        ..Decision::default()
    };
    let mut attempts: Vec<Attempt> = Vec::new();
    // Price rows of the attempts in order: the reservation estimate is
    // priced on the first *priced* candidate (review m4).
    let mut attempt_prices: Vec<Option<catalog::PriceRow>> = Vec::new();
    let mut first_output_estimate: Option<Option<i64>> = None;
    let mut context_rejected = false;
    for candidate in candidates {
        let price_row = match catalog::get_price(&state.pool, &candidate.offering_id).await {
            Ok(row) => row,
            Err(e) => {
                tracing::error!(error = %e, "reading price failed");
                None
            }
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
        });
        let decision_idx = decision.candidates.len() - 1;

        if candidate.credential.is_none() {
            decision.candidates[decision_idx].outcome = "skipped_no_credential";
            continue;
        }
        // `max_tokens` clamped to this offering's output limit; the estimated
        // input must fit the context minus that reservation, or this offering
        // is out before any upstream call.
        let max_tokens =
            crate::upstream::clamp_max_tokens(parsed.max_tokens, candidate.max_output_tokens);
        if let Some(context) = candidate.context_length {
            // The same content-only estimator as settlement (review M1).
            let estimated_input = (input_chars / 4) as i64;
            let reservation = max_tokens.unwrap_or(0);
            if estimated_input.saturating_add(reservation) > context {
                decision.candidates[decision_idx].outcome = "skipped_context";
                context_rejected = true;
                continue;
            }
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
        attempt_prices.push(price_row);
        attempts.push(Attempt {
            decision_idx,
            candidate,
            body,
        });
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
        decision.reason = decision_reason(&decision);
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
    let estimated_usd = attempt_prices
        .iter()
        .find_map(|price| money::compute_cost(price.as_ref(), Some(&estimate_usage)).cost_usd)
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
    let plan = Plan {
        parsed_stream: parsed.stream,
        attempts,
        decision,
        requested_model: parsed.model,
        bytes_in,
        input_chars,
        has_request_body: bytes_in > 0,
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

struct Plan {
    parsed_stream: bool,
    attempts: Vec<Attempt>,
    decision: Decision,
    requested_model: String,
    bytes_in: u64,
    /// Message-text characters, the input estimator's base (review M1).
    input_chars: usize,
    has_request_body: bool,
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
    } = plan;
    let mut decision = decision;
    let end = match parsed_stream {
        true => run_streaming(&state, attempts, &mut decision, &tx, cancel, total).await,
        false => run_buffered(&state, attempts, &mut decision, &tx, cancel, total).await,
    };

    // Settlement runs before the terminal item is sent, so the rows exist
    // when the failure reaches the client.
    let (result, failure, status, error_class) = match end {
        RunEnd::Delivered(result) => (result, None, 200i64, None),
        RunEnd::Failed { result, failure } => {
            // A stream that failed after its first byte was already committed
            // as 200; only failures before it can still change the status.
            let status = match result.bytes_out > 0 {
                true => 200i64,
                false => failure.status.as_u16() as i64,
            };
            let code: &'static str = failure.code;
            (result, Some(failure), status, Some(code))
        }
        RunEnd::Disconnected(result) => (
            result,
            None,
            CLIENT_DISCONNECTED_STATUS,
            Some("client_disconnected"),
        ),
    };
    let mut decision = result.decision;
    decision.reason = decision_reason(&decision);
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
            started,
            ttft_ms: result.ttft_ms,
            bytes_out: result.bytes_out,
            bytes_in,
            input_chars,
            has_request_body,
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

    for attempt in attempts {
        let decision_idx = attempt.decision_idx;
        let candidate = &attempt.candidate;
        last = Some((candidate.offering_id.clone(), candidate.model_id.clone()));
        let offering_id = candidate.offering_id.clone();
        let model_id = candidate.model_id.clone();

        let (mut stream, first, ttft_ms) = match start_attempt(state, &attempt, &mut cancel, total)
            .await
        {
            AttemptStart::Started {
                stream,
                first,
                ttft_ms,
            } => {
                sent = true;
                (stream, first, ttft_ms)
            }
            AttemptStart::Canceled => {
                return RunEnd::Disconnected(last_result(decision, &last, None, 0, 0, None, sent));
            }
            AttemptStart::Failed(e) => {
                if let Some(credential) = &candidate.credential {
                    let class = e.classify();
                    if let Some(cooldown) = class.cooldown {
                        record_cooldown(&state.pool, &credential.id, cooldown).await;
                    }
                    record_failure(decision, decision_idx, &class, upstream_status(&e));
                    let failure = failure_from_class(&class);
                    if can_fail_over(&class, &failure, &mut rate_budget) {
                        last_failure = Some(failure);
                        continue;
                    }
                    return RunEnd::Failed {
                        result: last_result(decision, &last, None, 0, 0, None, sent),
                        failure,
                    };
                }
                let failure = ChatFailure::internal();
                return RunEnd::Failed {
                    result: last_result(decision, &last, None, 0, 0, None, sent),
                    failure,
                };
            }
        };

        // The first chunk decides the boundary: inspect before forwarding, so
        // a protocol error on the very first chunk can still fail over.
        let mut inspector = SseInspector::new();
        inspector.push(&first);
        if inspector.overflowed {
            // Line-cap overflow is terminal even here: retrying could just
            // buffer the cap again per candidate (review M2).
            let class = UpstreamError::Body("response line too large".to_string()).classify();
            record_failure(decision, decision_idx, &class, None);
            return RunEnd::Failed {
                result: last_result(decision, &last, None, 0, 0, None, sent),
                failure: failure_from_class(&class),
            };
        }
        if inspector.protocol_error {
            let class = UpstreamError::Body("malformed SSE".to_string()).classify();
            record_failure(decision, decision_idx, &class, None);
            let failure = failure_from_class(&class);
            if can_fail_over(&class, &failure, &mut rate_budget) {
                last_failure = Some(failure);
                continue;
            }
            return RunEnd::Failed {
                result: last_result(decision, &last, None, 0, 0, None, sent),
                failure,
            };
        }

        decision.candidates[decision_idx].outcome = "chosen";
        decision.chosen_offering_id = Some(offering_id.clone());

        // Forwarding starts here: from now on no failover is allowed.
        let first_len = first.len() as u64;
        let mut bytes_out = 0u64;
        match send_item(&mut cancel, tx, total, Ok(first)).await {
            Sent::Gone => {
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
                    failure: total_timeout_failure(),
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
        )
        .await;
        return match end {
            PumpEnd::Done => RunEnd::Delivered(chosen_result(
                decision,
                &offering_id,
                &model_id,
                inspector.usage.clone(),
                inspector.output_chars,
                bytes_out,
                Some(ttft_ms),
                sent,
            )),
            PumpEnd::Failed(failure) => {
                // The chosen record keeps its outcome but records how the
                // stream ended; the response was already committed.
                decision.candidates[decision_idx].error_class = Some(failure.code);
                decision.candidates[decision_idx].status = Some(failure.status.as_u16() as i64);
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
            PumpEnd::Disconnected => RunEnd::Disconnected(chosen_result(
                decision,
                &offering_id,
                &model_id,
                inspector.usage.clone(),
                inspector.output_chars,
                bytes_out,
                Some(ttft_ms),
                sent,
            )),
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

    for attempt in attempts {
        let decision_idx = attempt.decision_idx;
        let candidate = &attempt.candidate;
        last = Some((candidate.offering_id.clone(), candidate.model_id.clone()));
        let offering_id = candidate.offering_id.clone();
        let model_id = candidate.model_id.clone();

        let (mut stream, first, ttft_ms) = match start_attempt(state, &attempt, &mut cancel, total)
            .await
        {
            AttemptStart::Started {
                stream,
                first,
                ttft_ms,
            } => {
                sent = true;
                (stream, first, ttft_ms)
            }
            AttemptStart::Canceled => {
                return RunEnd::Disconnected(last_result(decision, &last, None, 0, 0, None, sent));
            }
            AttemptStart::Failed(e) => {
                let class = e.classify();
                if let Some(credential) = &candidate.credential
                    && let Some(cooldown) = class.cooldown
                {
                    record_cooldown(&state.pool, &credential.id, cooldown).await;
                }
                record_failure(decision, decision_idx, &class, upstream_status(&e));
                let failure = failure_from_class(&class);
                if can_fail_over(&class, &failure, &mut rate_budget) {
                    last_failure = Some(failure);
                    continue;
                }
                return RunEnd::Failed {
                    result: last_result(decision, &last, None, 0, 0, None, sent),
                    failure,
                };
            }
        };

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
        let (usage, output_chars, failure) = match read_failure {
            // A cap failure is terminal and the oversized buffer is never
            // parsed (review m5).
            Some(e) => (None, 0, Some(e)),
            None => {
                let (usage, output_chars, protocol_error) = sse::inspect_json_body(&buffer);
                let failure = match protocol_error {
                    true => Some(UpstreamError::Body("invalid JSON body".to_string())),
                    false => None,
                };
                (usage, output_chars, failure)
            }
        };
        let Some(e) = failure else {
            decision.candidates[decision_idx].outcome = "chosen";
            decision.chosen_offering_id = Some(offering_id.clone());
            let bytes_out = buffer.len() as u64;
            return match send_item(&mut cancel, tx, total, Ok(Bytes::from(buffer))).await {
                Sent::Ok => RunEnd::Delivered(chosen_result(
                    decision,
                    &offering_id,
                    &model_id,
                    usage,
                    output_chars,
                    bytes_out,
                    Some(ttft_ms),
                    sent,
                )),
                Sent::Gone => RunEnd::Disconnected(chosen_result(
                    decision,
                    &offering_id,
                    &model_id,
                    usage,
                    output_chars,
                    bytes_out,
                    Some(ttft_ms),
                    sent,
                )),
                Sent::TimedOut => RunEnd::Failed {
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
                },
            };
        };

        let class = e.classify();
        record_failure(decision, decision_idx, &class, upstream_status(&e));
        let failure = failure_from_class(&class);
        if can_fail_over(&class, &failure, &mut rate_budget) {
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
    Failed(ChatFailure),
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
) -> PumpEnd {
    loop {
        let chunk = match or_cancel(cancel, stream.next_chunk()).await {
            Wait::Canceled => return PumpEnd::Disconnected,
            Wait::Done(result) => result,
        };
        match chunk {
            Err(e) => {
                let class = e.classify();
                return PumpEnd::Failed(failure_from_class(&class));
            }
            Ok(None) => {
                return match inspector.truncated() {
                    false => PumpEnd::Done,
                    true => PumpEnd::Failed(failure_from_class(
                        &UpstreamError::Body("truncated stream".to_string()).classify(),
                    )),
                };
            }
            Ok(Some(bytes)) => {
                inspector.push(&bytes);
                if inspector.failed() {
                    // A malformed payload or a line-buffer overflow (review
                    // M2): both end the stream in-band, never fail over.
                    let class =
                        UpstreamError::Body("malformed or oversized SSE".to_string()).classify();
                    return PumpEnd::Failed(failure_from_class(&class));
                }
                let len = bytes.len() as u64;
                match send_item(cancel, tx, total, Ok(bytes)).await {
                    Sent::Ok => *bytes_out += len,
                    Sent::Gone => return PumpEnd::Disconnected,
                    Sent::TimedOut => return PumpEnd::Failed(total_timeout_failure()),
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

/// Failover is allowed only before the first byte, never for 400/401/403, and
/// a `429` advances at most once (spec failure rules).
fn can_fail_over(class: &FailureClass, failure: &ChatFailure, rate_budget: &mut i32) -> bool {
    if !class.retryable_before_first_byte {
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
    started: Instant,
    ttft_ms: Option<i64>,
    bytes_out: u64,
    bytes_in: u64,
    has_request_body: bool,
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
            started,
            ttft_ms: None,
            bytes_out: 0,
            bytes_in,
            has_request_body: bytes_in > 0,
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

/// Writes the `usage_events` + `request_logs` pair, settles the request's
/// reservation (budgets and token caps, decision D7), and books the `usage_daily`
/// rollup — all in one transaction. Exactly one pair per request (a failover
/// never double-spends). Accounting failures are logged, never surfaced: the
/// response must not depend on the ledger.
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

    let price = match &settlement.offering_id {
        Some(offering_id) => match catalog::get_price(&state.pool, offering_id).await {
            Ok(price) => price,
            Err(e) => {
                tracing::error!(error = %e, "reading the settled price failed");
                None
            }
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
          api_key_id, has_request_body) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

    let outcome = match (usage_result, log_result, reserve_result, rollup_result) {
        (Ok(_), Ok(_), Ok(_), Ok(_)) => tx.commit().await.map(|_| ()),
        (Err(e), _, _, _) | (_, Err(e), _, _) | (_, _, Err(e), _) | (_, _, _, Err(e)) => Err(e),
    };
    if let Err(e) = outcome {
        tracing::error!(
            error = %e,
            request_id = %settlement.meta.request_id,
            "usage accounting write failed"
        );
    }
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
        self, Actor, CredentialInput, ModelInput, OfferingInput, PriceInput, ProviderInput,
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
            },
        )
        .await
        .expect("credential");
        (provider.id, credential.id)
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
    async fn upstream_auth_failures_are_never_retried() {
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
            chat_request(&s.cookie, chat_body("m", true)),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_GATEWAY);
        assert_eq!(response.body["error"]["code"], "upstream_auth");
        assert_eq!(
            stub.request_count(),
            1,
            "401/403 are never retried, even on another candidate"
        );

        // The broken credential is cooled down, though.
        let cooldown = cooldown_of(&s.app, &cred1).await.expect("cooldown set");
        assert!(!cooldown.is_empty());

        let row = &usage_rows(&s.app).await[0];
        assert_eq!(row.error_class.as_deref(), Some("upstream_auth"));
        assert_eq!(row.status, Some(502));
        let decision = decision_of(&log_rows(&s.app).await[0]);
        assert_eq!(decision["candidates"][0]["error_class"], "upstream_auth");
        assert_eq!(decision["candidates"][1]["outcome"], "not_tried");
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
    async fn validation_failures_are_invalid_request_and_still_logged() {
        let stub = Stub::spawn(vec![]).await;
        let s = seeded().await;
        let router = s.app.router();

        let many: Vec<serde_json::Value> = (0..=MAX_MESSAGES)
            .map(|i| json!({"role": "user", "content": format!("m{i}")}))
            .collect();
        let too_many = send(
            router.clone(),
            chat_request(&s.cookie, json!({"model": "m", "messages": many})),
        )
        .await;
        assert_eq!(too_many.status, StatusCode::BAD_REQUEST);
        assert_eq!(too_many.body["error"]["code"], "invalid_request");

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
        assert_eq!(rows.len(), 4, "every completion is accounted for");
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

        let at_limit = (0..MAX_MESSAGES)
            .map(|i| json!({"role": "user", "content": i}))
            .collect::<Vec<_>>();
        assert!(
            parse_request(
                json!({"model": "m", "messages": at_limit})
                    .to_string()
                    .as_bytes()
            )
            .is_ok(),
            "exactly {MAX_MESSAGES} messages is fine"
        );

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
        }
        .classify();
        let rate_limited = UpstreamError::Http {
            status: 429,
            retry_after: None,
        }
        .classify();
        let auth = UpstreamError::Http {
            status: 401,
            retry_after: None,
        }
        .classify();

        let mut budget = 1;
        assert!(can_fail_over(
            &retryable,
            &failure_from_class(&retryable),
            &mut budget
        ));
        assert_eq!(budget, 1, "transient failures do not spend the 429 budget");
        assert!(can_fail_over(
            &rate_limited,
            &failure_from_class(&rate_limited),
            &mut budget
        ));
        assert_eq!(budget, 0);
        assert!(
            !can_fail_over(
                &rate_limited,
                &failure_from_class(&rate_limited),
                &mut budget
            ),
            "429 fails over at most once"
        );
        assert!(!can_fail_over(
            &auth,
            &failure_from_class(&auth),
            &mut budget
        ));
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

    /// The grant matrix on the data plane: default deny for users, allow on
    /// both subjects to pass, deny beats allow.
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

        // Grants on both subjects (the intersection) let the request through.
        for (subject_type, subject_id) in [
            ("user", owner.clone()),
            ("api_key", created.view.id.clone()),
        ] {
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
        let allowed = send(s.app.router(), bearer(&created.key, chat_body("m", false))).await;
        assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.body);
        let visible = send(s.app.router(), listing(&created.key)).await;
        assert_eq!(visible.body["data"].as_array().expect("data").len(), 1);

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

        // ...and one refused before anything is sent: a non-admin key with no
        // grants may not use the only offering (403 access_denied).
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
            started: Instant::now(),
            ttft_ms: None,
            bytes_out: 0,
            bytes_in: 0,
            has_request_body: true,
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
}
