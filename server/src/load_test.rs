//! M3 acceptance load test: memory stays flat under 200 concurrent streams
//! (`PLAN.md`, M3 acceptance block). The gateway runs in-process against the
//! scripted stub upstream ([`crate::upstream::stub`]) and serves two waves of
//! [`STREAMS`] concurrent streaming `POST /v1/chat/completions` requests.
//! Process RSS and open fds are sampled from `/proc` before, during, and
//! after the waves, and the test asserts:
//!
//! * (a) every stream completes (`data: [DONE]`, one settled usage row, and
//!   no upstream hang-ups recorded by the stub),
//! * (b) RSS growth over the pre-load baseline stays under
//!   [`MAX_RSS_GROWTH_KIB`] — at peak and after the waves drain,
//! * (c) no fd leaks: fds stay within a bounded socket budget at peak and
//!   settle back to the same level across waves (a leak that opens fds per
//!   request adds one wave's worth and trips [`FD_SLACK`]).
//!
//! Linux-only by construction (`/proc/self/...`). Run it with `just
//! load-test`; it is `#[ignore]`d so the `just check` suite stays fast. This
//! is a correctness test, not a benchmark: the streams are tiny and the
//! whole run takes a few seconds of streaming.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use futures_util::future::join_all;
use serde_json::json;

use crate::auth::Role;
use crate::catalog::{self, Actor, CredentialInput, ModelInput, OfferingInput, ProviderInput};
use crate::test_support::{TestApp, api_request, body_text, login, send_streaming};
use crate::upstream::stub::{Scenario, Stub};

/// Concurrent streaming requests per wave.
const STREAMS: usize = 200;

/// Two waves: the second wave proves the first left no leaked fds behind.
const WAVES: usize = 2;

/// Each stub stream emits a few SSE events, then holds the stream open this
/// long before `[DONE]`: long enough that all 200 streams overlap despite
/// dispatch skew, short enough to keep the run to a few seconds.
const STALL: Duration = Duration::from_secs(3);

/// A wave that takes longer than this is not concurrent: serial execution
/// would take `STREAMS × STALL` = 10 minutes.
const MAX_WAVE_WALL: Duration = Duration::from_secs(30);

/// Allowed RSS growth over the pre-load baseline: 100 MiB. The streams are
/// tiny (four SSE events each); measured on this machine the 400-stream run
/// grows the debug test binary by ~35 MiB (the allocator keeps it after the
/// drain). 100 MiB absorbs profile and allocator noise while still catching
/// the failure this guards against: per-stream buffering that scales with
/// concurrency (200 streams keeping a 1 MiB buffer each already sit at the
/// bound).
const MAX_RSS_GROWTH_KIB: u64 = 100 * 1024;

/// The socket budget: every in-flight stream owns one TCP socket per end, and
/// both ends live in this process (the gateway's pooled client and the
/// in-process stub server). At most one wave's connections are live while the
/// other wave's may still sit in the pools, so `2 × STREAMS × WAVES` is the
/// ceiling — anything above it is fds that are not sockets of this load.
const MAX_OPEN_SOCKETS: usize = 2 * STREAMS * WAVES;

/// Allowed fd churn for the leak assertions. Legitimate noise between two
/// identical waves is a handful of SQLite/runtime fds; a leak that opens fds
/// per stream adds one wave's worth (≥ [`STREAMS`]) and is caught.
const FD_SLACK: usize = 64;

/// One `/proc` snapshot.
#[derive(Debug, Clone, Copy)]
struct ProcSample {
    /// `VmRSS` from `/proc/self/status`, in KiB.
    rss_kib: u64,
    /// Entry count of `/proc/self/fd`. The scan holds one directory fd of
    /// its own; it appears in every sample alike, so deltas and bounds are
    /// unaffected.
    fds: usize,
}

/// Samples process RSS and open fds from `/proc` (Linux-only by design).
fn sample() -> ProcSample {
    let status = std::fs::read_to_string("/proc/self/status")
        .expect("read /proc/self/status (the load test is Linux-only)");
    let rss_kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse().ok())
        .expect("VmRSS in /proc/self/status");
    let fds = std::fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd (the load test is Linux-only)")
        .count();
    ProcSample { rss_kib, fds }
}

/// Samples every `interval` until `stop` is set, then returns the peak RSS
/// and fd counts seen across the whole run.
fn spawn_sampler(
    stop: Arc<AtomicBool>,
    interval: Duration,
) -> tokio::task::JoinHandle<(u64, usize)> {
    tokio::spawn(async move {
        let first = sample();
        let (mut peak_rss, mut peak_fds) = (first.rss_kib, first.fds);
        while !stop.load(Ordering::Relaxed) {
            let now = sample();
            peak_rss = peak_rss.max(now.rss_kib);
            peak_fds = peak_fds.max(now.fds);
            tokio::time::sleep(interval).await;
        }
        (peak_rss, peak_fds)
    })
}

/// Seeds one provider + credential + model + offering whose upstream is the
/// stub (mirrors the chat pipeline tests' seeding).
async fn seed(app: &TestApp, uid: &str, base_url: &str) {
    let actor = Actor {
        user_id: uid,
        ip: "127.0.0.1",
    };
    let provider = catalog::create_provider(
        &app.state.pool,
        &actor,
        &ProviderInput {
            name: "stub".to_string(),
            base_url: base_url.to_string(),
            kind: None,
            enabled: None,
            preset: None,
            connectors: None,
        },
    )
    .await
    .expect("provider");
    catalog::create_credential(
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
            balance_gate: None,
        },
    )
    .await
    .expect("credential");
    let model = catalog::create_model(
        &app.state.pool,
        &actor,
        &ModelInput {
            canonical_name: "load-model".to_string(),
            family: None,
            modalities_input: None,
            modalities_output: None,
            context_length: None,
            max_output_tokens: None,
            capabilities: None,
        },
    )
    .await
    .expect("model");
    catalog::create_offering(
        &app.state.pool,
        &app.state.secrets,
        &actor,
        &OfferingInput {
            provider_id: provider.id,
            model_id: model.id,
            upstream_model_id: "test-model".to_string(),
            context_override: None,
            max_output_override: None,
            enabled: None,
            headers: None,
        },
    )
    .await
    .expect("offering");
}

/// One streaming chat request (no `X-CSRF-Token`: `/v1` is CSRF-exempt).
fn chat_request(cookie: &str) -> axum::http::Request<axum::body::Body> {
    api_request(
        Method::POST,
        "/v1/chat/completions",
        Some(json!({
            "model": "load-model",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "max_tokens": 8,
        })),
        Some(cookie),
        None,
    )
}

/// Fires `streams` concurrent streaming requests, drains them all, and
/// returns how long the wave took. Panics if any stream fails to complete.
async fn wave(app: &TestApp, cookie: &str, streams: usize) -> Duration {
    let started = Instant::now();
    let in_flight = (0..streams).map(|_| {
        let router = app.router();
        let request = chat_request(cookie);
        async move {
            let (status, _headers, body) = send_streaming(router, request).await;
            assert_eq!(status, StatusCode::OK, "every stream is accepted");
            body_text(body).await
        }
    });
    let texts = tokio::time::timeout(Duration::from_secs(120), join_all(in_flight))
        .await
        .expect("wave drains within the budget");
    let elapsed = started.elapsed();
    for text in &texts {
        assert!(
            text.contains("data: [DONE]"),
            "every stream completes with [DONE]: {text}"
        );
    }
    elapsed
}

/// M3 acceptance: memory stays flat under 200 concurrent streams. See the
/// module docs for the sampled metrics and their bounds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "load test; run via `just load-test`"]
async fn memory_stays_flat_under_200_concurrent_streams() {
    let warmups = 1usize;
    let total_streams = warmups + STREAMS * WAVES;

    // One instant stream (warm-up), then one stalled stream per load request:
    // the queue is consumed in order and extra requests would get a loud 500.
    let mut scenarios = vec![Scenario::Stream {
        events: vec![r#"{"choices":[{"delta":{"content":"warm"}}]}"#.to_string()],
        usage: None,
    }];
    scenarios.extend(std::iter::repeat_n(
        Scenario::Stall {
            after: 4,
            delay: STALL,
        },
        STREAMS * WAVES,
    ));
    let stub = Stub::spawn(scenarios).await;

    let app = TestApp::new().await;
    let uid = app
        .create_user("admin@example.test", Role::Admin, true)
        .await;
    let (cookie, _csrf) = login(&app.router(), "admin@example.test").await;
    seed(&app, &uid, &stub.base_url).await;

    // Warm-up: one completed stream primes the connection pool, the SQLite
    // caches, and the allocator before the baseline is sampled.
    wave(&app, &cookie, warmups).await;

    let baseline = sample();
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = spawn_sampler(Arc::clone(&stop), Duration::from_millis(25));

    let mut wave_times = Vec::new();
    let mut settled_after = Vec::new();
    for _ in 0..WAVES {
        wave_times.push(wave(&app, &cookie, STREAMS).await);
        // Let dropped streams and half-closed sockets settle before the
        // wave's fd snapshot.
        tokio::time::sleep(Duration::from_millis(500)).await;
        settled_after.push(sample());
    }

    stop.store(true, Ordering::Relaxed);
    let (peak_rss_kib, peak_fds) = sampler.await.expect("sampler task");
    let settled = *settled_after.last().expect("one sample per wave");

    println!(
        "load-test: {total_streams} streams ({STREAMS} × {WAVES} waves + {warmups} warm-up), \
         wave wall {:?}, \
         RSS KiB baseline {} → peak {} → settled {}, \
         fds baseline {} → peak {} → settled {:?}",
        wave_times,
        baseline.rss_kib,
        peak_rss_kib,
        settled.rss_kib,
        baseline.fds,
        peak_fds,
        settled_after.iter().map(|s| s.fds).collect::<Vec<_>>(),
    );

    // (a) every stream completed and settled; none was aborted mid-flight.
    assert_eq!(
        stub.request_count(),
        total_streams,
        "exactly one upstream call per stream"
    );
    assert!(
        stub.disconnects().is_empty(),
        "the gateway hung up on no stream"
    );
    let usage_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&app.state.pool)
        .await
        .expect("usage row count");
    assert_eq!(
        usage_rows, total_streams as i64,
        "every stream settled exactly one usage row"
    );

    // The waves were concurrent, not serial (see [`MAX_WAVE_WALL`]).
    for (i, took) in wave_times.iter().enumerate() {
        assert!(
            *took < MAX_WAVE_WALL,
            "wave {i} took {took:?}: the streams did not run concurrently"
        );
    }
    // Each in-flight stream costs two sockets (client + in-process stub end):
    // reaching `STREAMS` fds over baseline means at least half the wave was
    // in flight at one instant (dispatch skew may stagger the rest).
    assert!(
        peak_fds >= baseline.fds + STREAMS,
        "the load never reached ~{STREAMS} concurrent streams: peak fds {peak_fds}, baseline {}",
        baseline.fds
    );

    // (b) RSS stays bounded over baseline, at peak and after draining.
    let peak_growth = peak_rss_kib.saturating_sub(baseline.rss_kib);
    let settled_growth = settled.rss_kib.saturating_sub(baseline.rss_kib);
    assert!(
        peak_growth <= MAX_RSS_GROWTH_KIB,
        "peak RSS grew {peak_growth} KiB over baseline (> {MAX_RSS_GROWTH_KIB} KiB bound)"
    );
    assert!(
        settled_growth <= MAX_RSS_GROWTH_KIB,
        "settled RSS grew {settled_growth} KiB over baseline (> {MAX_RSS_GROWTH_KIB} KiB bound)"
    );

    // (c) no fd leaks: bounded at peak and settled, and no growth between
    // two identical waves.
    assert!(
        peak_fds <= baseline.fds + MAX_OPEN_SOCKETS + FD_SLACK,
        "peak fds {peak_fds} exceeded the socket budget (baseline {}, + {MAX_OPEN_SOCKETS} + {FD_SLACK})",
        baseline.fds
    );
    assert!(
        settled.fds <= baseline.fds + MAX_OPEN_SOCKETS + FD_SLACK,
        "settled fds {} exceeded the socket budget (baseline {}, + {MAX_OPEN_SOCKETS} + {FD_SLACK})",
        settled.fds,
        baseline.fds
    );
    let after_wave_one = settled_after[0].fds;
    assert!(
        settled.fds <= after_wave_one + FD_SLACK,
        "fds leaked across waves: wave 1 settled at {after_wave_one}, wave 2 at {} (> +{FD_SLACK})",
        settled.fds
    );
}
