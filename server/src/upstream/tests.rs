//! Unit tests for the upstream client: request build (headers merged, bearer
//! inserted), the error taxonomy mapping, and chunked reads against the
//! scripted stub.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::*;
use crate::secrets::MasterKey;
use crate::upstream::stub::{Scenario, Stub};

fn secrets() -> SecretBox {
    SecretBox::new(MasterKey::from_bytes([7u8; 32]))
}

fn sealed(pairs: &[(&str, &str)], row_id: &str) -> String {
    let map: BTreeMap<String, serde_json::Value> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
        .collect();
    let normalized = crate::catalog::headers::normalize(&map).expect("normalize");
    crate::catalog::headers::seal(&normalized, &secrets(), row_id).expect("seal")
}

fn budget() -> Budget {
    Budget {
        first_byte: deadline_in(Duration::from_secs(5)),
        idle: Duration::from_secs(5),
        total: deadline_in(Duration::from_secs(10)),
    }
}

fn deadline_in(d: Duration) -> tokio::time::Instant {
    tokio::time::Instant::now() + d
}

fn build_target<'a>(
    base_url: &'a str,
    credential_id: &'a str,
    secret_cipher: &'a str,
    credential_headers_json: Option<&'a str>,
    offering_headers_json: Option<&'a str>,
) -> UpstreamTarget<'a> {
    UpstreamTarget {
        base_url,
        credential_id,
        secret_cipher,
        credential_headers_json,
        offering_id: "off-1",
        offering_headers_json,
    }
}

#[test]
fn build_merges_headers_offering_wins_and_never_debugs_secrets() {
    let boxx = secrets();
    let secret_cipher = boxx.encrypt("sk-live-secret", "cred-1").expect("seal");
    let cred_headers = sealed(&[("x-tenant", "team-a"), ("x-region", "eu")], "cred-1");
    let off_headers = sealed(&[("x-region", "us")], "off-1");

    let call = UpstreamCall::build(
        &boxx,
        &build_target(
            "http://upstream.example/v1/",
            "cred-1",
            &secret_cipher,
            Some(&cred_headers),
            Some(&off_headers),
        ),
        serde_json::json!({"model": "m"}),
    )
    .expect("build");
    assert_eq!(call.url, "http://upstream.example/v1/chat/completions");
    assert_eq!(
        call.headers.get("x-tenant").map(String::as_str),
        Some("team-a")
    );
    assert_eq!(call.headers.get("x-region").map(String::as_str), Some("us"));
    assert_eq!(call.secret.as_str(), "sk-live-secret");

    let debug = format!("{call:?}");
    assert!(!debug.contains("sk-live-secret"), "{debug}");
    assert!(!debug.contains("team-a"), "{debug}");
}

#[test]
fn build_fails_on_foreign_row_material_without_leaking_it() {
    let boxx = secrets();
    let secret_cipher = boxx.encrypt("sk-live-secret", "cred-1").expect("seal");
    let cred_headers = sealed(&[("x-tenant", "team-a")], "cred-1");
    let off_headers = sealed(&[("x-region", "us")], "off-1");

    let err = UpstreamCall::build(
        &boxx,
        &build_target(
            "http://upstream.example",
            "cred-2", // wrong row id: the ciphertext must not open
            &secret_cipher,
            Some(&cred_headers),
            Some(&off_headers),
        ),
        serde_json::json!({}),
    )
    .expect_err("wrong row id must fail");
    let message = format!("{err}");
    assert!(!message.contains("sk-live-secret"), "{message}");
    assert!(matches!(err, UpstreamError::Credential(_)));
    assert_eq!(err.classify().code, "internal");
}

#[test]
fn clamp_max_tokens_respects_the_offering_limit() {
    assert_eq!(clamp_max_tokens(Some(100), Some(64)), Some(64));
    assert_eq!(clamp_max_tokens(Some(10), Some(64)), Some(10));
    assert_eq!(clamp_max_tokens(Some(10), None), Some(10));
    assert_eq!(clamp_max_tokens(None, Some(64)), None);
    assert_eq!(clamp_max_tokens(None, None), None);
}

#[test]
fn build_body_rewrites_the_wire_fields_and_keeps_extras() {
    let mut extra = serde_json::Map::new();
    extra.insert("temperature".to_string(), 0.5.into());
    extra.insert(
        "stream_options".to_string(),
        serde_json::json!({"include_usage": false, "x": 1}),
    );
    let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];

    let body = build_body(&extra, &messages, "upstream-model", true, Some(128));
    assert_eq!(body["model"], "upstream-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 128);
    assert_eq!(body["messages"], serde_json::json!(messages));
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(
        body["stream_options"],
        serde_json::json!({"include_usage": true, "x": 1}),
        "include_usage is forced on and existing keys survive"
    );

    let non_stream = build_body(&extra, &messages, "upstream-model", false, None);
    assert_eq!(non_stream["stream"], false);
    assert!(
        non_stream.get("max_tokens").is_none(),
        "absent stays absent"
    );
    assert_eq!(
        non_stream["stream_options"],
        serde_json::json!({"include_usage": false, "x": 1}),
        "stream_options is untouched when not streaming"
    );
}

#[test]
fn classify_covers_the_taxonomy_table() {
    let cases: Vec<(UpstreamError, &str, StatusCode, bool)> = vec![
        (
            UpstreamError::Http {
                status: 400,
                retry_after: None,
                detail: None,
            },
            "invalid_request",
            StatusCode::BAD_REQUEST,
            false,
        ),
        (
            UpstreamError::Http {
                status: 401,
                retry_after: None,
                detail: None,
            },
            "upstream_auth",
            StatusCode::BAD_GATEWAY,
            true,
        ),
        (
            UpstreamError::Http {
                status: 403,
                retry_after: None,
                detail: None,
            },
            "upstream_auth",
            StatusCode::BAD_GATEWAY,
            true,
        ),
        (
            UpstreamError::Http {
                status: 402,
                retry_after: None,
                detail: None,
            },
            "upstream_quota",
            StatusCode::BAD_GATEWAY,
            true,
        ),
        (
            UpstreamError::Http {
                status: 429,
                retry_after: None,
                detail: None,
            },
            "upstream_rate_limited",
            StatusCode::TOO_MANY_REQUESTS,
            true,
        ),
        (
            UpstreamError::Http {
                status: 500,
                retry_after: None,
                detail: None,
            },
            "upstream_error",
            StatusCode::BAD_GATEWAY,
            true,
        ),
        (
            UpstreamError::Http {
                status: 404,
                retry_after: None,
                detail: None,
            },
            "upstream_error",
            StatusCode::BAD_GATEWAY,
            true,
        ),
        (
            UpstreamError::Timeout {
                kind: TimeoutKind::Idle,
            },
            "upstream_timeout",
            StatusCode::GATEWAY_TIMEOUT,
            true,
        ),
        (
            UpstreamError::Transport("reset".to_string()),
            "upstream_stream_failed",
            StatusCode::BAD_GATEWAY,
            true,
        ),
        (
            UpstreamError::Body("truncated".to_string()),
            "upstream_stream_failed",
            StatusCode::BAD_GATEWAY,
            true,
        ),
        (
            UpstreamError::TooLarge,
            "upstream_stream_failed",
            StatusCode::BAD_GATEWAY,
            false,
        ),
    ];
    for (error, code, status, retryable) in cases {
        let class = error.classify();
        assert_eq!(class.code, code, "{error:?}");
        assert_eq!(class.status, status, "{error:?}");
        assert_eq!(class.retryable_before_first_byte, retryable, "{error:?}");
    }

    // Cooldowns: Retry-After wins, else the default; never for 500s.
    let limited = UpstreamError::Http {
        status: 429,
        retry_after: Some(Duration::from_secs(7)),
        detail: None,
    };
    assert_eq!(limited.classify().cooldown, Some(Duration::from_secs(7)));
    let limited_default = UpstreamError::Http {
        status: 429,
        retry_after: None,
        detail: None,
    };
    assert_eq!(limited_default.classify().cooldown, Some(DEFAULT_COOLDOWN));
    let transient = UpstreamError::Http {
        status: 500,
        retry_after: None,
        detail: None,
    };
    assert_eq!(transient.classify().cooldown, None);

    // Breaker accounting (review M1): the caller's own `invalid_request` and
    // our `internal` configuration failures never count toward the
    // (credential, offering) breaker; every upstream-service class does.
    let breaker_cases: Vec<(UpstreamError, bool)> = vec![
        (
            UpstreamError::Http {
                status: 400,
                retry_after: None,
                detail: None,
            },
            false,
        ),
        (
            UpstreamError::Http {
                status: 422,
                retry_after: None,
                detail: None,
            },
            false,
        ),
        (UpstreamError::Credential("bad".to_string()), false),
        (
            UpstreamError::Http {
                status: 401,
                retry_after: None,
                detail: None,
            },
            true,
        ),
        (
            UpstreamError::Http {
                status: 402,
                retry_after: None,
                detail: None,
            },
            true,
        ),
        (
            UpstreamError::Http {
                status: 429,
                retry_after: None,
                detail: None,
            },
            true,
        ),
        (
            UpstreamError::Http {
                status: 500,
                retry_after: None,
                detail: None,
            },
            true,
        ),
        (
            UpstreamError::Timeout {
                kind: TimeoutKind::Idle,
            },
            true,
        ),
    ];
    for (error, counts) in breaker_cases {
        assert_eq!(error.classify().counts_toward_breaker, counts, "{error:?}");
    }
}

#[test]
fn credential_header_values_are_marked_sensitive() {
    // Review m4: merged authorization / x-api-key values must be redacted by
    // reqwest itself (its error reporting and debug output honour the flag).
    for name in ["authorization", "x-api-key", "X-Api-Key", "Authorization"] {
        let value = header_value(name, "sk-live-secret").expect("header value");
        assert!(value.is_sensitive(), "{name} must be marked sensitive");
        let debug = format!("{value:?}");
        assert!(!debug.contains("sk-live-secret"), "{debug}");
    }
    let value = header_value("x-tenant", "team-a").expect("header value");
    assert!(!value.is_sensitive(), "ordinary headers are not sensitive");
    assert_eq!(value, "team-a");
}

#[tokio::test]
async fn client_reads_a_stream_chunk_by_chunk() {
    let stub = Stub::spawn(vec![Scenario::Stream {
        events: vec![r#"{"choices":[{"delta":{"content":"hello"}}]}"#.to_string()],
        usage: Some(serde_json::json!({"prompt_tokens": 1, "completion_tokens": 2})),
    }])
    .await;

    let boxx = secrets();
    let secret_cipher = boxx.encrypt("sk-live-secret", "cred-1").expect("seal");
    let call = UpstreamCall::build(
        &boxx,
        &build_target(
            &stub.base_url,
            "cred-1",
            &secret_cipher,
            Some(&sealed(&[("x-tenant", "team-a")], "cred-1")),
            Some(&sealed(&[], "off-1")),
        ),
        serde_json::json!({"model": "m", "stream": true}),
    )
    .expect("build");

    let client = UpstreamClient::new(Duration::from_secs(5)).expect("client");
    let mut stream = client.open(&call, budget()).await.expect("open");

    let mut seen = Vec::new();
    while let Some(chunk) = stream.next_chunk().await.expect("chunk") {
        seen.push(String::from_utf8_lossy(&chunk).to_string());
    }
    let all = seen.concat();
    assert!(all.contains("hello"), "{all}");
    assert!(all.contains("[DONE]"), "{all}");

    let request = &stub.requests()[0];
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer sk-live-secret")
    );
    assert_eq!(
        request.headers.get("x-tenant").map(String::as_str),
        Some("team-a")
    );
}

#[tokio::test]
async fn client_maps_statuses_and_retry_after() {
    let stub = Stub::spawn(vec![
        Scenario::Status {
            code: 429,
            headers: vec![
                ("retry-after".to_string(), "7".to_string()),
                ("x-request-id".to_string(), "up-req-1".to_string()),
            ],
            body: r#"{"error":{"message":"slow down","code":"rate_limit_exceeded"}}"#.to_string(),
        },
        Scenario::Status {
            code: 500,
            headers: Vec::new(),
            body: r#"{"error":{"message":"upstream exploded"}}"#.to_string(),
        },
    ])
    .await;
    let client = UpstreamClient::new(Duration::from_secs(5)).expect("client");
    let boxx = secrets();
    let secret_cipher = boxx.encrypt("sk", "cred-1").expect("seal");
    let cred_headers = sealed(&[], "cred-1");
    let off_headers = sealed(&[], "off-1");
    let call = || {
        UpstreamCall::build(
            &boxx,
            &build_target(
                &stub.base_url,
                "cred-1",
                &secret_cipher,
                Some(&cred_headers),
                Some(&off_headers),
            ),
            serde_json::json!({}),
        )
        .expect("build")
    };

    let err = client.open(&call(), budget()).await.expect_err("429");
    assert!(matches!(err, UpstreamError::Http { status: 429, .. }));
    assert_eq!(err.classify().code, "upstream_rate_limited");
    assert_eq!(err.classify().cooldown, Some(Duration::from_secs(7)));
    let detail = err.error_detail().expect("detail");
    assert_eq!(detail.provider_code.as_deref(), Some("rate_limit_exceeded"));
    assert_eq!(
        detail.detail.as_deref(),
        Some("slow down (request id: up-req-1)")
    );

    let err = client.open(&call(), budget()).await.expect_err("500");
    assert_eq!(err.classify().code, "upstream_error");
    let detail = err.error_detail().expect("detail");
    assert_eq!(detail.detail.as_deref(), Some("upstream exploded"));
}

#[tokio::test]
async fn client_times_out_the_first_byte_and_the_stall() {
    // Slow start: nothing within the first-byte budget.
    let stub = Stub::spawn(vec![Scenario::SlowStart {
        delay: Duration::from_secs(5),
        then: Box::new(Scenario::Stream {
            events: vec![],
            usage: None,
        }),
    }])
    .await;
    let client = UpstreamClient::new(Duration::from_millis(500)).expect("client");
    let boxx = secrets();
    let secret_cipher = boxx.encrypt("sk", "cred-1").expect("seal");
    let empty = sealed(&[], "cred-1");
    let call = UpstreamCall::build(
        &boxx,
        &build_target(
            &stub.base_url,
            "cred-1",
            &secret_cipher,
            Some(&empty),
            Some(&sealed(&[], "off-1")),
        ),
        serde_json::json!({}),
    )
    .expect("build");

    let short_budget = Budget {
        first_byte: deadline_in(Duration::from_millis(200)),
        idle: Duration::from_secs(5),
        total: deadline_in(Duration::from_secs(10)),
    };
    let started = Instant::now();
    let err = client.open(&call, short_budget).await.expect_err("timeout");
    assert!(matches!(
        err,
        UpstreamError::Timeout {
            kind: TimeoutKind::FirstByte
        }
    ));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "must not hang: {:?}",
        started.elapsed()
    );

    // Stall between chunks: idle budget on the second read.
    let stub = Stub::spawn(vec![Scenario::Stall {
        after: 1,
        delay: Duration::from_secs(5),
    }])
    .await;
    let call = UpstreamCall::build(
        &boxx,
        &build_target(
            &stub.base_url,
            "cred-1",
            &secret_cipher,
            Some(&empty),
            Some(&sealed(&[], "off-1")),
        ),
        serde_json::json!({}),
    )
    .expect("build");
    let idle_budget = Budget {
        first_byte: deadline_in(Duration::from_secs(5)),
        idle: Duration::from_millis(200),
        total: deadline_in(Duration::from_secs(10)),
    };
    let mut stream = client.open(&call, idle_budget).await.expect("open");
    assert!(stream.next_chunk().await.expect("first").is_some());
    let started = Instant::now();
    let err = stream.next_chunk().await.expect_err("idle timeout");
    assert!(matches!(
        err,
        UpstreamError::Timeout {
            kind: TimeoutKind::Idle
        }
    ));
    assert!(started.elapsed() < Duration::from_secs(2), "must not hang");
}

#[tokio::test]
async fn explicit_authorization_header_overrides_the_bearer() {
    let stub = Stub::spawn(vec![Scenario::Json {
        body: "{}".to_string(),
    }])
    .await;
    let boxx = secrets();
    let secret_cipher = boxx.encrypt("sk-live-secret", "cred-1").expect("seal");
    let call = UpstreamCall::build(
        &boxx,
        &build_target(
            &stub.base_url,
            "cred-1",
            &secret_cipher,
            Some(&sealed(&[("authorization", "Api-Key xyz")], "cred-1")),
            Some(&sealed(&[], "off-1")),
        ),
        serde_json::json!({}),
    )
    .expect("build");
    let client = UpstreamClient::new(Duration::from_secs(5)).expect("client");
    client.open(&call, budget()).await.expect("open");

    let request = &stub.requests()[0];
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Api-Key xyz"),
        "the merged header wins over the default bearer"
    );
}
