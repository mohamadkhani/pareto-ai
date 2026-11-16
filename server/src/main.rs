//! pareto-ai gateway — entry point.

mod auth;
mod bootstrap;
mod catalog;
mod chat;
mod config;
mod db;
mod error;
mod ip;
mod openapi;
mod password;
mod pricing;
mod probe;
mod routes;
mod secrets;
#[cfg(feature = "embedded-ui")]
mod static_files;
#[cfg(test)]
mod test_support;
mod upstream;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::Router;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::secrets::{SecretBox, require_master_key};

/// Shared application state. Cheap to clone; the pool is internally pooled.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: sqlx::SqlitePool,
    /// Credential secret encryption (D16); built from `GATEWAY_MASTER_KEY`.
    pub secrets: Arc<SecretBox>,
    /// Shared HTTP client for upstream calls (M3): pooled connections.
    pub upstream: Arc<upstream::UpstreamClient>,
    pub started: Instant,
    /// In-memory login failure tracking, shared by all requests.
    pub login_limiter: Arc<auth::LoginRateLimiter>,
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
    // Credential encryption is required from milestone 2 on: without the key
    // no credential can be stored safely, so the gateway refuses to start.
    let secrets = Arc::new(require_master_key(config.master_key.as_ref())?);
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

    bootstrap::ensure_admin(&pool).await?;

    let state = AppState {
        config,
        pool,
        secrets,
        upstream,
        started: Instant::now(),
        login_limiter: Arc::new(auth::LoginRateLimiter::new()),
    };

    let request_id_header = axum::http::HeaderName::from_static("x-request-id");
    let app = Router::new()
        .merge(routes::public_routes())
        // The API router mounts itself at `/api` with its own fallbacks:
        // unknown `/api/*` paths and wrong methods get the uniform API error
        // shape instead of falling through to the UI.
        .merge(routes::api_routes(state.clone()))
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::new(
            request_id_header.clone(),
            MakeRequestUuid,
        ))
        .layer(PropagateRequestIdLayer::new(request_id_header))
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
