//! AEAD box for provider credential secrets (decision D16).
//!
//! Secrets are sealed with AES-256-GCM from the `ring` crate — `aes-gcm` and
//! `chacha20poly1305` are not in the offline cargo cache — using the 32-byte
//! `GATEWAY_MASTER_KEY` (base64url or hex). The stored format is
//! `v1:<b64url nonce>:<b64url ciphertext+tag>` with a fresh random 12-byte
//! nonce per encryption and the credential row id as associated data, so a
//! ciphertext moved to another row fails to open instead of leaking.
//!
//! The key material never appears in `Debug` output, is never logged, and is
//! zeroized on drop (key bytes and plaintext buffers alike). The version
//! prefix is stable and rejected on read when unknown, so a future key
//! rotation can version its format.

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroize;
use zeroize::Zeroizing;

/// Prefix of every ciphertext this module produces.
pub const VERSION_PREFIX: &str = "v1";

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error(
        "GATEWAY_MASTER_KEY is required to encrypt provider credentials; \
         generate one with `openssl rand -base64 32 | tr '+/' '-_'` and restart"
    )]
    Missing,
    #[error(
        "GATEWAY_MASTER_KEY must decode to exactly 32 bytes (got {got}); \
         generate one with `openssl rand -base64 32 | tr '+/' '-_'`"
    )]
    KeyLength { got: usize },
    #[error(
        "GATEWAY_MASTER_KEY must be base64url or hex; \
         generate one with `openssl rand -base64 32 | tr '+/' '-_'`"
    )]
    KeyEncoding,
    #[error("a secret could not be encrypted: the random generator failed")]
    Rng,
    #[error("a secret could not be encrypted: sealing the plaintext failed")]
    Seal,
    #[error("stored secret is not in the v1:<nonce>:<ciphertext> format")]
    Format,
    #[error("stored secret uses unsupported version {0:?}; only v1 is supported")]
    Version(String),
    #[error("stored secret failed to authenticate: wrong key or tampered ciphertext")]
    Decrypt,
    #[error("decrypted secret is not valid UTF-8")]
    Utf8,
}

/// A parsed `GATEWAY_MASTER_KEY`: exactly 32 key bytes.
///
/// `Debug` prints a fixed mask so the key cannot leak into a log line or a
/// panic message, and `Drop` zeroes the bytes so the key does not linger in
/// freed memory.
#[derive(Clone)]
pub struct MasterKey([u8; KEY_LEN]);

impl Drop for MasterKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl MasterKey {
    /// Wraps raw key bytes. The caller vouches for the length.
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Parses `GATEWAY_MASTER_KEY`: base64url (padded or not) or hex,
    /// decoding to exactly 32 bytes. Empty input is a length error of 0.
    pub fn parse(raw: &str) -> Result<Self, SecretError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SecretError::KeyLength { got: 0 });
        }

        // The decoded key bytes are wiped once they are copied into the
        // fixed-size array.
        let decoded = Zeroizing::new(
            if trimmed.len() == KEY_LEN * 2 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
                hex_decode(trimmed).ok_or(SecretError::KeyEncoding)?
            } else {
                URL_SAFE_NO_PAD
                    .decode(trimmed)
                    .or_else(|_| URL_SAFE.decode(trimmed))
                    .map_err(|_| SecretError::KeyEncoding)?
            },
        );

        let bytes: [u8; KEY_LEN] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| SecretError::KeyLength { got: decoded.len() })?;
        Ok(Self(bytes))
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(****)")
    }
}

fn hex_decode(raw: &str) -> Option<Vec<u8>> {
    let digits = raw.as_bytes();
    let mut out = Vec::with_capacity(digits.len() / 2);
    for pair in digits.as_chunks::<2>().0 {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// Encrypts and decrypts credential secrets with AES-256-GCM (D16).
///
/// Holds the raw key bytes and builds the `ring` key per operation, so the
/// box itself is trivially `Send`/`Sync` and safe to share through `AppState`.
/// `Debug` is redacted for the same reason as [`MasterKey`], and `Drop`
/// zeroes the stored key bytes.
pub struct SecretBox {
    key: [u8; KEY_LEN],
}

impl Drop for SecretBox {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl SecretBox {
    /// Builds the box from a parsed master key.
    pub fn new(key: MasterKey) -> Self {
        Self { key: key.0 }
    }

    /// Seals `plaintext` under `aad` (the credential row id), returning
    /// `v1:<b64url nonce>:<b64url ct>`. Fails only when the OS random
    /// generator or the key setup fails.
    pub fn encrypt(&self, plaintext: &str, aad: &str) -> Result<String, SecretError> {
        let key = self.aead_key()?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| SecretError::Rng)?;

        // Zeroized on drop: until the tag is appended this buffer is the
        // plaintext, and afterwards it is ciphertext that costs nothing to
        // wipe.
        let mut buf = Zeroizing::new(plaintext.as_bytes().to_vec());
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(aad.as_bytes()),
            &mut *buf,
        )
        .map_err(|_| SecretError::Seal)?;

        Ok(format!(
            "{VERSION_PREFIX}:{}:{}",
            URL_SAFE_NO_PAD.encode(nonce_bytes),
            URL_SAFE_NO_PAD.encode(buf.as_slice()),
        ))
    }

    /// Reverses [`SecretBox::encrypt`]. Rejects a foreign version prefix, a
    /// malformed envelope, a tampered ciphertext, the wrong key, and a
    /// ciphertext whose AAD does not match `aad`.
    ///
    /// The returned `String` is the caller's to wipe; the intermediate
    /// plaintext buffer is zeroized on drop.
    pub fn decrypt(&self, ciphertext: &str, aad: &str) -> Result<String, SecretError> {
        let mut parts = ciphertext.split(':');
        let (version, nonce_b64, ct_b64) =
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some(v), Some(n), Some(c), None) => (v, n, c),
                _ => return Err(SecretError::Format),
            };
        if version != VERSION_PREFIX {
            return Err(SecretError::Version(version.to_string()));
        }

        let nonce_raw = URL_SAFE_NO_PAD
            .decode(nonce_b64)
            .or_else(|_| URL_SAFE.decode(nonce_b64))
            .map_err(|_| SecretError::Format)?;
        let nonce: [u8; NONCE_LEN] = nonce_raw.try_into().map_err(|_| SecretError::Format)?;
        // Zeroized on drop: `open_in_place` writes the plaintext back into it.
        let mut buf = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(ct_b64)
                .or_else(|_| URL_SAFE.decode(ct_b64))
                .map_err(|_| SecretError::Format)?,
        );

        let key = self.aead_key()?;
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad.as_bytes()),
                &mut buf,
            )
            .map_err(|_| SecretError::Decrypt)?;
        let text = std::str::from_utf8(plain).map_err(|_| SecretError::Utf8)?;
        Ok(text.to_string())
    }

    fn aead_key(&self) -> Result<LessSafeKey, SecretError> {
        let unbound =
            UnboundKey::new(&AES_256_GCM, &self.key).map_err(|_| SecretError::KeyEncoding)?;
        Ok(LessSafeKey::new(unbound))
    }
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox(****)")
    }
}

/// Builds the credential [`SecretBox`] from the configured master key.
///
/// Fails with an actionable message when `GATEWAY_MASTER_KEY` is absent, so
/// the gateway cannot come up with credential encryption unavailable.
pub fn require_master_key(master_key: Option<&MasterKey>) -> Result<SecretBox, SecretError> {
    match master_key {
        Some(key) => Ok(SecretBox::new(key.clone())),
        None => Err(SecretError::Missing),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_a() -> MasterKey {
        MasterKey::from_bytes([0x2a; KEY_LEN])
    }

    fn key_b() -> MasterKey {
        MasterKey::from_bytes([0x2b; KEY_LEN])
    }

    /// 32 bytes as base64url without padding: `key_a()`'s bytes.
    fn key_a_base64url() -> String {
        URL_SAFE_NO_PAD.encode([0x2a; KEY_LEN])
    }

    #[test]
    fn round_trip_recovers_the_plaintext() {
        let boxx = SecretBox::new(key_a());
        let cipher = boxx
            .encrypt("sk-live-super-secret", "cred-1")
            .expect("encrypt");
        assert!(cipher.starts_with("v1:"), "stable version prefix: {cipher}");
        assert_eq!(
            boxx.decrypt(&cipher, "cred-1").expect("decrypt"),
            "sk-live-super-secret"
        );
    }

    #[test]
    fn two_encryptions_of_the_same_plaintext_differ() {
        let boxx = SecretBox::new(key_a());
        let first = boxx.encrypt("same", "cred-1").expect("encrypt");
        let second = boxx.encrypt("same", "cred-1").expect("encrypt");
        assert_ne!(first, second, "each encryption uses a fresh random nonce");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let boxx = SecretBox::new(key_a());
        let cipher = boxx.encrypt("secret-value", "cred-1").expect("encrypt");

        let mut parts: Vec<String> = cipher.split(':').map(str::to_string).collect();
        let mut ct = URL_SAFE_NO_PAD.decode(&parts[2]).expect("ct b64");
        ct[0] ^= 0xff;
        parts[2] = URL_SAFE_NO_PAD.encode(ct);
        let tampered = parts.join(":");

        assert!(matches!(
            boxx.decrypt(&tampered, "cred-1"),
            Err(SecretError::Decrypt)
        ));
    }

    #[test]
    fn wrong_key_fails_cleanly() {
        let cipher = SecretBox::new(key_a())
            .encrypt("secret-value", "cred-1")
            .expect("encrypt");
        let err = SecretBox::new(key_b())
            .decrypt(&cipher, "cred-1")
            .expect_err("wrong key must fail");
        // The message is actionable for an operator but reveals nothing.
        let message = err.to_string();
        assert!(!message.contains("secret-value"));
        assert!(matches!(err, SecretError::Decrypt));
    }

    #[test]
    fn wrong_aad_fails_the_ciphertext_moved_between_rows() {
        let boxx = SecretBox::new(key_a());
        let cipher = boxx.encrypt("secret-value", "cred-1").expect("encrypt");
        assert!(matches!(
            boxx.decrypt(&cipher, "cred-2"),
            Err(SecretError::Decrypt)
        ));
    }

    #[test]
    fn unknown_version_prefix_is_rejected() {
        let boxx = SecretBox::new(key_a());
        let cipher = boxx.encrypt("secret-value", "cred-1").expect("encrypt");
        let v2 = cipher.replacen("v1:", "v2:", 1);
        match boxx.decrypt(&v2, "cred-1") {
            Err(SecretError::Version(v)) => assert_eq!(v, "v2"),
            other => panic!("expected a version error, got {other:?}"),
        }

        // Malformed envelopes fail on the format, not on a panic.
        for bad in [
            "",
            "v1",
            "v1:",
            "v1:only-two",
            "v1:a:b:c",
            "::",
            "v1:!!!:!!!",
        ] {
            assert!(
                matches!(
                    boxx.decrypt(bad, "cred-1"),
                    Err(SecretError::Format) | Err(SecretError::Version(_))
                ),
                "{bad:?} must not decrypt"
            );
        }
    }

    #[test]
    fn master_key_accepts_base64url_and_hex() {
        let from_b64 = MasterKey::parse(&key_a_base64url()).expect("base64url key");
        assert_eq!(from_b64.0, [0x2a; KEY_LEN]);

        let from_b64_padded =
            MasterKey::parse(&URL_SAFE.encode([0x2a; KEY_LEN])).expect("padded base64url key");
        assert_eq!(from_b64_padded.0, [0x2a; KEY_LEN]);

        let from_hex = MasterKey::parse(&"2a".repeat(KEY_LEN)).expect("hex key");
        assert_eq!(from_hex.0, [0x2a; KEY_LEN]);
    }

    #[test]
    fn master_key_rejects_bad_input_with_actionable_errors() {
        let short = MasterKey::parse(&URL_SAFE_NO_PAD.encode([0u8; 16]))
            .expect_err("16 bytes must be rejected");
        assert!(matches!(short, SecretError::KeyLength { got: 16 }));
        assert!(
            short.to_string().contains("openssl rand"),
            "the error tells the operator how to generate a key"
        );

        assert!(matches!(
            MasterKey::parse(""),
            Err(SecretError::KeyLength { got: 0 })
        ));
        assert!(matches!(
            MasterKey::parse("not base64!!"),
            Err(SecretError::KeyEncoding)
        ));
        // 64 hex chars decode to 32 bytes; 63 chars are not a hex key and
        // must not be accepted as base64url either.
        assert!(matches!(
            MasterKey::parse(&"2a".repeat(31)),
            Err(SecretError::KeyLength { .. }) | Err(SecretError::KeyEncoding)
        ));
    }

    #[test]
    fn debug_output_never_carries_key_material() {
        let key = MasterKey::from_bytes([0x2a; KEY_LEN]);
        assert_eq!(format!("{key:?}"), "MasterKey(****)");
        let boxx = SecretBox::new(key);
        assert_eq!(format!("{boxx:?}"), "SecretBox(****)");
    }

    #[test]
    fn missing_master_key_fails_startup_with_an_actionable_message() {
        let missing = require_master_key(None).expect_err("no key must fail");
        let message = missing.to_string();
        assert!(matches!(missing, SecretError::Missing));
        assert!(message.contains("GATEWAY_MASTER_KEY"), "{message}");
        assert!(message.contains("openssl rand"), "{message}");

        let key = key_a();
        let built = require_master_key(Some(&key)).expect("present key builds a box");
        assert_eq!(format!("{built:?}"), "SecretBox(****)");
    }
}
