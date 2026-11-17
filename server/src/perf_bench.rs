//! Performance harness (docs/research/13-performance.md).
//!
//! Everything here is one `#[ignore]`d test so `just check` stays fast; run it
//! with `just bench` (which passes `--ignored --nocapture --test-threads=1`).
//!
//! The harness measures the chat data plane against the scripted stub upstream
//! (`crate::upstream::stub`), so no external network is involved:
//!
//! * **Query inventory**: the database is opened with `log_statements(TRACE)`
//!   (`db::open_logged`) and a tracing layer records every `sqlx::query` event;
//!   one request is counted at a time.
//! * **Latency / RPS**: waves of `POST /v1/chat/completions` at concurrency
//!   1/10/50/200 through the real router (`Router::oneshot`, no TCP listener —
//!   RPS is therefore an upper bound of a socket-served deployment).
//! * **Stage microbenchmarks**: parse, API-key auth, session load, grants,
//!   credential decrypt, reservation open, the real settle transaction, and a
//!   direct stub upstream call.
//! * **SQLite write path**: 10k sequential settlements (WAL sampling + a
//!   checkpoint), settlements with concurrent readers, and 50/200 concurrent
//!   writers.
//!
//! Numbers are printed as tables; the test never asserts on them (a slow CI
//! machine must not fail the suite) except that every request completes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures_util::future::join_all;
use serde_json::json;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::auth::Role;
use crate::catalog::{self, Actor, CredentialInput, ModelInput, OfferingInput, ProviderInput};
use crate::money::{self, Period, ReservationInput};
use crate::test_support::{TestApp, body_text, send, send_streaming};
use crate::upstream::stub::{Scenario, Stub};

// ---------------------------------------------------------------------------
// SQL statement capture
// ---------------------------------------------------------------------------

static CAPTURE: AtomicBool = AtomicBool::new(false);
static STATEMENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A tracing layer that records `sqlx::query` events while [`CAPTURE`] is on.
struct StatementLayer;

#[derive(Default)]
struct SummaryVisitor {
    summary: Option<String>,
    statement: Option<String>,
    message: Option<String>,
    error: Option<String>,
}

impl Visit for SummaryVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "summary" => self.summary = Some(value.to_string()),
            "db.statement" => self.statement = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            "error" => self.error = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        let rendered = rendered.trim_matches('"').to_string();
        match field.name() {
            "summary" => self.summary = Some(rendered),
            "db.statement" => self.statement = Some(rendered),
            "message" => self.message = Some(rendered),
            "error" => self.error = Some(rendered),
            _ => {}
        }
    }
}

impl<S: Subscriber> Layer<S> for StatementLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = SummaryVisitor::default();
        // Errors are always recorded: the write-path probes report whether a
        // settlement failed and why, instead of only counting rows.
        if *event.metadata().level() == tracing::Level::ERROR {
            event.record(&mut visitor);
            let message = match (visitor.message, visitor.error) {
                (Some(message), Some(error)) => format!("{message}: {error}"),
                (Some(message), None) => message,
                (None, Some(error)) => error,
                (None, None) => "(error event)".to_string(),
            };
            ERRORS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(message);
            return;
        }
        if !CAPTURE.load(Ordering::Relaxed) || event.metadata().target() != "sqlx::query" {
            return;
        }
        event.record(&mut visitor);
        let text = visitor.summary.or(visitor.statement).unwrap_or_default();
        if !text.is_empty() {
            STATEMENTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(text);
        }
    }
}

/// Installs the statement-counting subscriber once per test process.
fn install_statement_meter() {
    use tracing_subscriber::filter::LevelFilter as TraceLevel;
    use tracing_subscriber::prelude::*;

    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let subscriber = tracing_subscriber::registry()
            .with(TraceLevel::TRACE)
            .with(StatementLayer);
        // A second global subscriber would be a bug in the harness; ignoring
        // the error keeps the test runnable if one was installed elsewhere.
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

fn begin_capture() {
    STATEMENTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    CAPTURE.store(true, Ordering::SeqCst);
}

fn end_capture() -> Vec<String> {
    CAPTURE.store(false, Ordering::SeqCst);
    std::mem::take(&mut *STATEMENTS.lock().unwrap_or_else(|e| e.into_inner()))
}

// ---------------------------------------------------------------------------
// Statistics helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Summary {
    count: usize,
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    mean_us: f64,
    max_us: u64,
}

fn summarize(mut samples: Vec<u64>) -> Summary {
    samples.sort_unstable();
    let count = samples.len();
    let pick = |q: f64| -> u64 {
        if samples.is_empty() {
            return 0;
        }
        let rank = (q * count as f64).ceil().max(1.0) as usize;
        samples[rank.saturating_sub(1).min(count - 1)]
    };
    Summary {
        count,
        p50_us: pick(0.50),
        p95_us: pick(0.95),
        p99_us: pick(0.99),
        mean_us: samples.iter().map(|v| *v as f64).sum::<f64>() / count.max(1) as f64,
        max_us: samples.last().copied().unwrap_or(0),
    }
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// Everything one benchmarked request needs, seeded once per app.
struct Bench {
    key: String,
    key_id: String,
    user_id: String,
    offering_id: String,
    model_id: String,
    credential_id: String,
    secret_cipher: String,
}

fn request_with_key(key: &str, stream: bool) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "model": "bench-model",
                "messages": [{"role": "user", "content": "measure the hot path, please"}],
                "max_tokens": 64,
                "stream": stream,
            })
            .to_string(),
        ))
        .expect("bench request")
}

/// Seeds one admin user, one API key, one provider/credential/model/offering
/// with a manual price and a total budget (so settlement writes a bucket).
async fn seed(app: &TestApp, base_url: &str) -> Bench {
    let user_id = app
        .create_user("bench@example.test", Role::Admin, true)
        .await;
    let actor = Actor {
        user_id: &user_id,
        ip: "127.0.0.1",
    };
    let key = crate::api_keys::create_key(
        &app.state.pool,
        &actor,
        &user_id,
        &crate::api_keys::ApiKeyInput {
            name: "bench".to_string(),
            expires_at: None,
            scopes: None,
            rate_limit_rpm: Some(10_000_000),
            rate_limit_tpm: Some(1_000_000_000),
        },
    )
    .await
    .expect("bench api key");

    let provider = catalog::create_provider(
        &app.state.pool,
        &actor,
        &ProviderInput {
            name: "stub".to_string(),
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
            label: "stub-key".to_string(),
            secret: "sk-stub-secret".to_string(),
            headers: None,
            max_concurrency: None,
            balance_usd: None,
            balance_source: None,
            balance_low_threshold: None,
        },
    )
    .await
    .expect("credential");
    let model = catalog::create_model(
        &app.state.pool,
        &actor,
        &ModelInput {
            canonical_name: "bench-model".to_string(),
            family: None,
            modalities_input: None,
            modalities_output: None,
            context_length: Some(1_000_000),
            max_output_tokens: Some(4096),
            capabilities: None,
        },
    )
    .await
    .expect("model");
    let offering = catalog::create_offering(
        &app.state.pool,
        &app.state.secrets,
        &actor,
        &OfferingInput {
            provider_id: provider.id.clone(),
            model_id: model.id.clone(),
            upstream_model_id: "bench-model".to_string(),
            context_override: None,
            max_output_override: None,
            enabled: None,
            headers: None,
        },
    )
    .await
    .expect("offering");
    sqlx::query(
        "INSERT INTO prices (id, offering_id, input_per_m, output_per_m, currency, source) \
         VALUES (?, ?, 1.0, 2.0, 'USD', 'manual')",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&offering.id)
    .execute(&app.state.pool)
    .await
    .expect("price row");
    money::set_budget(
        &app.state.pool,
        &actor,
        app.state.config.tz,
        &user_id,
        Period::Total,
        1_000_000.0,
    )
    .await
    .expect("budget");

    let secret_cipher: String =
        sqlx::query_scalar("SELECT secret_cipher FROM provider_credentials WHERE id = ?")
            .bind(&credential.id)
            .fetch_one(&app.state.pool)
            .await
            .expect("credential cipher");

    Bench {
        key: key.key.clone(),
        key_id: key.view.id.clone(),
        user_id,
        offering_id: offering.id,
        model_id: model.id,
        credential_id: credential.id,
        secret_cipher,
    }
}

fn stream_scenario() -> Scenario {
    Scenario::Stream {
        events: vec![
            r#"{"choices":[{"delta":{"content":"hello"}}]}"#.to_string(),
            r#"{"choices":[{"delta":{"content":" there"}}]}"#.to_string(),
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.to_string(),
        ],
        usage: Some(json!({"prompt_tokens": 12, "completion_tokens": 8})),
    }
}

fn json_scenario() -> Scenario {
    Scenario::Json {
        body: json!({
            "id": "cmpl-bench",
            "object": "chat.completion",
            "model": "bench-model",
            "choices": [{"message": {"role": "assistant", "content": "hello there"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 8},
        })
        .to_string(),
    }
}

/// Runs `n` requests in sequential batches of `concurrency`, returning
/// per-request wall times in microseconds and the inclusive wall time.
async fn wave(
    app: &TestApp,
    key: &str,
    n: usize,
    concurrency: usize,
    stream: bool,
) -> (Vec<u64>, Duration) {
    let router = app.router();
    let started = Instant::now();
    let mut samples = Vec::with_capacity(n);
    let mut remaining = n;
    while remaining > 0 {
        let batch = remaining.min(concurrency);
        let futures = (0..batch).map(|_| {
            let router = router.clone();
            let request = request_with_key(key, stream);
            async move {
                let t = Instant::now();
                if stream {
                    let (status, _headers, body) = send_streaming(router, request).await;
                    assert_eq!(status, StatusCode::OK, "stream accepted");
                    let text = body_text(body).await;
                    assert!(text.contains("data: [DONE]"), "stream completed: {text}");
                } else {
                    let response = send(router, request).await;
                    assert_eq!(response.status, StatusCode::OK, "buffered accepted");
                }
                t.elapsed().as_micros() as u64
            }
        });
        samples.extend(join_all(futures).await);
        remaining -= batch;
    }
    (samples, started.elapsed())
}

// ---------------------------------------------------------------------------
// The probe
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "performance probe; run via `just bench`"]
async fn hot_path_performance_probe() {
    install_statement_meter();

    // --- A. Query inventory ------------------------------------------------
    // Stub queue: warm-up + one counted streaming request + one counted
    // failover request + one counted request with three offerings + one
    // counted buffered request.
    let mut scenarios = vec![
        stream_scenario(), // warm-up
        stream_scenario(), // counted: single streaming candidate
        Scenario::Status {
            code: 500,
            headers: Vec::new(),
            body: "{}".to_string(),
        },
        stream_scenario(), // failover target
        stream_scenario(), // counted: three offerings
        json_scenario(),   // counted: buffered
    ];
    scenarios.extend(std::iter::repeat_n(stream_scenario(), 4_000));
    let stub = Stub::spawn(scenarios).await;

    let app = {
        let base = std::env::var_os("GATEWAY_PERF_DB_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("gateway-perf-{}", uuid::Uuid::new_v4()))
            });
        println!("\n== environment\ndatabase directory: {}", base.display());
        TestApp::with_config_logging_at(base, |_| {}).await
    };
    let bench = seed(&app, &stub.base_url).await;

    // Warm up the pool, caches, and connection.
    let _ = wave(&app, &bench.key, 1, 1, true).await;

    begin_capture();
    let (_, _) = wave(&app, &bench.key, 1, 1, true).await;
    let single_stream = end_capture();
    print_statement_table("single streaming request (1 candidate)", &single_stream);

    // Failover needs a second candidate offering: add one, count, drop it.
    add_extra_offerings(&app, &bench.user_id, 1).await;
    begin_capture();
    let (_, _) = wave(&app, &bench.key, 1, 1, true).await; // failover: 500 then stream
    let failed_over = end_capture();
    print_statement_table("streaming request with one failover", &failed_over);
    clear_extra_offerings(&app).await;

    // Add two more offerings of the same model, then count the request again.
    add_extra_offerings(&app, &bench.user_id, 2).await;
    begin_capture();
    let (_, _) = wave(&app, &bench.key, 1, 1, true).await;
    let three_offerings = end_capture();
    print_statement_table(
        "streaming request (3 candidate offerings)",
        &three_offerings,
    );
    // Back to one candidate for the latency waves.
    clear_extra_offerings(&app).await;

    begin_capture();
    let (_, _) = wave(&app, &bench.key, 1, 1, false).await;
    let buffered = end_capture();
    print_statement_table("buffered request (1 candidate)", &buffered);

    // --- B. Latency and RPS ------------------------------------------------
    // A buffered request needs a JSON scenario; the streaming stub's remaining
    // queue is all SSE. Switch the provider to a second stub for those waves.
    let buffered_stub = Stub::spawn(vec![json_scenario(); 4_000]).await;
    println!("\n== latency / throughput (in-process router, stub upstream)");
    println!(
        "  {:<10} {:<8} {:>9} {:>9} {:>9} {:>12} {:>10}",
        "mode", "conc", "p50 ms", "p95 ms", "p99 ms", "req/s", "wall s"
    );
    for stream in [false, true] {
        let base_url = match stream {
            true => stub.base_url.clone(),
            false => buffered_stub.base_url.clone(),
        };
        set_base_url(&app, &base_url).await;
        for concurrency in [1usize, 10, 50, 200] {
            let n = match concurrency {
                1 => 300,
                10 => 500,
                _ => 800,
            };
            let (samples, wall) = wave(&app, &bench.key, n, concurrency, stream).await;
            let s = summarize(samples);
            println!(
                "  {:<10} {:<8} {:>9.3} {:>9.3} {:>9.3} {:>12.1} {:>10.2}",
                if stream { "stream" } else { "buffered" },
                concurrency,
                ms(s.p50_us),
                ms(s.p95_us),
                ms(s.p99_us),
                n as f64 / wall.as_secs_f64(),
                wall.as_secs_f64()
            );
        }
    }

    // --- C. Stage microbenchmarks -----------------------------------------
    println!("\n== stage microbenchmarks (single-threaded)");
    let mut stage_rows: Vec<(String, String)> = Vec::new();

    // Request parsing.
    let raw = request_with_key(&bench.key, true).into_body();
    let raw = axum::body::to_bytes(raw, 4096).await.expect("body");
    let mut samples = Vec::new();
    for _ in 0..20_000 {
        let t = Instant::now();
        assert!(crate::chat::parse_for_measurement(&raw));
        samples.push(t.elapsed().as_micros() as u64);
    }
    stage_rows.push((
        "request parsing (parse+validate)".to_string(),
        fmt_summary(summarize(samples)),
    ));

    // API-key auth: SELECT + last_used_at UPDATE.
    let mut samples = Vec::new();
    for _ in 0..2_000 {
        let t = Instant::now();
        crate::api_keys::authenticate(&app.state.pool, &bench.key, &app.state.key_usage_stamps)
            .await
            .expect("auth");
        samples.push(t.elapsed().as_micros() as u64);
    }
    stage_rows.push((
        "api-key auth (SELECT; last_used_at at most once/60 s)".to_string(),
        fmt_summary(summarize(samples)),
    ));

    // Session auth: SELECT join + last_seen_at UPDATE.
    let (cookie, _csrf) = crate::test_support::login(&app.router(), "bench@example.test").await;
    let session_token = cookie
        .strip_prefix("gateway_session=")
        .expect("session cookie")
        .to_string();
    let mut samples = Vec::new();
    for _ in 0..2_000 {
        let t = Instant::now();
        crate::auth::load_session(&app.state.pool, &session_token, &app.state.session_activity)
            .await
            .expect("session")
            .expect("session row");
        samples.push(t.elapsed().as_micros() as u64);
    }
    stage_rows.push((
        "session auth (SELECT join; last_seen_at at most once/5 min)".to_string(),
        fmt_summary(summarize(samples)),
    ));

    // Grants.
    let mut samples = Vec::new();
    for _ in 0..2_000 {
        let t = Instant::now();
        crate::access::Grants::load(&app.state.pool, &bench.user_id, Some(&bench.key_id))
            .await
            .expect("grants");
        samples.push(t.elapsed().as_micros() as u64);
    }
    stage_rows.push((
        "grants load (1 SELECT)".to_string(),
        fmt_summary(summarize(samples)),
    ));

    // Credential decryption (the full UpstreamCall::build).
    let mut samples = Vec::new();
    for _ in 0..20_000 {
        let t = Instant::now();
        let call = crate::upstream::UpstreamCall::build(
            &app.state.secrets,
            &crate::upstream::UpstreamTarget {
                base_url: &stub.base_url,
                credential_id: &bench.credential_id,
                secret_cipher: &bench.secret_cipher,
                credential_headers_json: None,
                offering_id: &bench.offering_id,
                offering_headers_json: None,
            },
            json!({"model": "bench-model"}),
        )
        .expect("upstream call");
        std::hint::black_box(call);
        samples.push(t.elapsed().as_micros() as u64);
    }
    stage_rows.push((
        "credential decrypt + UpstreamCall::build".to_string(),
        fmt_summary(summarize(samples)),
    ));

    // Reservation open (with a cleanup DELETE outside the timed region).
    let mut samples = Vec::new();
    for i in 0..2_000 {
        let request_id = format!("open-bench-{i}");
        let t = Instant::now();
        money::open_reservation(
            &app.state.pool,
            app.state.config.tz,
            &ReservationInput {
                request_id: request_id.clone(),
                user_id: bench.user_id.clone(),
                api_key_id: Some(bench.key_id.clone()),
                offering_id: bench.offering_id.clone(),
                model_id: bench.model_id.clone(),
                estimated_tokens: 160,
                estimated_usd: 0.001,
            },
        )
        .await
        .expect("reservation");
        samples.push(t.elapsed().as_micros() as u64);
        sqlx::query("DELETE FROM reservations WHERE request_id = ?")
            .bind(&request_id)
            .execute(&app.state.pool)
            .await
            .expect("cleanup reservation");
    }
    stage_rows.push((
        "reservation open transaction".to_string(),
        fmt_summary(summarize(samples)),
    ));

    // Settle transaction, 10k sequential, with pre-inserted reservations.
    let settle = settle_bench(&app, &bench, 10_000, true).await;
    stage_rows.push((
        "settle transaction (10k sequential)".to_string(),
        fmt_summary(summarize(settle.samples.clone())),
    ));
    stage_rows.push((
        "settle: WAL size before/peak/after/truncated (KiB)".to_string(),
        format!(
            "{} / {} / {} / {}",
            settle.wal_first / 1024,
            settle.wal_peak / 1024,
            settle.wal_last / 1024,
            settle.wal_after_checkpoint / 1024
        ),
    ));
    stage_rows.push((
        "settle: WAL checkpoint (pages busy/log/checkpointed)".to_string(),
        format!(
            "{}/{}/{}",
            settle.checkpoint.0, settle.checkpoint.1, settle.checkpoint.2
        ),
    ));

    // Direct upstream call against the stub (full body read).
    let mut samples = Vec::new();
    for _ in 0..1_000 {
        let call = crate::upstream::UpstreamCall::build(
            &app.state.secrets,
            &crate::upstream::UpstreamTarget {
                base_url: &stub.base_url,
                credential_id: &bench.credential_id,
                secret_cipher: &bench.secret_cipher,
                credential_headers_json: None,
                offering_id: &bench.offering_id,
                offering_headers_json: None,
            },
            json!({
                "model": "bench-model",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": true,
                "max_tokens": 64,
            }),
        )
        .expect("upstream call");
        let budget = crate::upstream::Budget {
            first_byte: tokio::time::Instant::now() + Duration::from_secs(5),
            idle: Duration::from_secs(5),
            total: tokio::time::Instant::now() + Duration::from_secs(10),
        };
        let t = Instant::now();
        let mut stream = app
            .state
            .upstream
            .open(&call, budget)
            .await
            .expect("stub opens");
        while stream.next_chunk().await.expect("chunk").is_some() {}
        samples.push(t.elapsed().as_micros() as u64);
    }
    stage_rows.push((
        "upstream call (stub, open + full stream)".to_string(),
        fmt_summary(summarize(samples)),
    ));

    // fsync cost on the same filesystem as the database, to interpret
    // `synchronous=FULL`: every write transaction commits with an fsync.
    let fsync_path = app.data_dir().join("fsync-probe.bin");
    let mut fsync_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&fsync_path)
        .expect("fsync probe file");
    let mut samples = Vec::new();
    for _ in 0..2_000 {
        fsync_file.write_all(&[0u8; 4096]).expect("write");
        let t = Instant::now();
        fsync_file.sync_all().expect("fsync");
        samples.push(t.elapsed().as_micros() as u64);
    }
    std::fs::remove_file(&fsync_path).ok();
    stage_rows.push((
        "fsync 4 KiB (same filesystem as DB)".to_string(),
        fmt_summary(summarize(samples)),
    ));

    // Telemetry refresh cost (one rebuild over the 10k samples).
    let t = Instant::now();
    let refreshed = app
        .state
        .telemetry
        .refresh(&app.state.pool)
        .await
        .expect("telemetry refresh");
    stage_rows.push((
        format!("telemetry refresh ({refreshed} offerings)"),
        format!("{:.3} ms", t.elapsed().as_secs_f64() * 1000.0),
    ));

    // Decision JSON: parse + serialise the largest captured payload.
    let decision: Option<String> = sqlx::query_scalar(
        "SELECT decision_json FROM request_logs WHERE decision_json IS NOT NULL \
         ORDER BY length(decision_json) DESC LIMIT 1",
    )
    .fetch_optional(&app.state.pool)
    .await
    .expect("decision json");
    if let Some(decision) = decision {
        let mut samples = Vec::new();
        for _ in 0..20_000 {
            let t = Instant::now();
            let parsed: serde_json::Value = serde_json::from_str(&decision).expect("parse");
            let text = serde_json::to_string(&parsed).expect("serialize");
            std::hint::black_box(text);
            samples.push(t.elapsed().as_micros() as u64);
        }
        stage_rows.push((
            format!("decision_json parse+serialise ({} B)", decision.len()),
            fmt_summary(summarize(samples)),
        ));
    }

    for (label, value) in &stage_rows {
        println!("  {label:<44} {value}");
    }

    // --- D. SQLite write path ---------------------------------------------
    println!("\n== SQLite write path");
    let pragmas = read_pragmas(&app.state.pool).await;
    for (name, value) in &pragmas {
        println!("  {name:<24} {value}");
    }

    // Concurrent readers while settlements run.
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let pool = app.state.pool.clone();
            let stop = std::sync::Arc::clone(&stop);
            tokio::spawn(async move {
                let mut queries = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let _: Option<f64> = sqlx::query_scalar(
                        "SELECT COALESCE(SUM(cost_usd), 0.0) FROM usage_events WHERE user_id = ?",
                    )
                    .bind("bench-reader")
                    .fetch_one(&pool)
                    .await
                    .ok();
                    queries += 1;
                }
                queries
            })
        })
        .collect();
    let with_readers = settle_bench(&app, &bench, 2_000, false).await;
    stop.store(true, Ordering::Relaxed);
    let reader_queries: u64 = join_all(readers)
        .await
        .into_iter()
        .map(|r| r.expect("reader task"))
        .sum();
    println!(
        "  settle with 2 concurrent readers: p50 {:.3} ms, p95 {:.3} ms, {} settlements, {} reader queries",
        ms(summarize(with_readers.samples.clone()).p50_us),
        ms(summarize(with_readers.samples.clone()).p95_us),
        with_readers.samples.len(),
        reader_queries
    );

    // Concurrent writers.
    for writers in [50usize, 200] {
        let per_writer = 20;
        let expected = writers * per_writer;
        let before = count_usage(&app).await;
        ERRORS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let app_state = app.state.clone();
        let user = bench.user_id.clone();
        let offering = bench.offering_id.clone();
        let model = bench.model_id.clone();
        let started = Instant::now();
        let tasks = (0..writers).map(|w| {
            let state = app_state.clone();
            let user = user.clone();
            let offering = offering.clone();
            let model = model.clone();
            tokio::spawn(async move {
                let mut samples = Vec::with_capacity(per_writer);
                for i in 0..per_writer {
                    let request_id = format!("writer-{writers}-{w}-{i}");
                    sqlx::query(
                        "INSERT INTO reservations \
                         (id, request_id, user_id, offering_id, estimated_tokens, estimated_usd, \
                          created_at, status) \
                         VALUES (?, ?, ?, ?, 100, 0.001, ?, 'open')",
                    )
                    .bind(uuid::Uuid::new_v4().to_string())
                    .bind(&request_id)
                    .bind(&user)
                    .bind(&offering)
                    .bind(catalog::now_ts())
                    .execute(&state.pool)
                    .await
                    .expect("seed reservation");
                    let t = Instant::now();
                    crate::chat::settle_for_measurement(
                        &state,
                        &request_id,
                        &user,
                        &offering,
                        &model,
                    )
                    .await;
                    samples.push(t.elapsed().as_micros() as u64);
                }
                samples
            })
        });
        let collected = join_all(tasks).await;
        let wall = started.elapsed();
        let samples: Vec<u64> = collected
            .into_iter()
            .flat_map(|r| r.expect("writer task"))
            .collect();
        let after = count_usage(&app).await;
        let s = summarize(samples);
        println!(
            "  {writers:>3} concurrent writers × {per_writer}: p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms, \
             {:.0} settlements/s, rows {}/{} (wall {:.2} s)",
            ms(s.p50_us),
            ms(s.p95_us),
            ms(s.max_us),
            expected as f64 / wall.as_secs_f64(),
            after - before,
            expected,
            wall.as_secs_f64()
        );
        let errors = ERRORS.lock().unwrap_or_else(|e| e.into_inner());
        if !errors.is_empty() {
            let mut unique = errors.clone();
            unique.sort();
            unique.dedup();
            println!(
                "      settlement errors: {} ({} unique)",
                errors.len(),
                unique.len()
            );
            for error in unique.iter().take(3) {
                println!("      - {error}");
            }
        }
    }

    // WAL behaviour after the whole run.
    let wal = wal_size(&app).await;
    println!("  WAL now: {} KiB", wal / 1024);
}

fn fmt_summary(s: Summary) -> String {
    format!(
        "n={} p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms, mean {:.3} ms",
        s.count,
        ms(s.p50_us),
        ms(s.p95_us),
        ms(s.p99_us),
        ms(s.mean_us.round() as u64)
    )
}

/// A dedicated WAL-growth probe: 50k sequential settlements on a fresh
/// database, WAL file size sampled every 5k writes, then a TRUNCATE
/// checkpoint. This answers "does the WAL balloon?" without the noise of the
/// main probe's mixed workload.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "WAL growth probe; run via `just bench wal_growth`"]
async fn wal_growth_under_sustained_writes() {
    install_statement_meter();
    let stub = Stub::spawn(vec![]).await;
    let app = {
        let base = std::env::var_os("GATEWAY_PERF_DB_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("gateway-wal-{}", uuid::Uuid::new_v4()))
            });
        println!("\n== WAL growth\ndatabase directory: {}", base.display());
        TestApp::with_config_logging_at(base, |_| {}).await
    };
    let bench = seed(&app, &stub.base_url).await;
    let db = app.data_dir().join("gateway.db");

    let mut total = 0usize;
    let mut peak = 0u64;
    for batch in 0..10 {
        for i in 0..5_000 {
            let request_id = format!("wal-{batch}-{i}");
            sqlx::query(
                "INSERT INTO reservations \
                 (id, request_id, user_id, offering_id, estimated_tokens, estimated_usd, \
                  created_at, status) \
                 VALUES (?, ?, ?, ?, 1200, 0.01, ?, 'open')",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&request_id)
            .bind(&bench.user_id)
            .bind(&bench.offering_id)
            .bind(catalog::now_ts())
            .execute(&app.state.pool)
            .await
            .expect("seed reservation");
            crate::chat::settle_for_measurement(
                &app.state,
                &request_id,
                &bench.user_id,
                &bench.offering_id,
                &bench.model_id,
            )
            .await;
        }
        total += 5_000;
        let size = wal_size_path(&db).await;
        peak = peak.max(size);
        println!(
            "  after {total:>6} settlements: WAL file {:>8} KiB",
            size / 1024
        );
    }
    let checkpoint: (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&app.state.pool)
        .await
        .expect("checkpoint");
    let after = wal_size_path(&db).await;
    println!(
        "  peak {:.1} MiB, checkpoint busy/log/checkpointed {}/{}/{}, after TRUNCATE {:.1} KiB",
        peak as f64 / (1024.0 * 1024.0),
        checkpoint.0,
        checkpoint.1,
        checkpoint.2,
        after as f64 / 1024.0
    );
}

fn print_statement_table(title: &str, statements: &[String]) {
    println!("\n== {title} ({} statements)", statements.len());
    for (i, statement) in statements.iter().enumerate() {
        println!("  {:>2}. {}", i + 1, statement);
    }
}

async fn add_extra_offerings(app: &TestApp, user_id: &str, count: usize) {
    let actor = Actor {
        user_id,
        ip: "127.0.0.1",
    };
    let provider_id: String = sqlx::query_scalar("SELECT id FROM providers LIMIT 1")
        .fetch_one(&app.state.pool)
        .await
        .expect("provider id");
    let model_id: String = sqlx::query_scalar("SELECT id FROM models LIMIT 1")
        .fetch_one(&app.state.pool)
        .await
        .expect("model id");
    for i in 0..count {
        catalog::create_offering(
            &app.state.pool,
            &app.state.secrets,
            &actor,
            &OfferingInput {
                provider_id: provider_id.clone(),
                model_id: model_id.clone(),
                upstream_model_id: format!("bench-model-extra-{i}"),
                context_override: None,
                max_output_override: None,
                enabled: None,
                headers: None,
            },
        )
        .await
        .expect("extra offering");
    }
}

async fn clear_extra_offerings(app: &TestApp) {
    sqlx::query("DELETE FROM offerings WHERE upstream_model_id LIKE 'bench-model-extra-%'")
        .execute(&app.state.pool)
        .await
        .expect("drop extra offerings");
}

async fn set_base_url(app: &TestApp, base_url: &str) {
    sqlx::query("UPDATE providers SET base_url = ?")
        .bind(base_url)
        .execute(&app.state.pool)
        .await
        .expect("switch provider base_url");
}

async fn count_usage(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&app.state.pool)
        .await
        .expect("usage count")
}

struct SettleRun {
    samples: Vec<u64>,
    wal_first: u64,
    wal_peak: u64,
    wal_last: u64,
    wal_after_checkpoint: u64,
    checkpoint: (i64, i64, i64),
}

/// Runs `n` real settlement transactions, each preceded by a reservation row
/// (inserted outside the timed region), and samples the WAL size.
async fn settle_bench(app: &TestApp, bench: &Bench, n: usize, sample_wal: bool) -> SettleRun {
    let db = app.data_dir().join("gateway.db");
    let mut samples = Vec::with_capacity(n);
    let (mut wal_first, mut wal_peak, mut wal_last) = (0u64, 0u64, 0u64);
    for i in 0..n {
        let request_id = format!("settle-bench-{}-{i}", uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO reservations \
             (id, request_id, user_id, offering_id, estimated_tokens, estimated_usd, created_at, status) \
             VALUES (?, ?, ?, ?, 1200, 0.01, ?, 'open')",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&request_id)
        .bind(&bench.user_id)
        .bind(&bench.offering_id)
        .bind(catalog::now_ts())
        .execute(&app.state.pool)
        .await
        .expect("seed reservation");
        let t = Instant::now();
        crate::chat::settle_for_measurement(
            &app.state,
            &request_id,
            &bench.user_id,
            &bench.offering_id,
            &bench.model_id,
        )
        .await;
        samples.push(t.elapsed().as_micros() as u64);
        if sample_wal && i % 100 == 0 {
            let size = wal_size_path(&db).await;
            if i == 0 {
                wal_first = size;
            }
            wal_peak = wal_peak.max(size);
            wal_last = size;
        }
    }
    let checkpoint: (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&app.state.pool)
        .await
        .expect("checkpoint");
    let wal_after_checkpoint = wal_size_path(&db).await;
    SettleRun {
        samples,
        wal_first,
        wal_peak,
        wal_last,
        wal_after_checkpoint,
        checkpoint,
    }
}

async fn wal_size(app: &TestApp) -> u64 {
    wal_size_path(&app.data_dir().join("gateway.db")).await
}

async fn wal_size_path(db: &Path) -> u64 {
    let wal = format!("{}-wal", db.display());
    std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0)
}

async fn read_pragmas(pool: &sqlx::SqlitePool) -> Vec<(String, String)> {
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(pool)
        .await
        .expect("journal_mode");
    let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(pool)
        .await
        .expect("synchronous");
    let busy: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(pool)
        .await
        .expect("busy_timeout");
    let autocheckpoint: i64 = sqlx::query_scalar("PRAGMA wal_autocheckpoint")
        .fetch_one(pool)
        .await
        .expect("wal_autocheckpoint");
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(pool)
        .await
        .expect("page_size");
    let cache: i64 = sqlx::query_scalar("PRAGMA cache_size")
        .fetch_one(pool)
        .await
        .expect("cache_size");
    vec![
        ("journal_mode".to_string(), journal),
        (
            "synchronous".to_string(),
            format!("{synchronous} (0=OFF, 1=NORMAL, 2=FULL, 3=EXTRA)"),
        ),
        ("busy_timeout".to_string(), format!("{busy} ms")),
        (
            "wal_autocheckpoint".to_string(),
            format!("{autocheckpoint} pages"),
        ),
        ("page_size".to_string(), format!("{page_size} B")),
        ("cache_size".to_string(), format!("{cache} pages")),
        ("pool max_connections".to_string(), "8".to_string()),
    ]
}

// ---------------------------------------------------------------------------
// Password hashing (D39)
// ---------------------------------------------------------------------------

/// Measures Argon2id vs the pre-migration PBKDF2 cost and the end-to-end login
/// latency through the real router. Numbers only; nothing asserts on them.
///
/// Run with `just bench password_hash_probe`.
#[tokio::test]
#[ignore = "performance probe; run with `just bench password_hash_probe`"]
async fn password_hash_probe() {
    use crate::password;

    let mut rows: Vec<(String, f64, String)> = Vec::new();
    let sample = |run: &dyn Fn()| {
        let mut times = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            run();
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        times[2]
    };

    let argon = password::Argon2Cost::production();
    let stored = password::hash_password_with_cost("bench-password", argon).expect("hash");
    let argon_hash_ms = sample(&|| {
        password::hash_password_with_cost("bench-password", argon).expect("hash");
    });
    let argon_verify_ms = sample(&|| {
        password::verify_password("bench-password", &stored).expect("verify");
    });
    rows.push((
        format!(
            "argon2id m={}KiB t={} p={}",
            argon.m_cost_kib, argon.t_cost, argon.p_cost
        ),
        argon_hash_ms,
        format!("verify {argon_verify_ms:.1} ms"),
    ));

    let pbkdf2_hash_ms = sample(&|| {
        password::hash_password_pbkdf2("bench-password", password::PBKDF2_ITERATIONS)
            .expect("pbkdf2");
    });
    rows.push((
        format!("pbkdf2-hmac-sha256 {}k", password::PBKDF2_ITERATIONS / 1000),
        pbkdf2_hash_ms,
        "verify ≈ hash".to_string(),
    ));

    // End-to-end logins through the router at production cost.
    password::set_test_cost(argon.m_cost_kib, argon.t_cost, argon.p_cost);
    let app = TestApp::new().await;
    password::set_test_cost(argon.m_cost_kib, argon.t_cost, argon.p_cost);
    app.create_user("argon@example.com", Role::Admin, true)
        .await;

    let legacy = password::hash_password_pbkdf2(
        crate::test_support::TEST_PASSWORD,
        password::PBKDF2_ITERATIONS,
    )
    .expect("legacy hash");
    let legacy_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO users (id, email, display_name, password_hash, role, is_active) \
         VALUES (?, 'legacy@example.com', 'legacy@example.com', ?, 'user', 1)",
    )
    .bind(&legacy_id)
    .bind(&legacy)
    .execute(&app.state.pool)
    .await
    .expect("legacy user");

    let router = app.router();
    let login = |email: &'static str| {
        let router = router.clone();
        async move {
            let started = Instant::now();
            let response = send(
                router,
                crate::test_support::api_request(
                    Method::POST,
                    "/api/auth/login",
                    Some(json!({
                        "email": email,
                        "password": crate::test_support::TEST_PASSWORD,
                    })),
                    None,
                    None,
                ),
            )
            .await;
            assert_eq!(response.status, StatusCode::OK, "{}", response.body);
            started.elapsed().as_secs_f64() * 1000.0
        }
    };

    // Warm the connection pool and the schema cache.
    let _ = login("argon@example.com").await;
    let mut argon_login = Vec::new();
    for _ in 0..5 {
        argon_login.push(login("argon@example.com").await);
    }
    argon_login.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    rows.push((
        "login, argon2id row (end to end)".to_string(),
        argon_login[2],
        format!("p50 over 5; min {:.1} ms", argon_login[0]),
    ));

    // The first legacy login verifies PBKDF2 and re-hashes to Argon2id.
    let upgrade_login = login("legacy@example.com").await;
    rows.push((
        "login, pbkdf2 row (verify + upgrade)".to_string(),
        upgrade_login,
        "first login after the migration".to_string(),
    ));
    let mut legacy_login = Vec::new();
    for _ in 0..5 {
        legacy_login.push(login("legacy@example.com").await);
    }
    legacy_login.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    rows.push((
        "login, upgraded row (end to end)".to_string(),
        legacy_login[2],
        "subsequent logins".to_string(),
    ));

    password::set_test_cost(8 * 1024, 1, 1);

    let mut out = String::from("\npassword hashing (this build, this machine)\n");
    out.push_str(&format!(
        "{:<44} {:>10}  {}\n",
        "operation", "p50 ms", "note"
    ));
    for (label, value, note) in &rows {
        out.push_str(&format!("{label:<44} {value:>10.1}  {note}\n"));
    }
    print!("{out}");
    let _ = std::io::stdout().flush();
}
