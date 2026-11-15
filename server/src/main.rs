//! pareto-ai gateway — entry point.

mod config;
mod db;
mod routes;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::Router;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::config::Config;

/// Shared application state. Cheap to clone; the pool is internally pooled.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: sqlx::SqlitePool,
    pub started: Instant,
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
    if config.master_key.is_none() {
        tracing::warn!(
            "GATEWAY_MASTER_KEY is not set; credential encryption is unavailable until milestone 2"
        );
    }

    let pool = db::open(&config.database_path())
        .await
        .context("starting database")?;

    let state = AppState {
        config,
        pool,
        started: Instant::now(),
    };

    let request_id_header = axum::http::HeaderName::from_static("x-request-id");
    let app = Router::new()
        .merge(routes::public_routes())
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::new(
            request_id_header.clone(),
            MakeRequestUuid,
        ))
        .layer(PropagateRequestIdLayer::new(request_id_header))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(state.config.listen)
        .await
        .with_context(|| format!("binding {}", state.config.listen))?;

    tracing::info!(
        listen = %state.config.listen,
        version = %state.config.version,
        database = %state.config.database_path().display(),
        "gateway started"
    );

    axum::serve(listener, app)
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
