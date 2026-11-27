//! SQLite connection pool and migrations.
//!
//! One file, WAL mode, foreign keys on. The pool is created at startup and
//! passed to routes; opening the database creates the data directory when it
//! does not exist yet.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Connection, SqlitePool};

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

    // Migrations run on a dedicated connection with foreign keys OFF. SQLite
    // cannot drop a table-level constraint, so a rebuild (e.g. 0022 widening
    // the `UNIQUE` on `offerings`) must create a new table, copy, drop the old
    // one, and rename. `DROP TABLE` runs an implicit DELETE, so with foreign
    // keys ON the child rows of `offerings` (`prices`, `usage_events`,
    // `telemetry_attempts`, combo members, ...) cascade away. `PRAGMA
    // foreign_keys` cannot be toggled inside the transaction `Migrator::apply`
    // always opens, so the connection itself must start with it off; the
    // pool's own connections keep it ON (see the `foreign_keys(true)` above).
    let migration_options = options.clone().foreign_keys(false);
    run_migrations(&migration_options)
        .await
        .with_context(|| format!("opening database {}", path.display()))?;

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .with_context(|| format!("opening database {}", path.display()))?;

    // One-time data repair that pure SQL cannot express: the strict catalog
    // identity is computed in Rust. It is guarded by a `db_meta` key and is a
    // no-op on a fresh database, so it belongs to the migration boundary.
    crate::catalog::repair_duplicate_models(&pool)
        .await
        .context("repairing duplicate catalog models")?;

    Ok(pool)
}

/// Applies every pending migration on one fresh connection.
///
/// The connection is built from `options`, so the caller decides the foreign
/// keys state: [`open_inner`] passes them off (a rebuild drops a referenced
/// table; see the comment there). Failure mode: any connection or migration
/// error, wrapped by the caller.
async fn run_migrations(options: &SqliteConnectOptions) -> Result<()> {
    let mut connection = sqlx::SqliteConnection::connect_with(options)
        .await
        .context("opening the migration connection")?;
    let migrator = sqlx::migrate!("./migrations");
    let result = async {
        adopt_renumbered_migrations(&mut connection, &migrator).await?;
        migrator
            .run(&mut connection)
            .await
            .context("running migrations")
    }
    .await;
    drop(connection);
    result
}

/// Moves a database onto current migration numbers when a build from before
/// this branch merged `main` recorded them differently.
///
/// The combo work first numbered its migrations 21 (`offering routes`) and 22
/// (`combo pool ordering`). `main` later took 21 for the request-log columns
/// (D64), so the same files are 22 and 23 now (D65); their SQL bodies are
/// unchanged and only the number in the header comment moved. sqlx refuses to
/// start when a recorded migration is absent from the resolved set, so the two
/// bookkeeping rows are renumbered, with the checksum of the file that carries
/// that SQL today. This is a no-op for a fresh database, for one already on the
/// current numbers, and for any other database.
async fn adopt_renumbered_migrations(
    connection: &mut sqlx::SqliteConnection,
    migrator: &sqlx::migrate::Migrator,
) -> Result<()> {
    /// `(recorded version, recorded description, current version)`, newest
    /// first so a move never lands on a number that has not been vacated yet.
    const RENUMBERED: [(i64, &str, i64); 2] =
        [(22, "combo pool ordering", 23), (21, "offering routes", 22)];

    let has_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(&mut *connection)
    .await
    .context("looking for the migrations table")?;
    if has_table == 0 {
        return Ok(());
    }

    let applied: std::collections::HashMap<i64, String> =
        sqlx::query_as("SELECT version, description FROM _sqlx_migrations WHERE version >= 21")
            .fetch_all(&mut *connection)
            .await
            .context("reading applied migrations")?
            .into_iter()
            .collect();

    let mut occupied: std::collections::HashSet<i64> = applied.keys().copied().collect();
    let mut moves = Vec::new();
    for (recorded, description, current) in RENUMBERED {
        if applied.get(&recorded).map(String::as_str) != Some(description) {
            continue;
        }
        if occupied.contains(&current) {
            continue;
        }
        let Some(target) = migrator.iter().find(|m| m.version == current) else {
            continue;
        };
        occupied.remove(&recorded);
        occupied.insert(current);
        moves.push((recorded, description, target));
    }
    if moves.is_empty() {
        return Ok(());
    }

    let mut transaction = connection
        .begin()
        .await
        .context("opening the renumbering transaction")?;
    for (recorded, description, target) in moves {
        sqlx::query(
            "UPDATE _sqlx_migrations SET version = ?, description = ?, checksum = ? \
             WHERE version = ? AND description = ?",
        )
        .bind(target.version)
        .bind(target.description.as_ref())
        .bind(target.checksum.to_vec())
        .bind(recorded)
        .bind(description)
        .execute(&mut *transaction)
        .await
        .context("renumbering an applied migration")?;
        log::warn!(
            "renumbered applied migration {recorded} to {} ({})",
            target.version,
            target.description
        );
    }
    transaction
        .commit()
        .await
        .context("committing the renumbering")
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

    /// Regression for the 0022 rebuild (D65): dropping the referenced
    /// `offerings` table with foreign keys ON cascades the child rows away.
    /// The upgrade path must run migrations with foreign keys off, so a
    /// non-empty database keeps its `prices` and `usage_events` rows and the
    /// rebuilt `offerings` gains the new `route`/`routing_json` columns.
    #[tokio::test]
    async fn upgrading_past_0022_keeps_child_rows() {
        let dir = std::env::temp_dir().join(format!("gateway-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create data dir");
        let path = dir.join("gateway.db");

        // Build the database a pre-D65 install has: migrations 0001..0020
        // applied, then the provider/model/offering/price/usage rows the 0022
        // rebuild must preserve. Running the embedded migrations filtered to
        // `<= 20` records the same checksums the real runner then expects.
        let mut pre_upgrade = sqlx::migrate!("./migrations");
        let subset: Vec<_> = pre_upgrade
            .migrations
            .iter()
            .filter(|migration| migration.version <= 20)
            .cloned()
            .collect();
        pre_upgrade.migrations = std::borrow::Cow::Owned(subset);

        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let mut connection = sqlx::SqliteConnection::connect_with(&options)
            .await
            .expect("open raw connection");
        pre_upgrade
            .run(&mut connection)
            .await
            .expect("apply migrations 0001..0020");

        let now = "2026-01-01T00:00:00.000Z";
        sqlx::query(
            "INSERT INTO providers (id, name, base_url, enabled, created_at, updated_at) \
             VALUES ('p', 'openrouter', 'https://openrouter.ai/api/v1', 1, ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&mut connection)
        .await
        .expect("provider");
        sqlx::query(
            "INSERT INTO models (id, canonical_name, created_at, updated_at) \
             VALUES ('m', 'openai/gpt-4o', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&mut connection)
        .await
        .expect("model");
        sqlx::query(
            "INSERT INTO offerings \
               (id, provider_id, model_id, upstream_model_id, enabled, created_at, updated_at) \
             VALUES ('o', 'p', 'm', 'openai/gpt-4o', 1, ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&mut connection)
        .await
        .expect("offering");
        sqlx::query(
            "INSERT INTO prices (id, offering_id, input_per_m, source, created_at, updated_at) \
             VALUES ('pr', 'o', 2.5, 'openrouter', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&mut connection)
        .await
        .expect("price");
        sqlx::query(
            "INSERT INTO usage_events (request_id, offering_id, occurred_at) \
             VALUES ('req-1', 'o', ?)",
        )
        .bind(now)
        .execute(&mut connection)
        .await
        .expect("usage event");
        drop(connection);

        // Exercise the real production entry point: `open` runs the remaining
        // migrations with foreign keys off (the 0022 rebuild drops the
        // referenced `offerings` table; with foreign keys on the child rows
        // cascade away) and returns a pool whose own connections keep foreign
        // keys on. Reverting `open_inner`'s call site would break this test.
        let pool = open(&path)
            .await
            .expect("upgrade through the production entry point");

        let prices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prices WHERE offering_id = 'o'")
            .fetch_one(&pool)
            .await
            .expect("price count");
        assert_eq!(prices, 1, "the price row survives the offerings rebuild");
        let linked: Option<String> =
            sqlx::query_scalar("SELECT offering_id FROM usage_events WHERE request_id = 'req-1'")
                .fetch_one(&pool)
                .await
                .expect("usage event");
        assert_eq!(
            linked.as_deref(),
            Some("o"),
            "usage_events.offering_id is not nulled by the rebuild"
        );
        let route: Option<String> =
            sqlx::query_scalar("SELECT route FROM offerings WHERE id = 'o'")
                .fetch_one(&pool)
                .await
                .expect("rebuilt offering");
        assert_eq!(
            route.as_deref(),
            Some(""),
            "the copied row keeps an empty route"
        );
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('offerings')")
                .fetch_all(&pool)
                .await
                .expect("offering columns");
        assert!(
            columns.contains(&"route".to_string()),
            "columns: {columns:?}"
        );
        assert!(
            columns.contains(&"routing_json".to_string()),
            "columns: {columns:?}"
        );

        // The pool's own connections enforce foreign keys even though the
        // migration connection did not.
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .expect("foreign_keys pragma");
        assert_eq!(foreign_keys, 1, "the pool enforces foreign keys");

        drop(pool);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A database migrated by a build from before this branch merged `main`
    /// records the offerings rebuild as migration 21 and the combo knobs as 22.
    /// `main` owns 21 for the request-log columns (D64), so those two files are
    /// 22 and 23 (D65) and startup must adopt the recorded numbers instead of
    /// failing with `MigrateError::VersionMissing(21)`.
    #[tokio::test]
    async fn adopts_the_pre_merge_migration_numbers() {
        use std::borrow::Cow;

        use sqlx::migrate::Migration;

        let dir = std::env::temp_dir().join(format!("gateway-legacy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create data dir");
        let path = dir.join("gateway.db");

        let embedded = sqlx::migrate!("./migrations");
        let file = |version: i64| {
            embedded
                .iter()
                .find(|migration| migration.version == version)
                .expect("embedded migration")
                .clone()
        };

        // The pre-merge numbering: the same SQL files, recorded as 21 and 22.
        let mut legacy = sqlx::migrate!("./migrations");
        let mut migrations: Vec<_> = legacy
            .migrations
            .iter()
            .filter(|migration| migration.version <= 20)
            .cloned()
            .collect();
        migrations.push(Migration {
            version: 21,
            description: Cow::Borrowed("offering routes"),
            ..file(22)
        });
        migrations.push(Migration {
            version: 22,
            description: Cow::Borrowed("combo pool ordering"),
            ..file(23)
        });
        legacy.migrations = Cow::Owned(migrations);

        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let mut connection = sqlx::SqliteConnection::connect_with(&options)
            .await
            .expect("open raw connection");
        legacy
            .run(&mut connection)
            .await
            .expect("migrate with the pre-merge numbering");

        // A routed offering written after the rebuild: adopting the number must
        // not run the rebuild again, because that would reset `route` to ''.
        let now = "2026-01-01T00:00:00.000Z";
        sqlx::query(
            "INSERT INTO providers (id, name, base_url, enabled, created_at, updated_at) \
             VALUES ('p', 'openrouter', 'https://openrouter.ai/api/v1', 1, ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&mut connection)
        .await
        .expect("provider");
        sqlx::query(
            "INSERT INTO models (id, canonical_name, created_at, updated_at) \
             VALUES ('m', 'openai/gpt-4o', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&mut connection)
        .await
        .expect("model");
        sqlx::query(
            "INSERT INTO offerings (id, provider_id, model_id, upstream_model_id, route) \
             VALUES ('o', 'p', 'm', 'openai/gpt-4o', 'azure')",
        )
        .execute(&mut connection)
        .await
        .expect("routed offering");
        drop(connection);

        let pool = open(&path)
            .await
            .expect("start on a database with the pre-merge numbering");

        let applied: Vec<(i64, String)> =
            sqlx::query_as("SELECT version, description FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .expect("applied migrations");
        let description = |version: i64| {
            applied
                .iter()
                .find(|(applied_version, _)| *applied_version == version)
                .map(|(_, description)| description.as_str())
        };
        assert_eq!(applied.last().map(|(version, _)| *version), Some(23));
        assert_eq!(description(21), Some("request log target"));
        assert_eq!(description(22), Some("offering routes"));
        assert_eq!(description(23), Some("combo pool ordering"));
        assert_eq!(
            applied.len(),
            23,
            "every migration is recorded once: {applied:?}"
        );

        let route: String = sqlx::query_scalar("SELECT route FROM offerings WHERE id = 'o'")
            .fetch_one(&pool)
            .await
            .expect("offering");
        assert_eq!(route, "azure", "the adopted rebuild did not run again");
        let request_log_columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('request_logs')")
                .fetch_all(&pool)
                .await
                .expect("request_logs columns");
        assert!(
            request_log_columns.contains(&"credential_id".to_string()),
            "migration 21 still applied: {request_log_columns:?}"
        );

        drop(pool);

        std::fs::remove_dir_all(&dir).ok();
    }
}
