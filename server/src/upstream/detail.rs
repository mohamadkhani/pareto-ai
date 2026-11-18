//! Bounded, redacted excerpts of an upstream error response (decision D49).
//!
//! The provider's own error text is what lets an operator tell a bad model id
//! from an exhausted account or an outage, so it is captured — but only as a
//! short excerpt and only after known secrets are removed. Two rules keep this
//! from becoming a body store (D11):
//!
//! * no request or response body and no header is stored, only the provider's
//!   `error.message` / `error.code` / `error.type`, its `x-request-id`, and the
//!   HTTP status;
//! * every string is truncated by **characters** (not bytes) and passed
//!   through [`Redactor`], which removes the credential secret, every custom
//!   header value, and common key shapes (`Bearer …`, `sk-…`). A string that
//!   still contains a known secret after redaction is dropped entirely.
//!
//! The redactor is built where the call is built (it needs the decrypted
//! secret and the merged headers) and travels with the response stream so a
//! transport failure mid-body is redacted the same way.

use std::collections::BTreeMap;

/// Longest provider message kept, in characters.
pub const MAX_MESSAGE_CHARS: usize = 512;
/// Longest provider error code/type kept, in characters.
pub const MAX_CODE_CHARS: usize = 64;
/// Longest provider request id appended to the detail, in characters.
pub const MAX_REQUEST_ID_CHARS: usize = 128;
/// How much of a non-JSON error body is read before parsing gives up.
const MAX_BODY_BYTES: usize = 8 * 1024;
/// How long reading the error body may take before only the status is kept.
const ERROR_BODY_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);
const REDACTED: &str = "[redacted]";

/// What the operator sees for one failed attempt: the provider's own code and
/// a bounded, redacted excerpt of its message. The HTTP status lives next to it
/// in `upstream_status`, so it is not duplicated here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorDetail {
    pub provider_code: Option<String>,
    pub detail: Option<String>,
}

/// Removes known secrets and common key shapes from free text.
#[derive(Debug, Default, Clone)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    /// Builds a redactor from the call's decrypted secret and merged headers.
    /// Values shorter than eight characters are skipped: they are not secret
    /// material and would mangle ordinary words.
    pub fn new(secret: &str, headers: &BTreeMap<String, String>) -> Self {
        let mut secrets = Vec::new();
        if secret.chars().count() >= 4 {
            secrets.push(secret.to_string());
        }
        for value in headers.values() {
            if value.chars().count() >= 8 && !secrets.iter().any(|seen| seen == value) {
                secrets.push(value.clone());
            }
        }
        Self { secrets }
    }

    /// A redactor with no known values; still removes credential-shaped runs.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The text with every known secret and every `Bearer …` / `sk-…` value
    /// replaced by `[redacted]`. If a known secret survives (impossible by
    /// construction, but checked defensively) the whole text is dropped.
    pub fn redact(&self, text: &str) -> String {
        let mut redacted = text.to_string();
        for secret in &self.secrets {
            redacted = redacted.replace(secret, REDACTED);
        }
        redacted = redact_bearer_values(&redacted);
        redacted = redact_prefixed_values(&redacted, "sk-");
        redacted = redact_assignment(&redacted, "api_key");
        redacted = redact_assignment(&redacted, "api-key");
        redacted = redact_assignment(&redacted, "key");
        redacted = redact_assignment(&redacted, "token");
        if self
            .secrets
            .iter()
            .any(|secret| redacted.contains(secret.as_str()))
        {
            return REDACTED.to_string();
        }
        redacted
    }
}

/// Cuts `text` to at most `max` characters without splitting a character.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// Collapses whitespace so a multi-line provider message stays one line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replaces the value after `Bearer ` until the next whitespace/punctuation.
fn redact_bearer_values(text: &str) -> String {
    replace_value_after(text, "bearer ", is_token_char)
}

/// Replaces the run after `prefix` (`sk-`) with `[redacted]` when it looks like
/// a key: at least six token characters.
fn redact_prefixed_values(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let lower = text.to_ascii_lowercase();
    let mut cursor = 0usize;
    while let Some(relative) = lower[cursor..].find(prefix) {
        let start = cursor + relative;
        let value_start = start + prefix.len();
        let value_end = text[value_start..]
            .char_indices()
            .find(|(_, c)| !is_token_char(*c))
            .map(|(index, _)| value_start + index)
            .unwrap_or(text.len());
        if value_end.saturating_sub(value_start) >= 6 {
            out.push_str(&text[cursor..start]);
            out.push_str(prefix);
            out.push_str(REDACTED);
            cursor = value_end;
        } else {
            out.push_str(&text[cursor..value_start]);
            cursor = value_start;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

/// Replaces `name=value` / `name: value` where the value is token-shaped and at
/// least eight characters, e.g. `api_key=abc123…`.
fn redact_assignment(text: &str, name: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(relative) = lower[cursor..].find(name) {
        let start = cursor + relative;
        let mut scan = start + name.len();
        let bytes = text.as_bytes();
        // Optional quote, then `=` or `:` and optional whitespace.
        let mut quoted = false;
        if bytes.get(scan) == Some(&b'"') || bytes.get(scan) == Some(&b'\'') {
            quoted = true;
            scan += 1;
        }
        while bytes.get(scan) == Some(&b' ') {
            scan += 1;
        }
        if !matches!(bytes.get(scan), Some(b'=') | Some(b':')) {
            out.push_str(&text[cursor..start + name.len()]);
            cursor = start + name.len();
            continue;
        }
        scan += 1;
        while bytes.get(scan) == Some(&b' ') {
            scan += 1;
        }
        let value_start = scan;
        let value_end = text[value_start..]
            .char_indices()
            .find(|(_, c)| !is_token_char(*c) && *c != '"' && *c != '\'')
            .map(|(index, _)| value_start + index)
            .unwrap_or(text.len());
        if value_end.saturating_sub(value_start) < 8 {
            out.push_str(&text[cursor..value_start]);
            cursor = value_start;
            continue;
        }
        out.push_str(&text[cursor..value_start]);
        out.push_str(REDACTED);
        cursor = value_end;
        let _ = quoted;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Replaces the token after the first `marker` (e.g. `bearer `).
fn replace_value_after(text: &str, marker: &str, is_char: fn(char) -> bool) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(relative) = lower[cursor..].find(marker) {
        let start = cursor + relative;
        let value_start = start + marker.len();
        let value_end = text[value_start..]
            .char_indices()
            .find(|(_, c)| !is_char(*c))
            .map(|(index, _)| value_start + index)
            .unwrap_or(text.len());
        out.push_str(&text[cursor..value_start]);
        out.push_str(REDACTED);
        cursor = value_end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Characters that can appear in an opaque token (key, bearer value).
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '~' | '+' | '/' | '=')
}

/// Parses a provider error body into [`ErrorDetail`]. A JSON body is read for
/// `error.message` / `error.code` / `error.type` (with top-level fallbacks); a
/// non-JSON body (an HTML proxy page, plain text) keeps its first
/// [`MAX_MESSAGE_CHARS`] characters instead. `request_id` is the provider's
/// `x-request-id`, appended so a support ticket can name the failed call.
pub fn detail_from_body(body: &[u8], request_id: Option<&str>, redactor: &Redactor) -> ErrorDetail {
    let head = &body[..body.len().min(MAX_BODY_BYTES)];
    let raw = String::from_utf8_lossy(head);
    let mut code = None;
    let mut message = None;
    if let Ok(payload) = serde_json::from_str::<serde_json::Value>(raw.trim()) {
        let error = payload.get("error");
        let container = error.filter(|value| value.is_object());
        code = container
            .and_then(|value| value.get("code"))
            .and_then(scalar_text)
            .or_else(|| {
                container
                    .and_then(|value| value.get("type"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .or_else(|| payload.get("code").and_then(scalar_text))
            .or_else(|| {
                payload
                    .get("type")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            });
        message = container
            .and_then(|value| value.get("message"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| error.and_then(|value| value.as_str()).map(str::to_string))
            .or_else(|| {
                payload
                    .get("message")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            });
    }
    let message = message.unwrap_or_else(|| raw.trim().to_string());
    let message = one_line(&message);
    let detail = match message.trim().is_empty() {
        true => None,
        false => Some(truncate_chars(&message, MAX_MESSAGE_CHARS)),
    };
    let detail = match (detail, request_id) {
        (Some(mut detail), Some(id)) => {
            detail.push_str(" (request id: ");
            detail.push_str(&truncate_chars(id, MAX_REQUEST_ID_CHARS));
            detail.push(')');
            Some(detail)
        }
        (detail, _) => detail,
    };
    ErrorDetail {
        provider_code: code
            .map(|code| truncate_chars(&code, MAX_CODE_CHARS))
            .map(|code| redactor.redact(&code))
            .filter(|code| !code.is_empty()),
        detail: detail.map(|detail| redactor.redact(&detail)),
    }
}

fn scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// A provider error extracted from an SSE `data:` payload or a buffered JSON
/// body: its own code and message, before redaction (the caller has the
/// stream's [`Redactor`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub code: Option<String>,
    pub message: String,
}

/// Extracts `error` from a response payload. Handles `{"error":{"message":…}}`
/// and `{"error":"…"}`, plus a top-level `message`/`code` fallback.
pub fn provider_error(payload: &serde_json::Value) -> Option<ProviderError> {
    let error = payload.get("error")?;
    if error.is_null() {
        return None;
    }
    let container = error.as_object();
    let message = container
        .and_then(|value| value.get("message"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| error.as_str().map(str::to_string))
        .or_else(|| {
            payload
                .get("message")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| error.to_string());
    let code = container
        .and_then(|value| value.get("code"))
        .and_then(scalar_text)
        .or_else(|| {
            container
                .and_then(|value| value.get("type"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| payload.get("code").and_then(scalar_text));
    Some(ProviderError { code, message })
}

impl ProviderError {
    /// The stored shape: bounded, one line, redacted.
    pub fn detail(&self, redactor: &Redactor) -> ErrorDetail {
        let message = one_line(&self.message);
        ErrorDetail {
            provider_code: self
                .code
                .as_deref()
                .map(|code| truncate_chars(code, MAX_CODE_CHARS))
                .map(|code| redactor.redact(&code))
                .filter(|code| !code.is_empty()),
            detail: match message.trim().is_empty() {
                true => None,
                false => Some(redactor.redact(&truncate_chars(&message, MAX_MESSAGE_CHARS))),
            },
        }
    }
}

/// Reads at most [`MAX_BODY_BYTES`] of a non-success response under a short
/// budget. `None` when the body could not be read in time or failed — the
/// status alone is still kept.
pub async fn read_error_body(response: &mut reqwest::Response) -> Option<Vec<u8>> {
    let deadline = tokio::time::Instant::now() + ERROR_BODY_BUDGET;
    let mut body = Vec::new();
    loop {
        if body.len() >= MAX_BODY_BYTES {
            break;
        }
        match tokio::time::timeout_at(deadline, response.chunk()).await {
            Err(_) | Ok(Err(_)) => return None,
            Ok(Ok(None)) => break,
            Ok(Ok(Some(chunk))) => {
                let room = MAX_BODY_BYTES - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
        }
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        let mut headers = BTreeMap::new();
        headers.insert("x-api-key".to_string(), "header-secret-123".to_string());
        Redactor::new("sk-live-secret-value", &headers)
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("سلام دنیا", 4), "سلام");
        assert_eq!(truncate_chars("abc", 10), "abc");
    }

    #[test]
    fn redacts_known_secret_header_value_and_key_shapes() {
        let redacted = redactor().redact(
            "bad key sk-live-secret-value and header-secret-123; \
             also Bearer abc.def-123 and sk-proj-abcdefghij and api_key=1234567890abcdef",
        );
        assert!(!redacted.contains("sk-live-secret-value"), "{redacted}");
        assert!(!redacted.contains("header-secret-123"), "{redacted}");
        assert!(!redacted.contains("abc.def-123"), "{redacted}");
        assert!(!redacted.contains("sk-proj-abcdefghij"), "{redacted}");
        assert!(!redacted.contains("1234567890abcdef"), "{redacted}");
        assert!(redacted.contains(REDACTED), "{redacted}");
    }

    #[test]
    fn ordinary_words_are_not_mangled() {
        let text = "model gpt-4o is not available; use key: value in the body";
        assert_eq!(redactor().redact(text), text);
    }

    #[test]
    fn a_json_body_yields_message_code_and_provider_request_id() {
        let body = br#"{"error":{"message":"model\nnot found","code":"model_not_found","type":"invalid_request_error"}}"#;
        let detail = detail_from_body(body, Some("req-abc"), &redactor());
        assert_eq!(detail.provider_code.as_deref(), Some("model_not_found"));
        assert_eq!(
            detail.detail.as_deref(),
            Some("model not found (request id: req-abc)")
        );
    }

    #[test]
    fn a_non_json_body_keeps_a_bounded_excerpt() {
        let body = format!("<html>{}</html>", "x".repeat(2000));
        let detail = detail_from_body(body.as_bytes(), None, &redactor());
        assert_eq!(detail.provider_code, None);
        let text = detail.detail.expect("excerpt");
        assert_eq!(text.chars().count(), MAX_MESSAGE_CHARS);
    }

    #[test]
    fn a_body_with_a_secret_is_redacted_before_storage() {
        let body = br#"{"error":{"message":"bad key sk-live-secret-value"}}"#;
        let detail = detail_from_body(body, None, &redactor());
        assert!(!detail.detail.unwrap().contains("sk-live-secret-value"));
    }

    #[test]
    fn sse_error_payloads_expose_code_and_message() {
        let payload = serde_json::json!({
            "error": { "message": "upstream overloaded", "code": 503, "type": "server_error" }
        });
        let error = provider_error(&payload).expect("error");
        assert_eq!(error.code.as_deref(), Some("503"));
        assert_eq!(error.message, "upstream overloaded");

        let plain = serde_json::json!({"error": "quota exceeded"});
        assert_eq!(
            provider_error(&plain).expect("error").message,
            "quota exceeded"
        );
        assert!(provider_error(&serde_json::json!({"error": null})).is_none());
    }
}
