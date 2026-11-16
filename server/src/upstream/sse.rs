//! SSE inspection for the upstream chat stream.
//!
//! The stream is forwarded to the client untouched; this module only *watches*
//! it: it extracts the `usage` object from the final chunk (input, output,
//! cache, and reasoning tokens), notices `[DONE]`, and flags protocol errors
//! (a `data:` payload that is not JSON, or an upstream `error` event) and
//! truncation (stream end without `[DONE]`). It also counts output **content
//! characters** so the estimation fallback (`chars/4`) has something to work
//! with: only `choices[].message.content` / `choices[].delta.content` (plus
//! `reasoning` when present) is counted — never JSON scaffolding or raw body
//! bytes (review M1).
//!
//! Known limitation: SSE events built from several `data:` lines (one JSON
//! payload continued across lines) are not reassembled; each `data:` line is
//! inspected as its own payload, so such an event is flagged `protocol_error`.
//! Forwarding is unaffected — the bytes reach the client untouched either way.
//!
//! The line buffer is capped at [`MAX_LINE_BYTES`]: a single huge line (or a
//! stream with no `\n` at all, e.g. CR-only line endings) can otherwise
//! accumulate without bound per request (review M2). Overflow sets
//! [`SseInspector::overflowed`]; the caller must end the stream with an
//! in-band `upstream_stream_failed` error and never fail over.

use serde_json::Value;

/// Token counts as reported by an OpenAI-compatible `usage` object. Fields the
/// upstream omits stay `None` and are stored as NULL — never as 0. A negative
/// count is garbage from a broken upstream and is clamped to `0` at parse
/// time (review M2) so no downstream arithmetic can ever *credit* a budget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
}

/// One non-negative token count from the `usage` object: missing or negative
/// values become `Some(0)` / `None` — never a negative (review M2).
fn non_negative_int(value: &serde_json::Value, key: &str) -> Option<i64> {
    value
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .map(|count| count.max(0))
}

/// Extracts `usage` from a chunk object or a non-streaming response body.
/// `None` when the object carries no (or a null) `usage`. Every component is
/// clamped to be non-negative (review M2): `prompt_tokens: -5000000` books 0,
/// never a credit.
pub fn usage_from_json(value: &serde_json::Value) -> Option<Usage> {
    let usage = value.get("usage")?;
    if !usage.is_object() {
        return None;
    }
    let int = |key: &str| non_negative_int(usage, key);
    let details_int = |section: &str, keys: &[&str]| {
        let details = usage.get(section)?;
        keys.iter().find_map(|key| non_negative_int(details, key))
    };
    Some(Usage {
        input_tokens: int("prompt_tokens"),
        output_tokens: int("completion_tokens"),
        cache_read_tokens: details_int("prompt_tokens_details", &["cached_tokens"]),
        cache_write_tokens: details_int(
            "prompt_tokens_details",
            &["cache_creation_tokens", "cache_creation_input_tokens"],
        ),
        reasoning_tokens: details_int("completion_tokens_details", &["reasoning_tokens"]),
    })
}

/// Characters of one JSON string or text-part array (multimodal content).
/// Shared with the input estimator in [`crate::chat`].
pub fn text_chars(value: &Value) -> usize {
    match value {
        Value::String(text) => text.chars().count(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text"))
            .map(text_chars)
            .sum(),
        _ => 0,
    }
}

/// Content characters of one completion chunk: `choices[].message.content` /
/// `choices[].delta.content`, plus `reasoning` when present. JSON scaffolding
/// and every other field count for nothing.
pub fn content_chars(payload: &Value) -> usize {
    let Some(choices) = payload.get("choices").and_then(Value::as_array) else {
        return 0;
    };
    let field = |container: &Value, key: &str| container.get(key).map(text_chars).unwrap_or(0);
    choices
        .iter()
        .filter_map(|choice| choice.get("message").or_else(|| choice.get("delta")))
        .map(|container| field(container, "content") + field(container, "reasoning"))
        .sum()
}

/// The largest incomplete `data:` line kept between chunks; see the module
/// docs (review M2).
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Incremental SSE parser: feed it the forwarded bytes, ask it at the end what
/// it saw. Line buffering is internal, so a `data:` line split across two
/// chunks is parsed once, not twice.
#[derive(Debug, Default)]
pub struct SseInspector {
    partial: Vec<u8>,
    /// Last `usage` seen in a `data:` payload (the final chunk's wins).
    pub usage: Option<Usage>,
    /// `[DONE]` was received: the stream ended cleanly.
    pub saw_done: bool,
    /// A `data:` payload was not JSON, or carried a non-null `error`.
    pub protocol_error: bool,
    /// The line buffer overflowed [`MAX_LINE_BYTES`]; the stream is doomed and
    /// nothing else is inspected.
    pub overflowed: bool,
    /// Content characters of `data:` payloads forwarded (excluding `[DONE]`),
    /// for the token estimation fallback — never scaffolding, never bytes.
    pub output_chars: usize,
}

impl SseInspector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the next body chunk. Bytes are only inspected, never modified.
    /// Stops inspecting entirely once [`Self::overflowed`] is set.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.overflowed {
            return;
        }
        self.partial.extend_from_slice(bytes);
        while let Some(newline) = self.partial.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.partial.drain(..=newline).collect();
            line.pop(); // '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.len() > MAX_LINE_BYTES {
                self.overflow();
                return;
            }
            self.handle_line(&line);
        }
        if self.partial.len() > MAX_LINE_BYTES {
            self.overflow();
        }
    }

    /// True when the stream is over and was not cleanly terminated.
    pub fn truncated(&self) -> bool {
        !self.saw_done
    }

    /// The stream failed in a way that must end it in-band with
    /// `upstream_stream_failed` (malformed payload or line-buffer overflow).
    /// Neither may fail over: by the time this is true, bytes may already have
    /// reached the client.
    pub fn failed(&self) -> bool {
        self.protocol_error || self.overflowed
    }

    fn overflow(&mut self) {
        self.overflowed = true;
        self.partial.clear();
    }

    fn handle_line(&mut self, line: &[u8]) {
        // SSE comments (`: keep-alive`) and blank lines carry no data.
        if line.is_empty() || line[0] == b':' {
            return;
        }
        let Some(colon) = line.iter().position(|b| *b == b':') else {
            // A bare field name with an empty value; nothing to inspect.
            return;
        };
        if &line[..colon] != b"data" {
            return;
        }
        let mut value = &line[colon + 1..];
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        let text = String::from_utf8_lossy(value);
        if text.trim() == "[DONE]" {
            self.saw_done = true;
            return;
        }
        match serde_json::from_str::<serde_json::Value>(&text) {
            Err(_) => self.protocol_error = true,
            Ok(payload) => {
                if payload.get("error").is_some_and(|e| !e.is_null()) {
                    self.protocol_error = true;
                }
                self.output_chars += content_chars(&payload);
                if let Some(usage) = usage_from_json(&payload) {
                    self.usage = Some(usage);
                }
            }
        }
    }
}

/// Extracts `usage` and content characters from a non-streaming response body
/// and flags an unparseable or errored body, mirroring what [`SseInspector`]
/// does for streams. Content characters: `choices[].message.content` (plus
/// `reasoning`), never the JSON scaffolding (review M1).
pub fn inspect_json_body(body: &[u8]) -> (Option<Usage>, usize, bool) {
    let text = String::from_utf8_lossy(body);
    match serde_json::from_str::<serde_json::Value>(&text) {
        Err(_) => (None, 0, true),
        Ok(payload) => {
            let protocol_error = payload.get("error").is_some_and(|e| !e.is_null());
            (
                usage_from_json(&payload),
                content_chars(&payload),
                protocol_error,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_full_usage_from_the_final_chunk() {
        let mut inspector = SseInspector::new();
        inspector.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n");
        assert!(inspector.usage.is_none());
        inspector.push(
            b"data: {\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\
             \"prompt_tokens_details\":{\"cached_tokens\":3,\"cache_creation_tokens\":2},\
             \"completion_tokens_details\":{\"reasoning_tokens\":4}}}\n\n",
        );
        inspector.push(b"data: [DONE]\n\n");
        assert!(!inspector.truncated());
        assert!(!inspector.protocol_error);
        assert_eq!(
            inspector.usage,
            Some(Usage {
                input_tokens: Some(11),
                output_tokens: Some(7),
                cache_read_tokens: Some(3),
                cache_write_tokens: Some(2),
                reasoning_tokens: Some(4),
            })
        );
    }

    #[test]
    fn missing_fields_stay_none_and_null_usage_is_missing() {
        let mut inspector = SseInspector::new();
        inspector.push(b"data: {\"usage\":{\"prompt_tokens\":5}}\n\n");
        assert_eq!(
            inspector.usage,
            Some(Usage {
                input_tokens: Some(5),
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            })
        );

        let mut null_usage = SseInspector::new();
        null_usage.push(b"data: {\"usage\":null}\n\n");
        assert!(null_usage.usage.is_none(), "null usage is missing usage");

        // OpenRouter-style cache creation lives under a different key.
        let mut openrouter = SseInspector::new();
        openrouter.push(
            b"data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\
             \"prompt_tokens_details\":{\"cache_creation_input_tokens\":9}}}\n\n",
        );
        assert_eq!(openrouter.usage.unwrap().cache_write_tokens, Some(9));
    }

    #[test]
    fn hostile_usage_counts_are_clamped_never_negative() {
        // A lying upstream: negative counts and i64 extremes. The parse clamps
        // to 0 so no downstream arithmetic can credit a budget or overflow
        // (review M2).
        let mut hostile = SseInspector::new();
        hostile.push(
            b"data: {\"usage\":{\"prompt_tokens\":-5000000,\"completion_tokens\":-1,\
             \"prompt_tokens_details\":{\"cached_tokens\":-3,\"cache_creation_tokens\":-4},\
             \"completion_tokens_details\":{\"reasoning_tokens\":-9223372036854775808}}}\n\n",
        );
        assert_eq!(
            hostile.usage,
            Some(Usage {
                input_tokens: Some(0),
                output_tokens: Some(0),
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(0),
                reasoning_tokens: Some(0),
            }),
            "negative counts book 0, never a credit"
        );

        // Absurd but non-negative extremes parse without wrapping.
        let mut extreme = SseInspector::new();
        extreme.push(
            b"data: {\"usage\":{\"prompt_tokens\":9223372036854775807,\
             \"completion_tokens\":9223372036854775807,\
             \"completion_tokens_details\":{\"reasoning_tokens\":9223372036854775807}}}\n\n",
        );
        let usage = extreme.usage.expect("usage");
        assert_eq!(usage.input_tokens, Some(i64::MAX));
        assert_eq!(usage.reasoning_tokens, Some(i64::MAX));
    }

    #[test]
    fn line_split_across_chunks_is_parsed_once() {
        let mut inspector = SseInspector::new();
        inspector.push(b"data: {\"usage\":{\"prom");
        assert!(inspector.usage.is_none(), "no complete line yet");
        inspector.push(b"pt_tokens\":5}}\n\ndata: [DONE]\n\n");
        assert_eq!(inspector.usage.unwrap().input_tokens, Some(5));
        assert!(inspector.saw_done);
    }

    #[test]
    fn malformed_and_error_payloads_are_protocol_errors() {
        let mut malformed = SseInspector::new();
        malformed.push(b"data: {not json\n\n");
        assert!(malformed.protocol_error);
        assert!(malformed.failed());
        assert!(malformed.truncated(), "no [DONE] was seen");

        let mut error_event = SseInspector::new();
        error_event.push(b"data: {\"error\":{\"message\":\"boom\"}}\n\n");
        assert!(
            error_event.protocol_error,
            "upstream error events are failures"
        );

        let mut ok = SseInspector::new();
        ok.push(b": keep-alive\n\nevent: message\ndata: {\"usage\":{}}\n\n");
        assert!(
            !ok.protocol_error,
            "comments, blank lines, and other fields are fine"
        );
    }

    #[test]
    fn output_chars_count_content_not_json_scaffolding() {
        // 39 characters on the wire, one content character: the estimate must
        // see the "x", not the frame around it (review M1).
        let frame = r#"{"choices":[{"delta":{"content":"x"}}]}"#;
        assert_eq!(frame.chars().count(), 39);

        let mut inspector = SseInspector::new();
        inspector.push(format!("data: {frame}\n\n").as_bytes());
        inspector.push(b"data: [DONE]\n\n");
        assert_eq!(inspector.output_chars, 1, "content only, not {frame}");

        // `reasoning` counts too; empty and non-content fields do not.
        let mut reasoning = SseInspector::new();
        reasoning.push(b"data: {\"choices\":[{\"delta\":{\"reasoning\":\"abcd\"}}]}\n\n");
        reasoning.push(b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n");
        assert_eq!(reasoning.output_chars, 4);

        // A non-streaming body mirrors the stream: "hello", not 43 characters.
        let body = br#"{"choices":[{"message":{"content":"hello"}}]}"#;
        let (usage, chars, protocol_error) = inspect_json_body(body);
        assert!(usage.is_none(), "no usage field means no usage");
        assert_eq!(chars, 5);
        assert!(!protocol_error);
        let (_, chars, protocol_error) = inspect_json_body(b"not json");
        assert_eq!(chars, 0, "unparseable bodies claim no content");
        assert!(protocol_error, "an unparseable body is a body failure");
        let (usage, _, protocol_error) = inspect_json_body(br#"{"error":{"message":"x"}}"#);
        assert!(usage.is_none());
        assert!(protocol_error);
    }

    #[test]
    fn a_single_huge_line_overflows_the_line_cap() {
        // One complete line larger than the cap, fed in two pushes.
        let huge = format!("data: {}\n\n", "x".repeat(MAX_LINE_BYTES + 1));
        let (head, tail) = huge.split_at(MAX_LINE_BYTES / 2);

        let mut inspector = SseInspector::new();
        inspector.push(head.as_bytes());
        assert!(!inspector.overflowed, "half a line fits");
        inspector.push(tail.as_bytes());
        assert!(inspector.overflowed, "the completed line is over the cap");
        assert!(inspector.failed());
        assert_eq!(
            inspector.output_chars, 0,
            "nothing is inspected after overflow"
        );

        // Once overflowed the inspector stays done: later pushes are ignored.
        inspector.push(b"data: {\"usage\":{\"prompt_tokens\":5}}\n\n");
        assert!(inspector.usage.is_none());
    }

    #[test]
    fn cr_only_line_endings_overflow_the_cap_too() {
        // SSE lines separated by bare CR never form a `\n`-terminated line, so
        // without the cap they would accumulate forever.
        let cr_line = format!("data: {}\r", "y".repeat(128));
        let mut inspector = SseInspector::new();
        let mut fed = 0;
        while fed <= MAX_LINE_BYTES {
            inspector.push(cr_line.as_bytes());
            fed += cr_line.len();
        }
        assert!(
            inspector.overflowed,
            "unterminated lines are capped at 1 MiB"
        );
        assert!(inspector.failed());
    }
}
