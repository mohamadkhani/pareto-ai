//! pareto-ai gateway — entry point.

mod access;
mod api_keys;
mod auth;
mod benchmark_sync;
mod benchmarks;
mod bootstrap;
mod breaker;
mod catalog;
mod chat;
mod combos;
mod config;
mod connectors;
mod db;
mod error;
mod ip;
#[cfg(test)]
mod load_test;
mod master_key;
mod mix;
mod money;
mod openapi;
mod password;
#[cfg(test)]
mod perf_bench;
mod pricing;
mod probe;
mod reports;
mod request_id;
mod routes;
mod routing;
mod secrets;
mod setup;
#[cfg(feature = "embedded-ui")]
mod static_files;
mod supervisor;
mod telemetry;
#[cfg(test)]
mod test_support;
mod throttle;
mod upstream;
mod users;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::secrets::SecretBox;

/// Shared application state. Cheap to clone; the pool is internally pooled.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: sqlx::SqlitePool,
    /// Credential secret encryption (D16); built from `GATEWAY_MASTER_KEY` or
    /// the generated `<data>/master.key`.
    pub secrets: Arc<SecretBox>,
    /// First-run mode machine and master-key provenance.
    pub setup: Arc<setup::SetupState>,
    /// Shared HTTP client for upstream calls (M3): pooled connections.
    pub upstream: Arc<upstream::UpstreamClient>,
    pub started: Instant,
    /// In-memory login failure tracking, shared by all requests.
    pub login_limiter: Arc<auth::LoginRateLimiter>,
    /// Per-key RPM/TPM token buckets for the data plane (M4).
    pub key_limiter: Arc<api_keys::KeyRateLimiter>,
    /// Meshed timestamps for the API-key `last_used_at` column (M4): the
    /// stamp is written at most once per key per
    /// [`api_keys::USAGE_STAMP_INTERVAL`] instead of on every request.
    pub key_usage_stamps: Arc<api_keys::KeyUsageStamps>,
    /// Session activity stamps (`sessions.last_seen_at`) at a slower cadence.
    pub session_activity: Arc<auth::SessionActivity>,
    /// Resolved token-mix profiles (D34), loaded once and invalidated by the
    /// admin mutations.
    pub mix_profiles: Arc<mix::MixProfileCache>,
    /// Per-offering rolling telemetry cache (M5), refreshed in the background.
    pub telemetry: Arc<telemetry::TelemetryStore>,
    /// Per-(credential, offering) circuit breaker state (M5).
    pub breaker: Arc<breaker::BreakerRegistry>,
    /// Guard shared by the background benchmark sync and the admin trigger,
    /// so two syncs never overlap (D31).
    pub benchmark_sync: Arc<tokio::sync::Mutex<()>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("gateway=info")),
        )
        .init();

    let config = Arc::new(Config::from_env().context("reading configuration")?);

    // Operator commands run without serving (no master key needed: rollups
    // hold no secrets). `gateway rebuild-rollups` re-derives `usage_daily`
    // from `usage_events` and exits.
    if std::env::args().nth(1).as_deref() == Some("rebuild-rollups") {
        let pool = db::open(&config.database_path())
            .await
            .context("starting database")?;
        let stats = reports::rebuild_rollups(&pool, config.tz)
            .await
            .context("rebuilding usage rollups")?;
        println!(
            "rebuilt usage_daily: {} rows from {} usage_events",
            stats.rows, stats.events
        );
        return Ok(());
    }
    // Credential encryption needs a key from the very first request. An
    // explicit `GATEWAY_MASTER_KEY` wins; otherwise the key is read from (or
    // generated into) `<data>/master.key`. Only the path is logged, never the
    // key.
    let resolved_key = master_key::resolve(config.master_key.as_ref(), &config.data_dir)?;
    if let Some(path) = &resolved_key.path {
        tracing::info!(
            path = %path.display(),
            source = resolved_key.source.as_str(),
            "credential master key loaded from the data directory"
        );
    }
    let secrets = Arc::new(master_key::secret_box(&resolved_key));
    let master_key_source = resolved_key.source;

    let upstream = Arc::new(
        upstream::UpstreamClient::new(config.timeout_connect)
            .context("building the upstream HTTP client")?,
    );
    if config.trusted_proxies.is_empty() && !config.listen.ip().is_loopback() {
        tracing::warn!(
            "GATEWAY_TRUSTED_PROXY is not set while listening on {} (non-loopback); \
             behind a reverse proxy every client will be recorded as the proxy's address, \
             because X-Forwarded-For is only read from trusted proxies",
            config.listen
        );
    }

    let pool = db::open(&config.database_path())
        .await
        .context("starting database")?;

    let bootstrap = bootstrap::ensure_admin(&pool).await?;
    let mode = setup::resolve_mode(&pool)
        .await
        .context("resolving setup mode")?;
    if bootstrap.setup_required() != (mode == setup::Mode::Setup) {
        // Defensive: the two derive from the same table and setting; a
        // disagreement means something changed under us mid-start.
        tracing::warn!(?bootstrap, mode = mode.as_str(), "setup mode mismatch");
    }

    // Exposure rule: setup and unprotected are local conveniences. Binding a
    // network interface in those modes would publish every route without a
    // password, so it refuses to start unless the operator insists.
    match setup::exposure(config.listen, mode, config.allow_public_no_auth) {
        Err(message) => bail!(message),
        Ok(Some(warning)) => tracing::warn!(%warning, "gateway is exposed"),
        Ok(None) => {}
    }
    match mode {
        setup::Mode::Setup => tracing::warn!(
            bind = %config.listen,
            "no users exist: open the console to run the first-run setup wizard, or send \
             POST /api/setup/skip to run without a password on this machine"
        ),
        setup::Mode::Unprotected => tracing::warn!(
            bind = %config.listen,
            "running without a password (setup was skipped); open Settings to set one"
        ),
        setup::Mode::Protected => {}
    }

    let breaker_threshold = config.breaker_threshold;
    let breaker_cooldown = config.breaker_cooldown;
    let state = AppState {
        config,
        pool,
        secrets,
        setup: Arc::new(setup::SetupState::new(mode, master_key_source)),
        upstream,
        started: Instant::now(),
        login_limiter: Arc::new(auth::LoginRateLimiter::new()),
        key_limiter: Arc::new(api_keys::KeyRateLimiter::new()),
        key_usage_stamps: Arc::new(api_keys::KeyUsageStamps::new()),
        session_activity: Arc::new(auth::SessionActivity::new()),
        mix_profiles: Arc::new(mix::MixProfileCache::new()),
        telemetry: Arc::new(telemetry::TelemetryStore::new()),
        breaker: Arc::new(breaker::BreakerRegistry::new(breaker::BreakerConfig {
            threshold: breaker_threshold,
            cooldown: breaker_cooldown,
        })),
        benchmark_sync: Arc::new(tokio::sync::Mutex::new(())),
    };

    // Warm the telemetry cache from `telemetry_snapshots` before the first
    // request is served — routing must not impute 0.5 for every offering
    // until the first background refresh lands (review m1 wires
    // `TelemetryStore::warm`; `warm_at_startup` is what runs here and what
    // the test pins).
    telemetry::warm_at_startup(&state.pool, &state.telemetry).await;

    // Background upkeep (M4): stale reservations must not lock budgets, and
    // the usage rollups are rebuilt periodically on top of the incremental
    // settlement updates. M5 adds the telemetry cache refresh (derived from
    // `telemetry_attempts`, the per-attempt samples the settle transaction
    // writes — decision D26).
    //
    // Supervised (review finding 12): each job is an infinite loop, so a task
    // that terminates has panicked or hit a bug. `supervisor::supervise` keeps
    // the `JoinHandle`, logs `error!` when the task terminates, and restarts
    // the job with exponential backoff (1 s doubling to 60 s, reset after a
    // healthy minute); the gateway keeps serving while the job is down. The
    // handles live for the lifetime of the process.
    // Automatic benchmark download (D31): a supervised job that syncs the
    // sources whose last success is older than the 5 h TTL.
    let benchmark_fetcher =
        benchmark_sync::HttpFetcher::new().context("building the benchmark sync client")?;

    let _background: [tokio::task::JoinHandle<()>; 6] = [
        supervisor::supervise("reservation reconciler", {
            let pool = state.pool.clone();
            let timeout = state.config.timeout_total;
            move || money::spawn_reconciler(pool.clone(), timeout)
        }),
        supervisor::supervise("usage rollup", {
            let pool = state.pool.clone();
            let tz = state.config.tz;
            move || reports::spawn_rollup_job(pool.clone(), tz)
        }),
        supervisor::supervise("telemetry refresh", {
            let pool = state.pool.clone();
            let store = state.telemetry.clone();
            let interval = state.config.telemetry_refresh;
            move || telemetry::spawn_telemetry_job(pool.clone(), store.clone(), interval)
        }),
        supervisor::supervise("benchmark sync", {
            let pool = state.pool.clone();
            let gate = state.benchmark_sync.clone();
            let fetcher = benchmark_fetcher.clone();
            move || benchmark_sync::spawn_sync_job(pool.clone(), fetcher.clone(), gate.clone())
        }),
        // D35: sticky pins expire after the combo's TTL; prune them
        // opportunistically every minute so the table stays bounded.
        supervisor::supervise("combo sticky prune", {
            let pool = state.pool.clone();
            move || combos::spawn_prune_job(pool.clone(), std::time::Duration::from_secs(60))
        }),
        // D42: provider balance connectors refresh each credential on their
        // own cadence (default 15 min, min 60 s, max 24 h) with exponential
        // backoff and jitter. Catalog refresh stays on demand.
        supervisor::supervise("provider balance refresh", {
            let pool = state.pool.clone();
            let secrets = state.secrets.clone();
            move || connectors::spawn_balance_job(pool.clone(), secrets.clone())
        }),
    ];

    let app = request_id::stack(
        Router::new()
            .merge(routes::public_routes())
            // The API router mounts itself at `/api` with its own fallbacks:
            // unknown `/api/*` paths and wrong methods get the uniform API error
            // shape instead of falling through to the UI.
            .merge(routes::api_routes(state.clone())),
    )
    .layer(TraceLayer::new_for_http())
    .with_state(state.clone());

    // The compiled Angular app is the fallback handler for everything outside
    // `/api`: either a static asset or an app route.
    #[cfg(feature = "embedded-ui")]
    let app = app.fallback(static_files::serve);

    let listener = tokio::net::TcpListener::bind(state.config.listen)
        .await
        .with_context(|| format!("binding {}", state.config.listen))?;

    tracing::info!(
        listen = %state.config.listen,
        version = %state.config.version,
        database = %state.config.database_path().display(),
        "gateway started"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server loop")?;

    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM, then lets in-flight requests finish.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
