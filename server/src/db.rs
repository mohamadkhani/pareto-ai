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
    open_inner(path, false).await
}

/// Opens the database like [`open`], additionally logging every statement at
/// `TRACE` (the performance harness counts them; see `perf_bench`). Test-only:
/// production never pays for statement logging.
#[cfg(test)]
pub async fn open_logged(path: &Path) -> Result<SqlitePool> {
    open_inner(path, true).await
}

async fn open_inner(path: &Path, log_statements: bool) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating data directory {}", parent.display()))?;
    }

    let mut options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // Documented in docs/research/06-database-choice.md §4: WAL with
        // `synchronous = NORMAL` survives a process crash; a machine power
        // loss may lose the newest commits (usage data only, never
        // corruption). Without this, SQLite stays at its default FULL and
        // fsyncs every write commit — three commits per chat request
        // (auth `last_used_at`, reservation, settlement), measured at
        // ~0.33 ms per fsync on NVMe.
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    if log_statements {
        use sqlx::ConnectOptions;
        options = options.log_statements(log::LevelFilter::Trace);
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .with_context(|| format!("opening database {}", path.display()))?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    // One-time data repair that pure SQL cannot express: the strict catalog
    // identity is computed in Rust. It is guarded by a `db_meta` key and is a
    // no-op on a fresh database, so it belongs to the migration boundary.
    crate::catalog::repair_duplicate_models(&pool)
        .await
        .context("repairing duplicate catalog models")?;

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

        // The documented pragmas (docs/research/06-database-choice.md §4 and
        // docs/research/13-performance.md): WAL, NORMAL, busy timeout.
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .expect("journal_mode");
        assert_eq!(journal, "wal");
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&pool)
            .await
            .expect("synchronous");
        assert_eq!(synchronous, 1, "synchronous = NORMAL");
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&pool)
            .await
            .expect("busy_timeout");
        assert_eq!(busy_timeout, 5_000);

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
