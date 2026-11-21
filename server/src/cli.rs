//! Operator commands that run against the database without serving HTTP.
//!
//! `gateway reset-password` (decision D56) is the recovery path for a
//! deployment whose last admin forgot the password. It needs the same
//! filesystem access as the server itself, accepts the password only through a
//! hidden prompt (or one line on stdin), and applies the admin-reset rules from
//! [`crate::users::reset_password_as_operator`].

use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sqlx::SqlitePool;

use crate::db;
use crate::users;

/// Help text printed by `gateway reset-password --help` and on bad arguments.
const USAGE: &str = "\
usage: gateway reset-password [--email <address>] [--data-dir <path>]

Resets one account's password from the host that holds the database.
The new password is read from the terminal (or one line on stdin, when
stdin is not a terminal). Sessions are deleted, live API keys are revoked,
and the account must choose a new password at its next sign-in.

  --email      account to reset; omit it when the database has one real account
  --data-dir   directory holding gateway.db (default: GATEWAY_DATA_DIR or ./data)";

/// The parsed arguments of `gateway reset-password`.
#[derive(Debug, Default, PartialEq, Eq)]
struct ResetArgs {
    email: Option<String>,
    data_dir: Option<PathBuf>,
}

/// One account as the CLI needs it.
#[derive(Debug, sqlx::FromRow)]
struct Account {
    id: String,
    email: String,
    role: String,
}

/// Runs `gateway reset-password` with the arguments after the subcommand.
///
/// `configured_data_dir` is the server's data directory from `GATEWAY_DATA_DIR`
/// (or `./data`); `--data-dir` still overrides it. Only the directory is needed
/// here, so the command works without the rest of the server configuration.
///
/// Failure mode: a bad argument, a missing database, an unknown or ambiguous
/// account, a too-short password, or a database error. The message always names
/// what the operator must fix.
pub async fn run_reset_password(configured_data_dir: &Path, args: &[String]) -> Result<()> {
    run_reset_password_with(configured_data_dir, args, None).await
}

/// [`run_reset_password`] with an injected password, so tests can cover the
/// whole path without a terminal. `None` reads from the terminal or stdin.
async fn run_reset_password_with(
    configured_data_dir: &Path,
    args: &[String],
    password: Option<&str>,
) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let parsed = parse_args(args).map_err(|message| anyhow::anyhow!("{message}\n\n{USAGE}"))?;
    let data_dir = resolve_data_dir(parsed.data_dir.as_deref(), configured_data_dir)?;
    let database = existing_database(&data_dir)?;
    let pool = db::open(&database)
        .await
        .with_context(|| format!("opening {}", database.display()))?;

    let account = select_account(&pool, parsed.email.as_deref()).await?;
    let password = match password {
        Some(password) => password.to_string(),
        None => read_password()?,
    };
    let outcome = users::reset_password_as_operator(&pool, &account.id, &password)
        .await
        .context("resetting the password")?;

    println!("Password reset for {}.", outcome.email);
    println!("  database:         {}", database.display());
    println!("  sessions revoked: {}", outcome.sessions_revoked);
    println!("  API keys revoked: {}", outcome.api_keys_revoked);
    if !outcome.is_active {
        println!(
            "The account is deactivated: it still cannot sign in until an admin activates it."
        );
    }
    println!("The account must choose a new password at its next sign-in.");
    Ok(())
}

/// Parses the arguments after `reset-password`; every unknown flag is an error
/// so a typo cannot silently reset the wrong thing.
fn parse_args(args: &[String]) -> Result<ResetArgs, String> {
    let mut parsed = ResetArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--email" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--email needs an address".to_string())?;
                let email = value.trim().to_string();
                if email.is_empty() {
                    return Err("--email must not be empty".to_string());
                }
                parsed.email = Some(email);
            }
            "--data-dir" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--data-dir needs a path".to_string())?;
                if value.trim().is_empty() {
                    return Err("--data-dir must not be empty".to_string());
                }
                parsed.data_dir = Some(PathBuf::from(value));
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
        index += 1;
    }
    Ok(parsed)
}

/// Resolves `--data-dir` (relative to the working directory) or falls back to
/// the configured directory, which `Config::from_env` already made absolute.
fn resolve_data_dir(flag: Option<&Path>, configured: &Path) -> Result<PathBuf> {
    match flag {
        Some(path) if path.is_absolute() => Ok(path.to_path_buf()),
        Some(path) => Ok(std::env::current_dir()?.join(path)),
        None => Ok(configured.to_path_buf()),
    }
}

/// Refuses a missing or empty file. A wrong working directory must not open
/// (and migrate) a fresh database that then looks like "no accounts": that
/// would create a second gateway.db and hide the real one.
fn existing_database(data_dir: &Path) -> Result<PathBuf> {
    let path = data_dir.join("gateway.db");
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => bail!(
            "no database at {}; pass --data-dir or set GATEWAY_DATA_DIR to the directory \
             that holds gateway.db",
            path.display()
        ),
    };
    if metadata.len() == 0 {
        bail!(
            "{} is empty; refusing to open it, because a wrong --data-dir would create a \
             fresh database and hide the real one",
            path.display()
        );
    }
    Ok(path)
}

/// Picks the account to reset: an explicit email, or the only real account.
async fn select_account(pool: &SqlitePool, email: Option<&str>) -> Result<Account> {
    const SELECT: &str = "SELECT id, email, role FROM users WHERE is_placeholder = 0";
    if let Some(email) = email {
        let account = sqlx::query_as::<_, Account>(&format!("{SELECT} AND email = ?"))
            .bind(email)
            .fetch_optional(pool)
            .await
            .context("looking up the account")?;
        return account.with_context(|| format!("no account with email `{email}`"));
    }

    let accounts = sqlx::query_as::<_, Account>(&format!("{SELECT} ORDER BY email"))
        .fetch_all(pool)
        .await
        .context("listing accounts")?;
    match accounts.len() {
        0 => bail!("this database has no accounts; start the gateway and use the setup wizard"),
        1 => match accounts.into_iter().next() {
            Some(account) => Ok(account),
            None => bail!("this database has no accounts"),
        },
        _ => {
            let list = accounts
                .iter()
                .map(|account| format!("  {} ({})", account.email, account.role))
                .collect::<Vec<_>>()
                .join("\n");
            bail!("several accounts exist; choose one with --email:\n{list}")
        }
    }
}

/// Reads the new password twice from a terminal, or once from piped stdin.
fn read_password() -> Result<String> {
    if std::io::stdin().is_terminal() {
        let first = rpassword::prompt_password("New password: ").context("reading the password")?;
        let second =
            rpassword::prompt_password("Repeat password: ").context("reading the password")?;
        if first != second {
            bail!("the two passwords do not match");
        }
        Ok(first)
    } else {
        let mut input = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut input)
            .context("reading the password from stdin")?;
        let password = input.trim_end_matches(['\r', '\n']).to_string();
        if password.is_empty() {
            bail!("no password was provided on stdin");
        }
        Ok(password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_email_and_data_dir() {
        assert_eq!(
            parse_args(&args(&[
                "--email",
                " admin@example.com ",
                "--data-dir",
                "/srv/data"
            ])),
            Ok(ResetArgs {
                email: Some("admin@example.com".to_string()),
                data_dir: Some(PathBuf::from("/srv/data")),
            })
        );
        assert_eq!(parse_args(&[]), Ok(ResetArgs::default()));
    }

    #[test]
    fn rejects_typos_and_missing_values() {
        assert!(parse_args(&args(&["--emial", "a@b.c"])).is_err());
        assert!(parse_args(&args(&["--email"])).is_err());
        assert!(parse_args(&args(&["--email", "  "])).is_err());
        assert!(parse_args(&args(&["--data-dir"])).is_err());
    }

    #[test]
    fn refuses_a_data_dir_without_a_database() {
        let dir = std::env::temp_dir().join(format!("gateway-cli-{}", uuid::Uuid::new_v4()));
        let error = existing_database(&dir).expect_err("no database yet");
        assert!(error.to_string().contains("no database at"), "{error}");
        std::fs::create_dir_all(&dir).expect("data dir");
        assert!(existing_database(&dir).is_err());

        // A zero-byte file is not a database: opening it would migrate a fresh
        // one and report "no accounts" instead of naming the wrong directory.
        std::fs::write(dir.join("gateway.db"), b"").expect("empty file");
        let error = existing_database(&dir).expect_err("empty database file");
        assert!(error.to_string().contains("is empty"), "{error}");

        std::fs::write(dir.join("gateway.db"), b"SQLite format 3\0").expect("database file");
        assert_eq!(
            existing_database(&dir).expect("database"),
            dir.join("gateway.db")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    async fn test_pool() -> (PathBuf, SqlitePool) {
        let dir = std::env::temp_dir().join(format!("gateway-cli-{}", uuid::Uuid::new_v4()));
        let pool = crate::db::open(&dir.join("gateway.db"))
            .await
            .expect("open database");
        (dir, pool)
    }

    async fn add_user(pool: &SqlitePool, id: &str, email: &str, placeholder: bool) {
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active, \
                                is_placeholder) \
             VALUES (?, ?, ?, 'x', 'admin', 1, ?)",
        )
        .bind(id)
        .bind(email)
        .bind(email)
        .bind(i64::from(placeholder))
        .execute(pool)
        .await
        .expect("user");
    }

    #[tokio::test]
    async fn selects_the_only_real_account_and_ignores_the_placeholder() {
        let (dir, pool) = test_pool().await;
        add_user(&pool, "local", "local@example.invalid", true).await;
        add_user(&pool, "a", "admin@example.com", false).await;

        let account = select_account(&pool, None).await.expect("only account");
        assert_eq!(account.id, "a");
        assert_eq!(account.email, "admin@example.com");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn email_lookup_is_case_insensitive_and_unknown_emails_fail() {
        let (dir, pool) = test_pool().await;
        add_user(&pool, "a", "Admin@Example.com", false).await;

        let account = select_account(&pool, Some("admin@example.com"))
            .await
            .expect("case-insensitive");
        assert_eq!(account.id, "a");
        assert!(
            select_account(&pool, Some("nobody@example.com"))
                .await
                .is_err()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn several_accounts_require_an_explicit_email() {
        let (dir, pool) = test_pool().await;
        add_user(&pool, "a", "a@example.com", false).await;
        add_user(&pool, "b", "b@example.com", false).await;

        let error = select_account(&pool, None).await.expect_err("ambiguous");
        let message = error.to_string();
        assert!(message.contains("--email"), "{message}");
        assert!(message.contains("a@example.com"), "{message}");
        assert!(message.contains("b@example.com"), "{message}");

        // An empty database points at the setup wizard instead.
        let (other_dir, other_pool) = test_pool().await;
        let error = select_account(&other_pool, None).await.expect_err("empty");
        assert!(error.to_string().contains("setup wizard"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&other_dir).ok();
    }

    #[tokio::test]
    async fn resets_the_only_account_through_the_documented_path() {
        crate::password::set_test_cost(8 * 1024, 1, 1);
        let (dir, pool) = test_pool().await;
        add_user(&pool, "a", "admin@example.com", false).await;
        sqlx::query(
            "INSERT INTO sessions (id, token_hash, user_id, csrf_token, expires_at) \
             VALUES ('s1', 'h1', 'a', 'c', '2999-01-01T00:00:00.000Z')",
        )
        .execute(&pool)
        .await
        .expect("session");
        sqlx::query(
            "INSERT INTO api_keys (id, user_id, name, prefix, key_hash, created_at, updated_at) \
             VALUES ('k1', 'a', 'key', 'pk_live_ab12', 'hash', '2026-01-01T00:00:00.000Z', \
                     '2026-01-01T00:00:00.000Z')",
        )
        .execute(&pool)
        .await
        .expect("api key");
        drop(pool);

        // No flags: the configured data directory and the only account.
        run_reset_password_with(&dir, &[], Some("new-password-1"))
            .await
            .expect("reset through the CLI path");

        let pool = crate::db::open(&dir.join("gateway.db"))
            .await
            .expect("reopen");
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = 'a'")
            .fetch_one(&pool)
            .await
            .expect("hash");
        assert!(crate::password::verify_password("new-password-1", &hash).expect("verify"));
        let flagged: i64 =
            sqlx::query_scalar("SELECT must_change_password FROM users WHERE id = 'a'")
                .fetch_one(&pool)
                .await
                .expect("flag");
        assert_eq!(flagged, 1, "the operator's password must rotate");
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&pool)
            .await
            .expect("sessions");
        assert_eq!(sessions, 0);
        let revoked: Option<String> =
            sqlx::query_scalar("SELECT revoked_at FROM api_keys WHERE id = 'k1'")
                .fetch_one(&pool)
                .await
                .expect("key");
        assert!(revoked.is_some(), "live API keys are revoked");
        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'auth.password_reset_cli'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit");
        assert_eq!(audits, 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
