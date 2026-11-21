//! User accounts (M4): admin CRUD and self-service password changes.
//!
//! Accounts are created by admins only (decision D2): `POST /api/users`
//! takes an email, display name, role, and initial password. A password that
//! arrives from someone else (creation or an admin reset) sets
//! `must_change_password`: the account must rotate it through
//! `POST /api/auth/change-password` before anything else works (review
//! finding 7, deferred from M1). The bootstrap admin starts flagged the same
//! way.
//!
//! Password provenance decides key revocation (decision D23): a **self**
//! change keeps the account's API keys, an **admin reset** revokes all of
//! them (plus the sessions) — an account rotated for suspected compromise
//! must not keep `/v1` access through old keys.
//!
//! PATCH follows decision D20: `null`/omitted fields stay unchanged. An
//! all-null patch is a `400`. Every mutation writes its `audit_log` row in
//! the same transaction (`user.create` / `user.update` /
//! `auth.password_change`), and mutation transactions are write-first
//! (review m3) so they cannot fail on spurious `SQLITE_BUSY_SNAPSHOT`
//! upgrades under load.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;

use axum::http::StatusCode;

use crate::auth;
use crate::catalog::{self, Actor};
use crate::password::{self, PasswordError};
use crate::setup::Mode;

/// Shortest accepted password, same policy as the bootstrap admin.
pub const MIN_PASSWORD_LEN: usize = 12;

fn ts(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format(auth::TS_FMT).to_string()
}

/// Revokes every live API key of one account and returns how many were
/// revoked (decision D23). Shared by the admin password reset, the operator
/// reset, and the first-password promotion, so the rule exists once.
///
/// Failure mode: [`sqlx::Error`] when the update fails.
pub(crate) async fn revoke_live_api_keys<'e, E>(db: E, user_id: &str) -> Result<u64, sqlx::Error>
where
    E: sqlx::SqliteExecutor<'e>,
{
    Ok(sqlx::query(
        "UPDATE api_keys SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE user_id = ? AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(db)
    .await?
    .rows_affected())
}

/// One account as the API returns it. Never carries the password hash.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct UserView {
    pub id: String,
    pub email: String,
    pub display_name: String,
    /// `admin` or `user`.
    pub role: String,
    pub is_active: bool,
    /// When true the account must change its password before using the API
    /// or the UI (forced rotation).
    pub must_change_password: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UserInput {
    /// Sign-in address (case-insensitive, unique).
    pub email: String,
    pub display_name: String,
    /// `admin` or `user`.
    pub role: String,
    /// Initial password (at least [`MIN_PASSWORD_LEN`] characters).
    pub password: String,
    /// Defaults to `true`: an initial password set by an admin must be
    /// rotated by the account before it can be used for anything else.
    pub must_change_password: Option<bool>,
}

/// PATCH semantics (decision D20): `null`/omitted fields stay unchanged. A
/// `password` resets the account's password and — unless the patch says
/// otherwise — flags the account for forced rotation.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UserPatch {
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub is_active: Option<bool>,
    pub password: Option<String>,
    pub must_change_password: Option<bool>,
}

/// The body of `POST /api/auth/change-password`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PasswordChange {
    /// The account's current password.
    pub current_password: String,
    /// The new password (at least [`MIN_PASSWORD_LEN`] characters).
    pub new_password: String,
}

#[derive(Debug, thiserror::Error)]
pub enum UserError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit write failed: {0}")]
    Audit(#[from] catalog::CatalogError),
    #[error("{0}")]
    Invalid(String),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("an account with this email already exists")]
    EmailTaken,
    #[error("the current password does not match")]
    InvalidCredentials,
    #[error("the last active administrator cannot be deactivated or demoted")]
    LastAdmin,
    #[error("the local operator account is managed in Settings, not in the user list")]
    LocalOperator,
    #[error("accounts cannot be created while the gateway is unprotected")]
    SetupIncomplete,
    #[error("password hashing failed: {0}")]
    Password(#[from] PasswordError),
}

impl UserError {
    /// Maps the domain error onto the uniform API error shape.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        use crate::error::ApiError;
        match self {
            UserError::Invalid(message) => ApiError::bad_request(message).with_request_id(rid),
            UserError::NotFound(what) => ApiError::not_found(what).with_request_id(rid),
            UserError::EmailTaken => {
                ApiError::conflict("an account with this email already exists").with_request_id(rid)
            }
            UserError::InvalidCredentials => ApiError::invalid_credentials().with_request_id(rid),
            UserError::LastAdmin => ApiError::custom(
                StatusCode::CONFLICT,
                "last_active_admin",
                "this is the last active administrator; promote or activate another administrator first",
            )
            .with_request_id(rid),
            UserError::LocalOperator => ApiError::custom(
                StatusCode::CONFLICT,
                "local_operator",
                "the local operator account is managed in Settings, not in the user list",
            )
            .with_request_id(rid),
            UserError::SetupIncomplete => ApiError::custom(
                StatusCode::CONFLICT,
                "setup_incomplete",
                "set a password for the local administrator in Settings before creating accounts",
            )
            .with_request_id(rid),
            other => ApiError::internal("users", other).with_request_id(rid),
        }
    }
}

pub(crate) fn validate_email(raw: &str) -> Result<String, UserError> {
    let email = raw.trim();
    let (local, domain) = email
        .split_once('@')
        .ok_or_else(|| UserError::Invalid("email must look like name@example.com".to_string()))?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') || email.len() > 254 {
        return Err(UserError::Invalid(
            "email must look like name@example.com".to_string(),
        ));
    }
    Ok(email.to_string())
}

pub(crate) fn validate_password(raw: &str) -> Result<(), UserError> {
    if raw.chars().count() < MIN_PASSWORD_LEN {
        return Err(UserError::Invalid(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

fn validate_role(raw: &str) -> Result<auth::Role, UserError> {
    auth::Role::parse(raw)
        .ok_or_else(|| UserError::Invalid("role must be `admin` or `user`".to_string()))
}

const SELECT_USERS: &str = "SELECT id, email, display_name, role, is_active, \
     must_change_password, created_at, updated_at, last_login_at FROM users";

/// Lists all managed accounts, oldest first. The local-operator placeholder is
/// not a managed account: it appears as an ordinary row to the console
/// otherwise, and PATCH refuses it (D57).
pub async fn list_users(pool: &SqlitePool) -> Result<Vec<UserView>, UserError> {
    Ok(sqlx::query_as::<_, UserView>(&format!(
        "{SELECT_USERS} WHERE is_placeholder = 0 ORDER BY created_at, id"
    ))
    .fetch_all(pool)
    .await?)
}

/// Fetches one account by id.
pub async fn get_user(pool: &SqlitePool, user_id: &str) -> Result<UserView, UserError> {
    sqlx::query_as::<_, UserView>(&format!("{SELECT_USERS} WHERE id = ?"))
        .bind(user_id)
        .fetch_optional(pool)
        .await?
        .ok_or(UserError::NotFound("user"))
}

/// Creates an account, audited as `user.create`.
///
/// While the gateway is `unprotected` (D58) account creation is refused with
/// [`UserError::SetupIncomplete`]: without a password, one real account would
/// make the next restart `protected` with no administrator. The rule lives
/// here, not in the route, so every caller inherits it. The caller passes the
/// live `state.setup.mode()`; the service never reads the mode from the
/// database, because the in-memory mode is what closes the D58 race.
///
/// Failure mode: [`UserError::Invalid`] on bad input (email, role, short
/// password), [`UserError::EmailTaken`] on a duplicate email,
/// [`UserError::SetupIncomplete`] before the first password exists.
pub async fn create_user(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    input: &UserInput,
    mode: Mode,
) -> Result<UserView, UserError> {
    let email = validate_email(&input.email)?;
    let role = validate_role(&input.role)?;
    validate_password(&input.password)?;
    let display_name = input.display_name.trim();
    if display_name.is_empty() {
        return Err(UserError::Invalid(
            "display_name must not be empty".to_string(),
        ));
    }
    // Validate before the mode gate so a malformed body is a `400` in every
    // mode, as `utoipa` documents; nothing is written or hashed either way.
    if mode == Mode::Unprotected {
        return Err(UserError::SetupIncomplete);
    }
    // An admin-set initial password must be rotated unless the admin says
    // otherwise explicitly.
    let must_change = input.must_change_password.unwrap_or(true);
    let password_hash = password::hash_password_offload(input.password.clone()).await?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = ts(chrono::Utc::now());
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO users (id, email, display_name, password_hash, role, is_active, \
                            must_change_password, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 1, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&email)
    .bind(display_name)
    .bind(&password_hash)
    .bind(role.as_str())
    .bind(i64::from(must_change))
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await;
    if let Err(sqlx::Error::Database(ref db)) = result
        && db.is_unique_violation()
    {
        return Err(UserError::EmailTaken);
    }
    result?;
    let detail = serde_json::json!({
        "email": email,
        "role": role.as_str(),
        "must_change_password": must_change,
    });
    catalog::audit(&mut tx, actor, "user.create", "user", &id, &detail).await?;
    tx.commit().await?;
    get_user(pool, &id).await
}

/// Updates an account (decision D20: `null` keeps the current value), audited
/// as `user.update`. A `password` reset also flags forced rotation unless the
/// patch states `must_change_password` explicitly, and — as an **admin
/// reset** — revokes the account's API keys and sessions (decision D23). The
/// password hash itself is never echoed into the audit detail. The `UPDATE`
/// is the transaction's first statement and doubles as the existence check
/// (write-first, review m3); its `WHERE` clause also refuses to remove the
/// last active administrator (decision D57), so the invariant holds even for
/// concurrent patches.
///
/// Failure mode: [`UserError::Invalid`] on an all-null patch or bad values,
/// [`UserError::NotFound`] for an unknown account, [`UserError::LocalOperator`]
/// for the local-operator placeholder, [`UserError::LastAdmin`] when the patch
/// would deactivate or demote the last active administrator. Validation runs
/// before the transaction in every mode, so an invalid patch is a `400` even
/// when the row rules would refuse it; only valid input reaches the `404`/`409`
/// checks, which stay inside the write.
pub async fn update_user(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    user_id: &str,
    patch: &UserPatch,
) -> Result<UserView, UserError> {
    let display_name = patch.display_name.as_deref().map(str::trim);
    if display_name == Some("") {
        return Err(UserError::Invalid(
            "display_name must not be empty".to_string(),
        ));
    }
    let role = patch.role.as_deref().map(validate_role).transpose()?;
    let password_hash = patch
        .password
        .as_deref()
        .map(validate_password)
        .transpose()?;
    // Hashed before the transaction, which must stay write-first (review m3):
    // a patch the D57 guard later refuses with `409` still pays one hash. The
    // alternative — a read before the `UPDATE` — is the snapshot upgrade that
    // order exists to avoid.
    let password_hash = match (&patch.password, password_hash) {
        (Some(password), _) => Some(password::hash_password_offload(password.clone()).await?),
        (None, _) => None,
    };
    let must_change = match (&patch.password, patch.must_change_password) {
        (Some(_), explicit) => Some(explicit.unwrap_or(true)),
        (None, explicit) => explicit,
    };
    if display_name.is_none()
        && role.is_none()
        && patch.is_active.is_none()
        && password_hash.is_none()
        && must_change.is_none()
    {
        return Err(UserError::Invalid(
            "at least one field must be provided".to_string(),
        ));
    }

    let mut tx = pool.begin().await?;
    let now = ts(chrono::Utc::now());
    let role_value = role.map(|role| role.as_str());
    let active_value = patch.is_active.map(i64::from);
    // Write-first (review m3): the UPDATE takes the write lock up front and
    // its `RETURNING id` doubles as the existence check. The extra `WHERE`
    // terms enforce D57 in the same statement (no read-then-write race): the
    // patch is allowed when it keeps this account an active administrator,
    // when another active real administrator remains, or when this account was
    // not one to begin with. The local-operator placeholder is excluded
    // entirely: it is not a managed account, and editing its role or status
    // before the first password would let the promotion create a non-admin.
    let updated: Option<(String,)> = sqlx::query_as(
        "UPDATE users SET \
           display_name = COALESCE(?, display_name), \
           role = COALESCE(?, role), \
           is_active = COALESCE(?, is_active), \
           password_hash = COALESCE(?, password_hash), \
           must_change_password = COALESCE(?, must_change_password), \
           updated_at = ? \
         WHERE id = ? AND is_placeholder = 0 \
           AND ( \
             (COALESCE(?, role) = 'admin' AND COALESCE(?, is_active) = 1) \
             OR EXISTS ( \
               SELECT 1 FROM users AS other \
               WHERE other.id <> ? AND other.role = 'admin' AND other.is_active = 1 \
                 AND other.is_placeholder = 0 \
             ) \
             OR role <> 'admin' OR is_active <> 1 \
           ) \
         RETURNING id",
    )
    .bind(display_name)
    .bind(role_value)
    .bind(active_value)
    .bind(&password_hash)
    .bind(must_change.map(i64::from))
    .bind(&now)
    .bind(user_id)
    .bind(role_value)
    .bind(active_value)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if updated.is_none() {
        // The guard and the existence check share one statement; a miss means
        // "no such row", "the local operator", or "refused to remove the last
        // administrator".
        let row: Option<(String, i64)> =
            sqlx::query_as("SELECT id, is_placeholder FROM users WHERE id = ?")
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await?;
        return match row {
            Some((_, 1)) => Err(UserError::LocalOperator),
            Some(_) => Err(UserError::LastAdmin),
            None => Err(UserError::NotFound("user")),
        };
    }

    // A deactivated or re-passworded account loses its existing sessions.
    if patch.is_active == Some(false) || password_hash.is_some() {
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
    }
    // An admin password reset also revokes the account's API keys (decision
    // D23): a suspected compromise must not keep `/v1` access through old
    // keys. A self-service change never comes through here.
    let mut api_keys_revoked = 0u64;
    if password_hash.is_some() {
        api_keys_revoked = revoke_live_api_keys(&mut *tx, user_id).await?;
    }

    let detail = serde_json::json!({
        "display_name": display_name,
        "role": role.map(|role| role.as_str()),
        "is_active": patch.is_active,
        "password_reset": patch.password.is_some(),
        "must_change_password": must_change,
        "api_keys_revoked": api_keys_revoked,
    });
    catalog::audit(&mut tx, actor, "user.update", "user", user_id, &detail).await?;
    tx.commit().await?;
    get_user(pool, user_id).await
}

/// What [`reset_password_as_operator`] changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorResetOutcome {
    /// Email of the account that was reset.
    pub email: String,
    /// False when the account is deactivated; it still cannot sign in.
    pub is_active: bool,
    /// Sessions deleted by the reset.
    pub sessions_revoked: u64,
    /// API keys newly revoked by the reset.
    pub api_keys_revoked: u64,
}

/// Offline password reset used by `gateway reset-password` (decision D56).
///
/// This is an operator action: the caller already holds the database file, so
/// it applies the admin-reset rules (D23) — every session is deleted, every
/// live API key is revoked, and the account is flagged for forced rotation. One
/// transaction also writes the `auth.password_reset_cli` audit row with a
/// `NULL` actor, because no session identifies the operator.
///
/// Failure mode: [`UserError::Invalid`] for a too-short password,
/// [`UserError::NotFound`] for an unknown account, [`UserError::LocalOperator`]
/// for the local-operator placeholder (its password is set in Settings).
pub async fn reset_password_as_operator(
    pool: &SqlitePool,
    user_id: &str,
    new_password: &str,
) -> Result<OperatorResetOutcome, UserError> {
    validate_password(new_password)?;
    let password_hash = password::hash_password_offload(new_password.to_string()).await?;
    let now = ts(chrono::Utc::now());

    let mut tx = pool.begin().await?;
    // Write-first (review m3): the UPDATE doubles as the existence check and
    // returns the fields the audit row needs.
    let updated = sqlx::query_as::<_, (String, bool)>(
        "UPDATE users SET password_hash = ?, must_change_password = 1, updated_at = ? \
         WHERE id = ? AND is_placeholder = 0 \
         RETURNING email, is_active",
    )
    .bind(&password_hash)
    .bind(&now)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((email, is_active)) = updated else {
        // The write filters the placeholder out; name it instead of claiming
        // the account does not exist.
        let placeholder: Option<i64> =
            sqlx::query_scalar("SELECT is_placeholder FROM users WHERE id = ?")
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await?;
        return match placeholder {
            Some(1) => Err(UserError::LocalOperator),
            _ => Err(UserError::NotFound("user")),
        };
    };

    let sessions_revoked = sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let api_keys_revoked = revoke_live_api_keys(&mut *tx, user_id).await?;

    let detail = serde_json::json!({
        "source": "cli",
        "email": email,
        "sessions_revoked": sessions_revoked,
        "api_keys_revoked": api_keys_revoked,
        "must_change_password": true,
    });
    auth::audit(
        &mut *tx,
        None,
        "auth.password_reset_cli",
        Some("user"),
        Some(user_id),
        Some(&detail.to_string()),
        None,
    )
    .await
    .map_err(catalog::CatalogError::from)?;
    tx.commit().await?;

    Ok(OperatorResetOutcome {
        email,
        is_active,
        sessions_revoked,
        api_keys_revoked,
    })
}

/// Self-service password change: verifies `current_password`, sets the new
/// one, clears `must_change_password`, and rotates the account's *other*
/// sessions (the calling session survives). Audited as `auth.password_change`.
///
/// Failure mode: [`UserError::InvalidCredentials`] when the current password
/// does not match, [`UserError::Invalid`] for a too-short new password,
/// [`UserError::NotFound`] for an unknown account.
pub async fn change_password(
    pool: &SqlitePool,
    user_id: &str,
    keep_session_id: &str,
    current_password: &str,
    new_password: &str,
    ip: &str,
) -> Result<(), UserError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT id, password_hash FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    let Some((id, password_hash)) = row else {
        return Err(UserError::NotFound("user"));
    };
    if !password::verify_password_offload(current_password.to_string(), password_hash.clone())
        .await?
    {
        return Err(UserError::InvalidCredentials);
    }
    if current_password == new_password {
        return Err(UserError::Invalid(
            "the new password must differ from the current one".to_string(),
        ));
    }
    validate_password(new_password)?;
    let new_hash = password::hash_password_offload(new_password.to_string()).await?;

    let now = ts(chrono::Utc::now());
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE users SET password_hash = ?, must_change_password = 0, updated_at = ? WHERE id = ?",
    )
    .bind(&new_hash)
    .bind(&now)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND id <> ?")
        .bind(&id)
        .bind(keep_session_id)
        .execute(&mut *tx)
        .await?;
    let actor = Actor { user_id: &id, ip };
    catalog::audit(
        &mut tx,
        &actor,
        "auth.password_change",
        "user",
        &id,
        &serde_json::json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor<'a>(user_id: &'a str) -> Actor<'a> {
        Actor {
            user_id,
            ip: "127.0.0.1",
        }
    }

    async fn pool() -> (std::path::PathBuf, SqlitePool) {
        let dir = std::env::temp_dir().join(format!("gateway-users-{}", uuid::Uuid::new_v4()));
        let pool = crate::db::open(&dir.join("gateway.db"))
            .await
            .expect("open");
        (dir, pool)
    }

    fn input(email: &str) -> UserInput {
        UserInput {
            email: email.to_string(),
            display_name: "Test User".to_string(),
            role: "user".to_string(),
            password: "initial-password-1".to_string(),
            must_change_password: None,
        }
    }

    /// Calls the service as a protected gateway; the mode gate has its own
    /// test below.
    async fn create(
        pool: &SqlitePool,
        actor: &Actor<'_>,
        input: &UserInput,
    ) -> Result<UserView, UserError> {
        create_user(pool, actor, input, Mode::Protected).await
    }

    #[tokio::test]
    async fn created_users_are_flagged_for_rotation_and_audited() {
        password::set_test_cost(8 * 1024, 1, 1);
        let (dir, pool) = pool().await;
        let admin = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active, must_change_password) \
             VALUES (?, 'admin@example.com', 'Admin', 'x', 'admin', 1, 1)",
        )
        .bind(&admin)
        .execute(&pool)
        .await
        .expect("admin");

        let created = create(&pool, &actor(&admin), &input("new@example.com"))
            .await
            .expect("create");
        assert_eq!(created.role, "user");
        assert!(
            created.must_change_password,
            "admin-set passwords must rotate"
        );
        assert!(created.is_active);

        // The hash is stored and the plaintext never is.
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
            .bind(&created.id)
            .fetch_one(&pool)
            .await
            .expect("hash");
        assert!(hash.starts_with("$argon2id$"));
        assert!(password::verify_password("initial-password-1", &hash).expect("verify"));

        // Explicitly waiving rotation is honoured.
        let waived = UserInput {
            email: "waived@example.com".to_string(),
            must_change_password: Some(false),
            ..input("waived@example.com")
        };
        let waived = create(&pool, &actor(&admin), &waived)
            .await
            .expect("create");
        assert!(!waived.must_change_password);

        // Duplicate emails conflict; bad inputs are refused.
        let err = create(&pool, &actor(&admin), &input("new@example.com"))
            .await
            .expect_err("duplicate");
        assert!(matches!(err, UserError::EmailTaken), "{err:?}");
        assert!(
            create(&pool, &actor(&admin), &input("not-an-email"))
                .await
                .is_err()
        );
        let short = UserInput {
            password: "short".to_string(),
            ..input("short@example.com")
        };
        assert!(create(&pool, &actor(&admin), &short).await.is_err());
        let bad_role = UserInput {
            role: "root".to_string(),
            ..input("role@example.com")
        };
        assert!(create(&pool, &actor(&admin), &bad_role).await.is_err());

        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("audit");
        assert_eq!(actions, ["user.create", "user.create"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn unprotected_mode_refuses_creation_before_any_write() {
        let (dir, pool) = pool().await;

        let refused = create_user(
            &pool,
            &actor("local"),
            &input("new@example.com"),
            Mode::Unprotected,
        )
        .await
        .expect_err("unprotected mode refuses creation");
        assert!(matches!(refused, UserError::SetupIncomplete), "{refused:?}");

        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .expect("users");
        let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .expect("audit");
        assert_eq!((users, audits), (0, 0), "no row and no audit was written");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn patch_resets_passwords_flags_rotation_and_deactivates() {
        password::set_test_cost(8 * 1024, 1, 1);
        let (dir, pool) = pool().await;
        let admin = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active, must_change_password) \
             VALUES (?, 'admin@example.com', 'Admin', 'x', 'admin', 1, 0)",
        )
        .bind(&admin)
        .execute(&pool)
        .await
        .expect("admin");
        let user = create(&pool, &actor(&admin), &input("u@example.com"))
            .await
            .expect("create");

        // A password reset forces rotation and evicts sessions.
        sqlx::query(
            "INSERT INTO sessions (id, token_hash, user_id, csrf_token, expires_at) \
             VALUES ('s1', 'h1', ?, 'c', '2999-01-01T00:00:00.000Z')",
        )
        .bind(&user.id)
        .execute(&pool)
        .await
        .expect("session");
        let patched = update_user(
            &pool,
            &actor(&admin),
            &user.id,
            &UserPatch {
                password: Some("reset-password-1".to_string()),
                ..UserPatch::default()
            },
        )
        .await
        .expect("patch");
        assert!(patched.must_change_password, "a reset forces rotation");
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = ?")
            .bind(&user.id)
            .fetch_one(&pool)
            .await
            .expect("sessions");
        assert_eq!(sessions, 0, "the old sessions are gone");

        // A null field keeps its value; an all-null patch is invalid.
        let patched = update_user(
            &pool,
            &actor(&admin),
            &user.id,
            &UserPatch {
                display_name: Some("Renamed".to_string()),
                ..UserPatch::default()
            },
        )
        .await
        .expect("patch");
        assert_eq!(patched.display_name, "Renamed");
        assert_eq!(patched.role, "user");
        assert!(patched.must_change_password, "null keeps the flag");
        assert!(
            update_user(&pool, &actor(&admin), &user.id, &UserPatch::default())
                .await
                .is_err(),
            "an all-null patch is a 400"
        );

        // Deactivation flips the flag and audits.
        let patched = update_user(
            &pool,
            &actor(&admin),
            &user.id,
            &UserPatch {
                is_active: Some(false),
                ..UserPatch::default()
            },
        )
        .await
        .expect("patch");
        assert!(!patched.is_active);
        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE action LIKE 'user.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .expect("audit");
        assert_eq!(
            actions,
            ["user.create", "user.update", "user.update", "user.update"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn the_last_active_administrator_cannot_be_demoted_or_deactivated() {
        password::set_test_cost(8 * 1024, 1, 1);
        let (dir, pool) = pool().await;
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        for (id, email) in [
            (&first, "first@example.com"),
            (&second, "second@example.com"),
        ] {
            sqlx::query(
                "INSERT INTO users (id, email, display_name, password_hash, role, is_active) \
                 VALUES (?, ?, ?, 'x', 'admin', 1)",
            )
            .bind(id)
            .bind(email)
            .bind(email)
            .execute(&pool)
            .await
            .expect("admin");
        }

        // With another active administrator around, deactivation is allowed.
        let deactivated = update_user(
            &pool,
            &actor(&first),
            &second,
            &UserPatch {
                is_active: Some(false),
                ..UserPatch::default()
            },
        )
        .await
        .expect("deactivate one of two");
        assert!(!deactivated.is_active);

        // The remaining one is the last: neither deactivation nor demotion is.
        let refused = update_user(
            &pool,
            &actor(&first),
            &first,
            &UserPatch {
                is_active: Some(false),
                ..UserPatch::default()
            },
        )
        .await
        .expect_err("last admin stays active");
        assert!(matches!(refused, UserError::LastAdmin), "{refused:?}");
        let demoted = update_user(
            &pool,
            &actor(&first),
            &first,
            &UserPatch {
                role: Some("user".to_string()),
                ..UserPatch::default()
            },
        )
        .await
        .expect_err("last admin keeps the role");
        assert!(matches!(demoted, UserError::LastAdmin), "{demoted:?}");
        let (role, active): (String, i64) =
            sqlx::query_as("SELECT role, is_active FROM users WHERE id = ?")
                .bind(&first)
                .fetch_one(&pool)
                .await
                .expect("row");
        assert_eq!((role.as_str(), active), ("admin", 1), "refusals roll back");

        // A password reset does not remove admin access, so it still works.
        update_user(
            &pool,
            &actor(&first),
            &first,
            &UserPatch {
                password: Some("new-password-1".to_string()),
                ..UserPatch::default()
            },
        )
        .await
        .expect("password reset");

        // Reactivating the other administrator makes demotion possible again.
        update_user(
            &pool,
            &actor(&first),
            &second,
            &UserPatch {
                is_active: Some(true),
                ..UserPatch::default()
            },
        )
        .await
        .expect("reactivate");
        update_user(
            &pool,
            &actor(&first),
            &first,
            &UserPatch {
                role: Some("user".to_string()),
                ..UserPatch::default()
            },
        )
        .await
        .expect("demote with another admin");

        // An account that is already not an active administrator can still be
        // edited in a database that has none (the guard only removes access).
        update_user(
            &pool,
            &actor(&second),
            &first,
            &UserPatch {
                display_name: Some("Renamed".to_string()),
                ..UserPatch::default()
            },
        )
        .await
        .expect("non-admin update");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn change_password_requires_the_current_one_and_clears_the_flag() {
        password::set_test_cost(8 * 1024, 1, 1);
        let (dir, pool) = pool().await;
        let admin = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active, must_change_password) \
             VALUES (?, 'admin@example.com', 'Admin', 'x', 'admin', 1, 1)",
        )
        .bind(&admin)
        .execute(&pool)
        .await
        .expect("admin");
        let user = create(&pool, &actor(&admin), &input("u@example.com"))
            .await
            .expect("create");

        // A wrong current password never changes anything.
        let err = change_password(
            &pool,
            &user.id,
            "keep-me",
            "wrong-password-1",
            "brand-new-password",
            "127.0.0.1",
        )
        .await
        .expect_err("wrong current");
        assert!(matches!(err, UserError::InvalidCredentials), "{err:?}");

        // The new password must be long enough and different.
        assert!(
            change_password(
                &pool,
                &user.id,
                "keep-me",
                "initial-password-1",
                "short",
                "127.0.0.1"
            )
            .await
            .is_err()
        );
        assert!(
            change_password(
                &pool,
                &user.id,
                "keep-me",
                "initial-password-1",
                "initial-password-1",
                "127.0.0.1",
            )
            .await
            .is_err(),
            "reusing the current password is refused"
        );

        // The rotation clears the flag and keeps only the calling session.
        sqlx::query(
            "INSERT INTO sessions (id, token_hash, user_id, csrf_token, expires_at) \
             VALUES ('keep-me', 'h1', ?, 'c', '2999-01-01T00:00:00.000Z')",
        )
        .bind(&user.id)
        .execute(&pool)
        .await
        .expect("session");
        sqlx::query(
            "INSERT INTO sessions (id, token_hash, user_id, csrf_token, expires_at) \
             VALUES ('other', 'h2', ?, 'c', '2999-01-01T00:00:00.000Z')",
        )
        .bind(&user.id)
        .execute(&pool)
        .await
        .expect("session");
        change_password(
            &pool,
            &user.id,
            "keep-me",
            "initial-password-1",
            "brand-new-password",
            "127.0.0.1",
        )
        .await
        .expect("change");

        let updated = get_user(&pool, &user.id).await.expect("user");
        assert!(!updated.must_change_password, "rotation clears the flag");
        let sessions: Vec<String> = sqlx::query_scalar("SELECT id FROM sessions WHERE user_id = ?")
            .bind(&user.id)
            .fetch_all(&pool)
            .await
            .expect("sessions");
        assert_eq!(sessions, ["keep-me"]);
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
            .bind(&user.id)
            .fetch_one(&pool)
            .await
            .expect("hash");
        assert!(password::verify_password("brand-new-password", &hash).expect("verify"));

        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE action = 'auth.password_change'",
        )
        .fetch_all(&pool)
        .await
        .expect("audit");
        assert_eq!(actions.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn admin_password_reset_revokes_api_keys_but_self_change_keeps_them() {
        password::set_test_cost(8 * 1024, 1, 1);
        let (dir, pool) = pool().await;
        let admin = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active, must_change_password) \
             VALUES (?, 'admin@example.com', 'Admin', 'x', 'admin', 1, 0)",
        )
        .bind(&admin)
        .execute(&pool)
        .await
        .expect("admin");
        let user = create(&pool, &actor(&admin), &input("u@example.com"))
            .await
            .expect("create");
        let created = crate::api_keys::create_key(
            &pool,
            &actor(&admin),
            &user.id,
            &crate::api_keys::ApiKeyInput {
                name: "sdk".to_string(),
                expires_at: None,
                scopes: None,
                rate_limit_rpm: None,
                rate_limit_tpm: None,
            },
        )
        .await
        .expect("key");

        // A self-service change keeps the API keys (decision D23).
        change_password(
            &pool,
            &user.id,
            "keep-me",
            "initial-password-1",
            "brand-new-password",
            "127.0.0.1",
        )
        .await
        .expect("self change");
        assert!(
            crate::api_keys::authenticate(
                &pool,
                &created.key,
                &crate::api_keys::KeyUsageStamps::new()
            )
            .await
            .is_ok(),
            "a self change keeps API keys"
        );

        // An admin reset revokes them: an account rotated for suspected
        // compromise must not keep `/v1` access through old keys.
        update_user(
            &pool,
            &actor(&admin),
            &user.id,
            &UserPatch {
                password: Some("reset-password-1".to_string()),
                ..UserPatch::default()
            },
        )
        .await
        .expect("reset");
        let rejected = crate::api_keys::authenticate(
            &pool,
            &created.key,
            &crate::api_keys::KeyUsageStamps::new(),
        )
        .await
        .expect_err("revoked by the reset");
        assert!(
            matches!(rejected, crate::api_keys::KeyRejection::Revoked),
            "{rejected:?}"
        );

        // The audit records how many keys the reset revoked.
        let detail: String =
            sqlx::query_scalar("SELECT detail_json FROM audit_log WHERE action = 'user.update'")
                .fetch_one(&pool)
                .await
                .expect("audit detail");
        assert!(
            detail.contains("\"api_keys_revoked\":1"),
            "the reset's audit names the revocation: {detail}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn operator_reset_revokes_keys_and_sessions_and_audits_without_an_actor() {
        password::set_test_cost(8 * 1024, 1, 1);
        let (dir, pool) = pool().await;
        let user_id = uuid::Uuid::new_v4().to_string();
        let old_hash = password::hash_password("old-password-1").expect("hash");
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active) \
             VALUES (?, 'admin@example.com', 'Admin', ?, 'admin', 1)",
        )
        .bind(&user_id)
        .bind(&old_hash)
        .execute(&pool)
        .await
        .expect("user");
        sqlx::query(
            "INSERT INTO sessions (id, token_hash, user_id, csrf_token, expires_at) \
             VALUES ('s1', 'h1', ?, 'c', '2999-01-01T00:00:00.000Z')",
        )
        .bind(&user_id)
        .execute(&pool)
        .await
        .expect("session");
        sqlx::query(
            "INSERT INTO api_keys (id, user_id, name, prefix, key_hash, created_at, updated_at) \
             VALUES ('k1', ?, 'key', 'pk_live_ab12', 'hash', '2026-01-01T00:00:00.000Z', \
                     '2026-01-01T00:00:00.000Z')",
        )
        .bind(&user_id)
        .execute(&pool)
        .await
        .expect("api key");

        let outcome = reset_password_as_operator(&pool, &user_id, "new-password-1")
            .await
            .expect("reset");
        assert_eq!(outcome.email, "admin@example.com");
        assert!(outcome.is_active, "a reset never deactivates");
        assert_eq!(outcome.sessions_revoked, 1);
        assert_eq!(outcome.api_keys_revoked, 1);

        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
            .bind(&user_id)
            .fetch_one(&pool)
            .await
            .expect("hash");
        assert!(password::verify_password("new-password-1", &hash).expect("verify"));
        assert!(!password::verify_password("old-password-1", &hash).expect("verify"));

        let flagged: i64 =
            sqlx::query_scalar("SELECT must_change_password FROM users WHERE id = ?")
                .bind(&user_id)
                .fetch_one(&pool)
                .await
                .expect("rotation flag");
        assert_eq!(flagged, 1, "the operator's password must rotate");

        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = ?")
            .bind(&user_id)
            .fetch_one(&pool)
            .await
            .expect("sessions");
        assert_eq!(sessions, 0);
        let revoked: Option<String> =
            sqlx::query_scalar("SELECT revoked_at FROM api_keys WHERE id = 'k1'")
                .fetch_one(&pool)
                .await
                .expect("key");
        assert!(revoked.is_some(), "the reset revokes live API keys");

        let audit: Option<(Option<String>, String)> = sqlx::query_as(
            "SELECT actor_user_id, action FROM audit_log WHERE action = 'auth.password_reset_cli'",
        )
        .fetch_optional(&pool)
        .await
        .expect("audit");
        let (actor, action) = audit.expect("audit row");
        assert!(actor.is_none(), "no session identifies the operator");
        assert_eq!(action, "auth.password_reset_cli");

        // Bad input is refused before the transaction and writes no audit.
        let short = reset_password_as_operator(&pool, &user_id, "short")
            .await
            .expect_err("too short");
        assert!(matches!(short, UserError::Invalid(_)), "{short:?}");
        let unknown = reset_password_as_operator(&pool, "missing", "new-password-1")
            .await
            .expect_err("unknown account");
        assert!(
            matches!(unknown, UserError::NotFound("user")),
            "{unknown:?}"
        );
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'auth.password_reset_cli'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert_eq!(rows, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn operator_reset_refuses_the_local_placeholder() {
        password::set_test_cost(8 * 1024, 1, 1);
        let (dir, pool) = pool().await;
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active, is_placeholder) \
             VALUES ('local', 'local@example.invalid', 'Local', 'x', 'admin', 1, 1)",
        )
        .execute(&pool)
        .await
        .expect("placeholder");

        let refused = reset_password_as_operator(&pool, "local", "new-password-1")
            .await
            .expect_err("placeholder");
        assert!(matches!(refused, UserError::LocalOperator), "{refused:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
