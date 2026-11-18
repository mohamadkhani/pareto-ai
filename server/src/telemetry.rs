//! Per-offering rolling telemetry (M5): the passive speed/reliability
//! signals the router scores on.
//!
//! Everything is **derived** from `telemetry_attempts`, the per-attempt
//! sample table written with the settle transaction (decision D26): one row
//! per classified upstream attempt, **including failures rescued by
//! failover** — the single `usage_events` row per request could never teach
//! the router about the offering that failed before the next member served
//! (review M5). From the window come TTFT p50/p95, tokens per second,
//! end-to-end latency p95, failure rate, and latency stddev over the last
//! [`WINDOW_SAMPLES`] samples per offering. The hot request path only inserts
//! attempt rows; the cache is refreshed periodically in the background
//! ([`spawn_telemetry_job`]) and the windows are snapshotted to
//! `telemetry_snapshots` so a fresh process warms up without re-reading the
//! samples ([`TelemetryStore::warm`], wired at startup by
//! [`warm_at_startup`]).
//!
//! Sample semantics: a row is a **sample** unless its outcome is
//! `client_disconnected` (the client hung up — nothing to measure),
//! `invalid_request` (the upstream rejected the caller's request — not the
//! offering's service), or `upstream_quota` / `upstream_rate_limited`
//! (402/429: credential-side limits, **attributed to the credential** and
//! never to the offering's reliability — decision D26; the M3 cooldown is
//! what acts on them). A sample is a **failure** when its outcome is set
//! otherwise. Latency/TTFT/throughput statistics use successful samples only;
//! the failure rate uses the whole window.

use std::collections::HashMap;
use std::sync::RwLock;

use sqlx::SqlitePool;

/// The rolling window per offering: at most the last 200 settled samples.
pub const WINDOW_SAMPLES: usize = 200;

/// Outcome classes that never measure the offering's service (see the module
/// docs): the caller's own request, and credential-side quota/rate limits —
/// the latter are attributed to the credential (D26). The refresh query and
/// the retention prune are both built from this list.
const NON_OFFERING_OUTCOMES: [&str; 4] = [
    "client_disconnected",
    "invalid_request",
    "upstream_quota",
    "upstream_rate_limited",
];

/// Telemetry read/write failures. Failure mode: the previous cache keeps
/// serving; callers log and continue.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("telemetry ledger query failed: {0}")]
    Db(#[from] sqlx::Error),
    #[error("telemetry snapshot is malformed: {0}")]
    Snapshot(#[from] serde_json::Error),
}

/// One classified upstream attempt (decision D26), persisted with the settle
/// transaction into `telemetry_attempts`: the offering and credential the
/// attempt used, its outcome class (`None` = completed, otherwise the failure
/// taxonomy code), and what the attempt measured.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptSample {
    pub offering_id: String,
    pub credential_id: Option<String>,
    /// The failure taxonomy code (`upstream_auth`, `upstream_timeout`, ...),
    /// or `None` when the attempt completed.
    pub outcome: Option<&'static str>,
    pub latency_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub output_tokens: Option<i64>,
    /// The provider's own HTTP status, when it answered (D49).
    pub upstream_status: Option<i64>,
    /// The provider's own error code/type, bounded and redacted (D49).
    pub provider_error_code: Option<String>,
    /// The provider's error message, bounded and redacted (D49).
    pub error_detail: Option<String>,
}

/// Writes per-attempt samples inside the caller's transaction (the settle
/// transaction — review M5) and returns the number written. Attempts insert
/// in order, so the window's `id DESC` tie-break preserves attempt order.
/// Failure mode: [`sqlx::Error`] — the caller decides whether to roll the
/// settlement back (it logs and continues; telemetry must never fail a
/// response).
pub async fn record_attempts(
    tx: &mut sqlx::SqliteConnection,
    request_id: &str,
    samples: &[AttemptSample],
) -> Result<usize, sqlx::Error> {
    let occurred_at = crate::catalog::now_ts();
    for sample in samples {
        sqlx::query(
            "INSERT INTO telemetry_attempts \
             (request_id, offering_id, credential_id, outcome, latency_ms, ttft_ms, \
              output_tokens, upstream_status, provider_error_code, error_detail, occurred_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(&sample.offering_id)
        .bind(&sample.credential_id)
        .bind(sample.outcome)
        .bind(sample.latency_ms)
        .bind(sample.ttft_ms)
        .bind(sample.output_tokens)
        .bind(sample.upstream_status)
        .bind(&sample.provider_error_code)
        .bind(&sample.error_detail)
        .bind(&occurred_at)
        .execute(&mut *tx)
        .await?;
    }
    Ok(samples.len())
}

/// One settled request sample (the persisted window shape).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub latency_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub output_tokens: Option<i64>,
    pub failed: bool,
}

/// Rolling statistics over one offering's window. `None` metrics have no
/// samples yet — the router imputes them at the neutral 0.5 (decision D5:
/// no useful speed scores until real traffic exists).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OfferingStats {
    /// Samples in the window.
    pub samples: usize,
    /// Failed samples in the window.
    pub failures: usize,
    pub ttft_p50_ms: Option<f64>,
    pub ttft_p95_ms: Option<f64>,
    /// Mean output tokens per second over successful samples.
    pub tokens_per_sec: Option<f64>,
    pub latency_p95_ms: Option<f64>,
    pub latency_mean_ms: Option<f64>,
    pub latency_stddev_ms: Option<f64>,
    /// Failed samples over all samples, in `[0, 1]`.
    pub failure_rate: Option<f64>,
}

impl OfferingStats {
    /// Latency coefficient of variation (stddev / mean) for the jitter
    /// penalty; `None` without samples.
    pub fn latency_cv(&self) -> Option<f64> {
        let mean = self.latency_mean_ms.filter(|mean| *mean > 0.0)?;
        Some((self.latency_stddev_ms? / mean).clamp(0.0, 1.0))
    }
}

/// Nearest-rank percentile of an already-collected metric: `p95` of 200
/// samples is the 190th smallest. Empty input is `None`.
fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (q * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

/// Computes one offering's stats from its window (newest-first or any order).
/// Pure: same samples, same stats.
pub fn compute_stats(samples: &[Sample]) -> OfferingStats {
    let total = samples.len();
    let failures = samples.iter().filter(|sample| sample.failed).count();

    let mut ttfts: Vec<f64> = Vec::new();
    let mut latencies: Vec<f64> = Vec::new();
    let mut tps: Vec<f64> = Vec::new();
    for sample in samples.iter().filter(|sample| !sample.failed) {
        if let Some(ttft) = sample.ttft_ms {
            ttfts.push(ttft as f64);
        }
        if let Some(latency) = sample.latency_ms {
            latencies.push(latency as f64);
        }
        // Throughput over the generation phase (first byte to completion).
        if let (Some(output), Some(latency)) = (sample.output_tokens, sample.latency_ms) {
            let active_ms = latency.saturating_sub(sample.ttft_ms.unwrap_or(0));
            if active_ms > 0 {
                tps.push(output as f64 * 1000.0 / active_ms as f64);
            }
        }
    }
    ttfts.sort_by(f64::total_cmp);
    latencies.sort_by(f64::total_cmp);

    let latency_mean = match latencies.is_empty() {
        true => None,
        false => Some(latencies.iter().sum::<f64>() / latencies.len() as f64),
    };
    let latency_stddev = latency_mean.map(|mean| {
        let variance = latencies
            .iter()
            .map(|value| (value - mean) * (value - mean))
            .sum::<f64>()
            / latencies.len() as f64;
        variance.sqrt()
    });
    let tokens_per_sec = match tps.is_empty() {
        true => None,
        false => Some(tps.iter().sum::<f64>() / tps.len() as f64),
    };

    OfferingStats {
        samples: total,
        failures,
        ttft_p50_ms: percentile(&ttfts, 0.50),
        ttft_p95_ms: percentile(&ttfts, 0.95),
        tokens_per_sec,
        latency_p95_ms: percentile(&latencies, 0.95),
        latency_mean_ms: latency_mean,
        latency_stddev_ms: latency_stddev,
        failure_rate: match total {
            0 => None,
            _ => Some(failures as f64 / total as f64),
        },
    }
}

/// The SQL `NOT IN` list of [`NON_OFFERING_OUTCOMES`], quoted.
fn non_offering_outcomes_sql() -> String {
    NON_OFFERING_OUTCOMES
        .iter()
        .map(|outcome| format!("'{outcome}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The hot in-memory cache: offering id → rolling stats. Reads are lock-only;
/// refreshes happen off the request path.
#[derive(Default)]
pub struct TelemetryStore {
    inner: RwLock<HashMap<String, OfferingStats>>,
}

impl TelemetryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached stats of one offering (`None` when it has no samples —
    /// the caller imputes at 0.5).
    pub fn stats(&self, offering_id: &str) -> Option<OfferingStats> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(offering_id)
            .cloned()
    }

    /// Rebuilds the cache from `telemetry_attempts` (last [`WINDOW_SAMPLES`]
    /// samples per offering), prunes rows the window can no longer see, and
    /// upserts the windows into `telemetry_snapshots`. Returns the number of
    /// offerings refreshed. Failure mode: [`TelemetryError`]; the previous
    /// cache keeps serving.
    pub async fn refresh(&self, pool: &SqlitePool) -> Result<usize, TelemetryError> {
        let offering_ids: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT offering_id FROM telemetry_attempts")
                .fetch_all(pool)
                .await?;
        let excluded = non_offering_outcomes_sql();
        let sample_condition = format!("outcome IS NULL OR outcome NOT IN ({excluded})");
        let mut fresh = HashMap::with_capacity(offering_ids.len());
        let updated_at = crate::catalog::now_ts();
        for offering_id in offering_ids {
            let rows =
                sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<i64>, Option<String>)>(
                    &format!(
                        "SELECT latency_ms, ttft_ms, output_tokens, outcome \
                     FROM telemetry_attempts \
                     WHERE offering_id = ? \
                       AND ({sample_condition}) \
                     ORDER BY occurred_at DESC, id DESC \
                     LIMIT ?"
                    ),
                )
                .bind(&offering_id)
                .bind(WINDOW_SAMPLES as i64)
                .fetch_all(pool)
                .await?;
            let samples: Vec<Sample> = rows
                .into_iter()
                .map(|(latency_ms, ttft_ms, output_tokens, outcome)| Sample {
                    latency_ms,
                    ttft_ms,
                    output_tokens,
                    failed: outcome.is_some(),
                })
                .collect();
            if samples.is_empty() {
                // Nothing measurable in the window (every row was excluded):
                // the offering stays absent from the cache and the router
                // imputes its metrics at 0.5.
                continue;
            }
            let stats = compute_stats(&samples);
            let window = serde_json::json!({ "samples": samples });
            sqlx::query(
                "INSERT INTO telemetry_snapshots (offering_id, window_json, updated_at) \
                 VALUES (?, ?, ?) \
                 ON CONFLICT (offering_id) DO UPDATE \
                 SET window_json = excluded.window_json, updated_at = excluded.updated_at",
            )
            .bind(&offering_id)
            .bind(window.to_string())
            .bind(&updated_at)
            .execute(pool)
            .await?;
            fresh.insert(offering_id, stats);
        }
        // Retention (migration 0008): keep only the newest rows the window
        // can still see, per offering and per outcome class — the excluded
        // classes are kept as credential-attribution records but never enter
        // a window.
        sqlx::query(&format!(
            "DELETE FROM telemetry_attempts WHERE id IN ( \
               SELECT id FROM ( \
                 SELECT id, ROW_NUMBER() OVER ( \
                          PARTITION BY offering_id, \
                            CASE WHEN {sample_condition} THEN 1 ELSE 0 END \
                          ORDER BY occurred_at DESC, id DESC) AS rn \
                 FROM telemetry_attempts) \
               WHERE rn > ?)"
        ))
        .bind(WINDOW_SAMPLES as i64)
        .execute(pool)
        .await?;
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = fresh;
        Ok(self.inner.read().unwrap_or_else(|e| e.into_inner()).len())
    }

    /// Warms an empty cache from `telemetry_snapshots` (startup); a refresh
    /// follows shortly after and replaces the warm state. Returns the number
    /// of offerings loaded.
    pub async fn warm(&self, pool: &SqlitePool) -> Result<usize, TelemetryError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT offering_id, window_json FROM telemetry_snapshots")
                .fetch_all(pool)
                .await?;
        let mut loaded = 0usize;
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        for (offering_id, window_json) in rows {
            let window: serde_json::Value = serde_json::from_str(&window_json)?;
            let samples: Vec<Sample> = serde_json::from_value(
                window
                    .get("samples")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(Vec::new())),
            )?;
            inner.insert(offering_id, compute_stats(&samples));
            loaded += 1;
        }
        Ok(loaded)
    }
}

/// Startup wiring (review m1): warms the hot cache from
/// `telemetry_snapshots` before the first request is served, so routing does
/// not impute 0.5 for every offering until the first background refresh
/// lands. Failures are logged, never fatal.
pub async fn warm_at_startup(pool: &SqlitePool, store: &TelemetryStore) {
    match store.warm(pool).await {
        Ok(loaded) => tracing::debug!(offerings = loaded, "telemetry warmed from snapshots"),
        Err(e) => tracing::error!(error = %e, "warming telemetry from snapshots failed"),
    }
}

/// The background refresh job: rebuilds the cache every `interval` (the first
/// refresh runs immediately). Failures are logged and retried next tick; the
/// previous cache keeps serving.
pub fn spawn_telemetry_job(
    pool: SqlitePool,
    store: std::sync::Arc<TelemetryStore>,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match store.refresh(&pool).await {
                Ok(count) => tracing::debug!(offerings = count, "telemetry refreshed"),
                Err(e) => tracing::error!(error = %e, "telemetry refresh failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::catalog::{self, Actor, ModelInput, OfferingInput, ProviderInput};
    use crate::test_support::TestApp;

    fn sample(latency: i64, ttft: i64, output: i64, failed: bool) -> Sample {
        Sample {
            latency_ms: Some(latency),
            ttft_ms: Some(ttft),
            output_tokens: Some(output),
            failed,
        }
    }

    /// Seeds one offering (the sample table's foreign keys point at it).
    async fn seed_offering(app: &TestApp, uid: &str) -> String {
        let actor = Actor {
            user_id: uid,
            ip: "127.0.0.1",
        };
        let provider = catalog::create_provider(
            &app.state.pool,
            &actor,
            &ProviderInput {
                name: "p1".to_string(),
                base_url: "http://127.0.0.1:1".to_string(),
                kind: None,
                enabled: None,
            },
        )
        .await
        .expect("provider");
        let model = catalog::create_model(
            &app.state.pool,
            &actor,
            &ModelInput {
                canonical_name: "m".to_string(),
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
                upstream_model_id: "up".to_string(),
                context_override: None,
                max_output_override: None,
                enabled: None,
                headers: None,
            },
        )
        .await
        .expect("offering")
        .id
    }

    #[test]
    fn stats_cover_percentiles_throughput_failure_rate_and_stddev() {
        // 20 successful samples: latency 10..=200 ms (step 10), ttft 5 ms
        // earlier, 5 output tokens each; then 5 failed samples.
        let samples: Vec<Sample> = (1..=20)
            .map(|i| sample(i * 10, i * 10 - 5, 5, false))
            .chain((0..5).map(|_| sample(500, 50, 0, true)))
            .collect();
        let stats = compute_stats(&samples);
        assert_eq!(stats.samples, 25);
        assert_eq!(stats.failures, 5);
        assert_eq!(stats.failure_rate, Some(0.2));

        // 20 successful latencies 10..=200 step 10; nearest-rank p95 is the
        // 19th smallest = 190.
        assert_eq!(stats.latency_p95_ms, Some(190.0));
        assert_eq!(stats.ttft_p95_ms, Some(185.0));
        assert_eq!(stats.ttft_p50_ms, Some(95.0));
        // Failed samples carry no throughput: 5 tokens over (latency-ttft)=5ms.
        assert_eq!(stats.tokens_per_sec, Some(1000.0));
        let mean = (10..=200).step_by(10).sum::<i64>() as f64 / 20.0;
        assert_eq!(stats.latency_mean_ms, Some(mean));
        let stddev = stats.latency_stddev_ms.expect("stddev");
        assert!(stddev > 0.0 && stddev < mean, "{stddev} vs {mean}");
        assert!(stats.latency_cv().expect("cv") < 1.0);
    }

    #[test]
    fn empty_windows_have_no_metrics() {
        let stats = compute_stats(&[]);
        assert_eq!(stats, OfferingStats::default());
        assert_eq!(stats.failure_rate, None);
        assert_eq!(stats.latency_cv(), None);
    }

    #[tokio::test]
    async fn the_window_is_bounded_to_the_newest_200_samples() {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let offering = seed_offering(&app, &uid).await;
        // 250 rows: the oldest 50 are slow (latency 900), the newest 200 are
        // fast (latency 100). Only the newest 200 may count, and the refresh
        // prunes rows the window can no longer see (migration 0008).
        for i in 0..250i64 {
            let latency = match i < 50 {
                true => 900,
                false => 100,
            };
            insert_attempt(
                &app,
                &format!("req-{i}"),
                &offering,
                None,
                None,
                latency,
                Some(10),
                Some(4),
            )
            .await;
        }
        let store = TelemetryStore::new();
        store.refresh(&app.state.pool).await.expect("refresh");
        let stats = store.stats(&offering).expect("stats");
        assert_eq!(stats.samples, 200, "bounded window");
        assert_eq!(stats.latency_p95_ms, Some(100.0), "slow rows fell out");
        assert_eq!(stats.failure_rate, Some(0.0));
        let kept: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM telemetry_attempts WHERE offering_id = ?")
                .bind(&offering)
                .fetch_one(&app.state.pool)
                .await
                .expect("count");
        assert_eq!(kept, 200, "rows outside the window are pruned");
    }

    #[tokio::test]
    async fn refresh_derives_from_attempt_samples_and_persists_the_window() {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let offering = seed_offering(&app, &uid).await;
        insert_attempt(&app, "req-1", &offering, None, None, 100, Some(20), Some(8)).await;
        // A failure rescued by failover still teaches the router (review M5).
        insert_attempt(
            &app,
            "req-2",
            &offering,
            None,
            Some("upstream_error"),
            300,
            None,
            None,
        )
        .await;
        let store = TelemetryStore::new();
        assert!(store.stats(&offering).is_none());
        assert_eq!(store.refresh(&app.state.pool).await.expect("refresh"), 1);
        let stats = store.stats(&offering).expect("stats");
        assert_eq!(stats.samples, 2);
        assert_eq!(stats.failures, 1);
        assert_eq!(stats.failure_rate, Some(0.5));

        let snapshot: String =
            sqlx::query_scalar("SELECT window_json FROM telemetry_snapshots WHERE offering_id = ?")
                .bind(&offering)
                .fetch_one(&app.state.pool)
                .await
                .expect("snapshot");
        assert!(snapshot.contains("\"failed\""), "{snapshot}");
        assert!(snapshot.contains("100"), "{snapshot}");
    }

    #[tokio::test]
    async fn warm_starts_from_the_persisted_snapshots() {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let offering = seed_offering(&app, &uid).await;
        insert_attempt(&app, "req-1", &offering, None, None, 100, Some(20), Some(8)).await;
        TelemetryStore::new()
            .refresh(&app.state.pool)
            .await
            .expect("refresh");

        let cold = TelemetryStore::new();
        assert_eq!(cold.warm(&app.state.pool).await.expect("warm"), 1);
        let stats = cold.stats(&offering).expect("warmed stats");
        assert_eq!(stats.samples, 1);
        assert_eq!(stats.latency_p95_ms, Some(100.0));
    }

    #[tokio::test]
    async fn startup_warming_is_wired_through_warm_at_startup() {
        // Review m1: `TelemetryStore::warm` used to be dead code with a doc
        // promise. `warm_at_startup` is what `main` runs before serving; the
        // cache must be populated from the snapshots by it alone.
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let offering = seed_offering(&app, &uid).await;
        insert_attempt(&app, "req-1", &offering, None, None, 100, Some(20), Some(8)).await;
        TelemetryStore::new()
            .refresh(&app.state.pool)
            .await
            .expect("refresh");

        let cold = TelemetryStore::new();
        warm_at_startup(&app.state.pool, &cold).await;
        assert_eq!(
            cold.stats(&offering).expect("warmed at startup").samples,
            1,
            "the startup warm populated the cache"
        );
    }

    #[tokio::test]
    async fn non_offering_outcomes_are_not_samples_but_are_still_recorded() {
        // `client_disconnected` and `invalid_request` measure nothing about
        // the offering; `upstream_quota`/`upstream_rate_limited` belong to
        // the credential (decision D26) and must not touch the offering's
        // reliability.
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let offering = seed_offering(&app, &uid).await;
        for (i, outcome) in NON_OFFERING_OUTCOMES.iter().enumerate() {
            insert_attempt(
                &app,
                &format!("req-{i}"),
                &offering,
                None,
                Some(outcome),
                100,
                None,
                None,
            )
            .await;
        }
        let store = TelemetryStore::new();
        store.refresh(&app.state.pool).await.expect("refresh");
        assert!(
            store.stats(&offering).is_none(),
            "no row measures the offering's service"
        );
        let recorded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM telemetry_attempts WHERE offering_id = ?")
                .bind(&offering)
                .fetch_one(&app.state.pool)
                .await
                .expect("count");
        assert_eq!(
            recorded,
            NON_OFFERING_OUTCOMES.len() as i64,
            "every attempt is still recorded, with its credential attribution"
        );
    }

    #[tokio::test]
    async fn record_attempts_persists_one_row_per_attempt_with_its_credential() {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let offering = seed_offering(&app, &uid).await;
        let provider_id: String =
            sqlx::query_scalar("SELECT provider_id FROM offerings WHERE id = ?")
                .bind(&offering)
                .fetch_one(&app.state.pool)
                .await
                .expect("provider id");
        let actor = Actor {
            user_id: &uid,
            ip: "127.0.0.1",
        };
        let mut credentials = Vec::new();
        for label in ["k1", "k2"] {
            let credential = catalog::create_credential(
                &app.state.pool,
                &app.state.secrets,
                &actor,
                &provider_id,
                &catalog::CredentialInput {
                    label: label.to_string(),
                    secret: format!("sk-{label}"),
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
            credentials.push(credential.id);
        }
        let samples = vec![
            AttemptSample {
                offering_id: offering.clone(),
                credential_id: Some(credentials[0].clone()),
                outcome: Some("upstream_auth"),
                latency_ms: Some(12),
                ttft_ms: None,
                output_tokens: None,
                upstream_status: Some(401),
                provider_error_code: Some("invalid_api_key".to_string()),
                error_detail: Some("invalid key".to_string()),
            },
            AttemptSample {
                offering_id: offering.clone(),
                credential_id: Some(credentials[1].clone()),
                outcome: None,
                latency_ms: Some(120),
                ttft_ms: Some(30),
                output_tokens: Some(8),
                upstream_status: None,
                provider_error_code: None,
                error_detail: None,
            },
        ];
        let mut tx = app.state.pool.begin().await.expect("tx");
        let written = record_attempts(&mut tx, "req-1", &samples)
            .await
            .expect("record");
        tx.commit().await.expect("commit");
        assert_eq!(written, 2);

        let rows: Vec<(String, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT credential_id, outcome, output_tokens FROM telemetry_attempts ORDER BY id",
        )
        .fetch_all(&app.state.pool)
        .await
        .expect("rows");
        assert_eq!(
            rows,
            [
                (
                    credentials[0].clone(),
                    Some("upstream_auth".to_string()),
                    None
                ),
                (credentials[1].clone(), None, Some(8)),
            ],
            "attempt order, credential attribution, and outcomes round-trip"
        );
    }

    /// Inserts one `telemetry_attempts` row shaped like a classified attempt.
    #[allow(clippy::too_many_arguments)]
    async fn insert_attempt(
        app: &TestApp,
        request_id: &str,
        offering_id: &str,
        credential_id: Option<&str>,
        outcome: Option<&str>,
        latency_ms: i64,
        ttft_ms: Option<i64>,
        output_tokens: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO telemetry_attempts \
             (request_id, offering_id, credential_id, outcome, latency_ms, ttft_ms, \
              output_tokens, occurred_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, '2026-11-16T00:00:00.000Z')",
        )
        .bind(request_id)
        .bind(offering_id)
        .bind(credential_id)
        .bind(outcome)
        .bind(latency_ms)
        .bind(ttft_ms)
        .bind(output_tokens)
        .execute(&app.state.pool)
        .await
        .expect("insert attempt row");
    }
}
