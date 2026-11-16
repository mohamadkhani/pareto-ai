//! `POST /v1/chat/completions`: the OpenAI-compatible chat data plane (M3).
//!
//! The handler is thin: it authenticates (admin session until M4 API keys),
//! enforces the body-size limit, and hands the bytes to [`crate::chat`], which
//! validates, resolves an offering, forwards, streams, and settles usage.
//!
//! `/v1` is **exempt from the CSRF guard**: OpenAI-SDK clients never send
//! `X-CSRF-Token`, and the guard stays on cookie-authenticated `/api/*`.
//!
//! Client-visible contract (a future UI must know this): `stream: true`
//! answers `200 text/event-stream` and forwards the upstream SSE untouched
//! (`data:` lines and `data: [DONE]` included). If the stream dies after the
//! first byte the status cannot change anymore, so the failure surfaces
//! in-band as one `data: {"error": {"code", "message", "request_id"}}` event
//! in the uniform error shape, followed by `data: [DONE]`, and then the stream
//! ends. `stream: false` answers a complete `application/json` body, and every
//! failure is a plain error response in the uniform shape.

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use serde::Serialize;
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::RequireAdmin;
use crate::chat;
use crate::error::{self, ApiError, ErrorBody};

/// The OpenAI-compatible chat completion request. Fields not listed here
/// (`temperature`, `top_p`, `tools`, ...) pass through to the upstream
/// untouched.
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatCompletionRequest {
    /// Model to route (an upstream model id or a canonical name).
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

/// Handles one chat completion. Admin session required (M4 adds API keys);
/// CSRF-exempt like the rest of `/v1`.
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
        (status = 401, description = "not authenticated", body = ErrorBody),
        (status = 403, description = "not an admin", body = ErrorBody),
        (status = 404, description = "model_not_found", body = ErrorBody),
        (status = 429, description = "upstream_rate_limited", body = ErrorBody),
        (status = 502, description = "upstream_auth, upstream_quota, upstream_error, or upstream_stream_failed", body = ErrorBody),
        (status = 504, description = "upstream_timeout", body = ErrorBody)
    )
)]
pub async fn chat_completions(
    State(state): State<AppState>,
    RequireAdmin(admin): RequireAdmin,
    req: Request,
) -> Result<Response, ApiError> {
    let rid = error::request_id(req.headers()).or_else(|| Some(uuid::Uuid::new_v4().to_string()));
    let meta = chat::RequestMeta {
        request_id: rid.clone().unwrap_or_default(),
        user_id: admin.user_id,
        method: "POST".to_string(),
        path: "/v1/chat/completions".to_string(),
    };

    let limit = state.config.max_body_bytes;
    let body = match to_bytes(req.into_body(), limit).await {
        Ok(body) => body,
        Err(e) => {
            tracing::debug!(error = %e, "chat request body rejected");
            let failure = chat::ChatFailure {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_request",
                message: format!("request body exceeds the {limit} byte limit"),
                retry_after: None,
            };
            // Settled before the error response leaves: every completion has
            // its rows.
            chat::record_rejection(&state, &meta, limit as u64, &failure).await;
            return Err(failure.into_api_error(rid.clone()));
        }
    };

    match chat::execute(&state, &meta, body).await {
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
