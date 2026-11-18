//! Automatic TLS for a public domain (decision D54).
//!
//! The gateway only asks for a certificate when **all** of these hold:
//!
//! 1. `GATEWAY_TLS` is `auto` (the default),
//! 2. the bind address is a public interface (not loopback, private, or
//!    link-local), and
//! 3. `GATEWAY_PUBLIC_DOMAIN` names a valid hostname.
//!
//! With TLS active the gateway serves HTTPS on `GATEWAY_HTTPS_PORT` (default
//! 443), answers ACME HTTP-01 challenges on port 80, and redirects every other
//! port-80 request to HTTPS with `308`. The plain-HTTP listener on
//! `GATEWAY_LISTEN` keeps working, so a reverse proxy or a local operator is
//! never locked out.
//!
//! Issuance happens in a background task. A failure never blocks or stops the
//! gateway: the HTTP path keeps serving, the error is logged, and
//! `/api/setup/status` reports `tls.error` until a retry succeeds.

pub mod provider;
pub mod service;
pub mod store;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::http::Uri;
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use crate::config::{TlsConfig, TlsMode};
use crate::tls::provider::ChallengePublisher;
use crate::tls::store::leaf_details;

/// Certificates are renewed this far before their expiry.
pub const RENEW_BEFORE: chrono::Duration = chrono::Duration::days(30);
/// Floor for the renewal delay, so an already-due or failed renewal retries at
/// most once a minute instead of spinning.
pub const MIN_RENEWAL_DELAY: Duration = Duration::from_secs(60);

/// Why automatic TLS is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsOffReason {
    /// The bind is loopback/private/link-local: no public certificate can help.
    Local,
    /// `GATEWAY_TLS=off`.
    Off,
    /// A public bind without `GATEWAY_PUBLIC_DOMAIN`.
    NoDomain,
}

impl TlsOffReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Off => "off",
            Self::NoDomain => "no_domain",
        }
    }
}

/// The startup decision: either request a certificate for a domain, or keep
/// plain HTTP for a named reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsDecision {
    Disabled { reason: TlsOffReason },
    Enabled { domain: String },
}

/// Resolves the TLS decision from the bind address and configuration.
///
/// Precedence: an explicit `off` always wins; a local bind is never upgraded
/// (a public certificate cannot validate against a private address); a public
/// bind needs a domain.
pub fn decide(bind: SocketAddr, config: &TlsConfig) -> TlsDecision {
    if config.mode == TlsMode::Off {
        return TlsDecision::Disabled {
            reason: TlsOffReason::Off,
        };
    }
    if is_local_ip(bind.ip()) {
        return TlsDecision::Disabled {
            reason: TlsOffReason::Local,
        };
    }
    match &config.public_domain {
        Some(domain) => TlsDecision::Enabled {
            domain: domain.clone(),
        },
        None => TlsDecision::Disabled {
            reason: TlsOffReason::NoDomain,
        },
    }
}

/// True for addresses that cannot be validated by a public CA: loopback,
/// RFC 1918, link-local, and IPv6 unique-local.
pub fn is_local_ip(ip: IpAddr) -> bool {
    match ip {
        // `0.0.0.0` is deliberately not local: it is the usual public bind.
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_private() || v4.is_link_local())
        }
    }
}

/// JSON shape of `tls` in `/api/setup/status` and the Settings page.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TlsStatus {
    /// True only once a certificate is installed and HTTPS is being served.
    pub enabled: bool,
    /// Certificate hostname (configured even when issuance has failed).
    pub domain: Option<String>,
    /// Why TLS is off: `local`, `off`, `no_domain`, or `pending` while the
    /// first certificate is being obtained. `null` when enabled.
    pub reason: Option<String>,
    /// The last issuance/renewal failure, kept until a retry succeeds.
    pub error: Option<String>,
    /// Certificate expiry (ISO-8601 UTC) once enabled.
    pub expires_at: Option<String>,
}

impl TlsStatus {
    fn initial(decision: &TlsDecision, configured_domain: Option<&String>) -> Self {
        match decision {
            TlsDecision::Disabled { reason } => Self {
                enabled: false,
                domain: configured_domain.cloned(),
                reason: Some(reason.as_str().to_string()),
                error: None,
                expires_at: None,
            },
            TlsDecision::Enabled { domain } => Self {
                enabled: false,
                domain: Some(domain.clone()),
                reason: Some("pending".to_string()),
                error: None,
                expires_at: None,
            },
        }
    }

    /// True when this is the "public HTTP bind without usable TLS" state the
    /// console warns about.
    pub fn needs_attention(&self) -> bool {
        !self.enabled && self.reason.as_deref() != Some(TlsOffReason::Local.as_str())
    }
}

/// One installed certificate: the rustls configuration served to new
/// connections plus its expiry (for renewal scheduling).
struct ActiveCert {
    server_config: Arc<rustls::ServerConfig>,
    expires_at: DateTime<Utc>,
}

/// Shared TLS state: the immutable decision plus the observable status, the
/// current rustls configuration, and the live HTTP-01 challenge tokens.
pub struct TlsRuntime {
    config: TlsConfig,
    decision: TlsDecision,
    status: RwLock<TlsStatus>,
    active: RwLock<Option<ActiveCert>>,
    challenges: RwLock<HashMap<String, String>>,
}

impl std::fmt::Debug for TlsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsRuntime")
            .field("decision", &self.decision)
            .field("status", &self.status())
            .finish()
    }
}

impl TlsRuntime {
    pub fn new(config: TlsConfig, bind: SocketAddr) -> Self {
        let decision = decide(bind, &config);
        let status = TlsStatus::initial(&decision, config.public_domain.as_ref());
        Self {
            config,
            decision,
            status: RwLock::new(status),
            active: RwLock::new(None),
            challenges: RwLock::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &TlsConfig {
        &self.config
    }

    pub fn decision(&self) -> &TlsDecision {
        &self.decision
    }

    /// True when the decision calls for HTTPS and a background task should run.
    pub fn wants_tls(&self) -> bool {
        matches!(self.decision, TlsDecision::Enabled { .. })
    }

    pub fn domain(&self) -> Option<&str> {
        match &self.decision {
            TlsDecision::Enabled { domain } => Some(domain),
            TlsDecision::Disabled { .. } => None,
        }
    }

    /// The status shown by the API and the console.
    pub fn status(&self) -> TlsStatus {
        self.status
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Marks a failed issuance/renewal. HTTPS stops being advertised, but any
    /// previously installed configuration stays usable for existing
    /// connections and the HTTP listener is untouched.
    pub fn set_error(&self, message: impl Into<String>) {
        let message = message.into();
        let mut status = self
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        status.enabled = false;
        status.reason = None;
        status.error = Some(message);
        status.expires_at = None;
    }

    /// Parses `chain_pem`/`key_pem`, builds a rustls configuration, and makes it
    /// the one new TLS connections use. Failure leaves the previous
    /// configuration and the error status in place.
    pub fn install(&self, chain_pem: &str, key_pem: &str) -> Result<(), TlsError> {
        ensure_crypto_provider();
        let certs = parse_chain(chain_pem)?;
        let leaf = certs.first().ok_or(TlsError::EmptyChain)?;
        let (_, not_after) = leaf_details(leaf)?;
        let key = parse_key(key_pem)?;

        let mut server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| TlsError::Config(e.to_string()))?;
        server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let server_config = Arc::new(server_config);

        {
            let mut active = self
                .active
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *active = Some(ActiveCert {
                server_config,
                expires_at: not_after,
            });
        }

        let expires_at = not_after.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut status = self
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if status.domain.is_none() {
            status.domain = self.domain().map(str::to_string);
        }
        status.enabled = true;
        status.reason = None;
        status.error = None;
        status.expires_at = Some(expires_at);
        Ok(())
    }

    /// The rustls configuration for a new connection; `None` until a
    /// certificate is installed.
    pub fn active_server_config(&self) -> Option<Arc<rustls::ServerConfig>> {
        self.active
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|active| active.server_config.clone())
    }

    /// Expiry of the installed certificate, if any.
    pub fn active_expiry(&self) -> Option<DateTime<Utc>> {
        self.active
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|active| active.expires_at)
    }

    /// How long to wait before the next renewal, at least
    /// [`MIN_RENEWAL_DELAY`]. `None` when no certificate is installed (the
    /// service then retries issuance).
    pub fn renewal_delay(&self, now: DateTime<Utc>) -> Option<Duration> {
        self.active_expiry()
            .map(|expiry| renewal_delay(expiry, now))
    }

    fn set_challenge(&self, token: &str, key_authorization: &str) {
        self.challenges
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(token.to_string(), key_authorization.to_string());
    }

    fn clear_challenge(&self, token: &str) {
        self.challenges
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(token);
    }

    /// The key authorization for a live challenge token, if any.
    pub fn challenge(&self, token: &str) -> Option<String> {
        self.challenges
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(token)
            .cloned()
    }
}

impl ChallengePublisher for TlsRuntime {
    fn publish(&self, token: &str, key_authorization: &str) {
        self.set_challenge(token, key_authorization);
    }

    fn clear(&self, token: &str) {
        self.clear_challenge(token);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("the certificate chain is empty")]
    EmptyChain,
    #[error("the PEM data is invalid: {0}")]
    Pem(String),
    #[error("the certificate or key was rejected: {0}")]
    Config(String),
    #[error("the certificate store failed: {0}")]
    Store(#[from] crate::tls::store::StoreError),
}

/// Installs the process-wide rustls crypto provider once.
///
/// Both `ring` and `aws-lc-rs` end up enabled through transitive features, so
/// rustls refuses to choose on its own; this pins `aws-lc-rs`, the provider
/// `instant-acme` uses by default. Calling it again is a no-op.
pub fn ensure_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Parses the PEM certificate chain into DER certificates.
fn parse_chain(
    chain_pem: &str,
) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, TlsError> {
    let mut reader = std::io::Cursor::new(chain_pem.as_bytes());
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::Pem(format!("certificate chain: {e}")))?;
    if certs.is_empty() {
        return Err(TlsError::EmptyChain);
    }
    Ok(certs)
}

/// Parses the PEM private key (PKCS#8, PKCS#1, or SEC1).
fn parse_key(key_pem: &str) -> Result<rustls::pki_types::PrivateKeyDer<'static>, TlsError> {
    let mut reader = std::io::Cursor::new(key_pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| TlsError::Pem(format!("private key: {e}")))?
        .ok_or_else(|| TlsError::Pem("no private key found".to_string()))
}

/// Time from `now` until 30 days before `expires_at`, floored at
/// [`MIN_RENEWAL_DELAY`].
pub fn renewal_delay(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    let target = expires_at - RENEW_BEFORE;
    let delta = target - now;
    if delta <= chrono::Duration::zero() {
        return MIN_RENEWAL_DELAY;
    }
    delta.to_std().unwrap_or(MIN_RENEWAL_DELAY)
}

/// The `Location` for an HTTP→HTTPS redirect, omitting `:443`.
pub fn redirect_location(domain: &str, https_port: u16, path_and_query: &str) -> String {
    let port = match https_port {
        443 => String::new(),
        port => format!(":{port}"),
    };
    format!("https://{domain}{port}{path_and_query}")
}

/// The path-and-query of `uri`, or `/` when the URI carries neither.
pub fn path_and_query(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DEFAULT_ACME_DIRECTORY, TlsMode};

    fn tls_config(mode: TlsMode, domain: Option<&str>) -> TlsConfig {
        TlsConfig {
            mode,
            public_domain: domain.map(str::to_string),
            acme_email: None,
            https_port: 443,
            acme_directory: DEFAULT_ACME_DIRECTORY.to_string(),
            cert_dir: std::path::PathBuf::from("/tmp/unused-cert-store"),
        }
    }

    fn addr(value: &str) -> SocketAddr {
        value.parse().expect("address")
    }

    #[test]
    fn the_decision_matrix_is_exhaustive() {
        // Local binds are never upgraded, with or without a domain.
        for domain in [None, Some("gateway.example.com")] {
            assert_eq!(
                decide(addr("127.0.0.1:8081"), &tls_config(TlsMode::Auto, domain)),
                TlsDecision::Disabled {
                    reason: TlsOffReason::Local
                },
                "loopback, domain {domain:?}"
            );
        }
        for bind in [
            "10.0.0.5:8081",
            "192.168.1.20:8081",
            "169.254.1.1:8081",
            "[::1]:8081",
        ] {
            assert_eq!(
                decide(
                    addr(bind),
                    &tls_config(TlsMode::Auto, Some("gateway.example.com"))
                ),
                TlsDecision::Disabled {
                    reason: TlsOffReason::Local
                },
                "{bind}"
            );
        }

        // A public bind with a domain wants TLS.
        assert_eq!(
            decide(
                addr("0.0.0.0:8081"),
                &tls_config(TlsMode::Auto, Some("gateway.example.com"))
            ),
            TlsDecision::Enabled {
                domain: "gateway.example.com".to_string()
            }
        );

        // A public bind without a domain stays HTTP and says why.
        assert_eq!(
            decide(addr("0.0.0.0:8081"), &tls_config(TlsMode::Auto, None)),
            TlsDecision::Disabled {
                reason: TlsOffReason::NoDomain
            }
        );

        // `off` wins over a public bind and a domain.
        assert_eq!(
            decide(
                addr("0.0.0.0:8081"),
                &tls_config(TlsMode::Off, Some("gateway.example.com"))
            ),
            TlsDecision::Disabled {
                reason: TlsOffReason::Off
            }
        );
    }

    #[test]
    fn status_reports_the_off_reason_and_never_looks_like_https() {
        let runtime = TlsRuntime::new(
            tls_config(TlsMode::Auto, Some("gateway.example.com")),
            addr("127.0.0.1:8081"),
        );
        let status = runtime.status();
        assert!(!status.enabled);
        assert_eq!(status.reason.as_deref(), Some("local"));
        assert_eq!(status.domain.as_deref(), Some("gateway.example.com"));
        assert_eq!(status.error, None);
        assert_eq!(status.expires_at, None);
        assert!(
            !status.needs_attention(),
            "a local bind is expected, not a fault"
        );
        assert!(!runtime.wants_tls());

        let public = TlsRuntime::new(tls_config(TlsMode::Auto, None), addr("0.0.0.0:8081"));
        assert_eq!(public.status().reason.as_deref(), Some("no_domain"));
        assert!(public.status().needs_attention());

        let pending = TlsRuntime::new(
            tls_config(TlsMode::Auto, Some("gateway.example.com")),
            addr("0.0.0.0:8081"),
        );
        assert!(pending.wants_tls());
        assert_eq!(pending.status().reason.as_deref(), Some("pending"));
        assert!(pending.status().needs_attention());
    }

    #[test]
    fn a_failure_keeps_the_domain_and_names_the_error() {
        let runtime = TlsRuntime::new(
            tls_config(TlsMode::Auto, Some("gateway.example.com")),
            addr("0.0.0.0:8081"),
        );
        runtime.set_error("ACME request failed: connection refused");
        let status = runtime.status();
        assert!(!status.enabled);
        assert_eq!(status.domain.as_deref(), Some("gateway.example.com"));
        assert_eq!(status.reason, None);
        assert_eq!(
            status.error.as_deref(),
            Some("ACME request failed: connection refused")
        );
        assert_eq!(status.expires_at, None);
    }

    #[test]
    fn renewal_is_scheduled_thirty_days_before_expiry() {
        let now = DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("now");
        let expiry = now + chrono::Duration::days(90);
        let delay = renewal_delay(expiry, now);
        assert_eq!(
            delay.as_secs(),
            60 * 86_400,
            "90 days minus the 30-day margin"
        );
    }

    #[test]
    fn a_certificate_inside_the_window_or_expired_renews_immediately() {
        let now = DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("now");
        for expiry in [
            now + chrono::Duration::days(29),
            now + chrono::Duration::days(20),
            now,
            now - chrono::Duration::days(1),
        ] {
            assert_eq!(
                renewal_delay(expiry, now),
                MIN_RENEWAL_DELAY,
                "expiry {expiry}"
            );
        }
    }

    #[test]
    fn redirects_target_https_and_keep_the_path_and_query() {
        assert_eq!(
            redirect_location("gateway.example.com", 443, "/api/setup/status?x=1"),
            "https://gateway.example.com/api/setup/status?x=1"
        );
        assert_eq!(
            redirect_location("gateway.example.com", 8443, "/"),
            "https://gateway.example.com:8443/"
        );
        let uri: Uri = "/v1/models?limit=2".parse().expect("uri");
        assert_eq!(path_and_query(&uri), "/v1/models?limit=2");
    }
}
