//! The TLS background service: HTTP-01 challenges, issuance, renewal, and the
//! HTTPS listener.
//!
//! Startup never waits for ACME. [`spawn`] only starts a task; the plain-HTTP
//! listener in `main` is already serving by then. Inside the task:
//!
//! 1. bind port 80 (challenge responses, then redirects),
//! 2. load a usable certificate from `<data>/tls/`, or obtain one,
//! 3. serve HTTPS on `GATEWAY_HTTPS_PORT`,
//! 4. wake 30 days before expiry, renew, and hot-swap the rustls configuration
//!    (new connections pick it up; nothing restarts).
//!
//! Any failure sets `tls.error` for the status API and retries after
//! [`FAILURE_RETRY`]. It never stops the gateway or the plain-HTTP path.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::Utc;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::AppState;
use crate::tls::provider::{AcmeProvider, CertificateProvider, ProviderError};
use crate::tls::store::CertStore;
use crate::tls::{MIN_RENEWAL_DELAY, RENEW_BEFORE, TlsError, TlsRuntime, path_and_query};

/// How long to wait after a failed issuance before trying again.
pub const FAILURE_RETRY: Duration = Duration::from_secs(60 * 60);

/// `Strict-Transport-Security` sent on every HTTPS response.
pub const HSTS_VALUE: &str = "max-age=31536000";

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("{0}")]
    Provider(#[from] ProviderError),
    #[error("{0}")]
    Tls(#[from] TlsError),
    #[error("TLS was requested without a domain")]
    MissingDomain,
}

/// Loads a usable stored certificate, or issues a new one, then installs it.
///
/// Sets `tls.error` and returns the error on failure; the caller keeps serving
/// HTTP. A usable stored certificate skips the network entirely, which is what
/// makes a restart cheap and offline-safe.
pub async fn bootstrap_once(
    runtime: &Arc<TlsRuntime>,
    provider: &dyn CertificateProvider,
) -> Result<(), BootstrapError> {
    let Some(domain) = runtime.domain().map(str::to_string) else {
        return Err(BootstrapError::MissingDomain);
    };
    let store = CertStore::new(runtime.config().cert_dir.clone());

    let result: Result<(), BootstrapError> = async {
        if let Some(stored) = store.load().map_err(TlsError::from)?
            && stored.is_usable(Utc::now(), &domain, RENEW_BEFORE)
        {
            runtime.install(&stored.chain_pem, &stored.key_pem)?;
            return Ok(());
        }
        let issued = provider
            .obtain(&domain, runtime.clone())
            .await
            .map_err(BootstrapError::from)?;
        store
            .save(&issued.chain_pem, &issued.key_pem)
            .map_err(TlsError::from)?;
        runtime.install(&issued.chain_pem, &issued.key_pem)?;
        Ok(())
    }
    .await;

    if let Err(error) = &result {
        runtime.set_error(error.to_string());
    }
    result
}

/// Starts the TLS background task when the decision calls for HTTPS.
///
/// Does nothing at all for a local bind, a missing domain, or `GATEWAY_TLS=off`
/// — the plain-HTTP path is untouched.
pub fn spawn(state: &AppState, app: Router) {
    let runtime = state.tls.clone();
    if !runtime.wants_tls() {
        return;
    }
    let provider: Arc<dyn CertificateProvider> = Arc::new(AcmeProvider::new(runtime.config()));
    let state = Arc::new(state.clone());
    tokio::spawn(async move { run(state, app, provider).await });
}

/// The service loop. Public so an integration test could drive it with a fake
/// provider; production reaches it through [`spawn`].
pub async fn run(state: Arc<AppState>, app: Router, provider: Arc<dyn CertificateProvider>) {
    let runtime = state.tls.clone();
    let http_addr = SocketAddr::new(state.config.listen.ip(), 80);
    let listener = match TcpListener::bind(http_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            let message = format!("cannot listen on {http_addr} for ACME HTTP-01: {error}");
            tracing::error!(%error, %http_addr, "automatic TLS disabled for this run");
            runtime.set_error(message);
            return;
        }
    };

    let challenge_router = http_router(runtime.clone());
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, challenge_router).await {
            tracing::error!(%error, "ACME/redirect listener stopped");
        }
    });
    tracing::info!(
        domain = runtime.domain().unwrap_or("-"),
        %http_addr,
        "automatic TLS started; HTTP-01 challenges and HTTP→HTTPS redirects are served on port 80"
    );

    let mut https_listening = false;
    loop {
        match bootstrap_once(&runtime, provider.as_ref()).await {
            Ok(()) => {
                if !https_listening {
                    match bind_https(&state).await {
                        Ok(listener) => {
                            https_listening = true;
                            spawn_https(state.clone(), app.clone(), listener);
                        }
                        Err(error) => {
                            let message = format!("cannot serve HTTPS: {error}");
                            tracing::error!(%error, "HTTPS listener failed to bind");
                            runtime.set_error(message);
                            tokio::time::sleep(FAILURE_RETRY).await;
                            continue;
                        }
                    }
                }
                let delay = runtime
                    .renewal_delay(Utc::now())
                    .unwrap_or(MIN_RENEWAL_DELAY);
                tracing::info!(
                    domain = runtime.domain().unwrap_or("-"),
                    renew_in_secs = delay.as_secs(),
                    "certificate is active"
                );
                tokio::time::sleep(delay).await;
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    retry_in_secs = FAILURE_RETRY.as_secs(),
                    "automatic TLS failed; the gateway keeps serving plain HTTP"
                );
                tokio::time::sleep(FAILURE_RETRY).await;
            }
        }
    }
}

/// Binds the HTTPS listener on the same interface as `GATEWAY_LISTEN`.
async fn bind_https(state: &AppState) -> std::io::Result<TcpListener> {
    let addr = SocketAddr::new(state.config.listen.ip(), state.config.tls.https_port);
    TcpListener::bind(addr).await
}

/// Serves the app over TLS with the current rustls configuration.
fn spawn_https(state: Arc<AppState>, app: Router, listener: TcpListener) {
    use axum::serve::ListenerExt;

    let router = app.layer(middleware::from_fn(hsts));
    let runtime = state.tls.clone();
    tokio::spawn(async move {
        let service = router.into_make_service_with_connect_info::<SocketAddr>();
        // `tap_io` is how axum learns the peer address of a custom listener
        // (`Connected` is implemented for `TapIo`); the closure itself is a
        // no-op because TLS settings are chosen inside `accept`.
        let listener = TlsListener {
            inner: listener,
            runtime,
        }
        .tap_io(|_| {});
        if let Err(error) = axum::serve(listener, service).await {
            tracing::error!(%error, "HTTPS listener stopped");
        }
    });
}

/// Adds `Strict-Transport-Security` to a response; used only on the HTTPS
/// router, never on plain HTTP.
async fn hsts(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static(HSTS_VALUE),
    );
    response
}

/// The port-80 router: ACME challenges, and a redirect for everything else
/// once a certificate is active.
pub fn http_router(runtime: Arc<TlsRuntime>) -> Router {
    Router::new()
        .route("/.well-known/acme-challenge/{token}", get(challenge))
        .fallback(redirect)
        .with_state(runtime)
}

/// Answers an HTTP-01 challenge with the key authorization, or 404 when the
/// token is unknown (a stale or forged request).
pub async fn challenge(
    State(runtime): State<Arc<TlsRuntime>>,
    Path(token): Path<String>,
) -> Response {
    match runtime.challenge(&token) {
        Some(key_authorization) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            key_authorization,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "unknown challenge token").into_response(),
    }
}

/// `308` to HTTPS when a certificate is active; `503` while one is still being
/// obtained. The target uses the configured domain, never the `Host` header, so
/// a hostile request cannot turn this into an open redirect.
pub async fn redirect(
    State(runtime): State<Arc<TlsRuntime>>,
    uri: axum::extract::OriginalUri,
) -> Response {
    if runtime.active_server_config().is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "HTTPS is not ready yet: a certificate is still being obtained.\n",
        )
            .into_response();
    }
    let Some(domain) = runtime.domain() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let location =
        crate::tls::redirect_location(domain, runtime.config().https_port, &path_and_query(&uri.0));
    match HeaderValue::from_str(&location) {
        Ok(location) => (
            StatusCode::PERMANENT_REDIRECT,
            [(header::LOCATION, location)],
            "This gateway is served over HTTPS.\n",
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// An axum [`Listener`](axum::serve::Listener) that reads the current rustls
/// configuration for **each** accepted connection, so a renewed certificate is
/// picked up without a restart.
pub struct TlsListener {
    inner: TcpListener,
    runtime: Arc<TlsRuntime>,
}

impl fmt::Debug for TlsListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsListener").finish_non_exhaustive()
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, peer) = match self.inner.accept().await {
                Ok(value) => value,
                Err(error) => {
                    tracing::debug!(%error, "TLS accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let Some(config) = self.runtime.active_server_config() else {
                continue;
            };
            let acceptor = TlsAcceptor::from(config);
            match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await {
                Ok(Ok(stream)) => return (stream, peer),
                Ok(Err(error)) => {
                    tracing::debug!(%error, %peer, "TLS handshake failed");
                }
                Err(_) => {
                    tracing::debug!(%peer, "TLS handshake timed out");
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DEFAULT_ACME_DIRECTORY, TlsConfig, TlsMode};
    use crate::tls::provider::{ChallengePublisher, IssuedCert};
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    struct FakeProvider {
        outcome: Result<(String, String), String>,
    }

    #[async_trait::async_trait]
    impl CertificateProvider for FakeProvider {
        async fn obtain(
            &self,
            domain: &str,
            challenges: Arc<dyn ChallengePublisher>,
        ) -> Result<IssuedCert, ProviderError> {
            match &self.outcome {
                Ok((chain, key)) => {
                    // Exercise the same publish path the real ACME client uses.
                    challenges.publish("test-token", &format!("{domain}.thumbprint"));
                    challenges.clear("test-token");
                    Ok(IssuedCert {
                        chain_pem: chain.clone(),
                        key_pem: key.clone(),
                    })
                }
                Err(message) => Err(ProviderError::Acme(message.clone())),
            }
        }
    }

    fn tls_config(dir: &std::path::Path, domain: &str) -> TlsConfig {
        TlsConfig {
            mode: TlsMode::Auto,
            public_domain: Some(domain.to_string()),
            acme_email: None,
            https_port: 443,
            acme_directory: DEFAULT_ACME_DIRECTORY.to_string(),
            cert_dir: dir.join("tls"),
        }
    }

    fn self_signed(domain: &str) -> (String, String) {
        let cert =
            rcgen::generate_simple_self_signed(vec![domain.to_string()]).expect("generate cert");
        (cert.cert.pem(), cert.signing_key.serialize_pem())
    }

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("gateway-tls-service-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn a_fake_provider_issues_and_installs_a_certificate() {
        let dir = temp_dir();
        let runtime = Arc::new(TlsRuntime::new(
            tls_config(&dir, "gateway.example.com"),
            "0.0.0.0:8081".parse().expect("addr"),
        ));
        let (chain, key) = self_signed("gateway.example.com");
        let provider = FakeProvider {
            outcome: Ok((chain, key)),
        };

        bootstrap_once(&runtime, &provider).await.expect("issued");
        let status = runtime.status();
        assert!(status.enabled, "{status:?}");
        assert_eq!(status.domain.as_deref(), Some("gateway.example.com"));
        assert_eq!(status.reason, None);
        assert_eq!(status.error, None);
        assert!(status.expires_at.expect("expiry").ends_with('Z'));
        assert!(runtime.active_server_config().is_some());
        assert!(runtime.active_expiry().is_some());

        // The store now holds the certificate; a restart skips the network.
        let stored = CertStore::new(dir.join("tls"))
            .load()
            .expect("load")
            .expect("present");
        assert!(stored.is_usable(Utc::now(), "gateway.example.com", RENEW_BEFORE));
    }

    #[tokio::test]
    async fn a_failure_sets_the_error_status_and_a_retry_can_recover() {
        let dir = temp_dir();
        let runtime = Arc::new(TlsRuntime::new(
            tls_config(&dir, "gateway.example.com"),
            "0.0.0.0:8081".parse().expect("addr"),
        ));

        let failing = FakeProvider {
            outcome: Err("ACME could not validate the domain".to_string()),
        };
        let error = bootstrap_once(&runtime, &failing)
            .await
            .expect_err("must fail");
        assert!(error.to_string().contains("could not validate"), "{error}");
        let status = runtime.status();
        assert!(!status.enabled);
        assert_eq!(status.domain.as_deref(), Some("gateway.example.com"));
        assert!(
            status
                .error
                .as_deref()
                .is_some_and(|e| e.contains("validate")),
            "{status:?}"
        );
        assert!(runtime.active_server_config().is_none());

        // Startup was never blocked: the port-80 router still answers, with a
        // "not ready" response instead of a crash or a redirect loop.
        let response = http_router(runtime.clone())
            .oneshot(
                Request::builder()
                    .uri("/console")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // The next attempt, with a working provider, recovers.
        let (chain, key) = self_signed("gateway.example.com");
        let working = FakeProvider {
            outcome: Ok((chain, key)),
        };
        bootstrap_once(&runtime, &working).await.expect("recovered");
        assert!(runtime.status().enabled);
        assert_eq!(runtime.status().error, None);
    }

    #[tokio::test]
    async fn the_challenge_endpoint_returns_only_published_tokens() {
        let dir = temp_dir();
        let runtime = Arc::new(TlsRuntime::new(
            tls_config(&dir, "gateway.example.com"),
            "0.0.0.0:8081".parse().expect("addr"),
        ));
        runtime.publish("abc123", "abc123.thumbprint");

        let known = http_router(runtime.clone())
            .oneshot(
                Request::builder()
                    .uri("/.well-known/acme-challenge/abc123")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(known.status(), StatusCode::OK);
        let body = axum::body::to_bytes(known.into_body(), 1024)
            .await
            .expect("body");
        assert_eq!(&body[..], b"abc123.thumbprint");

        let unknown = http_router(runtime)
            .oneshot(
                Request::builder()
                    .uri("/.well-known/acme-challenge/other")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn port_80_redirects_only_after_the_certificate_exists() {
        let dir = temp_dir();
        let runtime = Arc::new(TlsRuntime::new(
            tls_config(&dir, "gateway.example.com"),
            "0.0.0.0:8081".parse().expect("addr"),
        ));

        let before = http_router(runtime.clone())
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/settings?tab=security")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(before.status(), StatusCode::SERVICE_UNAVAILABLE);

        let (chain, key) = self_signed("gateway.example.com");
        runtime.install(&chain, &key).expect("install");

        let after = http_router(runtime)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions?stream=1")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(after.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            after
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("https://gateway.example.com/v1/chat/completions?stream=1")
        );
    }

    #[tokio::test]
    async fn the_https_layer_adds_strict_transport_security() {
        let router = Router::new()
            .route("/", axum::routing::get(|| async { "ok" }))
            .layer(middleware::from_fn(hsts));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response
                .headers()
                .get(header::STRICT_TRANSPORT_SECURITY)
                .and_then(|value| value.to_str().ok()),
            Some(HSTS_VALUE)
        );
    }

    #[test]
    fn a_listener_without_a_certificate_has_no_handshake_material() {
        let dir = temp_dir();
        let runtime = TlsRuntime::new(
            tls_config(&dir, "gateway.example.com"),
            "0.0.0.0:8081".parse().expect("addr"),
        );
        assert!(runtime.active_server_config().is_none());
        assert!(runtime.renewal_delay(Utc::now()).is_none());
    }
}
