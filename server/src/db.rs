//! SQLite connection pool and migrations.
//!
//! One file, WAL mode, foreign keys on. The pool is created at startup and
//! passed to routes; opening the database creates the data directory when it
//! does not exist yet.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// Opens (or creates) the database, applies migrations, and returns a pool.
pub async fn open(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating data directory {}", parent.display()))?;
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .with_context(|| format!("opening database {}", path.display()))?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    Ok(pool)
}

/// Cheap liveness probe used by the health endpoint.
pub async fn ping(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(pool)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opens_and_migrates_a_fresh_database() {
        let dir = std::env::temp_dir().join(format!("gateway-test-{}", uuid::Uuid::new_v4()));
        let pool = open(&dir.join("gateway.db")).await.expect("open database");
        ping(&pool).await.expect("database answers ping");

        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(&pool)
                .await
                .expect("query sqlite_master");

        for table in ["settings", "users", "sessions", "audit_log"] {
            assert!(
                tables.contains(&table.to_string()),
                "missing table {table}: {tables:?}"
            );
        }

        let indexes: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .expect("query indexes");

        for index in [
            "idx_sessions_token_hash",
            "idx_sessions_user_id",
            "idx_audit_log_occurred_at",
            "idx_audit_log_actor_user_id",
        ] {
            assert!(
                indexes.contains(&index.to_string()),
                "missing index {index}: {indexes:?}"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
