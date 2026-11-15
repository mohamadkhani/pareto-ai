//! Runtime configuration, read once from the environment at startup.
//!
//! Secrets are accepted from the environment only; nothing secret is read from
//! a committed file. Missing optional values fall back to safe defaults, and a
//! missing master key is reported as a warning until milestone 2 needs it.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::ip::IpCidr;

#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub listen: SocketAddr,
    /// Directory that holds the SQLite database and its WAL files.
    pub data_dir: PathBuf,
    /// AEAD key for credential encryption at rest, as 32 bytes.
    /// Present from milestone 2; absent values disable encryption and are warned.
    pub master_key: Option<String>,
    /// IANA timezone used for budget periods (day/week/month boundaries).
    pub timezone: String,
    /// When true, session cookies are sent without `Secure` so the gateway can
    /// be used over plain HTTP during development (`GATEWAY_INSECURE_COOKIES=1`).
    pub insecure_cookies: bool,
    /// Reverse proxies trusted to vouch for the client address in
    /// `X-Forwarded-For` / `X-Real-IP` (`GATEWAY_TRUSTED_PROXY`, comma-separated
    /// CIDRs like `10.0.0.0/8,127.0.0.1/32`). Empty: the peer address is the
    /// client and those headers are never read.
    pub trusted_proxies: Vec<IpCidr>,
    /// Version reported by the health endpoint.
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("GATEWAY_LISTEN is not a valid socket address: {0}")]
    Listen(String),
    #[error("GATEWAY_TRUSTED_PROXY entry is not a valid CIDR: {0}")]
    TrustedProxy(String),
    #[error("cannot determine the current directory: {0}")]
    CurrentDir(#[from] std::io::Error),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let listen = env::var("GATEWAY_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:8081".to_string())
            .parse::<SocketAddr>()
            .map_err(|e| ConfigError::Listen(e.to_string()))?;

        // Defaults to `./data` so a first run works with no environment set.
        let data_dir = match env::var_os("GATEWAY_DATA_DIR") {
            Some(value) => {
                let path = PathBuf::from(value);
                match path.is_absolute() {
                    true => path,
                    false => env::current_dir()?.join(path),
                }
            }
            None => env::current_dir()?.join("data"),
        };

        Ok(Self {
            listen,
            data_dir,
            master_key: env::var("GATEWAY_MASTER_KEY")
                .ok()
                .filter(|v| !v.is_empty()),
            timezone: env::var("GATEWAY_TIMEZONE").unwrap_or_else(|_| "UTC".to_string()),
            insecure_cookies: env::var("GATEWAY_INSECURE_COOKIES").as_deref() == Ok("1"),
            trusted_proxies: match env::var("GATEWAY_TRUSTED_PROXY") {
                Ok(raw) => parse_trusted_proxies(&raw)?,
                Err(_) => Vec::new(),
            },
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    /// Path of the SQLite database file inside the data directory.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("gateway.db")
    }
}

/// Parses `GATEWAY_TRUSTED_PROXY`: comma-separated IPv4/IPv6 CIDRs, where a
/// bare address is a single-host entry. Fails naming the offending entry.
fn parse_trusted_proxies(raw: &str) -> Result<Vec<IpCidr>, ConfigError> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            IpCidr::parse(entry).map_err(|e| ConfigError::TrustedProxy(format!("{entry}: {e}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        // The test process may carry gateway variables, so assert on the parts
        // that must never change meaning.
        let cfg = Config::from_env().expect("config must parse");
        assert!(cfg.database_path().ends_with("gateway.db"));
        assert!(!cfg.version.is_empty());
    }

    #[test]
    fn parses_comma_separated_trusted_proxies() {
        let proxies =
            parse_trusted_proxies("10.0.0.0/8, 127.0.0.1/32 ,2001:db8::/32").expect("valid");
        assert_eq!(proxies.len(), 3);
        assert!(proxies[0].contains("10.9.8.7".parse().expect("ip")));
        assert!(proxies[2].contains("2001:db8::1".parse().expect("ip")));

        assert!(parse_trusted_proxies("").expect("empty").is_empty());
        assert!(parse_trusted_proxies(" , ").expect("blanks").is_empty());
    }

    #[test]
    fn rejects_malformed_trusted_proxy_entries() {
        let err = parse_trusted_proxies("10.0.0.0/8,garbage").expect_err("must fail");
        assert!(err.to_string().contains("garbage"), "{err}");
    }
}
