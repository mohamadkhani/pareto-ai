//! Password hashing: PBKDF2-HMAC-SHA256 with a per-password random salt.
//!
//! Decision D15: `argon2`, `bcrypt`, and `scrypt` are not in the offline cargo
//! cache, so passwords use PBKDF2-HMAC-SHA256 with 600k iterations (the OWASP
//! recommendation for this algorithm). The stored value carries the algorithm
//! id — `pbkdf2$sha256$<iterations>$<b64 salt>$<b64 hash>` — so a later
//! migration to Argon2id can detect old hashes and re-hash on next login.
//!
//! Verification never panics and never leaks the expected hash: malformed
//! stored values are reported as a plain mismatch (`Ok(false)`), and matching
//! uses a constant-time comparison.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pbkdf2::pbkdf2_hmac;
use rand::TryRngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// PBKDF2 iteration count, fixed at hash time and stored in the output.
pub const PBKDF2_ITERATIONS: u32 = 600_000;

const ALGORITHM: &str = "sha256";
const SALT_LEN: usize = 16;
const HASH_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("the operating system random generator failed: {0}")]
    Rng(String),
}

/// Hashes `password` with a fresh 16-byte random salt and [`PBKDF2_ITERATIONS`].
pub fn hash_password(password: &str) -> Result<String, PasswordError> {
    hash_password_with_iterations(password, current_iterations())
}

/// The iteration count [`hash_password`] uses: always [`PBKDF2_ITERATIONS`]
/// in production. Test binaries may lower it per thread (tests hash through
/// the same code paths but must not pay 600k derivations per call).
fn current_iterations() -> u32 {
    #[cfg(test)]
    {
        TEST_ITERATIONS.with(|value| value.get())
    }
    #[cfg(not(test))]
    {
        PBKDF2_ITERATIONS
    }
}

#[cfg(test)]
thread_local! {
    static TEST_ITERATIONS: std::cell::Cell<u32> =
        const { std::cell::Cell::new(PBKDF2_ITERATIONS) };
}

/// Lowers [`hash_password`]'s iteration count on the calling test thread.
/// Test-only: the production binary never compiles this.
#[cfg(test)]
pub fn set_test_iterations(iterations: u32) {
    TEST_ITERATIONS.with(|value| value.set(iterations));
}

/// Hashes with an explicit iteration count. The count is encoded in the output,
/// so [`verify_password`] accepts every value this function can produce.
pub fn hash_password_with_iterations(
    password: &str,
    iterations: u32,
) -> Result<String, PasswordError> {
    let mut salt = [0u8; SALT_LEN];
    rand::rngs::OsRng
        .try_fill_bytes(&mut salt)
        .map_err(|e| PasswordError::Rng(e.to_string()))?;

    let hash = pbkdf2_derive(password.as_bytes(), &salt, iterations);
    Ok(format!(
        "pbkdf2${ALGORITHM}${iterations}${}${}",
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(hash),
    ))
}

/// Verifies `password` against a stored hash in constant time.
///
/// Returns `Ok(false)` — never an error and never a panic — for malformed,
/// foreign, or truncated stored values, so a corrupt row cannot distinguish
/// itself from a wrong password.
pub fn verify_password(password: &str, stored: &str) -> Result<bool, PasswordError> {
    let Some(stored_hash) = parse_stored(stored) else {
        return Ok(false);
    };

    let derived = pbkdf2_derive(
        password.as_bytes(),
        &stored_hash.salt,
        stored_hash.iterations,
    );
    Ok(derived.as_slice().ct_eq(stored_hash.hash.as_slice()).into())
}

/// A decoded `pbkdf2$sha256$...` value.
struct StoredHash {
    salt: Vec<u8>,
    iterations: u32,
    hash: Vec<u8>,
}

fn pbkdf2_derive(password: &[u8], salt: &[u8], iterations: u32) -> [u8; HASH_LEN] {
    let mut out = [0u8; HASH_LEN];
    pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut out);
    out
}

/// Parses the stored format; anything unexpected is `None`, which callers turn
/// into "password does not match".
fn parse_stored(stored: &str) -> Option<StoredHash> {
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

    if scheme != "pbkdf2" || algorithm != ALGORITHM {
        return None;
    }

    let iterations = iterations.parse::<u32>().ok().filter(|n| *n > 0)?;
    let salt = URL_SAFE_NO_PAD.decode(salt).ok()?;
    let hash = URL_SAFE_NO_PAD.decode(hash).ok()?;
    if salt.len() != SALT_LEN || hash.len() != HASH_LEN {
        return None;
    }

    Some(StoredHash {
        salt,
        iterations,
        hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_succeeds() {
        let stored = hash_password_with_iterations("correct horse battery", 1_000)
            .expect("hashing must succeed");
        assert!(verify_password("correct horse battery", &stored).expect("verify must succeed"));
    }

    #[test]
    fn wrong_password_fails() {
        let stored = hash_password_with_iterations("correct horse battery", 1_000)
            .expect("hashing must succeed");
        assert!(!verify_password("Correct horse battery", &stored).expect("verify must succeed"));
        assert!(!verify_password("", &stored).expect("verify must succeed"));
        assert!(!verify_password("correct horse battery ", &stored).expect("verify must succeed"));
    }

    #[test]
    fn malformed_stored_values_fail_safely() {
        for stored in [
            "",
            "x",
            "pbkdf2$sha256$1000$short$short",
            "pbkdf2$sha256$0$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "pbkdf2$sha256$-1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "pbkdf2$sha256$1000$!!!not-base64!!!$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "pbkdf2$sha512$1000$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "argon2id$sha256$1000$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
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
    fn same_password_gives_different_salts() {
        let a =
            hash_password_with_iterations("same password", 1_000).expect("hashing must succeed");
        let b =
            hash_password_with_iterations("same password", 1_000).expect("hashing must succeed");
        assert_ne!(a, b, "each hash must carry its own salt");
        assert!(verify_password("same password", &a).expect("verify must succeed"));
        assert!(verify_password("same password", &b).expect("verify must succeed"));
    }

    #[test]
    fn production_hash_encodes_algorithm_and_iterations() {
        let stored = hash_password("format check").expect("hashing must succeed");
        let parts: Vec<&str> = stored.split('$').collect();
        assert_eq!(parts.len(), 5, "stored: {stored}");
        assert_eq!(parts[0], "pbkdf2");
        assert_eq!(parts[1], "sha256");
        assert_eq!(parts[2], "600000");
        assert!(verify_password("format check", &stored).expect("verify must succeed"));
    }
}
