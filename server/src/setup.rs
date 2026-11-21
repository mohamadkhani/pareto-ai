//! First-run setup: the mode machine and the exposure rules.
//!
//! With no users and no `GATEWAY_ADMIN_*` environment shortcut, the gateway
//! starts in **setup** mode instead of failing. The operator either creates the
//! first admin through the wizard (`setup` → `protected`) or explicitly skips
//! it for a local-only deployment (`setup` → `unprotected`). A gateway with at
//! least one user is always `protected`.
//!
//! Exposure rule: setup and unprotected are local conveniences. A bind address
//! that is not loopback refuses to start in those modes unless
//! `GATEWAY_ALLOW_PUBLIC_NO_AUTH=1` is set; with the flag it starts and keeps
//! the warning. Desktop-wrapper requests (`tauri://localhost`,
//! `https://tauri.localhost`, `wails://...`) and loopback peers count as local
//! for the wizard and the warning text.

use std::net::SocketAddr;
use std::sync::RwLock;

use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, header, request::Parts};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;

use crate::master_key::MasterKeySource;

/// The settings key that records an explicit "skip for now". It keeps the
/// unprotected mode across restarts while no user exists.
pub const SETTING_SETUP_SKIPPED: &str = "setup.skipped";

/// Which authentication state the gateway is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// No users; the wizard is the only way in.
    Setup,
    /// No users; explicitly skipped. The gateway acts as a trusted local
    /// operator and logs the exposure warning on every start.
    Unprotected,
    /// At least one account exists; normal session authentication applies.
    Protected,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Unprotected => "unprotected",
            Self::Protected => "protected",
        }
    }

    /// True only when authentication is enforced.
    pub fn is_protected(self) -> bool {
        self == Self::Protected
    }
}

/// Read-mostly setup state shared through [`crate::AppState`].
#[derive(Debug)]
pub struct SetupState {
    mode: RwLock<Mode>,
    master_key_source: MasterKeySource,
}

impl SetupState {
    pub fn new(mode: Mode, master_key_source: MasterKeySource) -> Self {
        Self {
            mode: RwLock::new(mode),
            master_key_source,
        }
    }

    pub fn mode(&self) -> Mode {
        *self
            .mode
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Moves the machine (setup → protected/unprotected, unprotected →
    /// protected). Only `resolve_mode` and the two setup actions call this.
    pub fn set_mode(&self, mode: Mode) {
        *self
            .mode
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = mode;
    }

    pub fn master_key_source(&self) -> MasterKeySource {
        self.master_key_source
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("the first admin account already exists")]
    AlreadyComplete,
    #[error("an account with this email already exists")]
    EmailTaken,
    #[error("audit write failed: {0}")]
    Audit(#[from] crate::auth::AuthError),
    #[error("password hashing failed: {0}")]
    Password(#[from] crate::password::PasswordError),
}

/// Derives the startup mode from the database: any real account means
/// protected, an explicit skip means unprotected, otherwise setup. The
/// local-operator placeholder does not count as an account.
pub async fn resolve_mode(pool: &SqlitePool) -> Result<Mode, sqlx::Error> {
    if real_user_count(pool).await? > 0 {
        return Ok(Mode::Protected);
    }
    let skipped: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(SETTING_SETUP_SKIPPED)
        .fetch_optional(pool)
        .await?;
    Ok(match skipped.as_deref() {
        Some("1") => Mode::Unprotected,
        _ => Mode::Setup,
    })
}

/// Number of real accounts in `users`; the local-operator placeholder does
/// not count.
pub async fn real_user_count(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_placeholder = 0")
        .fetch_one(pool)
        .await
}

/// Number of active, real administrators. Setup stays open until one exists
/// within the current process: a non-admin account does not make the gateway
/// administrable (D57). After a restart `resolve_mode` reports `protected` for
/// any real account, so a database with only non-admin accounts needs the
/// manual recovery path in the README.
///
/// Failure mode: [`sqlx::Error`] when the count query fails.
pub async fn real_admin_count(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM users \
         WHERE is_placeholder = 0 AND role = 'admin' AND is_active = 1",
    )
    .fetch_one(pool)
    .await
}

/// Seeds the local-operator placeholder account (migration 0016): the row
/// behind the synthetic [`crate::auth::local_operator`] identity, so every
/// audit and `*_updated_by` write during unprotected mode satisfies its
/// foreign key onto `users`. A no-op when the placeholder already exists or
/// when a real account does (the identity is only used without one).
pub async fn ensure_local_operator<'e, E>(db: E) -> Result<(), SetupError>
where
    E: sqlx::SqliteExecutor<'e>,
{
    sqlx::query(
        "INSERT OR IGNORE INTO users \
           (id, email, display_name, password_hash, role, is_active, must_change_password, \
            is_placeholder) \
         SELECT ?, 'local@localhost', 'Local operator', '!', 'admin', 1, 0, 1 \
         WHERE NOT EXISTS (SELECT 1 FROM users WHERE is_placeholder = 0)",
    )
    .bind(crate::auth::LOCAL_OPERATOR_ID)
    .execute(db)
    .await?;
    Ok(())
}

/// Records the explicit skip so the unprotected mode survives a restart, and
/// seeds the local-operator placeholder account (a no-op when a real account
/// exists).
pub async fn mark_skipped(pool: &SqlitePool, ip: &str) -> Result<(), SetupError> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at, updated_by) \
         VALUES (?, '1', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), NULL) \
         ON CONFLICT (key) DO UPDATE SET value = '1', \
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .bind(SETTING_SETUP_SKIPPED)
    .execute(&mut *tx)
    .await?;
    ensure_local_operator(&mut *tx).await?;
    crate::auth::audit(
        &mut *tx,
        None,
        "setup.skipped",
        Some("gateway"),
        None,
        Some(&serde_json::json!({ "mode": "unprotected" }).to_string()),
        Some(ip),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Creates the first admin from the wizard: role admin, active, and **not**
/// flagged for forced rotation (the operator chose this password themselves).
/// When the local-operator placeholder exists (setup was skipped earlier) it
/// is promoted to that admin **in place** — same row, so every audit and
/// `*_updated_by` reference to it stays valid — instead of a second row
/// appearing. The promotion forces `role = 'admin'` and `is_active = 1` and
/// revokes the placeholder's live API keys: keys minted while the gateway had
/// no password must not inherit the administrator's `/v1` access (D23).
///
/// Failure mode: [`SetupError::AlreadyComplete`] when an active real
/// administrator exists, [`SetupError::EmailTaken`] when the email already
/// belongs to an account, [`SetupError::Invalid`] for a bad email or a
/// too-short password.
pub async fn create_first_admin(
    pool: &SqlitePool,
    email: &str,
    password: &str,
    ip: &str,
) -> Result<String, SetupError> {
    // Fast path: refuse before hashing the password. The authoritative check
    // is repeated in the same statements as the writes below, so two concurrent
    // wizard requests cannot both create an administrator.
    if real_admin_count(pool).await? > 0 {
        return Err(SetupError::AlreadyComplete);
    }
    let email =
        crate::users::validate_email(email).map_err(|e| SetupError::Invalid(e.to_string()))?;
    crate::users::validate_password(password).map_err(|e| SetupError::Invalid(e.to_string()))?;
    let password = password.to_string();
    let password_hash = crate::password::hash_password_offload(password).await?;

    let mut tx = pool.begin().await?;
    // Write-first: both statements carry the "no active real administrator"
    // guard, and SQLite serialises writers, so the count cannot change between
    // them and a commit from another wizard request.
    //
    // The promotion forces `role` and `is_active`: the local operator row can
    // have been edited before the password existed (an older build allowed it,
    // and filesystem access always can), and the first password must yield an
    // administrator regardless.
    let promoted = sqlx::query_as::<_, (String,)>(
        "UPDATE users SET email = ?, display_name = ?, password_hash = ?, \
           role = 'admin', is_active = 1, \
           must_change_password = 0, is_placeholder = 0, \
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = ? AND is_placeholder = 1 \
           AND NOT EXISTS ( \
             SELECT 1 FROM users \
             WHERE is_placeholder = 0 AND role = 'admin' AND is_active = 1 \
           ) \
         RETURNING id",
    )
    .bind(&email)
    .bind(&email)
    .bind(&password_hash)
    .bind(crate::auth::LOCAL_OPERATOR_ID)
    .fetch_optional(&mut *tx)
    .await;
    let promoted = match promoted {
        Ok(row) => row,
        // The new email can belong to an existing real account; the unique
        // constraint then fires on this promotion, not just the insert below.
        Err(sqlx::Error::Database(ref db)) if db.is_unique_violation() => {
            return Err(SetupError::EmailTaken);
        }
        Err(error) => return Err(error.into()),
    };
    let (id, promoted) = match promoted {
        Some((id,)) => (id, true),
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            let inserted = sqlx::query(
                "INSERT INTO users (id, email, display_name, password_hash, role, is_active, \
                                    must_change_password) \
                 SELECT ?, ?, ?, ?, 'admin', 1, 0 \
                 WHERE NOT EXISTS ( \
                   SELECT 1 FROM users \
                   WHERE is_placeholder = 0 AND role = 'admin' AND is_active = 1 \
                 )",
            )
            .bind(&id)
            .bind(&email)
            .bind(&email)
            .bind(&password_hash)
            .execute(&mut *tx)
            .await;
            let inserted = match inserted {
                Ok(result) => result,
                Err(sqlx::Error::Database(ref db)) if db.is_unique_violation() => {
                    return Err(SetupError::EmailTaken);
                }
                Err(error) => return Err(error.into()),
            };
            if inserted.rows_affected() == 0 {
                return Err(SetupError::AlreadyComplete);
            }
            (id, false)
        }
    };
    // API keys minted while the gateway had no password belong to the same row
    // and would otherwise inherit the new administrator's `/v1` access (D23).
    // The insert path has no keys by construction.
    let api_keys_revoked = if promoted {
        crate::users::revoke_live_api_keys(&mut *tx, &id).await?
    } else {
        0
    };
    crate::auth::audit(
        &mut *tx,
        Some(&id),
        "auth.bootstrap_admin_created",
        Some("user"),
        Some(&id),
        Some(
            &serde_json::json!({
                "email": email,
                "source": "setup",
                "promoted_local_operator": promoted,
                "api_keys_revoked": api_keys_revoked,
            })
            .to_string(),
        ),
        Some(ip),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(
        email = %email,
        promoted_local_operator = promoted,
        api_keys_revoked,
        "created the first admin from the setup wizard"
    );
    Ok(id)
}

/// The startup exposure decision for a bind address and mode.
///
/// `Err` means the gateway must refuse to start; `Ok(Some(warning))` means it
/// may start but is exposed; `Ok(None)` means nothing to report.
pub fn exposure(
    bind: SocketAddr,
    mode: Mode,
    allow_public_no_auth: bool,
) -> Result<Option<String>, String> {
    if mode.is_protected() || bind.ip().is_loopback() {
        return Ok(None);
    }
    let warning = format!(
        "this gateway is reachable from the network without a password \
         (listening on {bind}, authentication {mode})",
        mode = mode.as_str()
    );
    if allow_public_no_auth {
        return Ok(Some(warning));
    }
    Err(format!(
        "refusing to start: listening on {bind} (not loopback) with authentication in \
         '{mode}' state would expose every route to the network without a password. \
         Either bind to 127.0.0.1 and finish setup locally, or set \
         GATEWAY_ALLOW_PUBLIC_NO_AUTH=1 to start anyway (not recommended)",
        mode = mode.as_str()
    ))
}

/// True when `value` is one of the desktop-wrapper origins (`tauri://...`,
/// `https://tauri.localhost`, `wails://...`).
pub fn is_desktop_origin(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.starts_with("tauri://")
        || value.starts_with("https://tauri.localhost")
        || value.starts_with("wails://")
}

/// True when the request's `Origin` or `Referer` names a desktop wrapper.
pub fn desktop_wrapper(headers: &HeaderMap) -> bool {
    [header::ORIGIN, header::REFERER].iter().any(|name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .is_some_and(is_desktop_origin)
    })
}

/// True when the request's `Origin` is absent, a desktop wrapper, or the
/// request's own origin (`http(s)://<Host>`).
///
/// The setup mutations sit outside the CSRF guard (there is no session yet) and
/// a browser can reach a loopback gateway from any web page, so an explicit
/// cross-site `Origin` is refused. Non-browser clients send no `Origin`.
pub fn origin_is_trusted(headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    if is_desktop_origin(origin) {
        return true;
    }
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    origin == format!("http://{host}") || origin == format!("https://{host}")
}

/// True when the TCP peer is a loopback address.
pub fn loopback_peer(parts: &Parts) -> bool {
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(addr)| addr.ip().is_loopback())
}

/// True when the request counts as local for the wizard and warning logic:
/// a loopback peer or a desktop-wrapper origin.
pub fn is_local(parts: &Parts) -> bool {
    loopback_peer(parts) || desktop_wrapper(&parts.headers)
}

/// The warning shown for a mode, worded calmly for a local request and in
/// danger terms for a network-reachable one.
pub fn warning_for(mode: Mode, local: bool) -> Option<String> {
    match (mode, local) {
        (Mode::Protected, _) => None,
        (Mode::Unprotected, true) => {
            Some("Running without a password on this machine.".to_string())
        }
        (Mode::Unprotected, false) => Some(
            "This gateway is reachable from the network without a password. \
             Set one in Settings."
                .to_string(),
        ),
        (Mode::Setup, true) => Some(
            "This gateway is not protected yet. Finish setup to set an admin password.".to_string(),
        ),
        (Mode::Setup, false) => Some(
            "This gateway is reachable from the network without a password. \
             Finish setup to set an admin password."
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use std::net::SocketAddr;

    fn parts(peer: Option<&str>, origin: Option<&str>, referer: Option<&str>) -> Parts {
        let mut builder = Request::builder().uri("/api/setup/status");
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        if let Some(referer) = referer {
            builder = builder.header(header::REFERER, referer);
        }
        let (mut parts, ()) = builder.body(()).expect("request").into_parts();
        if let Some(peer) = peer {
            parts.extensions.insert(ConnectInfo(
                peer.parse::<SocketAddr>().expect("test peer address"),
            ));
        }
        parts
    }

    #[tokio::test]
    async fn mode_derives_from_users_and_the_skip_setting() {
        use crate::test_support::TestApp;
        let app = TestApp::new().await;

        assert_eq!(
            resolve_mode(&app.state.pool).await.expect("mode"),
            Mode::Setup,
            "no users and no skip is setup"
        );

        mark_skipped(&app.state.pool, "127.0.0.1")
            .await
            .expect("skip");
        assert_eq!(
            resolve_mode(&app.state.pool).await.expect("mode"),
            Mode::Unprotected,
            "an explicit skip survives a restart"
        );

        app.create_user("admin@example.com", crate::auth::Role::Admin, true)
            .await;
        assert_eq!(
            resolve_mode(&app.state.pool).await.expect("mode"),
            Mode::Protected,
            "any user means protected"
        );

        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
            .fetch_all(&app.state.pool)
            .await
            .expect("audit");
        assert_eq!(actions, ["setup.skipped"]);
    }

    #[tokio::test]
    async fn only_active_real_admins_close_setup() {
        use crate::test_support::TestApp;
        let app = TestApp::new().await;
        assert_eq!(
            real_admin_count(&app.state.pool).await.expect("count"),
            0,
            "no users"
        );

        mark_skipped(&app.state.pool, "127.0.0.1")
            .await
            .expect("skip");
        assert_eq!(
            real_admin_count(&app.state.pool).await.expect("count"),
            0,
            "the placeholder is not a real administrator"
        );

        app.create_user("plain@example.com", crate::auth::Role::User, true)
            .await;
        assert_eq!(
            real_admin_count(&app.state.pool).await.expect("count"),
            0,
            "a non-admin account does not make the gateway administrable"
        );

        let admin = app
            .create_user("boss@example.com", crate::auth::Role::Admin, true)
            .await;
        assert_eq!(
            real_admin_count(&app.state.pool).await.expect("count"),
            1,
            "an active real administrator closes setup"
        );

        sqlx::query("UPDATE users SET is_active = 0 WHERE id = ?")
            .bind(&admin)
            .execute(&app.state.pool)
            .await
            .expect("deactivate");
        assert_eq!(
            real_admin_count(&app.state.pool).await.expect("count"),
            0,
            "a deactivated administrator does not count"
        );
    }

    #[tokio::test]
    async fn the_skip_seeds_the_local_operator_placeholder_without_counting_as_a_user() {
        use crate::test_support::TestApp;
        let app = TestApp::new().await;

        mark_skipped(&app.state.pool, "127.0.0.1")
            .await
            .expect("skip");
        let (total, placeholders): (i64, i64) =
            sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(is_placeholder), 0) FROM users")
                .fetch_one(&app.state.pool)
                .await
                .expect("users");
        assert_eq!(total, 1, "the placeholder is a real row in users");
        assert_eq!(placeholders, 1, "and it is flagged as the placeholder");
        assert_eq!(
            real_user_count(&app.state.pool).await.expect("count"),
            0,
            "the placeholder is not a real account"
        );

        // Seeding is idempotent and never adds a second row.
        mark_skipped(&app.state.pool, "127.0.0.1")
            .await
            .expect("skip again");
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&app.state.pool)
            .await
            .expect("users");
        assert_eq!(total, 1);
    }

    #[tokio::test]
    async fn the_placeholder_is_promoted_in_place_and_insecure_actions_stay_marked() {
        use crate::test_support::TestApp;
        let app = TestApp::new().await;
        mark_skipped(&app.state.pool, "127.0.0.1")
            .await
            .expect("skip");

        // While the placeholder acts (unprotected mode), audit rows say so.
        crate::auth::audit(
            &app.state.pool,
            Some(crate::auth::LOCAL_OPERATOR_ID),
            "test.insecure_action",
            None,
            None,
            None,
            None,
        )
        .await
        .expect("audit");
        let insecure: i64 = sqlx::query_scalar(
            "SELECT insecure FROM audit_log WHERE action = 'test.insecure_action'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("flag");
        assert_eq!(insecure, 1, "unprotected actions are flagged insecure");

        let id = create_first_admin(
            &app.state.pool,
            "boss@example.com",
            "first-admin-password",
            "127.0.0.1",
        )
        .await
        .expect("create");
        assert_eq!(
            id,
            crate::auth::LOCAL_OPERATOR_ID,
            "the placeholder is promoted in place, not replaced"
        );

        let (email, must_change, placeholder): (String, i64, i64) = sqlx::query_as(
            "SELECT email, must_change_password, is_placeholder FROM users WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&app.state.pool)
        .await
        .expect("row");
        assert_eq!(email, "boss@example.com");
        assert_eq!(must_change, 0, "the operator chose this password");
        assert_eq!(placeholder, 0, "it is a real account now");
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
            .bind(&id)
            .fetch_one(&app.state.pool)
            .await
            .expect("hash");
        assert!(crate::password::verify_password("first-admin-password", &hash).expect("verify"));

        // The same id acting as a real account is no longer insecure, and the
        // rows written before the promotion keep their flag.
        crate::auth::audit(
            &app.state.pool,
            Some(crate::auth::LOCAL_OPERATOR_ID),
            "test.action_after",
            None,
            None,
            None,
            None,
        )
        .await
        .expect("audit");
        let (before, after): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT insecure FROM audit_log WHERE action = 'test.insecure_action'), \
                    (SELECT insecure FROM audit_log WHERE action = 'test.action_after')",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("flags");
        assert_eq!(
            (before, after),
            (1, 0),
            "only insecure-mode actions are flagged"
        );

        assert_eq!(
            resolve_mode(&app.state.pool).await.expect("mode"),
            Mode::Protected,
            "a promoted account protects the gateway"
        );
        let again = create_first_admin(
            &app.state.pool,
            "second@example.com",
            "second-admin-password",
            "127.0.0.1",
        )
        .await
        .expect_err("a second first-admin must fail");
        assert!(matches!(again, SetupError::AlreadyComplete), "{again:?}");
    }

    #[tokio::test]
    async fn create_first_admin_is_unflagged_audited_and_only_once() {
        use crate::test_support::TestApp;
        let app = TestApp::new().await;

        let id = create_first_admin(
            &app.state.pool,
            " Boss@Example.com ",
            "first-admin-password",
            "127.0.0.1",
        )
        .await
        .expect("create");

        let (email, role, must_change): (String, String, i64) =
            sqlx::query_as("SELECT email, role, must_change_password FROM users WHERE id = ?")
                .bind(&id)
                .fetch_one(&app.state.pool)
                .await
                .expect("row");
        assert_eq!(email, "Boss@Example.com");
        assert_eq!(role, "admin");
        assert_eq!(must_change, 0, "the operator chose this password");

        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
            .bind(&id)
            .fetch_one(&app.state.pool)
            .await
            .expect("hash");
        assert!(hash.starts_with("$argon2id$"), "new hashes use Argon2id");
        assert!(crate::password::verify_password("first-admin-password", &hash).expect("verify"));

        let again = create_first_admin(
            &app.state.pool,
            "second@example.com",
            "second-admin-password",
            "127.0.0.1",
        )
        .await
        .expect_err("a second first-admin must fail");
        assert!(matches!(again, SetupError::AlreadyComplete), "{again:?}");

        mark_skipped(&app.state.pool, "127.0.0.1")
            .await
            .expect("the skip marker is idempotent");
    }

    #[tokio::test]
    async fn first_admin_rejects_bad_input() {
        use crate::test_support::TestApp;
        let app = TestApp::new().await;

        assert!(matches!(
            create_first_admin(&app.state.pool, "not-an-email", "long-enough-1", "ip").await,
            Err(SetupError::Invalid(_))
        ));
        assert!(matches!(
            create_first_admin(&app.state.pool, "a@example.com", "short", "ip").await,
            Err(SetupError::Invalid(_))
        ));
        assert_eq!(real_user_count(&app.state.pool).await.expect("count"), 0);
    }

    #[test]
    fn exposure_refuses_public_binds_unless_allowed() {
        let public: SocketAddr = "0.0.0.0:8081".parse().expect("addr");
        let loopback: SocketAddr = "127.0.0.1:8081".parse().expect("addr");

        // Loopback is always fine, in every mode.
        for mode in [Mode::Setup, Mode::Unprotected, Mode::Protected] {
            assert_eq!(exposure(loopback, mode, false).expect("loopback"), None);
        }

        // Protected is always fine, even on a public bind.
        assert_eq!(
            exposure(public, Mode::Protected, false).expect("protected"),
            None
        );

        // Setup and unprotected refuse without the flag.
        for mode in [Mode::Setup, Mode::Unprotected] {
            let err = exposure(public, mode, false).expect_err("must refuse");
            assert!(err.contains("refusing to start"), "{err}");
            assert!(err.contains("GATEWAY_ALLOW_PUBLIC_NO_AUTH=1"), "{err}");
            assert!(err.contains(mode.as_str()), "{err}");
        }

        // With the flag they start but keep the warning.
        for mode in [Mode::Setup, Mode::Unprotected] {
            let warning = exposure(public, mode, true)
                .expect("allowed")
                .expect("warning");
            assert!(warning.contains("without a password"), "{warning}");
            assert!(warning.contains(mode.as_str()), "{warning}");
        }
    }

    #[test]
    fn local_detection_accepts_loopback_and_desktop_wrappers() {
        assert!(is_local(&parts(Some("127.0.0.1:5555"), None, None)));
        assert!(is_local(&parts(Some("[::1]:5555"), None, None)));
        assert!(!is_local(&parts(Some("203.0.113.9:5555"), None, None)));
        assert!(!is_local(&parts(None, None, None)), "no peer is not local");

        for origin in [
            "tauri://localhost",
            "https://tauri.localhost",
            "wails://localhost",
            "wails://wails.localhost",
        ] {
            assert!(
                is_local(&parts(Some("203.0.113.9:5555"), Some(origin), None)),
                "origin {origin} counts as local"
            );
        }
        assert!(
            is_local(&parts(
                Some("203.0.113.9:5555"),
                None,
                Some("https://tauri.localhost/console")
            )),
            "a referer counts as local too"
        );
        assert!(
            !is_local(&parts(
                Some("203.0.113.9:5555"),
                Some("https://evil.example.com"),
                None
            )),
            "an ordinary web origin is not local"
        );
    }

    #[test]
    fn a_cross_site_origin_never_passes_the_setup_gate() {
        let origin = |origin: Option<&str>, host: Option<&str>| {
            let mut builder = Request::builder().uri("/api/setup/admin");
            if let Some(host) = host {
                builder = builder.header(header::HOST, host);
            }
            if let Some(origin) = origin {
                builder = builder.header(header::ORIGIN, origin);
            }
            let (parts, ()) = builder.body(()).expect("request").into_parts();
            origin_is_trusted(&parts.headers)
        };

        // No Origin (curl, scripts) and same-origin (the console itself) pass.
        assert!(origin(None, Some("127.0.0.1:8081")));
        assert!(origin(
            Some("http://127.0.0.1:8081"),
            Some("127.0.0.1:8081")
        ));
        assert!(origin(
            Some("https://gateway.internal"),
            Some("gateway.internal")
        ));
        // Desktop wrappers pass.
        assert!(origin(Some("tauri://localhost"), Some("127.0.0.1:8081")));
        assert!(origin(
            Some("https://tauri.localhost"),
            Some("127.0.0.1:8081")
        ));
        // A random web page does not, even though its peer is loopback.
        assert!(!origin(
            Some("https://evil.example.com"),
            Some("127.0.0.1:8081")
        ));
        assert!(!origin(Some("null"), Some("127.0.0.1:8081")));
        assert!(!origin(
            Some("http://127.0.0.1:9999"),
            Some("127.0.0.1:8081")
        ));
    }

    #[test]
    fn warnings_are_calm_locally_and_loud_on_the_network() {
        assert_eq!(warning_for(Mode::Protected, false), None);

        let local = warning_for(Mode::Unprotected, true).expect("warning");
        assert!(local.contains("on this machine"), "{local}");
        assert!(!local.contains("network"), "{local}");

        let public = warning_for(Mode::Unprotected, false).expect("warning");
        assert!(public.contains("reachable from the network"), "{public}");
        assert!(public.contains("Settings"), "{public}");

        assert!(warning_for(Mode::Setup, true).is_some());
        assert!(
            warning_for(Mode::Setup, false)
                .expect("warning")
                .contains("reachable from the network")
        );
    }
}
