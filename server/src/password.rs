//! Password hashing: Argon2id, with PBKDF2 rows still accepted.
//!
//! Decision D15 chose PBKDF2-HMAC-SHA256 while `argon2` was not available. D38
//! (crates.io reachable again) moves new hashes to **Argon2id** with
//! `m = 64 MiB`, `t = 3`, `p = 4`, and keeps both formats accepted:
//!
//! * `$argon2id$v=19$m=65536,t=3,p=4$<b64 salt>$<b64 hash>` — the standard PHC
//!   string the `argon2` crate produces.
//! * `pbkdf2$sha256$<iterations>$<b64 salt>$<b64 hash>` — rows written before
//!   the migration, verified with their own stored iteration count.
//!
//! [`needs_rehash`] tells a successful login to re-hash a PBKDF2 (or older
//! Argon2 parameter set) row with the current parameters. Verification never
//! panics and never leaks the expected hash: malformed stored values are a
//! plain mismatch (`Ok(false)`), and matching uses the crate's constant-time
//! comparison.
//!
//! Hashing is deliberately expensive, so request paths call
//! [`hash_password_offload`] / [`verify_password_offload`]: they move the
//! derivation to `tokio::task::spawn_blocking`, keeping a login from stalling
//! the async runtime.

use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pbkdf2::pbkdf2_hmac;
#[cfg(test)]
use rand::TryRngCore;
use sha2::Sha256;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(test)]
use std::time::Duration;
use subtle::ConstantTimeEq;

/// Argon2id memory cost in KiB (64 MiB), written into every new hash.
pub const ARGON2_M_COST_KIB: u32 = 64 * 1024;
/// Argon2id time cost (passes), written into every new hash.
pub const ARGON2_T_COST: u32 = 3;
/// Argon2id parallelism, written into every new hash.
pub const ARGON2_P_COST: u32 = 4;
/// Length of the derived hash in bytes.
pub const ARGON2_OUTPUT_LEN: usize = 32;

/// PBKDF2 iteration count of the pre-Argon2 format; accepted on read only.
#[cfg(test)]
pub const PBKDF2_ITERATIONS: u32 = 600_000;

const PBKDF2_ALGORITHM: &str = "sha256";
const PBKDF2_SALT_LEN: usize = 16;
const PBKDF2_HASH_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("password hashing failed: {0}")]
    Hash(String),
}

/// Argon2id hashing parameters as configured in this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Cost {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Argon2Cost {
    /// The production parameters (m=64 MiB, t=3, p=4).
    pub const fn production() -> Self {
        Self {
            m_cost_kib: ARGON2_M_COST_KIB,
            t_cost: ARGON2_T_COST,
            p_cost: ARGON2_P_COST,
        }
    }

    fn params(self) -> Result<Params, PasswordError> {
        Params::new(
            self.m_cost_kib,
            self.t_cost,
            self.p_cost,
            Some(ARGON2_OUTPUT_LEN),
        )
        .map_err(|e| PasswordError::Hash(e.to_string()))
    }

    fn hasher(self) -> Result<Argon2<'static>, PasswordError> {
        Ok(Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            self.params()?,
        ))
    }
}

/// Test-only Argon2 cost override. Zero means "production".
static TEST_M_COST: AtomicU32 = AtomicU32::new(0);
static TEST_T_COST: AtomicU32 = AtomicU32::new(0);
static TEST_P_COST: AtomicU32 = AtomicU32::new(0);

/// Test-only derivation delay in milliseconds, applied only to
/// [`SLOW_HASH_SENTINEL`]. Zero (the default) disables it.
#[cfg(test)]
static TEST_DELAY_MS: AtomicU64 = AtomicU64::new(0);

/// A password that makes a test-only artificial derivation delay kick in. No
/// real account uses it, so the delay cannot slow other tests down.
#[cfg(test)]
pub(crate) const SLOW_HASH_SENTINEL: &str = "slow-hash-sentinel-for-tests";

/// The Argon2id parameters [`hash_password`] uses.
pub fn cost() -> Argon2Cost {
    let m = TEST_M_COST.load(Ordering::Relaxed);
    let t = TEST_T_COST.load(Ordering::Relaxed);
    let p = TEST_P_COST.load(Ordering::Relaxed);
    if m == 0 || t == 0 || p == 0 {
        return Argon2Cost::production();
    }
    Argon2Cost {
        m_cost_kib: m,
        t_cost: t,
        p_cost: p,
    }
}

/// Lowers the Argon2id cost for the whole test process. Test-only: the
/// production binary never compiles this.
#[cfg(test)]
pub fn set_test_cost(m_cost_kib: u32, t_cost: u32, p_cost: u32) {
    TEST_M_COST.store(m_cost_kib, Ordering::Relaxed);
    TEST_T_COST.store(t_cost, Ordering::Relaxed);
    TEST_P_COST.store(p_cost, Ordering::Relaxed);
}

/// Makes a hash of [`SLOW_HASH_SENTINEL`] sleep for `delay` (test-only), so a
/// test can prove the derivation runs off the async runtime.
#[cfg(test)]
pub fn set_test_delay(delay: Duration) {
    TEST_DELAY_MS.store(delay.as_millis() as u64, Ordering::Relaxed);
}

/// The PBKDF2 iteration count tests use for pre-migration rows.
#[cfg(test)]
pub const TEST_PBKDF2_ITERATIONS: u32 = 1_000;

/// Applies the test-only delay hook when `password` is the sentinel.
fn maybe_delay(password: &str) {
    #[cfg(test)]
    {
        let ms = TEST_DELAY_MS.load(Ordering::Relaxed);
        if ms > 0 && password == SLOW_HASH_SENTINEL {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
    #[cfg(not(test))]
    {
        let _ = password;
    }
}

/// Hashes `password` with Argon2id, a fresh random salt, and [`cost`].
pub fn hash_password(password: &str) -> Result<String, PasswordError> {
    hash_password_with_cost(password, cost())
}

/// Hashes with an explicit cost. The cost is encoded in the output, so
/// [`verify_password`] accepts every value this function can produce.
pub fn hash_password_with_cost(password: &str, cost: Argon2Cost) -> Result<String, PasswordError> {
    maybe_delay(password);
    let hasher = cost.hasher()?;
    let hash = hasher
        .hash_password(password.as_bytes())
        .map_err(|e| PasswordError::Hash(e.to_string()))?;
    Ok(hash.to_string())
}

/// Hashes a pre-migration PBKDF2 row (sentinel/legacy tests only).
#[cfg(test)]
pub fn hash_password_pbkdf2(password: &str, iterations: u32) -> Result<String, PasswordError> {
    let mut salt = [0u8; PBKDF2_SALT_LEN];
    rand::rngs::OsRng
        .try_fill_bytes(&mut salt)
        .map_err(|e| PasswordError::Hash(e.to_string()))?;

    let hash = pbkdf2_derive(password.as_bytes(), &salt, iterations);
    Ok(format!(
        "pbkdf2${PBKDF2_ALGORITHM}${iterations}${}${}",
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(hash),
    ))
}

/// Verifies `password` against a stored Argon2id or PBKDF2 hash in constant
/// time.
///
/// Returns `Ok(false)` — never an error and never a panic — for malformed,
/// foreign, or truncated stored values, so a corrupt row cannot distinguish
/// itself from a wrong password.
pub fn verify_password(password: &str, stored: &str) -> Result<bool, PasswordError> {
    maybe_delay(password);
    if stored.starts_with("$argon2") {
        return Ok(verify_argon2(password, stored));
    }
    if stored.starts_with("pbkdf2$") {
        return Ok(verify_pbkdf2(password, stored));
    }
    Ok(false)
}

fn verify_argon2(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

fn verify_pbkdf2(password: &str, stored: &str) -> bool {
    let Some(parsed) = parse_pbkdf2(stored) else {
        return false;
    };
    let derived = pbkdf2_derive(password.as_bytes(), &parsed.salt, parsed.iterations);
    derived.as_slice().ct_eq(parsed.hash.as_slice()).into()
}

/// Whether a stored hash was written with different parameters than [`cost`]:
/// PBKDF2 rows and older Argon2 parameters are upgraded on the next successful
/// login. Malformed values are not rehashed (they never verify).
pub fn needs_rehash(stored: &str) -> bool {
    needs_rehash_with(stored, cost())
}

/// [`needs_rehash`] against an explicit cost, for tests and future callers
/// that pick their own parameters.
pub fn needs_rehash_with(stored: &str, current: Argon2Cost) -> bool {
    if stored.starts_with("pbkdf2$") {
        return true;
    }
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    if parsed.algorithm.as_str() != "argon2id" || parsed.version != Some(u32::from(Version::V0x13))
    {
        return true;
    }
    let params = &parsed.params;
    let read = |name: &str| params.get_decimal(name);
    read("m") != Some(current.m_cost_kib)
        || read("t") != Some(current.t_cost)
        || read("p") != Some(current.p_cost)
}

/// [`hash_password`] on the blocking thread pool. Use this on request paths.
///
/// Failure mode: [`PasswordError`] when the derivation itself fails (a bug or
/// exhausted memory), never because the task was cancelled.
pub async fn hash_password_offload(password: String) -> Result<String, PasswordError> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| PasswordError::Hash(format!("hashing task failed: {e}")))?
}

/// [`verify_password`] on the blocking thread pool. Use this on request paths.
///
/// Failure mode: [`PasswordError`] only when the blocking task panics; a
/// mismatched password is `Ok(false)`.
pub async fn verify_password_offload(
    password: String,
    stored: String,
) -> Result<bool, PasswordError> {
    tokio::task::spawn_blocking(move || verify_password(&password, &stored))
        .await
        .map_err(|e| PasswordError::Hash(format!("verification task failed: {e}")))?
}

/// A decoded `pbkdf2$sha256$...` value.
struct Pbkdf2Hash {
    salt: Vec<u8>,
    iterations: u32,
    hash: Vec<u8>,
}

fn pbkdf2_derive(password: &[u8], salt: &[u8], iterations: u32) -> [u8; PBKDF2_HASH_LEN] {
    let mut out = [0u8; PBKDF2_HASH_LEN];
    pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut out);
    out
}

/// Parses the PBKDF2 stored format; anything unexpected is `None`, which
/// callers turn into "password does not match".
fn parse_pbkdf2(stored: &str) -> Option<Pbkdf2Hash> {
    let mut parts = stored.split('$');
    let (scheme, algorithm, iterations, salt, hash) = match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some(scheme), Some(algorithm), Some(iterations), Some(salt), Some(hash), None) => {
            (scheme, algorithm, iterations, salt, hash)
        }
        _ => return None,
    };

    if scheme != "pbkdf2" || algorithm != PBKDF2_ALGORITHM {
        return None;
    }

    let iterations = iterations.parse::<u32>().ok().filter(|n| *n > 0)?;
    let salt = URL_SAFE_NO_PAD.decode(salt).ok()?;
    let hash = URL_SAFE_NO_PAD.decode(hash).ok()?;
    if salt.len() != PBKDF2_SALT_LEN || hash.len() != PBKDF2_HASH_LEN {
        return None;
    }

    Some(Pbkdf2Hash {
        salt,
        iterations,
        hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test in this module hashes with cheap parameters.
    fn fast() -> Argon2Cost {
        Argon2Cost {
            m_cost_kib: 8 * 1024,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn production_cost_matches_the_documented_parameters() {
        assert_eq!(Argon2Cost::production().m_cost_kib, 64 * 1024);
        assert_eq!(Argon2Cost::production().t_cost, 3);
        assert_eq!(Argon2Cost::production().p_cost, 4);
        assert_eq!(PBKDF2_ITERATIONS, 600_000, "old rows keep their count");
    }

    #[test]
    fn argon2_round_trip_succeeds() {
        let stored = hash_password_with_cost("correct horse battery", fast()).expect("hash");
        assert!(stored.starts_with("$argon2id$"), "stored: {stored}");
        assert!(verify_password("correct horse battery", &stored).expect("verify"));
    }

    #[test]
    fn argon2_rejects_wrong_passwords() {
        let stored = hash_password_with_cost("correct horse battery", fast()).expect("hash");
        assert!(!verify_password("Correct horse battery", &stored).expect("verify"));
        assert!(!verify_password("", &stored).expect("verify"));
        assert!(!verify_password("correct horse battery ", &stored).expect("verify"));
    }

    #[test]
    fn same_password_gives_different_salts() {
        let a = hash_password_with_cost("same password", fast()).expect("hash");
        let b = hash_password_with_cost("same password", fast()).expect("hash");
        assert_ne!(a, b, "each hash must carry its own salt");
        assert!(verify_password("same password", &a).expect("verify"));
        assert!(verify_password("same password", &b).expect("verify"));
    }

    #[test]
    fn malformed_stored_values_fail_safely() {
        for stored in [
            "",
            "x",
            "$argon2id$",
            "$argon2id$v=19$m=8192,t=1,p=1$short$short",
            "$argon2id$v=19$m=8192,t=1,p=1$!!!$!!!",
            "$argon2i$v=19$m=8192,t=1,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "pbkdf2$sha256$1000$short$short",
            "pbkdf2$sha256$0$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "pbkdf2$sha256$-1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "pbkdf2$sha512$1000$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "pbkdf2$sha256$1000$AAAAAAAAAAAAAAAAAAAAAA",
            "pbkdf2$sha256$1000$AAAAAAAAAAAAAAAAAAAAAA$AA$extra",
        ] {
            assert!(
                !verify_password("anything", stored).expect("verify must not error"),
                "must reject {stored:?}"
            );
        }
    }

    #[test]
    fn pbkdf2_rows_still_verify_and_are_flagged_for_upgrade() {
        let stored =
            hash_password_pbkdf2("legacy-password", TEST_PBKDF2_ITERATIONS).expect("legacy hash");
        assert!(stored.starts_with("pbkdf2$sha256$1000$"), "{stored}");
        assert!(verify_password("legacy-password", &stored).expect("verify"));
        assert!(!verify_password("wrong", &stored).expect("verify"));
        assert!(needs_rehash(&stored), "PBKDF2 rows upgrade on next login");

        // The upgrade path: verify the old row, then store a fresh Argon2 hash
        // that verifies the same password and needs no further rehash.
        let upgraded = hash_password_with_cost("legacy-password", fast()).expect("upgrade");
        assert!(verify_password("legacy-password", &upgraded).expect("verify"));
        assert!(!needs_rehash_with(&upgraded, fast()));
    }

    #[test]
    fn argon2_parameter_changes_are_flagged_for_upgrade() {
        let stored = hash_password_with_cost("pw", fast()).expect("hash");
        assert!(!needs_rehash_with(&stored, fast()));
        assert!(needs_rehash_with(&stored, Argon2Cost::production()));
        assert!(needs_rehash_with(
            &stored,
            Argon2Cost {
                m_cost_kib: 16 * 1024,
                ..fast()
            }
        ));
    }

    #[test]
    fn unreadable_rows_are_not_flagged() {
        assert!(!needs_rehash("garbage"));
        assert!(!needs_rehash(""));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offloaded_hashing_round_trips() {
        let stored = hash_password_offload("offloaded password".to_string())
            .await
            .expect("hash");
        assert!(
            verify_password_offload("offloaded password".to_string(), stored)
                .await
                .expect("verify")
        );
    }
}
