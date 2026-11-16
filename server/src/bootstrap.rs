//! First-run bootstrap: create the initial admin account.
//!
//! When the `users` table is empty, the admin is created from the
//! `GATEWAY_ADMIN_EMAIL` / `GATEWAY_ADMIN_PASSWORD` environment variables and
//! startup fails with an actionable error when they are missing or too weak.
//! The password is never logged and never stored in any long-lived struct: it
//! is read from the environment, hashed, and dropped.

use std::env;

use anyhow::{Context, Result, bail};
use sqlx::SqlitePool;

use crate::auth;
use crate::password;

const MIN_ADMIN_PASSWORD_LEN: usize = 12;

/// Creates the bootstrap admin when no user exists yet.
///
/// Fails startup when the table is empty and the environment does not provide
/// a usable admin email and password (at least 12 characters).
pub async fn ensure_admin(pool: &SqlitePool) -> Result<()> {
    let email = env::var("GATEWAY_ADMIN_EMAIL").ok();
    let password = env::var("GATEWAY_ADMIN_PASSWORD").ok();
    ensure_admin_with(pool, email, password).await
}

/// [`ensure_admin`] with the credentials passed in; separated for testing.
pub async fn ensure_admin_with(
    pool: &SqlitePool,
    email: Option<String>,
    admin_password: Option<String>,
) -> Result<()> {
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await
        .context("counting users")?;
    if users > 0 {
        return Ok(());
    }

    let email = match email {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => bail!(
            "no users exist and GATEWAY_ADMIN_EMAIL is not set; \
             set GATEWAY_ADMIN_EMAIL and GATEWAY_ADMIN_PASSWORD to create the first admin account"
        ),
    };
    let admin_password = match admin_password {
        Some(value) if value.len() >= MIN_ADMIN_PASSWORD_LEN => value,
        Some(_) => bail!(
            "GATEWAY_ADMIN_PASSWORD must be at least {MIN_ADMIN_PASSWORD_LEN} characters; \
             pick a longer password and restart"
        ),
        None => bail!(
            "no users exist and GATEWAY_ADMIN_PASSWORD is not set; \
             set GATEWAY_ADMIN_EMAIL and GATEWAY_ADMIN_PASSWORD to create the first admin account"
        ),
    };

    let user_id = uuid::Uuid::new_v4().to_string();
    let password_hash =
        password::hash_password(&admin_password).context("hashing the bootstrap admin password")?;

    sqlx::query(
        "INSERT INTO users (id, email, display_name, password_hash, role, is_active, must_change_password) \
         VALUES (?, ?, ?, ?, 'admin', 1, 1)",
    )
    .bind(&user_id)
    .bind(&email)
    .bind(&email)
    .bind(&password_hash)
    .execute(pool)
    .await
    .context("creating the bootstrap admin user")?;

    auth::audit(
        pool,
        Some(&user_id),
        "auth.bootstrap_admin_created",
        Some("user"),
        Some(&user_id),
        Some(serde_json::json!({ "email": email }).to_string().as_str()),
        None,
    )
    .await
    .context("auditing the bootstrap admin creation")?;

    tracing::info!(email = %email, "created bootstrap admin account");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (std::path::PathBuf, SqlitePool) {
        let dir = std::env::temp_dir().join(format!("gateway-bootstrap-{}", uuid::Uuid::new_v4()));
        let pool = crate::db::open(&dir.join("gateway.db"))
            .await
            .expect("open database");
        (dir, pool)
    }

    #[tokio::test]
    async fn creates_admin_when_users_is_empty() {
        let (dir, pool) = test_pool().await;
        ensure_admin_with(
            &pool,
            Some("boss@example.com".to_string()),
            Some("a-long-enough-password".to_string()),
        )
        .await
        .expect("bootstrap must succeed");

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
            "the bootstrap admin must rotate its password"
        );

        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("audit rows");
        assert_eq!(actions, ["auth.bootstrap_admin_created"]);
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
        ensure_admin_with(&pool, None, None)
            .await
            .expect("second bootstrap must not need credentials");

        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .expect("count users");
        assert_eq!(users, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fails_with_actionable_error_when_credentials_missing() {
        let (dir, pool) = test_pool().await;

        let err = ensure_admin_with(&pool, None, None)
            .await
            .expect_err("must fail without credentials");
        let message = err.to_string();
        assert!(message.contains("GATEWAY_ADMIN_EMAIL"), "{message}");
        assert!(message.contains("GATEWAY_ADMIN_PASSWORD"), "{message}");

        let err = ensure_admin_with(&pool, Some("boss@example.com".to_string()), None)
            .await
            .expect_err("must fail without password");
        assert!(err.to_string().contains("GATEWAY_ADMIN_PASSWORD"));

        let err = ensure_admin_with(&pool, Some("  ".to_string()), None)
            .await
            .expect_err("blank email is missing");
        assert!(err.to_string().contains("GATEWAY_ADMIN_EMAIL"));
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
