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

use crate::auth;
use crate::catalog::{self, Actor};
use crate::password::{self, PasswordError};

/// Shortest accepted password, same policy as the bootstrap admin.
pub const MIN_PASSWORD_LEN: usize = 12;

fn ts(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format(auth::TS_FMT).to_string()
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
            other => ApiError::internal("users", other).with_request_id(rid),
        }
    }
}

fn validate_email(raw: &str) -> Result<String, UserError> {
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

fn validate_password(raw: &str) -> Result<(), UserError> {
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

/// Lists all accounts, oldest first.
pub async fn list_users(pool: &SqlitePool) -> Result<Vec<UserView>, UserError> {
    Ok(
        sqlx::query_as::<_, UserView>(&format!("{SELECT_USERS} ORDER BY created_at, id"))
            .fetch_all(pool)
            .await?,
    )
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
/// Failure mode: [`UserError::Invalid`] on bad input (email, role, short
/// password), [`UserError::EmailTaken`] on a duplicate email.
pub async fn create_user(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    input: &UserInput,
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
    // An admin-set initial password must be rotated unless the admin says
    // otherwise explicitly.
    let must_change = input.must_change_password.unwrap_or(true);
    let password_hash = password::hash_password(&input.password)?;

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
/// (write-first, review m3).
///
/// Failure mode: [`UserError::Invalid`] on an all-null patch or bad values,
/// [`UserError::NotFound`] for an unknown account.
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
    let password_hash = match (&patch.password, password_hash) {
        (Some(password), _) => Some(password::hash_password(password)?),
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
    // Write-first (review m3): the UPDATE takes the write lock up front and
    // its `RETURNING id` doubles as the existence check.
    let updated: Option<(String,)> = sqlx::query_as(
        "UPDATE users SET \
           display_name = COALESCE(?, display_name), \
           role = COALESCE(?, role), \
           is_active = COALESCE(?, is_active), \
           password_hash = COALESCE(?, password_hash), \
           must_change_password = COALESCE(?, must_change_password), \
           updated_at = ? \
         WHERE id = ? \
         RETURNING id",
    )
    .bind(display_name)
    .bind(role.map(|role| role.as_str()))
    .bind(patch.is_active.map(i64::from))
    .bind(&password_hash)
    .bind(must_change.map(i64::from))
    .bind(&now)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if updated.is_none() {
        return Err(UserError::NotFound("user"));
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
        api_keys_revoked = sqlx::query(
            "UPDATE api_keys SET revoked_at = COALESCE(revoked_at, ?), updated_at = ? \
             WHERE user_id = ? AND revoked_at IS NULL",
        )
        .bind(&now)
        .bind(&now)
        .bind(user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
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
    if !password::verify_password(current_password, &password_hash)? {
        return Err(UserError::InvalidCredentials);
    }
    if current_password == new_password {
        return Err(UserError::Invalid(
            "the new password must differ from the current one".to_string(),
        ));
    }
    validate_password(new_password)?;
    let new_hash = password::hash_password(new_password)?;

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

    #[tokio::test]
    async fn created_users_are_flagged_for_rotation_and_audited() {
        password::set_test_iterations(1_000);
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

        let created = create_user(&pool, &actor(&admin), &input("new@example.com"))
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
        assert!(hash.starts_with("pbkdf2$"));
        assert!(password::verify_password("initial-password-1", &hash).expect("verify"));

        // Explicitly waiving rotation is honoured.
        let waived = UserInput {
            email: "waived@example.com".to_string(),
            must_change_password: Some(false),
            ..input("waived@example.com")
        };
        let waived = create_user(&pool, &actor(&admin), &waived)
            .await
            .expect("create");
        assert!(!waived.must_change_password);

        // Duplicate emails conflict; bad inputs are refused.
        let err = create_user(&pool, &actor(&admin), &input("new@example.com"))
            .await
            .expect_err("duplicate");
        assert!(matches!(err, UserError::EmailTaken), "{err:?}");
        assert!(
            create_user(&pool, &actor(&admin), &input("not-an-email"))
                .await
                .is_err()
        );
        let short = UserInput {
            password: "short".to_string(),
            ..input("short@example.com")
        };
        assert!(create_user(&pool, &actor(&admin), &short).await.is_err());
        let bad_role = UserInput {
            role: "root".to_string(),
            ..input("role@example.com")
        };
        assert!(create_user(&pool, &actor(&admin), &bad_role).await.is_err());

        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("audit");
        assert_eq!(actions, ["user.create", "user.create"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn patch_resets_passwords_flags_rotation_and_deactivates() {
        password::set_test_iterations(1_000);
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
        let user = create_user(&pool, &actor(&admin), &input("u@example.com"))
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
    async fn change_password_requires_the_current_one_and_clears_the_flag() {
        password::set_test_iterations(1_000);
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
        let user = create_user(&pool, &actor(&admin), &input("u@example.com"))
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
        password::set_test_iterations(1_000);
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
        let user = create_user(&pool, &actor(&admin), &input("u@example.com"))
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
}
