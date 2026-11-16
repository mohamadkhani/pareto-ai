//! OpenAI-compatible upstream client (M3).
//!
//! One wire format in and out (decision D3): this module turns a resolved
//! offering into a `POST {base_url}/chat/completions` call, streams the SSE
//! response back chunk by chunk, and maps upstream failures onto the error
//! taxonomy of `docs/specs/m3-chat-dataplane.md`.
//!
//! Secrets: the credential secret is decrypted here and goes on the wire as
//! bearer auth (unless a merged header overrides `authorization`) and nowhere
//! else. [`UpstreamCall`]'s `Debug` prints neither headers nor the secret, the
//! bearer value is marked sensitive so reqwest itself redacts it, and every
//! merged `authorization` / `x-api-key` value is marked sensitive the same way
//! (review m4). Upstream failures are logged at [`UpstreamError::classify`];
//! the error strings are secret-free by construction — keep them that way.
//!
//! Timeout budgets ([`Budget`]): the first-byte deadline covers everything
//! from request start to the first body chunk, the idle duration bounds the
//! gap between chunks, and the total deadline is absolute across failover
//! attempts. Cancellation is not handled here: dropping [`UpstreamStream`]
//! drops the response and closes the connection.

pub mod sse;

#[cfg(test)]
pub(crate) mod stub;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::time::Duration;

use axum::http::StatusCode;
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use zeroize::Zeroizing;

use crate::catalog::headers::merge_headers;
use crate::secrets::SecretBox;

/// How long a failed credential stays in cooldown when the upstream did not
/// say (401/403/402/429 without `Retry-After`).
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);

/// Timeout budgets for one upstream call, derived from the configured limits.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Deadline for response headers and the first body chunk.
    pub first_byte: tokio::time::Instant,
    /// Maximum gap between body chunks.
    pub idle: Duration,
    /// Absolute deadline for the whole client request (all attempts).
    pub total: tokio::time::Instant,
}

/// Which budget a [`UpstreamError::Timeout`] blew through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutKind {
    FirstByte,
    Idle,
    Total,
}

/// A failure of one upstream call. `classify` maps it onto the taxonomy.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    /// The connection or the body transport failed (no response, reset,
    /// broken framing).
    #[error("upstream transport failed: {0}")]
    Transport(String),
    /// A timeout budget elapsed.
    #[error("upstream timed out ({kind:?})")]
    Timeout { kind: TimeoutKind },
    /// The upstream answered with a non-2xx status.
    #[error("upstream returned HTTP {status}")]
    Http {
        status: u16,
        /// Parsed `Retry-After` (seconds), when the upstream sent one.
        retry_after: Option<Duration>,
    },
    /// The response body failed before it could be used: empty stream,
    /// truncated stream, or malformed SSE/JSON.
    #[error("upstream body failed: {0}")]
    Body(String),
    /// The buffered (non-streaming) response exceeded the in-memory cap.
    /// Terminal: retrying on another candidate could buffer the cap again per
    /// candidate (review m5).
    #[error("upstream response exceeded the buffering cap")]
    TooLarge,
    /// The stored credential or its headers could not be decrypted or were
    /// not valid header material. Configuration problem, never retryable.
    #[error("credential material unusable: {0}")]
    Credential(String),
}

/// The error taxonomy row an upstream failure maps to: the client-visible
/// `error.code` and HTTP status, whether failover is allowed before the first
/// byte, and the credential cooldown it implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureClass {
    pub code: &'static str,
    pub status: StatusCode,
    /// Failover is allowed only before the first byte reaches the client; this
    /// marks the classes that may fail over at all (never for 400/401/403).
    pub retryable_before_first_byte: bool,
    /// Cooldown to record on the credential (`Retry-After` when present).
    pub cooldown: Option<Duration>,
}

impl UpstreamError {
    /// Maps the failure onto the taxonomy table of the M3 spec. Position
    /// (before/after the first byte) is applied by the caller: failover is
    /// only ever allowed before the first byte. Logs a `warn` with the failure
    /// and its class (review m3); the error strings are secret-free.
    pub fn classify(&self) -> FailureClass {
        let class = |code, status, retryable, cooldown| FailureClass {
            code,
            status,
            retryable_before_first_byte: retryable,
            cooldown,
        };
        let class = match self {
            // 400/422 are returned to the caller; 401/403 cool the credential
            // down but are never retried (spec failure rules).
            UpstreamError::Http {
                status: 400 | 422, ..
            } => class("invalid_request", StatusCode::BAD_REQUEST, false, None),
            UpstreamError::Http {
                status: 401 | 403,
                retry_after,
            } => class(
                "upstream_auth",
                StatusCode::BAD_GATEWAY,
                false,
                Some(retry_after.unwrap_or(DEFAULT_COOLDOWN)),
            ),
            UpstreamError::Http {
                status: 402,
                retry_after,
            } => class(
                "upstream_quota",
                StatusCode::BAD_GATEWAY,
                true,
                Some(retry_after.unwrap_or(DEFAULT_COOLDOWN)),
            ),
            UpstreamError::Http {
                status: 429,
                retry_after,
            } => class(
                "upstream_rate_limited",
                StatusCode::TOO_MANY_REQUESTS,
                true,
                Some(retry_after.unwrap_or(DEFAULT_COOLDOWN)),
            ),
            // Every other non-2xx status (404, 500, 503, ...) is a transient
            // upstream failure: fail over before the first byte.
            UpstreamError::Http { .. } => {
                class("upstream_error", StatusCode::BAD_GATEWAY, true, None)
            }
            UpstreamError::Timeout { .. } => {
                class("upstream_timeout", StatusCode::GATEWAY_TIMEOUT, true, None)
            }
            UpstreamError::Transport(_) | UpstreamError::Body(_) => class(
                "upstream_stream_failed",
                StatusCode::BAD_GATEWAY,
                true,
                None,
            ),
            // A too-large buffered response is terminal: failover would let
            // every candidate buffer the cap again (review m5).
            UpstreamError::TooLarge => class(
                "upstream_stream_failed",
                StatusCode::BAD_GATEWAY,
                false,
                None,
            ),
            UpstreamError::Credential(_) => {
                class("internal", StatusCode::INTERNAL_SERVER_ERROR, false, None)
            }
        };
        tracing::warn!(error = %self, code = class.code, "upstream call failed");
        class
    }
}

/// A fully built upstream call. `Debug` prints the URL and body shape only —
/// never headers, never the secret.
pub struct UpstreamCall {
    pub url: String,
    /// Merged credential + offering headers (the offering wins on conflict).
    pub headers: BTreeMap<String, String>,
    /// Decrypted bearer secret; zeroized when dropped. Ignored when `headers`
    /// already sets `authorization`.
    pub secret: Zeroizing<String>,
    pub body: Bytes,
}

impl std::fmt::Debug for UpstreamCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamCall")
            .field("url", &self.url)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("secret", &"********")
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// The catalog rows one call is built from: the offering's provider base URL
/// plus the sealed credential and offering material (row ids are the AEAD
/// associated data, D16/D17).
pub struct UpstreamTarget<'a> {
    pub base_url: &'a str,
    pub credential_id: &'a str,
    pub secret_cipher: &'a str,
    pub credential_headers_json: Option<&'a str>,
    pub offering_id: &'a str,
    pub offering_headers_json: Option<&'a str>,
}

impl UpstreamCall {
    /// Builds `POST {base_url}/chat/completions` with the merged headers
    /// ([`merge_headers`], offering wins) and the decrypted secret as bearer
    /// auth. Fails when the stored credential material does not decrypt or the
    /// merged headers are not valid HTTP header material.
    pub fn build(
        secrets: &SecretBox,
        target: &UpstreamTarget<'_>,
        body: serde_json::Value,
    ) -> Result<Self, UpstreamError> {
        let headers = merge_headers(
            secrets,
            target
                .credential_headers_json
                .map(|raw| (raw, target.credential_id)),
            target
                .offering_headers_json
                .map(|raw| (raw, target.offering_id)),
        )
        .map_err(|e| UpstreamError::Credential(e.to_string()))?;
        let secret = Zeroizing::new(
            secrets
                .decrypt(target.secret_cipher, target.credential_id)
                .map_err(|e| UpstreamError::Credential(e.to_string()))?,
        );
        Ok(Self {
            url: format!("{}/chat/completions", target.base_url.trim_end_matches('/')),
            headers,
            secret,
            body: Bytes::from(body.to_string()),
        })
    }
}

/// HTTP client with the configured connect timeout. Shared through `AppState`
/// so upstream connections are pooled across requests.
pub struct UpstreamClient {
    client: reqwest::Client,
}

impl UpstreamClient {
    /// Builds the client. Fails only when the HTTP stack cannot be
    /// constructed.
    pub fn new(connect: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .connect_timeout(connect)
            // A redirecting OpenAI-compatible endpoint is misconfigured, and
            // following one could replay the credential to another host.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { client })
    }

    /// Sends the call and returns the response stream once the status line is
    /// in. Non-2xx responses become [`UpstreamError::Http`]; the first body
    /// chunk is read by [`UpstreamStream::next_chunk`] under the same
    /// first-byte budget.
    pub async fn open(
        &self,
        call: &UpstreamCall,
        budget: Budget,
    ) -> Result<UpstreamStream, UpstreamError> {
        let mut request = self
            .client
            .post(&call.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(call.body.clone());
        for (name, value) in &call.headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| UpstreamError::Credential(format!("header name: {e}")))?;
            let value = header_value(name.as_str(), value)?;
            request = request.header(name, value);
        }
        // Bearer unless a merged header already sets `authorization`
        // (offering/credential override wins over the default).
        if !call
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"))
        {
            let mut bearer =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", call.secret.as_str()))
                    .map_err(|e| UpstreamError::Credential(e.to_string()))?;
            bearer.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, bearer);
        }

        let response = match tokio::time::timeout_at(
            budget.first_byte.min(budget.total),
            request.send(),
        )
        .await
        {
            Err(_) => {
                return Err(UpstreamError::Timeout {
                    kind: match tokio::time::Instant::now() >= budget.total {
                        true => TimeoutKind::Total,
                        false => TimeoutKind::FirstByte,
                    },
                });
            }
            Ok(Err(e)) => return Err(UpstreamError::Transport(e.to_string())),
            Ok(Ok(response)) => response,
        };

        let status = response.status();
        if !status.is_success() {
            return Err(UpstreamError::Http {
                status: status.as_u16(),
                retry_after: parse_retry_after(response.headers()),
            });
        }

        Ok(UpstreamStream {
            inner: response.bytes_stream().boxed(),
            budget,
            first: true,
        })
    }
}

/// The open upstream response body. Each [`UpstreamStream::next_chunk`]
/// honours the idle and total budgets; dropping this closes the connection.
pub struct UpstreamStream {
    inner: BoxStream<'static, reqwest::Result<Bytes>>,
    budget: Budget,
    first: bool,
}

impl std::fmt::Debug for UpstreamStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamStream")
            .field("budget", &self.budget)
            .field("first", &self.first)
            .finish_non_exhaustive()
    }
}

impl UpstreamStream {
    /// The next body chunk (`Ok(None)` = clean end), within the first-byte
    /// budget for the first chunk and the idle budget afterwards, never past
    /// the total deadline.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, UpstreamError> {
        let was_first = self.first;
        self.first = false;
        let deadline = match was_first {
            true => self.budget.first_byte.min(self.budget.total),
            false => (tokio::time::Instant::now() + self.budget.idle).min(self.budget.total),
        };
        match tokio::time::timeout_at(deadline, self.inner.next()).await {
            Err(_) => Err(UpstreamError::Timeout {
                kind: match tokio::time::Instant::now() >= self.budget.total {
                    true => TimeoutKind::Total,
                    false if was_first => TimeoutKind::FirstByte,
                    false => TimeoutKind::Idle,
                },
            }),
            Ok(None) => Ok(None),
            Ok(Some(Ok(bytes))) => Ok(Some(bytes)),
            Ok(Some(Err(e))) => Err(UpstreamError::Transport(e.to_string())),
        }
    }
}

/// Builds one wire header value, marking credential-carrying headers
/// (`authorization`, `x-api-key`) sensitive so reqwest redacts them in its own
/// error reporting and debug output (review m4).
fn header_value(name: &str, value: &str) -> Result<reqwest::header::HeaderValue, UpstreamError> {
    let mut value = reqwest::header::HeaderValue::from_str(value)
        .map_err(|e| UpstreamError::Credential(format!("header value: {e}")))?;
    if name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("x-api-key") {
        value.set_sensitive(true);
    }
    Ok(value)
}

/// Parses `Retry-After` as delta-seconds or an HTTP date, when present and
/// sane. Absent or unparseable is `None`; the caller applies the default.
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = raw.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = chrono::DateTime::parse_from_rfc2822(raw.trim()).ok()?;
    let delta = at.signed_duration_since(chrono::Utc::now()).num_seconds();
    Some(Duration::from_secs(delta.max(0) as u64))
}

/// Clamps the requested `max_tokens` to the offering's effective output limit
/// (`max_output_override` or the model's `max_output_tokens`). Unknown limits
/// leave the request untouched.
pub fn clamp_max_tokens(requested: Option<i64>, limit: Option<i64>) -> Option<i64> {
    match (requested, limit) {
        (Some(requested), Some(limit)) => Some(requested.min(limit)),
        (requested, _) => requested,
    }
}

/// Builds the upstream request body: pass-through fields untouched, with
/// `model` rewritten to the upstream id, `stream` set explicitly, `max_tokens`
/// clamped to the offering limit, and — when streaming —
/// `stream_options.include_usage` forced on so the final chunk carries `usage`.
pub fn build_body(
    extra: &serde_json::Map<String, serde_json::Value>,
    messages: &[serde_json::Value],
    model: &str,
    stream: bool,
    max_tokens: Option<i64>,
) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    for (key, value) in extra {
        body.insert(key.clone(), value.clone());
    }
    body.insert("model".to_string(), model.into());
    body.insert("messages".to_string(), messages.to_vec().into());
    body.insert("stream".to_string(), stream.into());
    if let Some(max_tokens) = max_tokens {
        body.insert("max_tokens".to_string(), max_tokens.into());
    }
    if stream {
        let mut options = body
            .remove("stream_options")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        options.insert("include_usage".to_string(), true.into());
        body.insert("stream_options".to_string(), options.into());
    }
    serde_json::Value::Object(body)
}
