//! Runtime configuration, read once from the environment at startup.
//!
//! Every variable is optional: a start with no environment at all listens on
//! `127.0.0.1:8081`, keeps its data in `./data`, and generates its credential
//! master key on first start (`master_key.rs`). Secrets are accepted from the
//! environment only; nothing secret is read from a committed file.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::ip::IpCidr;
use crate::secrets::{self, MasterKey};

/// Bind address of a zero-configuration start (`GATEWAY_LISTEN` unset).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8081";
/// Data directory of a zero-configuration start, relative to the working
/// directory (`GATEWAY_DATA_DIR` unset).
pub const DEFAULT_DATA_DIR: &str = "data";

/// Largest request body the data plane accepts (`GATEWAY_MAX_BODY_BYTES`).
pub const DEFAULT_MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Upstream connect timeout (`GATEWAY_TIMEOUT_CONNECT_MS`).
pub const DEFAULT_TIMEOUT_CONNECT: Duration = Duration::from_secs(10);
/// Time from upstream request start to the first response byte
/// (`GATEWAY_TIMEOUT_FIRST_BYTE_MS`).
pub const DEFAULT_TIMEOUT_FIRST_BYTE: Duration = Duration::from_secs(30);
/// Maximum gap between upstream body chunks (`GATEWAY_TIMEOUT_IDLE_MS`).
pub const DEFAULT_TIMEOUT_IDLE: Duration = Duration::from_secs(60);
/// Total budget for one client request including failover attempts
/// (`GATEWAY_TIMEOUT_TOTAL_MS`).
pub const DEFAULT_TIMEOUT_TOTAL: Duration = Duration::from_secs(600);
/// Telemetry cache refresh interval (`GATEWAY_TELEMETRY_REFRESH_MS`).
pub const DEFAULT_TELEMETRY_REFRESH: Duration = Duration::from_secs(30);
/// Consecutive failures before a (credential, offering) breaker opens
/// (`GATEWAY_BREAKER_THRESHOLD`).
pub const DEFAULT_BREAKER_THRESHOLD: u32 = crate::breaker::DEFAULT_THRESHOLD;
/// How long an open breaker excludes the pair (`GATEWAY_BREAKER_COOLDOWN_MS`).
pub const DEFAULT_BREAKER_COOLDOWN: Duration = crate::breaker::DEFAULT_COOLDOWN;

#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub listen: SocketAddr,
    /// Directory that holds the SQLite database and its WAL files.
    pub data_dir: PathBuf,
    /// AEAD key for credential encryption at rest, as 32 bytes.
    /// `None` means "generate or read `<data>/master.key`" (see
    /// [`crate::master_key`]).
    pub master_key: Option<MasterKey>,
    /// When true, startup may continue on a non-loopback bind while the
    /// gateway is in setup or unprotected mode (`GATEWAY_ALLOW_PUBLIC_NO_AUTH=1`).
    /// The exposure warning still shows.
    pub allow_public_no_auth: bool,
    /// IANA timezone used for budget periods (day/week/month boundaries),
    /// as written by `GATEWAY_TIMEZONE`.
    pub timezone: String,
    /// [`Self::timezone`] parsed for use; startup fails on an unknown zone.
    pub tz: chrono_tz::Tz,
    /// When true, session cookies are sent without `Secure` so the gateway can
    /// be used over plain HTTP during development (`GATEWAY_INSECURE_COOKIES=1`).
    pub insecure_cookies: bool,
    /// Reverse proxies trusted to vouch for the client address in
    /// `X-Forwarded-For` / `X-Real-IP` (`GATEWAY_TRUSTED_PROXY`, comma-separated
    /// CIDRs like `10.0.0.0/8,127.0.0.1/32`). Empty: the peer address is the
    /// client and those headers are never read.
    pub trusted_proxies: Vec<IpCidr>,
    /// Largest chat request body accepted, in bytes
    /// (`GATEWAY_MAX_BODY_BYTES`, default 4 MiB).
    pub max_body_bytes: usize,
    /// Upstream TCP connect timeout (`GATEWAY_TIMEOUT_CONNECT_MS`, default 10s).
    pub timeout_connect: Duration,
    /// Budget from upstream request start to the first response byte
    /// (`GATEWAY_TIMEOUT_FIRST_BYTE_MS`, default 30s).
    pub timeout_first_byte: Duration,
    /// Maximum gap between upstream body chunks (`GATEWAY_TIMEOUT_IDLE_MS`,
    /// default 60s).
    pub timeout_idle: Duration,
    /// Total budget for one client request, failover attempts included
    /// (`GATEWAY_TIMEOUT_TOTAL_MS`, default 600s).
    pub timeout_total: Duration,
    /// How often the per-offering telemetry cache rebuilds from
    /// `telemetry_attempts` (`GATEWAY_TELEMETRY_REFRESH_MS`, default 30s).
    pub telemetry_refresh: Duration,
    /// Consecutive failures before a (credential, offering) circuit breaker
    /// opens (`GATEWAY_BREAKER_THRESHOLD`, default 5).
    pub breaker_threshold: u32,
    /// How long an open circuit breaker excludes the pair
    /// (`GATEWAY_BREAKER_COOLDOWN_MS`, default 60s).
    pub breaker_cooldown: Duration,
    /// Default token-mix profile selector (name or id) from
    /// `GATEWAY_TOKEN_MIX_PROFILE`. It wins over the
    /// `pricing.default_mix_profile` setting (decision D34); absent/blank
    /// means the setting decides.
    pub token_mix_profile: Option<String>,
    /// Version reported by the health endpoint.
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("GATEWAY_LISTEN is not a valid socket address: {0}")]
    Listen(String),
    #[error("GATEWAY_TRUSTED_PROXY entry is not a valid CIDR: {0}")]
    TrustedProxy(String),
    #[error(
        "GATEWAY_TIMEZONE is not a known IANA timezone: {0} \
         (use a zone name like UTC, Europe/Berlin, or Asia/Tehran)"
    )]
    Timezone(String),
    #[error("{name} is not a positive integer: {value}")]
    Number { name: &'static str, value: String },
    #[error("cannot determine the current directory: {0}")]
    CurrentDir(#[from] std::io::Error),
    #[error("{0}")]
    MasterKey(#[from] secrets::SecretError),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let listen = env::var("GATEWAY_LISTEN")
            .unwrap_or_else(|_| DEFAULT_LISTEN.to_string())
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
            None => env::current_dir()?.join(DEFAULT_DATA_DIR),
        };

        let timezone = env::var("GATEWAY_TIMEZONE").unwrap_or_else(|_| "UTC".to_string());
        let tz = parse_timezone(&timezone)?;

        Ok(Self {
            listen,
            data_dir,
            master_key: parse_master_key(env::var("GATEWAY_MASTER_KEY").ok().as_deref())?,
            allow_public_no_auth: env::var("GATEWAY_ALLOW_PUBLIC_NO_AUTH").as_deref() == Ok("1"),
            timezone,
            tz,
            insecure_cookies: env::var("GATEWAY_INSECURE_COOKIES").as_deref() == Ok("1"),
            trusted_proxies: match env::var("GATEWAY_TRUSTED_PROXY") {
                Ok(raw) => parse_trusted_proxies(&raw)?,
                Err(_) => Vec::new(),
            },
            max_body_bytes: parse_usize("GATEWAY_MAX_BODY_BYTES", DEFAULT_MAX_BODY_BYTES)?,
            timeout_connect: parse_ms("GATEWAY_TIMEOUT_CONNECT_MS", DEFAULT_TIMEOUT_CONNECT)?,
            timeout_first_byte: parse_ms(
                "GATEWAY_TIMEOUT_FIRST_BYTE_MS",
                DEFAULT_TIMEOUT_FIRST_BYTE,
            )?,
            timeout_idle: parse_ms("GATEWAY_TIMEOUT_IDLE_MS", DEFAULT_TIMEOUT_IDLE)?,
            timeout_total: parse_ms("GATEWAY_TIMEOUT_TOTAL_MS", DEFAULT_TIMEOUT_TOTAL)?,
            telemetry_refresh: parse_ms("GATEWAY_TELEMETRY_REFRESH_MS", DEFAULT_TELEMETRY_REFRESH)?,
            breaker_threshold: parse_u32("GATEWAY_BREAKER_THRESHOLD", DEFAULT_BREAKER_THRESHOLD)?,
            breaker_cooldown: parse_ms("GATEWAY_BREAKER_COOLDOWN_MS", DEFAULT_BREAKER_COOLDOWN)?,
            token_mix_profile: env::var("GATEWAY_TOKEN_MIX_PROFILE")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    /// Path of the SQLite database file inside the data directory.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("gateway.db")
    }
}

/// Parses `GATEWAY_TIMEZONE` into an IANA timezone (default `UTC`). Failure
/// mode: [`ConfigError::Timezone`] naming the offending value.
fn parse_timezone(raw: &str) -> Result<chrono_tz::Tz, ConfigError> {
    use std::str::FromStr;
    let trimmed = raw.trim();
    let name = if trimmed.is_empty() { "UTC" } else { trimmed };
    chrono_tz::Tz::from_str(name).map_err(|_| ConfigError::Timezone(name.to_string()))
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

/// Parses the `GATEWAY_MASTER_KEY` value: absent or empty is `None`, which
/// makes [`crate::master_key::resolve`] read or generate `<data>/master.key`;
/// a malformed value is a [`ConfigError::MasterKey`] telling the operator how
/// to fix it.
fn parse_master_key(raw: Option<&str>) -> Result<Option<MasterKey>, ConfigError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => Ok(Some(MasterKey::parse(value)?)),
    }
}

/// Reads a millisecond duration variable (`*_MS`), falling back to `default`
/// when absent or blank. Zero and non-numeric values are startup errors: a
/// silent 0 would disable the timeout.
fn parse_ms(name: &'static str, default: Duration) -> Result<Duration, ConfigError> {
    Ok(Duration::from_millis(parse_positive(
        name,
        env::var(name).ok(),
        default.as_millis() as u64,
    )?))
}

/// Reads an unsigned size variable, falling back to `default` when absent or
/// blank. Zero and non-numeric values are startup errors.
fn parse_usize(name: &'static str, default: usize) -> Result<usize, ConfigError> {
    parse_positive(name, env::var(name).ok(), default as u64).map(|value| value as usize)
}

/// Reads an unsigned 32-bit variable, falling back to `default` when absent
/// or blank. Zero, non-numeric, and out-of-range values are startup errors:
/// the conversion is checked, never a silent truncation (review m6 — `as u32`
/// wrapped values above 2^32).
fn parse_u32(name: &'static str, default: u32) -> Result<u32, ConfigError> {
    checked_u32(
        name,
        parse_positive(name, env::var(name).ok(), u64::from(default))?,
    )
}

/// The checked `u64 → u32` conversion behind [`parse_u32`]. Failure mode:
/// [`ConfigError::Number`] naming the variable and the offending value.
fn checked_u32(name: &'static str, value: u64) -> Result<u32, ConfigError> {
    u32::try_from(value).map_err(|_| ConfigError::Number {
        name,
        value: value.to_string(),
    })
}

/// Parses an unsigned variable value; absent or blank falls back to `default`,
/// while zero and non-numeric values fail naming the variable.
fn parse_positive(
    name: &'static str,
    raw: Option<String>,
    default: u64,
) -> Result<u64, ConfigError> {
    match raw {
        None => Ok(default),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(default);
            }
            trimmed
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| ConfigError::Number {
                    name,
                    value: raw.clone(),
                })
        }
    }
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
    fn the_no_configuration_start_uses_documented_defaults() {
        // The defaults are constants, so the README, the wizard, and the
        // binary cannot drift. The live check is conditional because the test
        // process may carry the variables (`just dev` does not set them).
        assert_eq!(DEFAULT_LISTEN, "127.0.0.1:8081");
        assert_eq!(DEFAULT_DATA_DIR, "data");

        let cfg = Config::from_env().expect("config must parse");
        if env::var_os("GATEWAY_LISTEN").is_none() {
            assert_eq!(cfg.listen.to_string(), DEFAULT_LISTEN);
            assert!(cfg.listen.ip().is_loopback());
        }
        if env::var_os("GATEWAY_DATA_DIR").is_none() {
            assert!(
                cfg.data_dir.ends_with(DEFAULT_DATA_DIR),
                "{:?}",
                cfg.data_dir
            );
        }
        if env::var_os("GATEWAY_MASTER_KEY").is_none() {
            assert!(
                cfg.master_key.is_none(),
                "without the variable the key comes from <data>/master.key"
            );
        }
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

    #[test]
    fn timeout_and_size_variables_parse_or_fail_by_name() {
        // Absent and blank fall back to the default.
        assert_eq!(
            parse_positive("X", None, 600).expect("absent falls back"),
            600
        );
        assert_eq!(
            parse_positive("X", Some("  ".to_string()), 600).expect("blank falls back"),
            600
        );
        assert_eq!(
            parse_positive("X", Some("1500".to_string()), 600).expect("number parses"),
            1500
        );

        // Zero would silently disable a timeout; it must not be accepted.
        for bad in ["0", "-5", "1.5", "abc"] {
            let err = parse_positive("GATEWAY_TIMEOUT_IDLE_MS", Some(bad.to_string()), 600)
                .expect_err("bad values must fail");
            assert!(
                err.to_string().contains("GATEWAY_TIMEOUT_IDLE_MS"),
                "the error names the variable: {err}"
            );
        }
    }

    #[test]
    fn breaker_threshold_conversions_are_checked_never_truncated() {
        // Review m6: `as u32` silently wrapped values above 2^32.
        assert_eq!(checked_u32("X", 5).expect("in range"), 5);
        assert_eq!(
            checked_u32("X", u64::from(u32::MAX)).expect("boundary"),
            u32::MAX
        );
        for bad in [u64::from(u32::MAX) + 1, u64::MAX] {
            let err =
                checked_u32("GATEWAY_BREAKER_THRESHOLD", bad).expect_err("out of range must fail");
            assert!(
                err.to_string().contains("GATEWAY_BREAKER_THRESHOLD"),
                "the error names the variable: {err}"
            );
        }
    }

    #[test]
    fn master_key_is_parsed_or_actionably_rejected() {
        // 43 base64url characters decode to 32 zero bytes.
        let parsed =
            parse_master_key(Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")).expect("parse");
        assert_eq!(
            format!("{:?}", parsed.expect("present key")),
            "MasterKey(****)",
            "the key never shows up in Debug output"
        );

        assert!(parse_master_key(None).expect("absent").is_none());
        assert!(parse_master_key(Some("   ")).expect("blank").is_none());

        let err = parse_master_key(Some("too-short")).expect_err("short key must fail");
        assert!(
            err.to_string().contains("openssl rand"),
            "the error is actionable: {err}"
        );
    }
}
