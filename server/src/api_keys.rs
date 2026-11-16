//! API keys (M4): issuance, verification, revocation, and per-key rate limits.
//!
//! A key is `pk_live_<43 base64url chars>` and is shown **once**, at creation.
//! Only `sha256(key)` is stored (`api_keys.key_hash`); the plaintext is never
//! logged, never returned again, and never leaves the creation response (its
//! `Debug` is redacted too). The `prefix` column (the first 12 characters) is
//! the only part lists show.
//!
//! Authentication on `/v1/*` accepts `Authorization: Bearer <key>`: the hash
//! is looked up, revocation/expiry and the owner's `is_active` are enforced,
//! `last_used_at` is stamped, and the per-key rate limits are charged. A
//! revoked, expired, or unknown key is `401`; a key of a deactivated account
//! is `403`. The `last_used_at` stamp is one cheap write per request today;
//! **follow-up** (review nit): batch it into periodic flushes if it ever
//! shows up in profiles.
//!
//! Rate limits are per-key RPM and TPM token buckets (in-memory, bounded at
//! [`LIMITER_MAX_ENTRIES`] entries with least-recently-touched eviction, so a
//! key-id flood cannot leak memory). Default 60 RPM / 100k TPM; a key may
//! override both. A trip answers `429` with `Retry-After`.
//!
//! Mutation transactions are **write-first** (review m3): their first
//! statement takes SQLite's write lock, so they cannot fail on spurious
//! `SQLITE_BUSY_SNAPSHOT` upgrades when settlements run concurrently.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row, SqlitePool};
use utoipa::ToSchema;

use crate::auth::{self, Role};
use crate::catalog::{self, Actor};

/// Every key starts with this marker (spec's `pk_...`).
pub const KEY_PREFIX: &str = "pk_live_";
/// Characters of the key shown in lists (the stored `prefix`).
pub const PREFIX_CHARS: usize = 12;
/// Requests per minute when a key does not override the limit.
pub const DEFAULT_RPM: i64 = 60;
/// Tokens per minute when a key does not override the limit.
pub const DEFAULT_TPM: i64 = 100_000;
/// Largest rate-limiter map; the least recently touched key is evicted.
pub const LIMITER_MAX_ENTRIES: usize = 10_000;

const TOKEN_BYTES: usize = 32;
const NAME_MAX_CHARS: usize = 200;

fn ts(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format(auth::TS_FMT).to_string()
}

/// One key as lists return it. Never carries the key itself — only the
/// visible [`prefix`](Self::prefix).
#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct ApiKeyView {
    pub id: String,
    pub user_id: String,
    pub name: String,
    /// The visible head of the key (e.g. `pk_live_ab12`); enough to tell keys
    /// apart, useless for authentication.
    pub prefix: String,
    /// Stored scope names (informational in v1; not enforced).
    pub scopes: Option<String>,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
    pub last_used_at: Option<String>,
    /// NULL means the defaults ([`DEFAULT_RPM`]/[`DEFAULT_TPM`]).
    pub rate_limit_rpm: Option<i64>,
    pub rate_limit_tpm: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// A freshly created key: the view plus the plaintext, which exists only in
/// this response and is never recoverable afterwards. `Debug` is redacted:
/// the plaintext key must never be printable through a stray `{:?}` (review
/// nit).
#[derive(Clone)]
pub struct CreatedKey {
    pub view: ApiKeyView,
    /// The full key (`pk_live_...`); returned exactly once.
    pub key: String,
}

impl std::fmt::Debug for CreatedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedKey")
            .field("view", &self.view)
            .field("key", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ApiKeyInput {
    /// Human-readable label (required).
    pub name: String,
    /// Optional expiry as an RFC-3339 timestamp.
    pub expires_at: Option<String>,
    /// Optional scope names (informational in v1).
    pub scopes: Option<Vec<String>>,
    /// Requests per minute; omitted means [`DEFAULT_RPM`].
    pub rate_limit_rpm: Option<i64>,
    /// Tokens per minute; omitted means [`DEFAULT_TPM`].
    pub rate_limit_tpm: Option<i64>,
}

/// PATCH semantics (decision D20): `null`/omitted fields stay unchanged.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ApiKeyPatch {
    pub name: Option<String>,
    pub expires_at: Option<String>,
    pub rate_limit_rpm: Option<i64>,
    pub rate_limit_tpm: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit write failed: {0}")]
    Audit(#[from] catalog::CatalogError),
    #[error("{0}")]
    Invalid(String),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("the operating system random generator failed: {0}")]
    Rng(String),
}

impl KeyError {
    /// Maps the domain error onto the uniform API error shape.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        use crate::error::ApiError;
        match self {
            KeyError::Invalid(message) => ApiError::bad_request(message).with_request_id(rid),
            KeyError::NotFound(what) => ApiError::not_found(what).with_request_id(rid),
            other => ApiError::internal("api key", other).with_request_id(rid),
        }
    }
}

/// Generates one key: the plaintext and its SHA-256 hex.
fn generate() -> Result<(String, String), KeyError> {
    let mut buf = [0u8; TOKEN_BYTES];
    rand::rngs::OsRng
        .try_fill_bytes(&mut buf)
        .map_err(|e| KeyError::Rng(e.to_string()))?;
    let key = format!("{KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(buf));
    let hash = auth::hash_token(&key);
    Ok((key, hash))
}

fn validate_name(name: &str) -> Result<String, KeyError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(KeyError::Invalid("name must not be empty".to_string()));
    }
    if trimmed.chars().count() > NAME_MAX_CHARS {
        return Err(KeyError::Invalid(format!(
            "name must be at most {NAME_MAX_CHARS} characters"
        )));
    }
    Ok(trimmed.to_string())
}

/// Validates an optional expiry; returns it normalized to UTC.
fn validate_expiry(expires_at: Option<&str>) -> Result<Option<String>, KeyError> {
    match expires_at {
        None => Ok(None),
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(raw.trim()).map_err(|_| {
                KeyError::Invalid("expires_at must be an ISO-8601 timestamp".to_string())
            })?;
            Ok(Some(ts(parsed.to_utc())))
        }
    }
}

fn validate_limits(rpm: Option<i64>, tpm: Option<i64>) -> Result<(), KeyError> {
    for (value, what) in [(rpm, "rate_limit_rpm"), (tpm, "rate_limit_tpm")] {
        if value.is_some_and(|value| value <= 0) {
            return Err(KeyError::Invalid(format!(
                "{what} must be a positive integer"
            )));
        }
    }
    Ok(())
}

/// Issues a key for a user, audited as `api_key.create`. The plaintext key is
/// part of the return value only; the audit row records the name and limits
/// but never the key or its prefix.
///
/// Failure mode: [`KeyError::Invalid`] on bad input, [`KeyError::NotFound`]
/// when the user does not exist.
pub async fn create_key(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    user_id: &str,
    input: &ApiKeyInput,
) -> Result<CreatedKey, KeyError> {
    let name = validate_name(&input.name)?;
    let expires_at = validate_expiry(input.expires_at.as_deref())?;
    validate_limits(input.rate_limit_rpm, input.rate_limit_tpm)?;
    let scopes = match &input.scopes {
        None => None,
        Some(scopes) => Some(
            serde_json::to_string(scopes)
                .map_err(|e| KeyError::Invalid(format!("scopes are not serializable: {e}")))?,
        ),
    };

    let (key, key_hash) = generate()?;
    let now = ts(chrono::Utc::now());
    let id = uuid::Uuid::new_v4().to_string();
    let prefix: String = key.chars().take(PREFIX_CHARS).collect();

    let mut tx = pool.begin().await?;
    // Write-first (review m3): the INSERT takes the write lock up front; the
    // foreign key on `user_id` rejects unknown users and maps to `NotFound`.
    let inserted = sqlx::query(
        "INSERT INTO api_keys \
           (id, user_id, name, prefix, key_hash, scopes_json, expires_at, revoked_at, \
            last_used_at, rate_limit_rpm, rate_limit_tpm, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(&name)
    .bind(&prefix)
    .bind(&key_hash)
    .bind(&scopes)
    .bind(&expires_at)
    .bind(input.rate_limit_rpm)
    .bind(input.rate_limit_tpm)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(ref db)) = inserted
        && db.is_foreign_key_violation()
    {
        return Err(KeyError::NotFound("user"));
    }
    inserted?;
    // Defence in depth when foreign keys are ever disabled: never keep a key
    // row without its user.
    let user: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    if user.is_none() {
        return Err(KeyError::NotFound("user"));
    }
    let detail = serde_json::json!({
        "name": name,
        "key_id": id,
        "expires_at": expires_at,
        "rate_limit_rpm": input.rate_limit_rpm,
        "rate_limit_tpm": input.rate_limit_tpm,
    });
    catalog::audit(&mut tx, actor, "api_key.create", "api_key", &id, &detail).await?;
    tx.commit().await?;

    let view = get_view(pool, &id).await?;
    Ok(CreatedKey { view, key })
}

/// Lists a user's keys, newest last by creation. Never returns key material.
pub async fn list_keys(pool: &SqlitePool, user_id: &str) -> Result<Vec<ApiKeyView>, KeyError> {
    Ok(sqlx::query_as::<_, ApiKeyView>(
        "SELECT id, user_id, name, prefix, scopes_json AS scopes, expires_at, revoked_at, \
                last_used_at, rate_limit_rpm, rate_limit_tpm, created_at, updated_at \
         FROM api_keys WHERE user_id = ? ORDER BY created_at, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

/// Revokes a key (idempotent on an already-revoked key), audited as
/// `api_key.revoke`. Revocation takes effect on the next request. Write-first
/// (review m3): the revocation UPDATE is the transaction's first statement
/// and its `RETURNING id` doubles as the existence check.
pub async fn revoke_key(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    key_id: &str,
) -> Result<ApiKeyView, KeyError> {
    let mut tx = pool.begin().await?;
    let now = ts(chrono::Utc::now());
    let existing: Option<(String,)> = sqlx::query_as(
        "UPDATE api_keys SET revoked_at = COALESCE(revoked_at, ?), updated_at = ? \
         WHERE id = ? \
         RETURNING id",
    )
    .bind(&now)
    .bind(&now)
    .bind(key_id)
    .fetch_optional(&mut *tx)
    .await?;
    if existing.is_none() {
        return Err(KeyError::NotFound("api key"));
    }
    let detail = serde_json::json!({ "key_id": key_id });
    catalog::audit(&mut tx, actor, "api_key.revoke", "api_key", key_id, &detail).await?;
    tx.commit().await?;
    get_view(pool, key_id).await
}

/// Updates a key's name, expiry, or rate limits (decision D20: `null` keeps
/// the current value), audited as `api_key.update`.
pub async fn update_key(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    key_id: &str,
    patch: &ApiKeyPatch,
) -> Result<ApiKeyView, KeyError> {
    let name = patch.name.as_deref().map(validate_name).transpose()?;
    let expires_at = validate_expiry(patch.expires_at.as_deref())?;
    validate_limits(patch.rate_limit_rpm, patch.rate_limit_tpm)?;
    if name.is_none()
        && expires_at.is_none()
        && patch.rate_limit_rpm.is_none()
        && patch.rate_limit_tpm.is_none()
    {
        return Err(KeyError::Invalid(
            "at least one field must be provided".to_string(),
        ));
    }

    let mut tx = pool.begin().await?;
    let now = ts(chrono::Utc::now());
    // Write-first (review m3): the UPDATE is the transaction's first
    // statement and its `RETURNING id` doubles as the existence check.
    let existing: Option<(String,)> = sqlx::query_as(
        "UPDATE api_keys SET \
           name = COALESCE(?, name), \
           expires_at = COALESCE(?, expires_at), \
           rate_limit_rpm = COALESCE(?, rate_limit_rpm), \
           rate_limit_tpm = COALESCE(?, rate_limit_tpm), \
           updated_at = ? \
         WHERE id = ? \
         RETURNING id",
    )
    .bind(&name)
    .bind(&expires_at)
    .bind(patch.rate_limit_rpm)
    .bind(patch.rate_limit_tpm)
    .bind(&now)
    .bind(key_id)
    .fetch_optional(&mut *tx)
    .await?;
    if existing.is_none() {
        return Err(KeyError::NotFound("api key"));
    }
    let detail = serde_json::json!({
        "key_id": key_id,
        "name": name,
        "expires_at": expires_at,
        "rate_limit_rpm": patch.rate_limit_rpm,
        "rate_limit_tpm": patch.rate_limit_tpm,
    });
    catalog::audit(&mut tx, actor, "api_key.update", "api_key", key_id, &detail).await?;
    tx.commit().await?;
    get_view(pool, key_id).await
}

async fn get_view(pool: &SqlitePool, key_id: &str) -> Result<ApiKeyView, KeyError> {
    let row = sqlx::query_as::<_, ApiKeyView>(
        "SELECT id, user_id, name, prefix, scopes_json AS scopes, expires_at, revoked_at, \
                last_used_at, rate_limit_rpm, rate_limit_tpm, created_at, updated_at \
         FROM api_keys WHERE id = ?",
    )
    .bind(key_id)
    .fetch_optional(pool)
    .await?;
    row.ok_or(KeyError::NotFound("api key"))
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// The verified identity behind a bearer key, with its effective limits.
#[derive(Debug, Clone)]
pub struct KeyContext {
    pub key_id: String,
    pub user_id: String,
    pub role: Role,
    pub rate_limit_rpm: i64,
    pub rate_limit_tpm: i64,
}

/// Why a presented key was refused; maps to the uniform error shape.
#[derive(Debug, thiserror::Error)]
pub enum KeyRejection {
    #[error("the API key is not valid")]
    Unknown,
    #[error("the API key has been revoked")]
    Revoked,
    #[error("the API key has expired")]
    Expired,
    #[error("the owning account is disabled")]
    DisabledUser,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

impl KeyRejection {
    /// The uniform `401`/`403` error for this rejection.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        use crate::error::ApiError;
        match self {
            KeyRejection::Unknown => ApiError::custom(
                axum::http::StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                "the API key is not valid",
            )
            .with_request_id(rid),
            KeyRejection::Revoked => ApiError::custom(
                axum::http::StatusCode::UNAUTHORIZED,
                "api_key_revoked",
                "the API key has been revoked",
            )
            .with_request_id(rid),
            KeyRejection::Expired => ApiError::custom(
                axum::http::StatusCode::UNAUTHORIZED,
                "api_key_expired",
                "the API key has expired",
            )
            .with_request_id(rid),
            KeyRejection::DisabledUser => ApiError::account_disabled().with_request_id(rid),
            KeyRejection::Db(e) => ApiError::internal("resolving api key", e).with_request_id(rid),
        }
    }
}

/// Verifies a presented key string against the stored hash and stamps
/// `last_used_at` on success.
///
/// Failure mode: [`KeyRejection`] (unknown, revoked, expired, or a disabled
/// owner) or [`KeyRejection::Db`]. The presented key is never logged.
pub async fn authenticate(pool: &SqlitePool, presented: &str) -> Result<KeyContext, KeyRejection> {
    let row: Option<SqliteRow> = sqlx::query(
        "SELECT k.id, k.user_id, k.revoked_at, k.expires_at, k.rate_limit_rpm, k.rate_limit_tpm, \
                u.role, u.is_active \
         FROM api_keys k JOIN users u ON u.id = k.user_id \
         WHERE k.key_hash = ?",
    )
    .bind(auth::hash_token(presented))
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Err(KeyRejection::Unknown);
    };

    let key_id: String = row.try_get("id")?;
    let user_id: String = row.try_get("user_id")?;
    let revoked_at: Option<String> = row.try_get("revoked_at")?;
    let expires_at: Option<String> = row.try_get("expires_at")?;
    let rpm: Option<i64> = row.try_get("rate_limit_rpm")?;
    let tpm: Option<i64> = row.try_get("rate_limit_tpm")?;
    let role_raw: String = row.try_get("role")?;
    let is_active: i64 = row.try_get("is_active")?;

    if revoked_at.is_some() {
        return Err(KeyRejection::Revoked);
    }
    if is_active != 1 {
        return Err(KeyRejection::DisabledUser);
    }
    let role = Role::parse(&role_raw).ok_or(KeyRejection::Unknown)?;

    let expired = match expires_at {
        None => false,
        Some(raw) => chrono::DateTime::parse_from_rfc3339(&raw)
            .map(|until| until <= chrono::Utc::now())
            // An unparseable expiry cannot be honoured safely (M3's session
            // rule): treat it as expired.
            .unwrap_or(true),
    };
    if expired {
        return Err(KeyRejection::Expired);
    }

    let now = ts(chrono::Utc::now());
    sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?")
        .bind(&now)
        .bind(&key_id)
        .execute(pool)
        .await?;

    Ok(KeyContext {
        key_id,
        user_id,
        role,
        rate_limit_rpm: rpm.unwrap_or(DEFAULT_RPM),
        rate_limit_tpm: tpm.unwrap_or(DEFAULT_TPM),
    })
}

// ---------------------------------------------------------------------------
// Per-key rate limits
// ---------------------------------------------------------------------------

/// A refilling token bucket.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last_refill: Instant,
}

impl Bucket {
    fn new(capacity: f64, now: Instant) -> Self {
        Self {
            tokens: capacity,
            capacity,
            refill_per_sec: capacity / 60.0,
            last_refill: now,
        }
    }

    /// Takes `n` tokens, returning the wait until `n` become available when
    /// the bucket cannot serve them. Requests larger than the bucket's whole
    /// capacity are refused with the time to a full bucket — they can never
    /// fit in one minute of this key's TPM budget.
    fn take(&mut self, n: f64, now: Instant) -> Result<(), Duration> {
        let elapsed = now.saturating_duration_since(self.last_refill);
        self.tokens =
            (self.tokens + elapsed.as_secs_f64() * self.refill_per_sec).min(self.capacity);
        self.last_refill = now;

        if n > self.capacity {
            let missing = self.capacity;
            return Err(self.wait_for(missing));
        }
        if self.tokens >= n {
            self.tokens -= n;
            return Ok(());
        }
        Err(self.wait_for(n - self.tokens))
    }

    fn wait_for(&self, missing: f64) -> Duration {
        if self.refill_per_sec <= f64::EPSILON {
            return Duration::from_secs(60);
        }
        Duration::from_secs_f64((missing / self.refill_per_sec).ceil())
    }
}

/// In-memory per-key rate limiter: one RPM and one TPM [`Bucket`] per key id,
/// bounded at [`LIMITER_MAX_ENTRIES`] entries with least-recently-touched
/// eviction. Time is passed in explicitly so the rules are unit-testable.
///
/// The buckets are charged separately: [`Self::charge_rpm`] runs per request
/// at authentication time, [`Self::charge_tpm`] runs once the request's token
/// estimate is known. Each trip returns the wait until its own bucket can
/// serve the request, which becomes the `Retry-After` of the `429`.
#[derive(Debug, Default)]
pub struct KeyRateLimiter {
    entries: Mutex<HashMap<String, Buckets>>,
}

#[derive(Debug)]
struct Buckets {
    rpm: Bucket,
    tpm: Bucket,
    touched: Instant,
}

impl KeyRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Charges one request against the key's RPM bucket. Returns the wait
    /// when the limit is exhausted.
    pub fn charge_rpm(&self, key_id: &str, limit: i64, now: Instant) -> Result<(), Duration> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let buckets = entry_for(&mut entries, key_id, now);
        buckets.touched = now;
        resize(&mut buckets.rpm, limit.max(1) as f64);
        buckets.rpm.take(1.0, now)
    }

    /// Charges `tokens` (the request's estimate) against the key's TPM
    /// bucket. Returns the wait when the limit is exhausted. A request whose
    /// estimate exceeds the whole per-minute budget can never fit and waits
    /// out a full refill.
    pub fn charge_tpm(
        &self,
        key_id: &str,
        limit: i64,
        tokens: i64,
        now: Instant,
    ) -> Result<(), Duration> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let buckets = entry_for(&mut entries, key_id, now);
        buckets.touched = now;
        resize(&mut buckets.tpm, limit.max(1) as f64);
        buckets.tpm.take(tokens.max(0) as f64, now)
    }

    /// Number of tracked keys; bounded by [`LIMITER_MAX_ENTRIES`].
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// The entry for `key_id`, creating (and evicting) as needed. Buckets start
/// full; the callers resize them to the key's current limits.
fn entry_for<'a>(
    entries: &'a mut HashMap<String, Buckets>,
    key_id: &str,
    now: Instant,
) -> &'a mut Buckets {
    if !entries.contains_key(key_id) && entries.len() >= LIMITER_MAX_ENTRIES {
        evict_oldest(entries);
    }
    entries
        .entry(key_id.to_string())
        .or_insert_with(|| Buckets {
            rpm: Bucket::new(1.0, now),
            tpm: Bucket::new(1.0, now),
            touched: now,
        })
}

/// Resizes a bucket to a new limit, keeping the fill fraction so a limit
/// change neither grants nor confiscates tokens mid-window.
fn resize(bucket: &mut Bucket, capacity: f64) {
    if (bucket.capacity - capacity).abs() > f64::EPSILON {
        let fraction = if bucket.capacity > 0.0 {
            bucket.tokens / bucket.capacity
        } else {
            1.0
        };
        bucket.capacity = capacity;
        bucket.refill_per_sec = capacity / 60.0;
        bucket.tokens = (fraction * capacity).min(capacity);
    }
}

fn evict_oldest(entries: &mut HashMap<String, Buckets>) {
    if let Some(oldest) = entries
        .iter()
        .min_by_key(|(_, buckets)| buckets.touched)
        .map(|(key, _)| key.clone())
    {
        entries.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Column;

    fn actor<'a>(user_id: &'a str) -> Actor<'a> {
        Actor {
            user_id,
            ip: "127.0.0.1",
        }
    }

    async fn pool_with_user() -> (std::path::PathBuf, SqlitePool, String) {
        let dir = std::env::temp_dir().join(format!("gateway-keys-{}", uuid::Uuid::new_v4()));
        let pool = crate::db::open(&dir.join("gateway.db"))
            .await
            .expect("open");
        let user_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active) \
             VALUES (?, ?, 'Key Owner', 'x', 'user', 1)",
        )
        .bind(&user_id)
        .bind(format!("{user_id}@example.com"))
        .execute(&pool)
        .await
        .expect("insert user");
        (dir, pool, user_id)
    }

    fn input() -> ApiKeyInput {
        ApiKeyInput {
            name: "sdk".to_string(),
            expires_at: None,
            scopes: None,
            rate_limit_rpm: None,
            rate_limit_tpm: None,
        }
    }

    #[tokio::test]
    async fn created_key_is_returned_once_and_never_stored() {
        let (dir, pool, user_id) = pool_with_user().await;
        let created = create_key(&pool, &actor(&user_id), &user_id, &input())
            .await
            .expect("create key");

        // The plaintext looks like a key and its visible head is the prefix.
        assert!(created.key.starts_with(KEY_PREFIX));
        assert_eq!(
            created.view.prefix,
            created.key.chars().take(PREFIX_CHARS).collect::<String>()
        );

        // The database holds only the hash; scanning every text column of
        // every table for the plaintext finds nothing.
        let hash: String = sqlx::query_scalar("SELECT key_hash FROM api_keys")
            .fetch_one(&pool)
            .await
            .expect("hash column");
        assert_eq!(hash, auth::hash_token(&created.key));
        assert_ne!(hash, created.key);
        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
                .fetch_all(&pool)
                .await
                .expect("tables");
        for table in tables {
            let rows = sqlx::query(&format!("SELECT * FROM {table}"))
                .fetch_all(&pool)
                .await
                .unwrap_or_default();
            for row in rows {
                for column in row.columns() {
                    // Non-text columns fail to decode as String and are
                    // skipped; the key could only ever be stored as text.
                    let value: Option<String> = row.try_get(column.ordinal()).ok().flatten();
                    if let Some(value) = value {
                        assert!(
                            !value.contains(&created.key),
                            "plaintext leaked into {table}.{}",
                            column.name()
                        );
                    }
                }
            }
        }

        // The audit row exists and carries neither the key nor its prefix.
        let (action, detail): (String, Option<String>) =
            sqlx::query_as("SELECT action, detail_json FROM audit_log")
                .fetch_one(&pool)
                .await
                .expect("audit row");
        assert_eq!(action, "api_key.create");
        let detail = detail.expect("detail");
        assert!(!detail.contains(&created.key));
        assert!(!detail.contains(&created.view.prefix));

        // And the plaintext is not printable through `Debug` either (review
        // nit).
        let debugged = format!("{created:?}");
        assert!(
            !debugged.contains(&created.key),
            "Debug redacts: {debugged}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn revoked_and_expired_keys_are_rejected() {
        let (dir, pool, user_id) = pool_with_user().await;
        let created = create_key(&pool, &actor(&user_id), &user_id, &input())
            .await
            .expect("create key");
        let ctx = authenticate(&pool, &created.key).await.expect("valid key");
        assert_eq!(ctx.user_id, user_id);
        assert_eq!(ctx.rate_limit_rpm, DEFAULT_RPM);
        assert_eq!(ctx.rate_limit_tpm, DEFAULT_TPM);

        // Unknown keys are refused with the same code shape.
        let unknown = authenticate(&pool, "pk_live_nope")
            .await
            .expect_err("unknown");
        assert!(matches!(unknown, KeyRejection::Unknown));

        // Revocation takes effect immediately.
        revoke_key(&pool, &actor(&user_id), &created.view.id)
            .await
            .expect("revoke");
        let rejected = authenticate(&pool, &created.key)
            .await
            .expect_err("revoked");
        assert!(matches!(rejected, KeyRejection::Revoked));

        // Expiry is enforced the same way.
        let expiring = ApiKeyInput {
            expires_at: Some("2020-01-01T00:00:00Z".to_string()),
            ..input()
        };
        let stale = create_key(&pool, &actor(&user_id), &user_id, &expiring)
            .await
            .expect("create expired");
        let rejected = authenticate(&pool, &stale.key).await.expect_err("expired");
        assert!(matches!(rejected, KeyRejection::Expired));

        // A deactivated owner's keys stop working too.
        sqlx::query("UPDATE users SET is_active = 0 WHERE id = ?")
            .bind(&user_id)
            .execute(&pool)
            .await
            .expect("deactivate");
        let fresh = ApiKeyInput {
            expires_at: None,
            ..input()
        };
        let created = create_key(&pool, &actor(&user_id), &user_id, &fresh)
            .await
            .expect("create");
        let rejected = authenticate(&pool, &created.key)
            .await
            .expect_err("disabled");
        assert!(matches!(rejected, KeyRejection::DisabledUser));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn patch_updates_limits_but_null_keeps_values() {
        let (dir, pool, user_id) = pool_with_user().await;
        let created = create_key(
            &pool,
            &actor(&user_id),
            &user_id,
            &ApiKeyInput {
                rate_limit_rpm: Some(30),
                rate_limit_tpm: Some(500),
                ..input()
            },
        )
        .await
        .expect("create");
        assert_eq!(created.view.rate_limit_rpm, Some(30));

        let patched = update_key(
            &pool,
            &actor(&user_id),
            &created.view.id,
            &ApiKeyPatch {
                rate_limit_rpm: Some(10),
                ..ApiKeyPatch::default()
            },
        )
        .await
        .expect("patch");
        assert_eq!(patched.rate_limit_rpm, Some(10));
        assert_eq!(patched.rate_limit_tpm, Some(500), "null keeps the value");

        // An all-null patch is a 400-worthy no-op (D20).
        let err = update_key(
            &pool,
            &actor(&user_id),
            &created.view.id,
            &ApiKeyPatch::default(),
        )
        .await
        .expect_err("empty patch");
        assert!(matches!(err, KeyError::Invalid(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rate_limiter_trips_rpm_and_tpm_with_retry_after() {
        let limiter = KeyRateLimiter::new();
        let t0 = Instant::now();

        // RPM: two requests pass, the third waits.
        assert!(limiter.charge_rpm("k", 2, t0).is_ok());
        assert!(limiter.charge_rpm("k", 2, t0).is_ok());
        let wait = limiter.charge_rpm("k", 2, t0).expect_err("third trips");
        assert!(wait > Duration::ZERO);
        // A minute later the bucket has refilled.
        assert!(
            limiter
                .charge_rpm("k", 2, t0 + Duration::from_secs(61))
                .is_ok()
        );

        // TPM: the estimate consumes the bucket; a request larger than the
        // whole bucket can never fit.
        assert!(limiter.charge_tpm("t", 1000, 900, t0).is_ok());
        let wait = limiter
            .charge_tpm("t", 1000, 200, t0)
            .expect_err("bucket drained");
        assert!(wait > Duration::ZERO);
        let wait = limiter
            .charge_tpm("t", 50, 500, t0)
            .expect_err("over capacity");
        assert!(wait > Duration::ZERO);
    }

    #[test]
    fn rate_limiter_isolates_keys_and_stays_bounded() {
        let limiter = KeyRateLimiter::new();
        let t0 = Instant::now();
        assert!(limiter.charge_rpm("a", 1, t0).is_ok());
        assert!(limiter.charge_rpm("a", 1, t0).is_err());
        assert!(
            limiter.charge_rpm("b", 1, t0).is_ok(),
            "other keys unaffected"
        );

        for i in 0..LIMITER_MAX_ENTRIES {
            limiter
                .charge_rpm(
                    &format!("key-{i}"),
                    60,
                    t0 + Duration::from_millis(i as u64),
                )
                .expect("charge");
        }
        assert!(limiter.len() <= LIMITER_MAX_ENTRIES);
        limiter
            .charge_rpm("newest", 60, t0 + Duration::from_secs(60))
            .expect("charge");
        assert!(limiter.len() <= LIMITER_MAX_ENTRIES, "map stays bounded");
    }
}
