//! The credential-encryption master key: from the environment or a key file.
//!
//! Decision D16 requires a 32-byte key for AES-256-GCM. Originally
//! `GATEWAY_MASTER_KEY` was mandatory, which made a zero-configuration start
//! impossible; D38 keeps the environment override but generates a key on first
//! start otherwise:
//!
//! 1. `GATEWAY_MASTER_KEY`, when set and well-formed, always wins.
//! 2. Otherwise `<data>/master.key` is read when it exists.
//! 3. Otherwise 32 random bytes are generated, written to `<data>/master.key`
//!    with mode `0600`, and used.
//!
//! Only the path is ever logged; the key itself never is. A different
//! environment key than the one that sealed existing credentials surfaces as
//! the ordinary "failed to authenticate" decryption error, never as a crash.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;

use crate::secrets::{MasterKey, SecretBox, SecretError};

/// File name of the generated master key inside the data directory.
pub const MASTER_KEY_FILE: &str = "master.key";

/// Where the key in use came from. Reported read-only in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterKeySource {
    /// `GATEWAY_MASTER_KEY` was set and well-formed.
    Env,
    /// Read from (or generated into) `<data>/master.key`.
    File,
}

impl MasterKeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
        }
    }
}

/// The resolved key and its provenance.
#[derive(Debug)]
pub struct ResolvedMasterKey {
    pub key: MasterKey,
    pub source: MasterKeySource,
    /// The key file's path, present for [`MasterKeySource::File`].
    pub path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum MasterKeyError {
    #[error("cannot read the master key file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot create the master key file {path}: {source}")]
    Create {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "the master key file {path} does not hold a valid key: {source}; \
         delete it to generate a new one, but credentials encrypted with the old key will no longer decrypt"
    )]
    Invalid {
        path: PathBuf,
        source: Box<SecretError>,
    },
    #[error("the operating system random generator failed while creating the master key: {0}")]
    Rng(String),
    #[error("cannot restrict the master key file {path} to its owner: {source}")]
    Permissions {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Resolves the master key per the module rules.
///
/// Creates `<data>/master.key` (mode `0600`) when no environment key is set and
/// no file exists. Failure mode: [`MasterKeyError`] naming the path, never the
/// key material.
pub fn resolve(
    env_key: Option<&MasterKey>,
    data_dir: &Path,
) -> Result<ResolvedMasterKey, MasterKeyError> {
    if let Some(key) = env_key {
        return Ok(ResolvedMasterKey {
            key: key.clone(),
            source: MasterKeySource::Env,
            path: None,
        });
    }

    std::fs::create_dir_all(data_dir).map_err(|source| MasterKeyError::Create {
        path: data_dir.to_path_buf(),
        source,
    })?;

    let path = data_dir.join(MASTER_KEY_FILE);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    // Create the file owner-only from the very first byte; `restrict_permissions`
    // below tightens a pre-existing file as well.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            let key = generate_key()?;
            // The file exists from `create_new`; write the encoded key and
            // close it before restricting the mode, so a crash between the two
            // steps cannot leave a world-readable key behind on a second start
            // (the read path below tightens the mode again).
            let encoded = format!("{}\n", URL_SAFE_NO_PAD.encode(key_bytes(&key)));
            file.write_all(encoded.as_bytes())
                .map_err(|source| MasterKeyError::Create {
                    path: path.clone(),
                    source,
                })?;
            file.sync_all().map_err(|source| MasterKeyError::Create {
                path: path.clone(),
                source,
            })?;
            drop(file);
            restrict_permissions(&path)?;
            tracing::info!(
                path = %path.display(),
                "generated a master key for credential encryption"
            );
            Ok(ResolvedMasterKey {
                key,
                source: MasterKeySource::File,
                path: Some(path),
            })
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            restrict_permissions(&path)?;
            let encoded =
                std::fs::read_to_string(&path).map_err(|source| MasterKeyError::Read {
                    path: path.clone(),
                    source,
                })?;
            let key =
                MasterKey::parse(encoded.trim()).map_err(|source| MasterKeyError::Invalid {
                    path: path.clone(),
                    source: Box::new(source),
                })?;
            Ok(ResolvedMasterKey {
                key,
                source: MasterKeySource::File,
                path: Some(path),
            })
        }
        Err(source) => Err(MasterKeyError::Create {
            path: path.clone(),
            source,
        }),
    }
}

/// Builds the credential [`SecretBox`] for an already resolved key.
pub fn secret_box(resolved: &ResolvedMasterKey) -> SecretBox {
    SecretBox::new(resolved.key.clone())
}

/// A fresh 32-byte key from the OS random generator.
fn generate_key() -> Result<MasterKey, MasterKeyError> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|e| MasterKeyError::Rng(e.to_string()))?;
    Ok(MasterKey::from_bytes(bytes))
}

/// The key's raw bytes are private; encode through the box-less accessor.
fn key_bytes(key: &MasterKey) -> &[u8] {
    key.expose_bytes()
}

/// Sets the key file to owner read/write only (`0600`).
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), MasterKeyError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        MasterKeyError::Permissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), MasterKeyError> {
    // Windows has no POSIX mode bits; the file inherits the directory ACL.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gateway-master-key-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn env_key(byte: u8) -> MasterKey {
        MasterKey::from_bytes([byte; 32])
    }

    #[test]
    fn generates_a_key_file_with_owner_only_permissions_and_reuses_it() {
        let dir = temp_dir("create");
        let first = resolve(None, &dir).expect("generate");
        assert_eq!(first.source, MasterKeySource::File);
        let path = first.path.clone().expect("path");
        assert_eq!(path, dir.join(MASTER_KEY_FILE));

        let contents = std::fs::read_to_string(&path).expect("key file");
        assert_eq!(
            contents.trim().len(),
            43,
            "base64url of 32 bytes is 43 characters"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the key file is owner-only");
        }

        // A second start reads the same key instead of generating one.
        let second = resolve(None, &dir).expect("reuse");
        assert_eq!(format!("{:?}", second.key), format!("{:?}", first.key));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .expect("loosen");
            resolve(None, &dir).expect("reuse");
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "an existing file is tightened again");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_environment_key_wins_and_creates_no_file() {
        let dir = temp_dir("env");
        let key = env_key(0x2a);
        let resolved = resolve(Some(&key), &dir).expect("env key");
        assert_eq!(resolved.source, MasterKeySource::Env);
        assert!(resolved.path.is_none());
        assert!(
            !dir.join(MASTER_KEY_FILE).exists(),
            "no file is written when the environment supplies the key"
        );

        // The box built from it round-trips a secret.
        let boxx = secret_box(&resolved);
        let sealed = boxx.encrypt("sk-secret", "cred-1").expect("seal");
        assert_eq!(boxx.decrypt(&sealed, "cred-1").expect("open"), "sk-secret");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_different_key_fails_to_open_existing_ciphertext_cleanly() {
        let dir = temp_dir("mismatch");
        let generated = resolve(None, &dir).expect("generate");
        let sealed = secret_box(&generated)
            .encrypt("sk-secret", "cred-1")
            .expect("seal");

        let mismatched = resolve(Some(&env_key(0x2b)), &dir).expect("env key");
        let err = secret_box(&mismatched)
            .decrypt(&sealed, "cred-1")
            .expect_err("wrong key must not open the ciphertext");
        assert!(matches!(err, SecretError::Decrypt), "{err:?}");
        assert!(!err.to_string().contains("sk-secret"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_invalid_key_file_fails_naming_the_path() {
        let dir = temp_dir("invalid");
        let path = dir.join(MASTER_KEY_FILE);
        std::fs::write(&path, "not-a-key\n").expect("write");
        let err = resolve(None, &dir).expect_err("invalid key file");
        let message = err.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("does not hold a valid key"), "{message}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_a_hex_key_file_written_by_hand() {
        let dir = temp_dir("hex");
        let path = dir.join(MASTER_KEY_FILE);
        std::fs::write(&path, format!("{}\n", "2a".repeat(32))).expect("write");
        let resolved = resolve(None, &dir).expect("hex key");
        assert_eq!(resolved.source, MasterKeySource::File);
        assert_eq!(format!("{:?}", resolved.key), "MasterKey(****)");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn creates_a_missing_data_directory() {
        let dir = temp_dir("nested").join("a").join("b");
        let resolved = resolve(None, &dir).expect("creates directories");
        assert!(resolved.path.expect("path").is_file());
        std::fs::remove_dir_all(dir.parent().expect("parent")).ok();
    }
}
