//! Runtime configuration, read once from the environment at startup.
//!
//! Secrets are accepted from the environment only; nothing secret is read from
//! a committed file. Missing optional values fall back to safe defaults, and a
//! missing master key is reported as a warning until milestone 2 needs it.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

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
    /// Version reported by the health endpoint.
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("GATEWAY_LISTEN is not a valid socket address: {0}")]
    Listen(String),
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
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    /// Path of the SQLite database file inside the data directory.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("gateway.db")
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
}
