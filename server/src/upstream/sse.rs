//! SSE inspection for the upstream chat stream.
//!
//! The stream is forwarded to the client untouched; this module only *watches*
//! it: it extracts the `usage` object from the final chunk (input, output,
//! cache, and reasoning tokens), notices `[DONE]`, and flags protocol errors
//! (a `data:` payload that is not JSON, or an upstream `error` event) and
//! truncation (stream end without `[DONE]`). It also counts output characters
//! so the estimation fallback (`chars/4`) has something to work with.

/// Token counts as reported by an OpenAI-compatible `usage` object. Fields the
/// upstream omits stay `None` and are stored as NULL — never as 0.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
}

/// Extracts `usage` from a chunk object or a non-streaming response body.
/// `None` when the object carries no (or a null) `usage`.
pub fn usage_from_json(value: &serde_json::Value) -> Option<Usage> {
    let usage = value.get("usage")?;
    if !usage.is_object() {
        return None;
    }
    let int = |key: &str| usage.get(key).and_then(serde_json::Value::as_i64);
    let details_int = |section: &str, keys: &[&str]| {
        let details = usage.get(section)?;
        keys.iter()
            .find_map(|key| details.get(*key).and_then(serde_json::Value::as_i64))
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
    /// Characters of `data:` payloads forwarded (excluding `[DONE]`), used
    /// for the token estimation fallback.
    pub output_chars: usize,
}

impl SseInspector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the next body chunk. Bytes are only inspected, never modified.
    pub fn push(&mut self, bytes: &[u8]) {
        self.partial.extend_from_slice(bytes);
        while let Some(newline) = self.partial.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.partial.drain(..=newline).collect();
            line.pop(); // '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.handle_line(&line);
        }
    }

    /// True when the stream is over and was not cleanly terminated.
    pub fn truncated(&self) -> bool {
        !self.saw_done
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
        self.output_chars += text.chars().count();
        match serde_json::from_str::<serde_json::Value>(&text) {
            Err(_) => self.protocol_error = true,
            Ok(payload) => {
                if payload.get("error").is_some_and(|e| !e.is_null()) {
                    self.protocol_error = true;
                }
                if let Some(usage) = usage_from_json(&payload) {
                    self.usage = Some(usage);
                }
            }
        }
    }
}

/// Counts the payload bytes of a non-streaming response body and extracts its
/// `usage`, mirroring what [`SseInspector`] does for streams.
pub fn inspect_json_body(body: &[u8]) -> (Option<Usage>, usize, bool) {
    let text = String::from_utf8_lossy(body);
    match serde_json::from_str::<serde_json::Value>(&text) {
        Err(_) => (None, text.chars().count(), true),
        Ok(payload) => {
            let protocol_error = payload.get("error").is_some_and(|e| !e.is_null());
            (
                usage_from_json(&payload),
                text.chars().count(),
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
    fn output_chars_count_data_payloads_without_done() {
        let mut inspector = SseInspector::new();
        inspector.push(b"data: {\"a\":1}\n\ndata: [DONE]\n\n");
        assert_eq!(inspector.output_chars, "{\"a\":1}".chars().count());
    }

    #[test]
    fn json_body_inspection_mirrors_the_stream() {
        let body = br#"{"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":8}}"#;
        let (usage, chars, protocol_error) = inspect_json_body(body);
        assert_eq!(usage.unwrap().input_tokens, Some(4));
        assert_eq!(chars, body.len());
        assert!(!protocol_error);

        let (usage, _, protocol_error) = inspect_json_body(b"not json");
        assert!(usage.is_none());
        assert!(protocol_error, "an unparseable body is a body failure");

        let (usage, _, protocol_error) = inspect_json_body(br#"{"error":{"message":"x"}}"#);
        assert!(usage.is_none());
        assert!(protocol_error);
    }
}
