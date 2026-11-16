//! Session lifecycle, CSRF protection, login rate limiting, and the audit log.
//!
//! A session is created on login: a 32-byte random token is sent to the client
//! as the `gateway_session` cookie (`HttpOnly; SameSite=Strict; Path=/`, plus
//! `Secure` unless `GATEWAY_INSECURE_COOKIES=1`, `Max-Age` seven days) and only
//! `sha256(token)` is stored. Every request that carries the cookie is checked
//! against the stored expiry; expired sessions are deleted on sight.
//!
//! State-changing `/api/*` requests must also carry the session's CSRF token in
//! the `X-CSRF-Token` header, compared in constant time. This is enforced by
//! `routes::csrf_guard` for every unsafe method except login.
//!
//! Login attempts are rate limited per `(ip, email)`: five failures within
//! fifteen minutes lock the pair out, and every further attempt doubles the
//! lockout up to fifteen minutes. A successful login clears the counter.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::{HeaderMap, request::Parts};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::AppState;
use crate::error::{ApiError, request_id};
use crate::ip::{IpCidr, unmap_ipv4};

/// Cookie that carries the opaque session token.
pub const SESSION_COOKIE: &str = "gateway_session";
/// Header that carries the session's CSRF token on state-changing requests.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Session lifetime in days; the cookie `Max-Age` matches it.
pub const SESSION_TTL_DAYS: i64 = 7;

const TOKEN_BYTES: usize = 32;
const USER_AGENT_MAX_CHARS: usize = 512;
/// Timestamp format shared with SQLite's `strftime('%Y-%m-%dT%H:%M:%fZ','now')`.
pub const TS_FMT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

const LOGIN_FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
const LOGIN_FAILURE_THRESHOLD: usize = 5;
const LOGIN_LOCK_BASE: Duration = Duration::from_secs(60);
const LOGIN_LOCK_MAX: Duration = Duration::from_secs(15 * 60);
const LOGIN_LIMITER_MAX_ENTRIES: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("the operating system random generator failed: {0}")]
    Rng(String),
    #[error("unknown user role {0:?} stored in the database")]
    UnknownRole(String),
}

/// Account role. `Admin` may use the admin API surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    User,
}

impl Role {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "user" => Some(Self::User),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
        }
    }

    pub fn is_admin(self) -> bool {
        self == Self::Admin
    }
}

/// The authenticated caller, resolved from the session cookie.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub email: String,
    pub display_name: String,
    pub role: Role,
    pub session_id: String,
    pub csrf_token: String,
    /// Forced password rotation is pending (review finding 7): the caller may
    /// only reach `/api/auth/change-password` (and logout and `me`) until it
    /// happens.
    pub must_change_password: bool,
}

/// A session as resolved from the database, joined with its user.
#[derive(Debug, Clone)]
pub struct SessionUser {
    pub session_id: String,
    pub user_id: String,
    pub email: String,
    pub display_name: String,
    pub role: Role,
    pub csrf_token: String,
    pub must_change_password: bool,
}

/// A user row as needed for credential checks.
#[derive(Debug, Clone)]
pub struct UserRecord {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,
    pub role: Role,
    pub is_active: bool,
    pub must_change_password: bool,
}

/// A freshly created session. `token` is the raw cookie value and is never
/// stored anywhere; it is returned exactly once, to the login response.
#[derive(Debug, Clone)]
pub struct CreatedSession {
    pub token: String,
    pub csrf_token: String,
}

/// SHA-256 of a session token as lowercase hex; only this is stored.
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex_encode(&digest)
}

/// Constant-time comparison of a stored CSRF token against a provided one.
/// Values of different lengths compare unequal without leaking where.
pub fn csrf_matches(stored: &str, provided: &str) -> bool {
    stored.as_bytes().ct_eq(provided.as_bytes()).into()
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// A fresh 32-byte random token, base64url without padding.
fn random_token() -> Result<String, AuthError> {
    let mut buf = [0u8; TOKEN_BYTES];
    rand::rngs::OsRng
        .try_fill_bytes(&mut buf)
        .map_err(|e| AuthError::Rng(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// UTC timestamp in the same shape SQLite writes.
fn now_ts() -> String {
    chrono::Utc::now().format(TS_FMT).to_string()
}

/// Extracts the `gateway_session` value from a request's `Cookie` header.
pub fn session_cookie_value(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookie::Cookie::split_parse(header)
        .filter_map(Result::ok)
        .find(|c| c.name() == SESSION_COOKIE)
        .map(|c| c.value().to_string())
}

/// Serializes the session cookie with its security attributes.
pub fn session_cookie(token: &str, secure: bool) -> String {
    cookie::Cookie::build((SESSION_COOKIE, token))
        .http_only(true)
        .same_site(cookie::SameSite::Strict)
        .path("/")
        .secure(secure)
        .max_age(cookie::time::Duration::days(SESSION_TTL_DAYS))
        .to_string()
}

/// A cookie that deletes the session cookie on logout.
pub fn cleared_session_cookie(secure: bool) -> String {
    cookie::Cookie::build((SESSION_COOKIE, ""))
        .http_only(true)
        .same_site(cookie::SameSite::Strict)
        .path("/")
        .secure(secure)
        .max_age(cookie::time::Duration::ZERO)
        .to_string()
}

/// Resolves a session token into its session and user.
///
/// Expired sessions and sessions of deactivated users are deleted and resolve
/// to `None`. `last_seen_at` is refreshed on every successful lookup.
pub async fn load_session(
    pool: &sqlx::SqlitePool,
    token: &str,
) -> Result<Option<SessionUser>, AuthError> {
    let row = sqlx::query(
        "SELECT s.id AS session_id, s.user_id, s.csrf_token, s.expires_at, \
                u.email, u.display_name, u.role, u.is_active, u.must_change_password \
         FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = ?",
    )
    .bind(hash_token(token))
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    use sqlx::Row;
    let session_id: String = row.try_get("session_id")?;
    let user_id: String = row.try_get("user_id")?;
    let csrf_token: String = row.try_get("csrf_token")?;
    let expires_at: String = row.try_get("expires_at")?;
    let email: String = row.try_get("email")?;
    let display_name: String = row.try_get("display_name")?;
    let role_raw: String = row.try_get("role")?;
    let is_active: i64 = row.try_get("is_active")?;
    let must_change_password: i64 = row.try_get("must_change_password")?;

    let role = Role::parse(&role_raw).ok_or_else(|| AuthError::UnknownRole(role_raw))?;

    let expired = chrono::DateTime::parse_from_rfc3339(&expires_at)
        .map(|until| until <= chrono::Utc::now())
        // An unparseable expiry cannot be honoured safely: treat it as expired.
        .unwrap_or(true);
    if expired || is_active != 1 {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(&session_id)
            .execute(pool)
            .await?;
        return Ok(None);
    }

    sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE id = ?")
        .bind(now_ts())
        .bind(&session_id)
        .execute(pool)
        .await?;

    Ok(Some(SessionUser {
        session_id,
        user_id,
        email,
        display_name,
        role,
        csrf_token,
        must_change_password: must_change_password == 1,
    }))
}

/// Whether the session's account is flagged for forced password rotation
/// (review finding 7). Cheap single lookup used by the `/api` rotation guard.
/// Failure mode: [`AuthError::Db`].
pub async fn session_must_change_password(
    pool: &sqlx::SqlitePool,
    token: &str,
) -> Result<bool, AuthError> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT u.must_change_password FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = ?",
    )
    .bind(hash_token(token))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(flag,)| flag == 1).unwrap_or(false))
}

/// Looks up a user by email (case-insensitive through the column collation).
pub async fn find_user_by_email(
    pool: &sqlx::SqlitePool,
    email: &str,
) -> Result<Option<UserRecord>, AuthError> {
    let row = sqlx::query(
        "SELECT id, email, display_name, password_hash, role, is_active, must_change_password \
         FROM users WHERE email = ?",
    )
    .bind(email)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    use sqlx::Row;
    let role_raw: String = row.try_get("role")?;
    let role = Role::parse(&role_raw).ok_or_else(|| AuthError::UnknownRole(role_raw))?;

    Ok(Some(UserRecord {
        id: row.try_get("id")?,
        email: row.try_get("email")?,
        display_name: row.try_get("display_name")?,
        password_hash: row.try_get("password_hash")?,
        role,
        is_active: row.try_get::<i64, _>("is_active")? == 1,
        must_change_password: row.try_get::<i64, _>("must_change_password")? == 1,
    }))
}

/// Finishes a successful login: deletes the account's other sessions
/// (session rotation), stamps `last_login_at`, creates the session, and writes
/// the audit row, atomically.
pub async fn complete_login(
    pool: &sqlx::SqlitePool,
    user_id: &str,
    email: &str,
    ip: &str,
    user_agent: Option<&str>,
) -> Result<CreatedSession, AuthError> {
    let token = random_token()?;
    let csrf_token = random_token()?;
    let session_id = uuid::Uuid::new_v4().to_string();
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(SESSION_TTL_DAYS)).format(TS_FMT);
    let user_agent = user_agent.map(truncate_user_agent);

    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
        .bind(now_ts())
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    // Session rotation: only the session this login just created survives, so
    // a stolen or stale cookie stops working as soon as the account signs in.
    sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO sessions (id, token_hash, user_id, csrf_token, expires_at, last_seen_at, ip, user_agent) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&session_id)
    .bind(hash_token(&token))
    .bind(user_id)
    .bind(&csrf_token)
    .bind(expires_at.to_string())
    .bind(now_ts())
    .bind(ip)
    .bind(&user_agent)
    .execute(&mut *tx)
    .await?;
    audit(
        &mut *tx,
        Some(user_id),
        "auth.login_success",
        Some("user"),
        Some(user_id),
        Some(json!({ "email": email }).to_string().as_str()),
        Some(ip),
    )
    .await?;
    tx.commit().await?;

    Ok(CreatedSession { token, csrf_token })
}

/// Logs out: deletes the session row and writes the audit row, atomically.
pub async fn end_session(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    actor_user_id: &str,
    ip: &str,
) -> Result<(), AuthError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut *tx,
        Some(actor_user_id),
        "auth.logout",
        Some("user"),
        Some(actor_user_id),
        None,
        Some(ip),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Writes one `audit_log` row.
#[allow(clippy::too_many_arguments)] // the audit columns are fixed by the schema
pub async fn audit<'e, E>(
    db: E,
    actor_user_id: Option<&str>,
    action: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    detail_json: Option<&str>,
    ip: Option<&str>,
) -> Result<(), AuthError>
where
    E: sqlx::SqliteExecutor<'e>,
{
    sqlx::query(
        "INSERT INTO audit_log (actor_user_id, action, target_type, target_id, detail_json, ip) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(actor_user_id)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(detail_json)
    .bind(ip)
    .execute(db)
    .await?;
    Ok(())
}

fn truncate_user_agent(ua: &str) -> String {
    ua.chars().take(USER_AGENT_MAX_CHARS).collect()
}

// ---------------------------------------------------------------------------
// Extractors
// ---------------------------------------------------------------------------

/// The caller's IP as attributed to the request.
///
/// The TCP peer address is the client unless the peer is listed in
/// `GATEWAY_TRUSTED_PROXY`: only a request arriving from a trusted proxy may
/// carry the client address in forwarding headers. Those headers are never
/// read otherwise. The client is the rightmost `X-Forwarded-For` hop that is
/// not itself a trusted proxy (`X-Real-IP` only when no `X-Forwarded-For`
/// value exists at all), and the peer itself is the client when every hop is
/// trusted or no hop parses. `"unknown"` is reported only when there is no
/// peer to attribute anything to. Never fails.
pub struct ClientIp(pub String);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(resolve_client_ip(
            parts,
            &state.config.trusted_proxies,
        )))
    }
}

/// The client address per [`ClientIp`]'s rules.
fn resolve_client_ip(parts: &Parts, trusted_proxies: &[IpCidr]) -> String {
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| unmap_ipv4(addr.ip()));

    match peer {
        // Forwarding headers are only consulted when the peer is a proxy the
        // operator explicitly trusts to set them.
        Some(peer) if trusted_proxies.iter().any(|cidr| cidr.contains(peer)) => {
            // A chain whose every hop is trusted (or that parses to nothing)
            // attributes the request to no other client, so the peer itself
            // is the client.
            forwarded_client_ip(parts, trusted_proxies).unwrap_or_else(|| peer.to_string())
        }
        Some(peer) => peer.to_string(),
        // Without peer information the headers cannot be attributed to any
        // proxy, so they are not trusted either.
        None => "unknown".to_string(),
    }
}

/// The client address as attested by a trusted proxy: the rightmost
/// `X-Forwarded-For` hop that is not itself a trusted proxy, else `X-Real-IP`
/// when no `X-Forwarded-For` value exists at all. Non-IP hops are ignored;
/// `None` when no usable value remains (the caller falls back to the peer).
fn forwarded_client_ip(parts: &Parts, trusted_proxies: &[IpCidr]) -> Option<String> {
    let xff = parts.headers.get_all("x-forwarded-for");
    if xff.iter().next().is_some() {
        // All header lines form one logical comma list, so hops appended by
        // later proxies sit to the right of client-supplied ones. The walk
        // then runs right-to-left, skipping every hop inside
        // `trusted_proxies`, and returns the first hop left of them: the
        // rightmost untrusted address (nginx `real_ip_recursive` semantics).
        // A duplicate header line therefore cannot smuggle a spoofed hop
        // into the decision: whatever the trusted proxies appended stays
        // right of it.
        return xff
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(", ")
            .rsplit(',')
            .filter_map(parse_client_ip)
            .find(|ip| !trusted_proxies.iter().any(|cidr| cidr.contains(*ip)))
            .map(|ip| ip.to_string());
    }

    parts
        .headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_client_ip)
        .map(|ip| ip.to_string())
}

/// A normalized IP address, or `None` when `value` is not one.
fn parse_client_ip(value: &str) -> Option<std::net::IpAddr> {
    let ip = value.trim().parse::<std::net::IpAddr>().ok()?;
    Some(unmap_ipv4(ip))
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let rid = request_id(&parts.headers);
        let token = session_cookie_value(&parts.headers)
            .ok_or_else(|| ApiError::unauthorized().with_request_id(rid.clone()))?;

        let session = load_session(&state.pool, &token)
            .await
            .map_err(|e| ApiError::internal("resolving session", e).with_request_id(rid.clone()))?
            .ok_or_else(|| ApiError::unauthorized().with_request_id(rid.clone()))?;

        Ok(Self {
            user_id: session.user_id,
            email: session.email,
            display_name: session.display_name,
            role: session.role,
            session_id: session.session_id,
            csrf_token: session.csrf_token,
            must_change_password: session.must_change_password,
        })
    }
}

/// Wraps [`AuthUser`] for admin-only routes: 401 when not authenticated, 403
/// when the account's role is not `admin`.
pub struct RequireAdmin(pub AuthUser);

impl FromRequestParts<AppState> for RequireAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if !user.role.is_admin() {
            return Err(ApiError::forbidden().with_request_id(request_id(&parts.headers)));
        }
        Ok(Self(user))
    }
}

// ---------------------------------------------------------------------------
// Data-plane authentication (`/v1/*`)
// ---------------------------------------------------------------------------

/// The authenticated caller of a `/v1/*` request (M4).
///
/// Two credential kinds are accepted:
///
/// * `Authorization: Bearer pk_live_...` — an API key: any active account,
///   subject to the key's revocation/expiry and its RPM bucket (the TPM
///   bucket is charged once the request's token estimate is known). Forced
///   password rotation does not block keys: the key is an independent
///   credential and only the *password* is suspect.
/// * the session cookie — an **admin** dashboard session (kept working from
///   M3); the account's forced rotation blocks it like every other session
///   route.
#[derive(Debug, Clone)]
pub struct DataPlaneCaller {
    pub user_id: String,
    pub role: Role,
    /// The key behind the request, `None` for session callers.
    pub api_key: Option<crate::api_keys::KeyContext>,
}

pub struct DataPlaneAuth(pub DataPlaneCaller);

impl FromRequestParts<AppState> for DataPlaneAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let rid = request_id(&parts.headers);

        if let Some(presented) = bearer_token(&parts.headers) {
            let key = crate::api_keys::authenticate(&state.pool, &presented)
                .await
                .map_err(|e| e.into_api_error(rid.clone()))?;
            // RPM is charged per request at authentication; TPM waits for
            // the token estimate inside the chat pipeline.
            if let Err(wait) = state.key_limiter.charge_rpm(
                &key.key_id,
                key.rate_limit_rpm,
                std::time::Instant::now(),
            ) {
                return Err(ApiError::custom(
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    "the API key's request rate limit is exhausted",
                )
                .with_retry_after(wait.as_secs())
                .with_request_id(rid));
            }
            return Ok(Self(DataPlaneCaller {
                user_id: key.user_id.clone(),
                role: key.role,
                api_key: Some(key),
            }));
        }

        let token = session_cookie_value(&parts.headers)
            .ok_or_else(|| ApiError::unauthorized().with_request_id(rid.clone()))?;
        let session = load_session(&state.pool, &token)
            .await
            .map_err(|e| ApiError::internal("resolving session", e).with_request_id(rid.clone()))?
            .ok_or_else(|| ApiError::unauthorized().with_request_id(rid.clone()))?;
        // Forced rotation comes first: "change it before anything else".
        // Otherwise the dashboard keeps its M3 behaviour: only admin sessions
        // use `/v1` directly; everyone else authenticates with an API key.
        if session.must_change_password {
            return Err(password_change_required(rid));
        }
        if !session.role.is_admin() {
            return Err(ApiError::forbidden().with_request_id(rid));
        }
        Ok(Self(DataPlaneCaller {
            user_id: session.user_id,
            role: session.role,
            api_key: None,
        }))
    }
}

/// The `403 password_change_required` error: the account must rotate its
/// password before anything else works (review finding 7).
pub fn password_change_required(rid: Option<String>) -> ApiError {
    ApiError::custom(
        axum::http::StatusCode::FORBIDDEN,
        "password_change_required",
        "the account must change its password before using the API",
    )
    .with_request_id(rid)
}

/// The `Authorization: Bearer <token>` value, when present.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
}

// ---------------------------------------------------------------------------
// Login rate limiter
// ---------------------------------------------------------------------------

/// In-memory login rate limiter, keyed by `(ip, email)`.
///
/// Five failures within fifteen minutes lock the key out for 60 seconds; every
/// further failure doubles the lockout, capped at fifteen minutes. A successful
/// login clears the key. The map is bounded at ten thousand entries, evicting
/// the least recently touched key, so unbounded address spoofing cannot leak
/// memory. Time is passed in explicitly so the rules are unit-testable.
#[derive(Debug, Default)]
pub struct LoginRateLimiter {
    entries: Mutex<HashMap<(String, String), LimiterEntry>>,
}

#[derive(Debug)]
struct LimiterEntry {
    failures: VecDeque<Instant>,
    locked_until: Option<Instant>,
    touched: Instant,
}

impl LoginRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remaining lockout for `(ip, email)`, or `None` when login may proceed.
    pub fn check(&self, ip: &str, email: &str, now: Instant) -> Option<Duration> {
        let mut entries = self.lock();
        let entry = entries.get_mut(&key(ip, email))?;
        entry.touched = now;
        match entry.locked_until {
            Some(until) if until > now => Some(until - now),
            _ => {
                entry.locked_until = None;
                None
            }
        }
    }

    /// Records a failed attempt and returns the resulting lockout, if any.
    /// Attempts made while locked count as failures and extend the lockout.
    pub fn record_failure(&self, ip: &str, email: &str, now: Instant) -> Option<Duration> {
        let mut entries = self.lock();
        let key = key(ip, email);
        if !entries.contains_key(&key) && entries.len() >= LOGIN_LIMITER_MAX_ENTRIES {
            evict_oldest(&mut entries);
        }

        let entry = entries.entry(key).or_insert_with(|| LimiterEntry {
            failures: VecDeque::new(),
            locked_until: None,
            touched: now,
        });
        entry.touched = now;
        entry
            .failures
            .retain(|at| now.saturating_duration_since(*at) < LOGIN_FAILURE_WINDOW);
        entry.failures.push_back(now);

        if entry.failures.len() < LOGIN_FAILURE_THRESHOLD {
            entry.locked_until = None;
            return None;
        }

        let excess = (entry.failures.len() - LOGIN_FAILURE_THRESHOLD) as u32;
        let doubled = LOGIN_LOCK_BASE
            .checked_mul(2u32.saturating_pow(excess.min(20)))
            .unwrap_or(LOGIN_LOCK_MAX);
        let lock = doubled.min(LOGIN_LOCK_MAX);
        entry.locked_until = Some(now + lock);
        Some(lock)
    }

    /// Clears the counter after a successful login.
    pub fn record_success(&self, ip: &str, email: &str) {
        self.lock().remove(&key(ip, email));
    }

    /// Number of tracked keys; bounded by [`LOGIN_LIMITER_MAX_ENTRIES`].
    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }

    #[cfg(test)]
    fn contains(&self, ip: &str, email: &str) -> bool {
        self.lock().contains_key(&key(ip, email))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), LimiterEntry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn key(ip: &str, email: &str) -> (String, String) {
    (ip.to_string(), email.to_ascii_lowercase())
}

fn evict_oldest(entries: &mut HashMap<(String, String), LimiterEntry>) {
    if let Some(oldest) = entries
        .iter()
        .min_by_key(|(_, entry)| entry.touched)
        .map(|(key, _)| key.clone())
    {
        entries.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_token_matches_known_sha256_vector() {
        // sha256("abc")
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn csrf_comparison_is_exact_and_total() {
        assert!(csrf_matches("token-a", "token-a"));
        assert!(!csrf_matches("token-a", "token-b"));
        assert!(!csrf_matches("token-a", "token-aa"));
        assert!(!csrf_matches("token-a", ""));
        assert!(!csrf_matches("", "token-a"));
    }

    #[test]
    fn session_cookie_round_trips_with_attributes() {
        let value = session_cookie("raw-token-value", true);
        assert!(value.contains("HttpOnly"));
        assert!(value.contains("SameSite=Strict"));
        assert!(value.contains("Path=/"));
        assert!(value.contains("Secure"));
        assert!(value.contains("Max-Age=604800"));

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("other=x; {value}").parse().expect("header value"),
        );
        assert_eq!(
            session_cookie_value(&headers).as_deref(),
            Some("raw-token-value")
        );
    }

    #[test]
    fn insecure_cookie_omits_secure_flag() {
        assert!(!session_cookie("t", false).contains("Secure"));
        let cleared = cleared_session_cookie(true);
        assert!(cleared.contains("Max-Age=0"));
    }

    #[test]
    fn limiter_locks_after_five_failures() {
        let limiter = LoginRateLimiter::new();
        let t0 = Instant::now();
        for i in 0..4 {
            assert_eq!(
                limiter.record_failure("1.2.3.4", "a@example.com", t0 + Duration::from_secs(i)),
                None,
                "failure {} must not lock",
                i + 1
            );
        }
        let lock = limiter
            .record_failure("1.2.3.4", "a@example.com", t0 + Duration::from_secs(5))
            .expect("fifth failure must lock");
        assert_eq!(lock, LOGIN_LOCK_BASE);
        assert_eq!(
            limiter.check("1.2.3.4", "a@example.com", t0 + Duration::from_secs(6)),
            Some(LOGIN_LOCK_BASE - Duration::from_secs(1))
        );
    }

    #[test]
    fn limiter_doubles_per_further_attempt_and_caps() {
        let limiter = LoginRateLimiter::new();
        let t0 = Instant::now();
        let mut last = Duration::ZERO;
        for i in 0..12 {
            if let Some(lock) =
                limiter.record_failure("ip", "user@example.com", t0 + Duration::from_secs(i))
            {
                assert!(lock >= last, "lockout must never shrink");
                last = lock;
            }
        }
        assert_eq!(last, LOGIN_LOCK_MAX, "lockout must cap at fifteen minutes");
    }

    #[test]
    fn limiter_lockout_expires() {
        let limiter = LoginRateLimiter::new();
        let t0 = Instant::now();
        for i in 0..5 {
            limiter.record_failure("ip", "user@example.com", t0 + Duration::from_secs(i));
        }
        assert!(
            limiter
                .check("ip", "user@example.com", t0 + Duration::from_secs(10))
                .is_some()
        );
        assert_eq!(
            limiter.check("ip", "user@example.com", t0 + Duration::from_secs(65)),
            None,
            "the 60s lockout (from the failure at t0+4s) must expire at t0+64s"
        );
    }

    #[test]
    fn limiter_success_clears_the_counter() {
        let limiter = LoginRateLimiter::new();
        let t0 = Instant::now();
        for i in 0..4 {
            limiter.record_failure("ip", "user@example.com", t0 + Duration::from_secs(i));
        }
        limiter.record_success("ip", "user@example.com");
        for i in 10..14 {
            assert_eq!(
                limiter.record_failure("ip", "user@example.com", t0 + Duration::from_secs(i)),
                None,
                "the counter must restart after a successful login"
            );
        }
    }

    #[test]
    fn limiter_isolates_keys() {
        let limiter = LoginRateLimiter::new();
        let t0 = Instant::now();
        for i in 0..5 {
            limiter.record_failure("ip-a", "user@example.com", t0 + Duration::from_secs(i));
        }
        assert!(limiter.check("ip-a", "user@example.com", t0).is_some());
        assert_eq!(limiter.check("ip-b", "user@example.com", t0), None);
        assert_eq!(limiter.check("ip-a", "other@example.com", t0), None);
    }

    #[test]
    fn limiter_evicts_oldest_beyond_the_bound() {
        let limiter = LoginRateLimiter::new();
        let t0 = Instant::now();
        for i in 0..LOGIN_LIMITER_MAX_ENTRIES {
            limiter.record_failure(
                &format!("ip-{i}"),
                "user@example.com",
                t0 + Duration::from_millis(i as u64),
            );
        }
        assert_eq!(limiter.len(), LOGIN_LIMITER_MAX_ENTRIES);

        limiter.record_failure("ip-new", "user@example.com", t0 + Duration::from_secs(20));
        assert_eq!(
            limiter.len(),
            LOGIN_LIMITER_MAX_ENTRIES,
            "map must stay bounded"
        );
        assert!(
            !limiter.contains("ip-0", "user@example.com"),
            "oldest key evicted"
        );
        assert!(limiter.contains("ip-new", "user@example.com"));
    }

    #[test]
    fn role_round_trips() {
        assert_eq!(Role::parse("admin"), Some(Role::Admin));
        assert_eq!(Role::parse("user"), Some(Role::User));
        assert_eq!(Role::parse("root"), None);
        assert_eq!(Role::Admin.as_str(), "admin");
    }

    /// Builds request parts with a peer address and optional forwarding
    /// headers, as `resolve_client_ip` sees them. Each `xff` entry becomes its
    /// own `X-Forwarded-For` header line.
    fn request_parts(peer: Option<SocketAddr>, xff: &[&str], real: Option<&str>) -> Parts {
        let (mut parts, ()) = axum::http::Request::builder()
            .uri("/api/auth/login")
            .body(())
            .expect("request parts")
            .into_parts();
        for value in xff {
            parts
                .headers
                .append("x-forwarded-for", (*value).parse().expect("header value"));
        }
        if let Some(real) = real {
            parts
                .headers
                .append("x-real-ip", real.parse().expect("header value"));
        }
        if let Some(addr) = peer {
            parts.extensions.insert(ConnectInfo(addr));
        }
        parts
    }

    fn trusted(values: &[&str]) -> Vec<IpCidr> {
        values
            .iter()
            .map(|value| IpCidr::parse(value).expect("test cidr"))
            .collect()
    }

    #[test]
    fn client_ip_from_a_trusted_proxy_reads_the_rightmost_forwarded_hop() {
        let proxies = trusted(&["10.0.0.0/8"]);
        // The rightmost untrusted hop is the one the trusted proxy appended;
        // the walk never reaches the client-controlled hop to its left.
        let parts = request_parts(
            Some("10.0.0.5:4242".parse().expect("peer")),
            &["203.0.113.200, 198.51.100.7"],
            None,
        );
        assert_eq!(resolve_client_ip(&parts, &proxies), "198.51.100.7");
    }

    #[test]
    fn client_ip_walks_chained_trusted_proxies_to_the_original_client() {
        // client (203.0.113.200) → LB (10.0.0.1) → nginx (192.168.0.5) →
        // gateway: both proxy hops are trusted. Each proxy appended its own
        // peer, so nginx forwards `client, LB`. Taking the rightmost hop
        // would pin every client behind the LB to the LB's address.
        let proxies = trusted(&["10.0.0.0/8", "192.168.0.0/16"]);
        let parts = request_parts(
            Some("192.168.0.5:443".parse().expect("peer")),
            &["203.0.113.200, 10.0.0.1"],
            None,
        );
        assert_eq!(resolve_client_ip(&parts, &proxies), "203.0.113.200");
    }

    #[test]
    fn client_ip_ignores_the_spoofed_line_of_duplicated_forwarded_headers() {
        let proxies = trusted(&["10.0.0.0/8"]);
        // The client sent its own `X-Forwarded-For` line and the proxy added
        // a second one carrying the real client address. Reading only the
        // first header line used to let the client pick its own address.
        let parts = request_parts(
            Some("10.0.0.5:4242".parse().expect("peer")),
            &["198.51.100.99", "203.0.113.30"],
            None,
        );
        assert_eq!(resolve_client_ip(&parts, &proxies), "203.0.113.30");
    }

    #[test]
    fn client_ip_falls_back_to_the_peer_when_the_whole_chain_is_trusted() {
        let proxies = trusted(&["10.0.0.0/8"]);
        let parts = request_parts(
            Some("10.0.0.5:4242".parse().expect("peer")),
            &["10.0.0.1, 10.0.0.2"],
            Some("203.0.113.20"),
        );
        // Every hop is a trusted proxy, so nothing attributes the request to
        // a client further away — and X-Real-IP is not consulted because an
        // X-Forwarded-For value exists.
        assert_eq!(resolve_client_ip(&parts, &proxies), "10.0.0.5");
    }

    #[test]
    fn client_ip_ignores_forwarding_headers_from_untrusted_peers() {
        let proxies = trusted(&["10.0.0.0/8"]);
        let parts = request_parts(
            Some("192.0.2.1:4242".parse().expect("peer")),
            &["203.0.113.9"],
            Some("203.0.113.10"),
        );
        assert_eq!(resolve_client_ip(&parts, &proxies), "192.0.2.1");
    }

    #[test]
    fn client_ip_never_trusts_headers_without_trusted_proxy_config() {
        let parts = request_parts(
            Some("10.0.0.5:4242".parse().expect("peer")),
            &["203.0.113.9"],
            None,
        );
        assert_eq!(resolve_client_ip(&parts, &[]), "10.0.0.5");

        // No peer either: the headers cannot be attributed to any proxy.
        let no_peer = request_parts(None, &["203.0.113.9"], Some("203.0.113.10"));
        assert_eq!(resolve_client_ip(&no_peer, &[]), "unknown");
    }

    #[test]
    fn client_ip_falls_back_to_real_ip_and_rejects_malformed_hops() {
        let proxies = trusted(&["127.0.0.1/32"]);
        // X-Real-IP is only consulted when no X-Forwarded-For value exists.
        let real = request_parts(
            Some("127.0.0.1:1".parse().expect("peer")),
            &[],
            Some("203.0.113.20"),
        );
        assert_eq!(resolve_client_ip(&real, &proxies), "203.0.113.20");

        // Garbage never becomes the recorded client address: nothing parses,
        // so the peer itself is the client.
        let garbage = request_parts(
            Some("127.0.0.1:1".parse().expect("peer")),
            &["not-an-ip"],
            None,
        );
        assert_eq!(resolve_client_ip(&garbage, &proxies), "127.0.0.1");

        // A malformed hop is ignored and the walk keeps going left: the
        // rightmost parseable untrusted hop is still the client.
        let garbage_tail = request_parts(
            Some("127.0.0.1:1".parse().expect("peer")),
            &["203.0.113.9, not-an-ip"],
            None,
        );
        assert_eq!(resolve_client_ip(&garbage_tail, &proxies), "203.0.113.9");
    }
}
