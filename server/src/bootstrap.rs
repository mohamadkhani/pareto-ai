//! First-run bootstrap: the optional non-interactive admin shortcut.
//!
//! With no users, `GATEWAY_ADMIN_EMAIL` **and** `GATEWAY_ADMIN_PASSWORD` still
//! create the first admin at startup (for container/CI provisioning) and the
//! account starts flagged `must_change_password`. When they are absent the
//! gateway no longer fails: it starts in **setup** mode and the wizard creates
//! the admin instead (`crate::setup`). Half a configuration (one variable set,
//! a blank email, a too-short password) is an operator error and fails startup
//! with an actionable message.
//!
//! The password is never logged and never stored in any long-lived struct: it
//! is read from the environment, hashed, and dropped.

use std::env;

use anyhow::{Context, Result, bail};
use sqlx::SqlitePool;

use crate::auth;
use crate::password;

/// What the startup bootstrap did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapOutcome {
    /// The environment shortcut just created the first admin.
    AdminCreated,
    /// Accounts already exist; nothing to do.
    UsersExist,
    /// No users and no environment shortcut: the setup wizard takes over.
    SetupRequired,
}

impl BootstrapOutcome {
    /// True when the gateway still needs the first-run setup wizard.
    pub fn setup_required(self) -> bool {
        self == Self::SetupRequired
    }
}

const MIN_ADMIN_PASSWORD_LEN: usize = 12;

/// Runs the environment-driven bootstrap (see the module docs).
///
/// Failure mode: an error naming the offending `GATEWAY_ADMIN_*` variable when
/// the shortcut is half configured or its password is too short.
pub async fn ensure_admin(pool: &SqlitePool) -> Result<BootstrapOutcome> {
    let email = env::var("GATEWAY_ADMIN_EMAIL").ok();
    let password = env::var("GATEWAY_ADMIN_PASSWORD").ok();
    ensure_admin_with(pool, email, password).await
}

/// [`ensure_admin`] with the credentials passed in; separated for testing.
pub async fn ensure_admin_with(
    pool: &SqlitePool,
    email: Option<String>,
    admin_password: Option<String>,
) -> Result<BootstrapOutcome> {
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_placeholder = 0")
        .fetch_one(pool)
        .await
        .context("counting users")?;
    if users > 0 {
        return Ok(BootstrapOutcome::UsersExist);
    }

    let email = match email {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => {
            if admin_password.is_some_and(|value| !value.is_empty()) {
                bail!(
                    "GATEWAY_ADMIN_PASSWORD is set but GATEWAY_ADMIN_EMAIL is not; \
                     set both to create the first admin non-interactively, or neither \
                     to use the setup wizard"
                );
            }
            return Ok(BootstrapOutcome::SetupRequired);
        }
    };
    let admin_password = match admin_password {
        Some(value) if value.len() >= MIN_ADMIN_PASSWORD_LEN => value,
        Some(_) => bail!(
            "GATEWAY_ADMIN_PASSWORD must be at least {MIN_ADMIN_PASSWORD_LEN} characters; \
             pick a longer password and restart"
        ),
        None => bail!(
            "GATEWAY_ADMIN_EMAIL is set but GATEWAY_ADMIN_PASSWORD is not; \
             set both to create the first admin non-interactively, or neither \
             to use the setup wizard"
        ),
    };

    let user_id = uuid::Uuid::new_v4().to_string();
    let password_hash =
        password::hash_password(&admin_password).context("hashing the bootstrap admin password")?;

    let mut tx = pool
        .begin()
        .await
        .context("starting the bootstrap transaction")?;
    // The local-operator placeholder is promoted in place so every existing
    // audit and `*_updated_by` reference to it stays valid. Role and status are
    // forced: the placeholder may have been edited before a password existed,
    // and the bootstrap admin must always be an active administrator.
    let promoted = sqlx::query_as::<_, (String,)>(
        "UPDATE users SET email = ?, display_name = ?, password_hash = ?, \
           role = 'admin', is_active = 1, \
           must_change_password = 1, is_placeholder = 0, \
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = ? AND is_placeholder = 1 \
         RETURNING id",
    )
    .bind(&email)
    .bind(&email)
    .bind(&password_hash)
    .bind(auth::LOCAL_OPERATOR_ID)
    .fetch_optional(&mut *tx)
    .await
    .context("promoting the local operator account")?;
    let (user_id, promoted) = match promoted {
        Some((id,)) => (id, true),
        None => {
            sqlx::query(
                "INSERT INTO users (id, email, display_name, password_hash, role, is_active, must_change_password) \
                 VALUES (?, ?, ?, ?, 'admin', 1, 1)",
            )
            .bind(&user_id)
            .bind(&email)
            .bind(&email)
            .bind(&password_hash)
            .execute(&mut *tx)
            .await
            .context("creating the bootstrap admin user")?;
            (user_id, false)
        }
    };
    // Keys minted while the gateway had no password belong to the promoted row
    // and must not inherit the administrator's `/v1` access (D23).
    let api_keys_revoked = if promoted {
        crate::users::revoke_live_api_keys(&mut *tx, &user_id)
            .await
            .context("revoking the local operator API keys")?
    } else {
        0
    };

    auth::audit(
        &mut *tx,
        Some(&user_id),
        "auth.bootstrap_admin_created",
        Some("user"),
        Some(&user_id),
        Some(
            serde_json::json!({
                "email": email,
                "source": "env",
                "promoted_local_operator": promoted,
                "api_keys_revoked": api_keys_revoked,
            })
            .to_string()
            .as_str(),
        ),
        None,
    )
    .await
    .context("auditing the bootstrap admin creation")?;
    tx.commit()
        .await
        .context("committing the bootstrap admin")?;

    tracing::info!(
        email = %email,
        promoted_local_operator = promoted,
        api_keys_revoked,
        "created bootstrap admin account from the environment"
    );
    Ok(BootstrapOutcome::AdminCreated)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (std::path::PathBuf, SqlitePool) {
        password::set_test_cost(8 * 1024, 1, 1);
        let dir = std::env::temp_dir().join(format!("gateway-bootstrap-{}", uuid::Uuid::new_v4()));
        let pool = crate::db::open(&dir.join("gateway.db"))
            .await
            .expect("open database");
        (dir, pool)
    }

    #[tokio::test]
    async fn creates_admin_when_users_is_empty() {
        let (dir, pool) = test_pool().await;
        let outcome = ensure_admin_with(
            &pool,
            Some("boss@example.com".to_string()),
            Some("a-long-enough-password".to_string()),
        )
        .await
        .expect("bootstrap must succeed");
        assert_eq!(outcome, BootstrapOutcome::AdminCreated);

        let (email, role, active): (String, String, i64) =
            sqlx::query_as("SELECT email, role, is_active FROM users")
                .fetch_one(&pool)
                .await
                .expect("user row");
        assert_eq!(email, "boss@example.com");
        assert_eq!(role, "admin");
        assert_eq!(active, 1);

        let must_change: i64 = sqlx::query_scalar("SELECT must_change_password FROM users")
            .fetch_one(&pool)
            .await
            .expect("flag");
        assert_eq!(
            must_change, 1,
            "an environment-supplied password must be rotated"
        );

        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("audit rows");
        assert_eq!(actions, ["auth.bootstrap_admin_created"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn promotes_the_local_operator_placeholder_in_place() {
        let (dir, pool) = test_pool().await;
        crate::setup::ensure_local_operator(&pool)
            .await
            .expect("seed the placeholder");

        let outcome = ensure_admin_with(
            &pool,
            Some("boss@example.com".to_string()),
            Some("a-long-enough-password".to_string()),
        )
        .await
        .expect("bootstrap must promote the placeholder");
        assert_eq!(outcome, BootstrapOutcome::AdminCreated);

        let (id, email, must_change, placeholder): (String, String, i64, i64) =
            sqlx::query_as("SELECT id, email, must_change_password, is_placeholder FROM users")
                .fetch_one(&pool)
                .await
                .expect("user row");
        assert_eq!(id, crate::auth::LOCAL_OPERATOR_ID, "same row, promoted");
        assert_eq!(email, "boss@example.com");
        assert_eq!(
            must_change, 1,
            "an environment-supplied password must be rotated"
        );
        assert_eq!(placeholder, 0, "it is a real account now");

        assert_eq!(
            ensure_admin_with(&pool, None, None)
                .await
                .expect("second bootstrap must not need credentials"),
            BootstrapOutcome::UsersExist
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn promotion_repairs_a_demoted_placeholder_and_revokes_its_keys() {
        let (dir, pool) = test_pool().await;
        crate::setup::ensure_local_operator(&pool)
            .await
            .expect("seed the placeholder");
        // An older build (or direct database access) could leave this state.
        sqlx::query("UPDATE users SET role = 'user', is_active = 0 WHERE id = ?")
            .bind(crate::auth::LOCAL_OPERATOR_ID)
            .execute(&pool)
            .await
            .expect("corrupt the placeholder");
        sqlx::query(
            "INSERT INTO api_keys (id, user_id, name, prefix, key_hash, created_at, updated_at) \
             VALUES ('k1', ?, 'minted without a password', 'pk_live_ab12', 'hash', \
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(crate::auth::LOCAL_OPERATOR_ID)
        .execute(&pool)
        .await
        .expect("placeholder key");

        ensure_admin_with(
            &pool,
            Some("boss@example.com".to_string()),
            Some("a-long-enough-password".to_string()),
        )
        .await
        .expect("bootstrap must promote the placeholder");

        let (role, active): (String, i64) = sqlx::query_as("SELECT role, is_active FROM users")
            .fetch_one(&pool)
            .await
            .expect("user row");
        assert_eq!((role.as_str(), active), ("admin", 1));
        let revoked: Option<String> =
            sqlx::query_scalar("SELECT revoked_at FROM api_keys WHERE id = 'k1'")
                .fetch_one(&pool)
                .await
                .expect("key");
        assert!(revoked.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn is_a_no_op_when_users_exist() {
        let (dir, pool) = test_pool().await;
        ensure_admin_with(
            &pool,
            Some("boss@example.com".to_string()),
            Some("a-long-enough-password".to_string()),
        )
        .await
        .expect("first bootstrap");
        assert_eq!(
            ensure_admin_with(&pool, None, None)
                .await
                .expect("second bootstrap must not need credentials"),
            BootstrapOutcome::UsersExist
        );

        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .expect("count users");
        assert_eq!(users, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn no_credentials_means_setup_mode_instead_of_failure() {
        let (dir, pool) = test_pool().await;
        let outcome = ensure_admin_with(&pool, None, None)
            .await
            .expect("no configuration must start in setup mode");
        assert!(outcome.setup_required());
        assert!(
            ensure_admin_with(&pool, Some("   ".to_string()), Some(String::new()))
                .await
                .expect("blank values count as unset")
                .setup_required()
        );

        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .expect("count users");
        assert_eq!(users, 0, "setup mode creates no account");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn half_a_configuration_fails_with_an_actionable_error() {
        let (dir, pool) = test_pool().await;

        let err = ensure_admin_with(&pool, Some("boss@example.com".to_string()), None)
            .await
            .expect_err("email without password must fail");
        assert!(err.to_string().contains("GATEWAY_ADMIN_PASSWORD"), "{err}");

        let err = ensure_admin_with(&pool, None, Some("a-long-enough-password".to_string()))
            .await
            .expect_err("password without email must fail");
        assert!(err.to_string().contains("GATEWAY_ADMIN_EMAIL"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_short_password_without_echoing_it() {
        let (dir, pool) = test_pool().await;
        let short = "too-short";
        let err = ensure_admin_with(
            &pool,
            Some("boss@example.com".to_string()),
            Some(short.to_string()),
        )
        .await
        .expect_err("short password must be rejected");
        let message = format!("{err:#}");
        assert!(message.contains("12"), "{message}");
        assert!(
            !message.contains(short),
            "the password must not leak: {message}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
