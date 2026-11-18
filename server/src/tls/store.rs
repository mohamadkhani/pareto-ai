//! On-disk certificate store under `<data>/tls/` (decision D54).
//!
//! Layout:
//!
//! | File | Contents | Mode |
//! | --- | --- | --- |
//! | `account.json` | ACME account credentials (contains a private key) | `0600` |
//! | `fullchain.pem` | leaf certificate followed by its intermediates | `0644` |
//! | `privkey.pem` | the leaf's PKCS#8 private key | `0600` |
//!
//! Writes go to a temporary file in the same directory and are renamed into
//! place, so a crash never leaves a half-written certificate behind. A missing
//! or unparsable file is reported as an error the caller handles by issuing a
//! fresh certificate; it never aborts startup.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use x509_parser::prelude::*;

/// Certificate, key, and expiry loaded from the store.
#[derive(Debug, Clone)]
pub struct StoredCert {
    /// PEM chain: leaf first, then intermediates.
    pub chain_pem: String,
    /// PEM PKCS#8 (or PKCS#1) private key for the leaf.
    pub key_pem: String,
    /// UTC instant after which the leaf is invalid.
    pub not_after: DateTime<Utc>,
    /// DNS names in the leaf's subject alternative name extension.
    pub domains: Vec<String>,
}

impl StoredCert {
    /// True when the certificate covers `domain` and stays valid at least
    /// `margin` from `now`.
    pub fn is_usable(&self, now: DateTime<Utc>, domain: &str, margin: chrono::Duration) -> bool {
        self.not_after > now + margin
            && self
                .domains
                .iter()
                .any(|name| name.eq_ignore_ascii_case(domain))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("reading the certificate store at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the certificate store holds no certificate yet")]
    Empty,
    #[error("the certificate store is unreadable: {0}")]
    Invalid(String),
}

/// The `<data>/tls/` directory. Every method takes the directory so a test can
/// use a temporary path.
#[derive(Debug, Clone)]
pub struct CertStore {
    dir: PathBuf,
}

impl CertStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Reads the stored certificate, verifying that the chain and key parse and
    /// that the leaf carries an expiry. `Ok(None)` means "nothing stored yet".
    pub fn load(&self) -> Result<Option<StoredCert>, StoreError> {
        let chain_path = self.path("fullchain.pem");
        let key_path = self.path("privkey.pem");
        let chain_pem = match fs::read_to_string(&chain_path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    path: chain_path,
                    source,
                });
            }
        };
        let key_pem = fs::read_to_string(&key_path).map_err(|source| StoreError::Io {
            path: key_path.clone(),
            source,
        })?;

        let certs = pem_certs(&chain_pem)?;
        let leaf = certs.first().ok_or(StoreError::Empty)?;
        let (domains, not_after) = leaf_details(leaf)?;
        Ok(Some(StoredCert {
            chain_pem,
            key_pem,
            not_after,
            domains,
        }))
    }

    /// Writes the chain and key atomically, restricting the key to the owner
    /// and creating the directory with mode `0700` on Unix.
    pub fn save(&self, chain_pem: &str, key_pem: &str) -> Result<(), StoreError> {
        fs::create_dir_all(&self.dir).map_err(|source| StoreError::Io {
            path: self.dir.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700));
        }
        write_atomic(&self.path("fullchain.pem"), chain_pem.as_bytes(), 0o644)?;
        write_atomic(&self.path("privkey.pem"), key_pem.as_bytes(), 0o600)?;
        Ok(())
    }

    /// Reads the saved ACME account credentials, if any.
    pub fn load_account(&self) -> Result<Option<String>, StoreError> {
        match fs::read_to_string(self.path("account.json")) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StoreError::Io {
                path: self.path("account.json"),
                source,
            }),
        }
    }

    /// Saves the ACME account credentials (they contain a private key), with
    /// mode `0600`.
    pub fn save_account(&self, json: &str) -> Result<(), StoreError> {
        fs::create_dir_all(&self.dir).map_err(|source| StoreError::Io {
            path: self.dir.clone(),
            source,
        })?;
        write_atomic(&self.path("account.json"), json.as_bytes(), 0o600)
    }
}

/// Parses every certificate in a PEM document, returning their DER bytes.
fn pem_certs(chain_pem: &str) -> Result<Vec<Vec<u8>>, StoreError> {
    let mut reader = Cursor::new(chain_pem.as_bytes());
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| StoreError::Invalid(format!("certificate PEM: {e}")))?;
    if certs.is_empty() {
        return Err(StoreError::Invalid(
            "no certificate found in fullchain.pem".to_string(),
        ));
    }
    Ok(certs.into_iter().map(|der| der.to_vec()).collect())
}

/// Extracts the DNS names and expiry from a DER-encoded leaf certificate.
pub fn leaf_details(der: &[u8]) -> Result<(Vec<String>, DateTime<Utc>), StoreError> {
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|e| StoreError::Invalid(format!("certificate DER: {e}")))?;
    let not_after = cert.validity().not_after.to_datetime();
    let not_after = DateTime::<Utc>::from_timestamp(not_after.unix_timestamp(), 0)
        .ok_or_else(|| StoreError::Invalid("certificate expiry out of range".to_string()))?;
    let mut domains = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            if let GeneralName::DNSName(name) = name {
                domains.push((*name).to_string());
            }
        }
    }
    Ok((domains, not_after))
}

/// Writes `bytes` to a temporary sibling and renames it over `path`.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), StoreError> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("file")
    ));
    fs::write(&tmp, bytes).map_err(|source| StoreError::Io {
        path: tmp.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)).map_err(|source| {
            StoreError::Io {
                path: tmp.clone(),
                source,
            }
        })?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    fs::rename(&tmp, path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Duration};
    use rcgen::{CertificateParams, KeyPair};

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gateway-tls-store-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// A self-signed cert for `name` whose expiry is `days` from today.
    fn self_signed(name: &str, days: i64) -> (String, String) {
        let mut params = CertificateParams::new(vec![name.to_string()]).expect("params");
        let today = Utc::now().date_naive() + Duration::days(days);
        params.not_after =
            rcgen::date_time_ymd(today.year(), today.month() as u8, today.day() as u8);
        let key = KeyPair::generate().expect("key");
        let cert = params.self_signed(&key).expect("certificate");
        (cert.pem(), key.serialize_pem())
    }

    #[test]
    fn a_missing_store_is_none_not_an_error() {
        let store = CertStore::new(temp_dir());
        assert!(store.load().expect("load").is_none());
        assert!(store.load_account().expect("account").is_none());
    }

    #[test]
    fn save_load_round_trips_chain_key_expiry_and_domain() {
        let dir = temp_dir();
        let store = CertStore::new(&dir);
        let (chain, key) = self_signed("gateway.example.com", 90);
        store.save(&chain, &key).expect("save");

        let loaded = store.load().expect("load").expect("present");
        assert_eq!(loaded.domains, ["gateway.example.com"]);
        assert!(loaded.not_after > Utc::now() + Duration::days(80));
        assert!(loaded.is_usable(Utc::now(), "gateway.example.com", Duration::days(30)));
        assert!(
            !loaded.is_usable(Utc::now(), "other.example.com", Duration::days(30)),
            "a certificate only covers its own names"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("privkey.pem"))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the private key is owner-only");
        }
    }

    #[test]
    fn a_certificate_is_not_usable_inside_the_renewal_margin() {
        let dir = temp_dir();
        let store = CertStore::new(&dir);
        let (chain, key) = self_signed("gateway.example.com", 10);
        store.save(&chain, &key).expect("save");
        let loaded = store.load().expect("load").expect("present");
        assert!(
            !loaded.is_usable(Utc::now(), "gateway.example.com", Duration::days(30)),
            "10 days left is inside the 30-day renewal window"
        );
    }

    #[test]
    fn corrupt_or_empty_files_are_reported() {
        let dir = temp_dir();
        let store = CertStore::new(&dir);
        fs::write(dir.join("fullchain.pem"), "not a certificate").expect("write");
        fs::write(dir.join("privkey.pem"), "not a key").expect("write");
        let err = store.load().expect_err("must fail");
        assert!(
            matches!(err, StoreError::Invalid(_) | StoreError::Empty),
            "{err}"
        );

        fs::write(dir.join("fullchain.pem"), "").expect("write");
        assert!(store.load().is_err());
    }

    #[test]
    fn account_credentials_round_trip_owner_only() {
        let dir = temp_dir();
        let store = CertStore::new(&dir);
        store
            .save_account(r#"{"id":"acct-1","key_pkcs8":"secret"}"#)
            .expect("save");
        assert_eq!(
            store.load_account().expect("load").as_deref(),
            Some(r#"{"id":"acct-1","key_pkcs8":"secret"}"#)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("account.json"))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
