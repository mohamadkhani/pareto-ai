//! Uniform API error shape.
//!
//! Every non-2xx API response is `{"error": {"code", "message", "request_id"}}`
//! with a sensible HTTP status. The `request_id` is the `x-request-id` value
//! the middleware already put on the request, so a body can be correlated with
//! the request log. It is `null` only when the request never carried one (for
//! example when a handler is invoked outside the HTTP stack, as in tests).
//! Internals and secrets are never included in a message.

use axum::Json;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// `{"error": {...}}`, the body of every API error response.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

/// The error object inside [`ErrorBody`].
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code, e.g. `invalid_credentials`.
    pub code: String,
    /// Human-readable summary. Never contains internals or secrets.
    pub message: String,
    /// The `x-request-id` of the failing request, when present.
    pub request_id: Option<String>,
}

/// A typed HTTP error that serializes into the uniform [`ErrorBody`] shape.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    request_id: Option<String>,
    retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            request_id: None,
            retry_after: None,
        }
    }

    /// 401: no valid session was presented.
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required",
        )
    }

    /// 401: email or password did not match (identical for unknown users).
    pub fn invalid_credentials() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "invalid email or password",
        )
    }

    /// 403: authenticated but not allowed (non-admin on an admin route).
    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "insufficient permissions",
        )
    }

    /// 403: the account exists and the password matched, but it is disabled.
    pub fn account_disabled() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "account_disabled",
            "this account is disabled",
        )
    }

    /// 403: a state-changing request lacked the session's CSRF token.
    pub fn csrf_failed() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "csrf_failed",
            "missing or invalid X-CSRF-Token header",
        )
    }

    /// 404: no API route matched this path.
    pub fn route_not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "route_not_found",
            "no API route matches this path",
        )
    }

    /// 405: the API route exists but not for this method.
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "this route does not accept that method",
        )
    }

    /// 400: malformed request body, query, or parameters.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    /// 429: login rate limit reached; `retry_after` is the lockout in seconds.
    /// 429 (not 423) is used because the condition is a temporary rate limit.
    pub fn locked(retry_after: u64) -> Self {
        Self {
            retry_after: Some(retry_after.max(1)),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "login_locked",
                "too many failed login attempts; try again later",
            )
        }
    }

    /// 500: unexpected failure. Logs `error` with `context` for the operator
    /// and tells the caller nothing beyond the fact that the request failed.
    pub fn internal(context: &'static str, error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, context, "request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal error",
        )
    }

    /// Attaches the request id so the body can be correlated with the logs.
    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        self.request_id = request_id;
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code.to_string(),
                    message: self.message,
                    request_id: self.request_id,
                },
            }),
        )
            .into_response();

        if let Some(retry_after) = self.retry_after
            && let Ok(value) = HeaderValue::from_str(&retry_after.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }

        response
    }
}

/// The `x-request-id` value the request-id middleware stamped on the request.
pub fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn error_body_carries_code_message_and_request_id() {
        let response = ApiError::invalid_credentials()
            .with_request_id(Some("req-123".to_string()))
            .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(value["error"]["code"], "invalid_credentials");
        assert_eq!(value["error"]["message"], "invalid email or password");
        assert_eq!(value["error"]["request_id"], "req-123");
    }

    #[tokio::test]
    async fn locked_error_sets_retry_after() {
        let response = ApiError::locked(120).into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from_static("120"))
        );
    }
}
