//! `POST /v1/chat/completions`: the OpenAI-compatible chat data plane (M3).
//!
//! The handler is thin: it authenticates (an API-key bearer token or an
//! admin session, M4), enforces the body-size limit, and hands the bytes to
//! [`crate::chat`], which validates, resolves an offering, enforces access
//! and spend limits, forwards, streams, and settles usage.
//!
//! `/v1` is **exempt from the CSRF guard**: OpenAI-SDK clients never send
//! `X-CSRF-Token`, and the guard stays on cookie-authenticated `/api/*`.
//!
//! Request ids (review B1): the ledger key is **server-generated** (the
//! request-id middleware's stamp or a fresh UUID) and is what error bodies and
//! the response `x-request-id` header carry. The client's optional
//! `x-request-id` echo is preserved only in the ledger's `client_request_id`
//! column — never as a key and never echoed back.
//!
//! Client-visible contract (a future UI must know this): `stream: true`
//! answers `200 text/event-stream` and forwards the upstream SSE untouched
//! (`data:` lines and `data: [DONE]` included). If the stream dies after the
//! first byte the status cannot change anymore, so the failure surfaces
//! in-band as one `data: {"error": {"code", "message", "request_id"}}` event
//! in the uniform error shape (preceded by `\n\n` so it cannot concatenate
//! onto a partial upstream line), followed by `data: [DONE]`, and then the
//! stream ends. `stream: false` answers a complete `application/json` body,
//! and every failure is a plain error response in the uniform shape.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use futures_util::StreamExt;
use serde::Serialize;
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::DataPlaneAuth;
use crate::chat;
use crate::error::{self, ApiError, ErrorBody};

/// The OpenAI-compatible chat completion request. Fields not listed here
/// (`temperature`, `top_p`, `tools`, ...) pass through to the upstream
/// untouched.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatCompletionRequest {
    /// Model to route: an upstream model id, a canonical name, or a combo in
    /// the `combo:<name>` namespace (M5 — expanded to its member offerings
    /// and routed by the combo's strategy).
    pub model: String,
    /// Conversation so far; at most 200 messages.
    pub messages: Vec<serde_json::Value>,
    /// Stream SSE chunks instead of one JSON body. Defaults to false.
    #[serde(default)]
    pub stream: bool,
    /// Maximum output tokens, clamped to the offering's output limit.
    pub max_tokens: Option<i64>,
}

/// A non-streaming completion body, passed through from the upstream
/// (documented shape; the gateway does not rewrite it).
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub model: String,
    pub choices: Vec<serde_json::Value>,
    /// Token accounting of the completion.
    pub usage: Option<ChatUsage>,
}

/// Token counts of one completion, as reported by the upstream.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatUsage {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

/// Handles one chat completion. Auth: `Authorization: Bearer pk_live_...`
/// (any account, subject to the key's grants and rate limits) or an admin
/// session (M3 behaviour kept for the dashboard); CSRF-exempt like the rest
/// of `/v1`.
///
/// Failover note: the candidate order is resolved once per request, before
/// the first attempt; a cooldown recorded while attempt 1 is failing (an
/// upstream 401/403/402/429) therefore does not re-order the candidates of
/// this same request — it demotes the credential only for later requests.
#[utoipa::path(
    post,
    path = "/v1/chat/completions",
    tag = "chat",
    request_body = ChatCompletionRequest,
    responses(
        (status = 200,
         description = "the completion: text/event-stream when `stream` is true \
                        (SSE chunks forwarded verbatim, errors in-band after first byte), \
                        application/json otherwise",
         content(
            (String = "text/event-stream"),
            (ChatCompletionResponse = "application/json")
         )),
        (status = 400, description = "invalid_request or context_length_exceeded", body = ErrorBody),
        (status = 401, description = "unauthorized, invalid_api_key, api_key_revoked, or api_key_expired", body = ErrorBody),
        (status = 402, description = "budget_exceeded: the spend cap refused the request before forwarding", body = ErrorBody),
        (status = 403, description = "forbidden (non-admin session), access_denied (grants), or password_change_required", body = ErrorBody),
        (status = 404, description = "model_not_found", body = ErrorBody),
        (status = 429, description = "rate_limited (per-key RPM/TPM, Retry-After), token_cap_exceeded, or upstream_rate_limited", body = ErrorBody),
        (status = 502, description = "upstream_auth, upstream_quota, upstream_error, or upstream_stream_failed", body = ErrorBody),
        (status = 504, description = "upstream_timeout", body = ErrorBody)
    )
)]
pub async fn chat_completions(
    State(state): State<AppState>,
    DataPlaneAuth(caller): DataPlaneAuth,
    req: Request,
) -> Result<Response, ApiError> {
    // The ledger key is server-generated (the middleware's stamp or a fresh
    // UUID); the client's echo goes only into `client_request_id` (review B1).
    let rid = error::request_id(req.headers()).or_else(|| Some(uuid::Uuid::new_v4().to_string()));
    let client_rid = crate::request_id::client_request_id(req.extensions());
    let session_hint = req
        .headers()
        .get("x-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let meta = chat::RequestMeta {
        request_id: rid.clone().unwrap_or_default(),
        client_request_id: client_rid,
        user_id: caller.user_id,
        role: caller.role,
        api_key: caller.api_key,
        session_hint,
        started_at: crate::catalog::now_ts(),
        method: "POST".to_string(),
        path: "/v1/chat/completions".to_string(),
    };

    // Read the body with a live byte count so `bytes_in` is the size actually
    // received — not the configured limit (review nit). The read stops as soon
    // as the limit is crossed; the unread remainder is never pulled.
    let limit = state.config.max_body_bytes;
    let mut frames = req.into_body().into_data_stream();
    let mut body = Vec::new();
    let mut bytes_in = 0u64;
    let mut rejected = None;
    while let Some(frame) = frames.next().await {
        match frame {
            Ok(chunk) => {
                bytes_in += chunk.len() as u64;
                if bytes_in > limit as u64 {
                    rejected = Some(chat::ChatFailure {
                        status: StatusCode::BAD_REQUEST,
                        code: "invalid_request",
                        message: format!("request body exceeds the {limit} byte limit"),
                        retry_after: None,
                    });
                    break;
                }
                body.extend_from_slice(&chunk);
            }
            Err(e) => {
                tracing::debug!(error = %e, "chat request body rejected");
                rejected = Some(chat::ChatFailure {
                    status: StatusCode::BAD_REQUEST,
                    code: "invalid_request",
                    message: "could not read the request body".to_string(),
                    retry_after: None,
                });
                break;
            }
        }
    }
    if let Some(failure) = rejected {
        // Settled before the error response leaves: every completion has
        // its rows.
        chat::record_rejection(&state, &meta, bytes_in, &failure).await;
        return Err(failure.into_api_error(rid.clone()));
    }

    match chat::execute(&state, &meta, Bytes::from(body)).await {
        Ok(chat::ChatOutcome::Json(bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bytes))
            .map_err(|e| ApiError::internal("building response", e).with_request_id(rid.clone())),
        Ok(chat::ChatOutcome::Sse(stream)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(stream)
            .map_err(|e| ApiError::internal("building response", e).with_request_id(rid.clone())),
        Err(failure) => Err(failure.into_api_error(rid)),
    }
}
