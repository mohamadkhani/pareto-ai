//! Benchmark importer (M5): external quality scores with provenance
//! (decision D5 — we measure speed ourselves, quality is imported).
//!
//! The importer reads an LMArena-style leaderboard document — the recorded
//! fixture `server/tests/fixtures/leaderboard.json` stands in for the live
//! endpoint, which is unreachable from this environment
//! (docs/research/09-environment.md). **Follow-up**: swap the fixture read in
//! the caller for a live fetch of the LMArena leaderboard (the `fetch` seam
//! mirrors [`crate::pricing::openrouter::run_sync`]).
//!
//! Guarantees:
//! * **Idempotent**: one row per `(model_id, source, category)` under the
//!   `UNIQUE` constraint; re-importing the same document changes nothing.
//! * **Overrides win**: the importer never writes `model_quality_overrides`,
//!   and [`quality_for_models`] resolves a user override before any synced
//!   score — a later import can never overwrite it.
//! * **Quality for routing** = override > latest benchmark (wide CIs shrunk
//!   toward the leaderboard mean, [`crate::routing::shrink`]) > model default
//!   (neutral 0.5, imputed). Only `scale = 'elo'` rows take part (D25): the
//!   automatic capability and percentage sources are stored for display with
//!   their own scale and never mixed into the Elo ranking (D31).
//!
//! Automatic download lives in [`crate::benchmark_sync`]; this module owns
//! the storage and the manual import the admin UI keeps.
//!
//! The module also serves the two admin listings the UI needs
//! (`GET /api/benchmarks`, `GET /api/quality-overrides`): [`list_scores`] and
//! [`list_overrides`], both newest first.

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::catalog::{self, Actor};
use crate::routing::{self, QualityScore};

/// The source recorded for fixture/live LMArena imports.
pub const DEFAULT_SOURCE: &str = "lmarena";

/// Benchmark storage failures (imports, override writes, and the admin
/// listings). Failure mode: a failed write commits nothing — the transaction
/// rolls back and nothing is half-imported; a failed read returns nothing.
#[derive(Debug, thiserror::Error)]
pub enum BenchmarkError {
    #[error("leaderboard document is malformed: {0}")]
    Malformed(String),
    #[error("benchmark storage failed: {0}")]
    Db(#[from] sqlx::Error),
    #[error("benchmark audit failed: {0}")]
    Audit(#[from] catalog::CatalogError),
}

impl BenchmarkError {
    /// The uniform API error for this failure.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        match self {
            Self::Malformed(message) => {
                crate::error::ApiError::bad_request(message).with_request_id(rid)
            }
            Self::Db(e) => {
                crate::error::ApiError::internal("benchmark storage", e).with_request_id(rid)
            }
            Self::Audit(e) => e.into_api_error(rid),
        }
    }
}

/// One existing benchmark row for the change detector: `(score, ci, rank,
/// source_url, fetched_at)`.
type ExistingRow = (f64, Option<f64>, Option<i64>, Option<String>, String);

/// One leaderboard entry (spec M5 scope 5).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Entry {
    pub model: String,
    pub vendor: Option<String>,
    pub rank: Option<i64>,
    pub score: f64,
    pub ci: Option<f64>,
    pub votes: Option<i64>,
    pub category: String,
}

/// The leaderboard document: provenance plus its entries.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Leaderboard {
    pub source: Option<String>,
    pub source_url: Option<String>,
    pub fetched_at: Option<String>,
    pub entries: Vec<Entry>,
}

/// Parses and validates a leaderboard document. Failure mode:
/// [`BenchmarkError::Malformed`] naming the offending field.
pub fn parse_leaderboard(raw: &str) -> Result<Leaderboard, BenchmarkError> {
    let board: Leaderboard = serde_json::from_str(raw)
        .map_err(|e| BenchmarkError::Malformed(format!("invalid JSON: {e}")))?;
    if board.entries.is_empty() {
        return Err(BenchmarkError::Malformed(
            "entries must not be empty".to_string(),
        ));
    }
    for entry in &board.entries {
        if entry.model.trim().is_empty() {
            return Err(BenchmarkError::Malformed(
                "entry model must not be empty".to_string(),
            ));
        }
        if !entry.score.is_finite() {
            return Err(BenchmarkError::Malformed(format!(
                "entry {} score must be finite",
                entry.model
            )));
        }
        if entry.ci.is_some_and(|ci| !ci.is_finite() || ci < 0.0) {
            return Err(BenchmarkError::Malformed(format!(
                "entry {} ci must be a non-negative number",
                entry.model
            )));
        }
        if entry.category != "text" && entry.category != "code" {
            return Err(BenchmarkError::Malformed(format!(
                "entry {} category must be text or code, got {:?}",
                entry.model, entry.category
            )));
        }
    }
    Ok(board)
}

/// What one import run changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportOutcome {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    /// Entries whose `model` matches no `models.canonical_name`.
    pub skipped_unknown_model: usize,
}

/// Imports a leaderboard document into `benchmarks`, upserting one row per
/// `(model_id, source, category)` and recording one `sync.benchmarks` audit
/// row. `model_quality_overrides` is never written here. Failure mode:
/// [`BenchmarkError`] — the transaction rolls back, nothing is half-imported.
pub async fn run_import(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    raw: &str,
) -> Result<ImportOutcome, BenchmarkError> {
    let board = parse_leaderboard(raw)?;
    let source = board
        .source
        .clone()
        .unwrap_or_else(|| DEFAULT_SOURCE.to_string());
    let fetched_at = board.fetched_at.clone().unwrap_or_else(catalog::now_ts);

    let mut tx = pool.begin().await?;
    let mut outcome = ImportOutcome {
        inserted: 0,
        updated: 0,
        unchanged: 0,
        skipped_unknown_model: 0,
    };

    for entry in &board.entries {
        let model_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM models WHERE canonical_name = ? COLLATE NOCASE")
                .bind(entry.model.trim())
                .fetch_optional(&mut *tx)
                .await?;
        let Some(model_id) = model_id else {
            outcome.skipped_unknown_model += 1;
            continue;
        };

        let existing: Option<ExistingRow> = sqlx::query_as(
            "SELECT score, ci, rank, source_url, fetched_at FROM benchmarks \
                 WHERE model_id = ? AND source = ? AND category = ?",
        )
        .bind(&model_id)
        .bind(&source)
        .bind(&entry.category)
        .fetch_optional(&mut *tx)
        .await?;

        let unchanged = existing
            .as_ref()
            .is_some_and(|(score, ci, rank, url, fetched)| {
                (*score - entry.score).abs() < f64::EPSILON
                    && *ci == entry.ci
                    && *rank == entry.rank
                    && *url == board.source_url
                    && *fetched == fetched_at
            });
        if unchanged {
            outcome.unchanged += 1;
            continue;
        }
        match existing {
            Some(_) => outcome.updated += 1,
            None => outcome.inserted += 1,
        }
        sqlx::query(
            "INSERT INTO benchmarks \
               (id, model_id, source, category, score, ci, rank, source_url, fetched_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (model_id, source, category) DO UPDATE SET \
               score = excluded.score, ci = excluded.ci, rank = excluded.rank, \
               source_url = excluded.source_url, fetched_at = excluded.fetched_at, \
               updated_at = excluded.updated_at",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&model_id)
        .bind(&source)
        .bind(&entry.category)
        .bind(entry.score)
        .bind(entry.ci)
        .bind(entry.rank)
        .bind(&board.source_url)
        .bind(&fetched_at)
        .bind(catalog::now_ts())
        .execute(&mut *tx)
        .await?;
    }

    catalog::audit(
        &mut tx,
        actor,
        "sync.benchmarks",
        "benchmark_import",
        &source,
        &serde_json::json!({
            "inserted": outcome.inserted,
            "updated": outcome.updated,
            "unchanged": outcome.unchanged,
            "skipped_unknown_model": outcome.skipped_unknown_model,
            // Entry provenance (vendor, votes, rank) lives in the run detail,
            // never in the scored columns (the D19 pattern).
            "entries": board.entries.iter().map(|entry| serde_json::json!({
                "model": entry.model,
                "vendor": entry.vendor,
                "votes": entry.votes,
                "rank": entry.rank,
                "category": entry.category,
            })).collect::<Vec<_>>(),
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(outcome)
}

/// How far outside the imported benchmark range an override may sit before it
/// is treated as a scale error (decision D25). The margin covers extrapolating
/// slightly past the observed field; a `0.99` against Elo scores (1351–1433)
/// is a different scale, not a score.
pub const OVERRIDE_SCALE_MARGIN: f64 = 500.0;

/// Rejects an override that is likely on the wrong scale (decision D25):
/// when benchmarks have been imported, the score must sit within
/// [`OVERRIDE_SCALE_MARGIN`] of the imported range. Without benchmarks there
/// is no scale to check against (`MIN`/`MAX` are NULL). Failure mode:
/// [`BenchmarkError::Malformed`] naming the offending value and the expected
/// range.
async fn validate_override_scale(pool: &SqlitePool, score: f64) -> Result<(), BenchmarkError> {
    let range: (Option<f64>, Option<f64>) =
        sqlx::query_as("SELECT MIN(score), MAX(score) FROM benchmarks WHERE scale = 'elo'")
            .fetch_one(pool)
            .await?;
    let (Some(min), Some(max)) = range else {
        return Ok(());
    };
    if score < min - OVERRIDE_SCALE_MARGIN || score > max + OVERRIDE_SCALE_MARGIN {
        return Err(BenchmarkError::Malformed(format!(
            "override score {score} looks like a different scale: imported benchmark scores \
             range from {min} to {max} (Elo-like); expected a value within \
             {OVERRIDE_SCALE_MARGIN:.0} points of that range"
        )));
    }
    Ok(())
}

/// Sets the operator's quality override for a model (audited as
/// `quality.override`). A later import never overwrites it. On the one
/// Elo-like quality scale (decision D25), a value far outside the imported
/// benchmark range is a likely scale error and is rejected with the range in
/// the message. Failure mode: [`BenchmarkError`].
pub async fn set_override(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    model_id: &str,
    score: f64,
) -> Result<(), BenchmarkError> {
    if !score.is_finite() {
        return Err(BenchmarkError::Malformed(
            "override score must be finite".to_string(),
        ));
    }
    validate_override_scale(pool, score).await?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO model_quality_overrides (model_id, score, updated_by, updated_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT (model_id) DO UPDATE SET \
           score = excluded.score, updated_by = excluded.updated_by, updated_at = excluded.updated_at",
    )
    .bind(model_id)
    .bind(score)
    .bind(actor.user_id)
    .bind(catalog::now_ts())
    .execute(&mut *tx)
    .await?;
    catalog::audit(
        &mut tx,
        actor,
        "quality.override",
        "model",
        model_id,
        &serde_json::json!({ "score": score }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Removes a model's quality override, audited only when one existed (a
/// no-op clear records nothing). Returns whether one existed.
/// Failure mode: [`BenchmarkError`].
pub async fn clear_override(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    model_id: &str,
) -> Result<bool, BenchmarkError> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query("DELETE FROM model_quality_overrides WHERE model_id = ?")
        .bind(model_id)
        .execute(&mut *tx)
        .await?;
    let cleared = result.rows_affected() > 0;
    if cleared {
        catalog::audit(
            &mut tx,
            actor,
            "quality.override_clear",
            "model",
            model_id,
            &serde_json::json!({}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(cleared)
}

/// One imported benchmark score joined with its model's name, as listed by
/// [`list_scores`] (`GET /api/benchmarks`).
#[derive(Debug, serde::Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct BenchmarkScoreRow {
    pub id: String,
    pub model_id: String,
    /// The catalog model's `canonical_name`. Falls back to the raw `model_id`
    /// when the model row is gone — unreachable in practice (the foreign key
    /// cascades benchmark rows away with their model), but the fallback keeps
    /// the row visible instead of hiding imported data.
    pub model_name: String,
    /// Where the score came from (the importer records [`DEFAULT_SOURCE`];
    /// the automatic sources record their own names).
    pub source: String,
    /// `text` or `code`.
    pub category: String,
    /// The source's native scale (D25/D31). Only `elo` feeds routing
    /// quality; `capability` and `percent` are display-only until a later
    /// calibration decision.
    pub scale: String,
    /// On the source's native scale (decision D25).
    pub score: f64,
    /// CI half-width of `score`, in the source's own units, when published.
    pub ci: Option<f64>,
    pub rank: Option<i64>,
    pub source_url: Option<String>,
    /// When the source document was fetched (RFC-3339 UTC).
    pub fetched_at: String,
}

/// One stored quality override joined with its model's name, as listed by
/// [`list_overrides`] (`GET /api/quality-overrides`).
#[derive(Debug, serde::Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct QualityOverrideRow {
    pub model_id: String,
    /// See [`BenchmarkScoreRow::model_name`].
    pub model_name: String,
    /// On the imported benchmark's Elo-like scale (decision D25).
    pub score: f64,
    /// The user id of the operator who last set the override; `NULL` once
    /// that account is deleted (`ON DELETE SET NULL`).
    pub updated_by: Option<String>,
    /// When the override was last set (RFC-3339 UTC).
    pub updated_at: String,
}

/// Every imported benchmark score with its model's name, newest first
/// (`fetched_at` descending; ties — rows of one imported document — break
/// deterministically on `model_id`, then `category`). Failure mode:
/// [`BenchmarkError::Db`].
pub async fn list_scores(pool: &SqlitePool) -> Result<Vec<BenchmarkScoreRow>, BenchmarkError> {
    let rows = sqlx::query_as::<_, BenchmarkScoreRow>(
        "SELECT b.id, b.model_id, \
                COALESCE(m.canonical_name, b.model_id) AS model_name, \
                b.source, b.category, b.scale, b.score, b.ci, b.rank, b.source_url, b.fetched_at \
         FROM benchmarks b LEFT JOIN models m ON m.id = b.model_id \
         ORDER BY b.fetched_at DESC, b.model_id, b.category",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every stored quality override with its model's name, newest first
/// (`updated_at` descending; ties break on `model_id`). Failure mode:
/// [`BenchmarkError::Db`].
pub async fn list_overrides(pool: &SqlitePool) -> Result<Vec<QualityOverrideRow>, BenchmarkError> {
    let rows = sqlx::query_as::<_, QualityOverrideRow>(
        "SELECT o.model_id, COALESCE(m.canonical_name, o.model_id) AS model_name, \
                o.score, o.updated_by, o.updated_at \
         FROM model_quality_overrides o LEFT JOIN models m ON m.id = o.model_id \
         ORDER BY o.updated_at DESC, o.model_id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// A resolved quality score plus the provenance the Models overview shows.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityDetail {
    pub score: f64,
    pub source: routing::QualitySource,
    pub imputed: bool,
    /// Category of the benchmark that supplied the provenance (`text`/`code`),
    /// even when an override supplies the score.
    pub category: Option<String>,
    pub ci: Option<f64>,
    pub rank: Option<i64>,
    /// When the latest benchmark was fetched; `None` without a benchmark.
    pub fetched_at: Option<String>,
    /// True when the operator's override supplied the score.
    pub overridden: bool,
}

/// The resolved routing quality of the given models: override > latest
/// benchmark (wide CIs shrunk toward the leaderboard mean of its
/// `(source, category)`) > neutral model default (0.5, imputed). Every
/// requested model gets an entry. The "latest" benchmark breaks ties on
/// `(source, category)`: generic quality prefers category `text` over `code`
/// (review nit — the alphabetical tie-break used to prefer `code`), then the
/// category name keeps the pick deterministic within one document. Failure
/// mode: [`BenchmarkError`].
pub async fn quality_for_models(
    pool: &SqlitePool,
    model_ids: &[String],
) -> Result<HashMap<String, QualityScore>, BenchmarkError> {
    let details = quality_details_for_models(pool, model_ids).await?;
    Ok(details
        .into_iter()
        .map(|(model_id, detail)| {
            (
                model_id,
                QualityScore {
                    score: detail.score,
                    source: detail.source,
                    imputed: detail.imputed,
                },
            )
        })
        .collect())
}

/// [`quality_for_models`] with the provenance (category, CI, rank, fetch time,
/// and whether an override won) needed by the Models overview. Same
/// resolution order and tie-break; failure mode: [`BenchmarkError`].
pub async fn quality_details_for_models(
    pool: &SqlitePool,
    model_ids: &[String],
) -> Result<HashMap<String, QualityDetail>, BenchmarkError> {
    let mut quality: HashMap<String, QualityDetail> = HashMap::new();
    if model_ids.is_empty() {
        return Ok(quality);
    }
    let placeholders = vec!["?"; model_ids.len()].join(", ");

    let overrides_sql = format!(
        "SELECT model_id, score FROM model_quality_overrides WHERE model_id IN ({placeholders})"
    );
    let mut overrides_query = sqlx::query_as::<_, (String, f64)>(&overrides_sql);
    for model_id in model_ids {
        overrides_query = overrides_query.bind(model_id);
    }
    let overrides: HashMap<String, f64> =
        overrides_query.fetch_all(pool).await?.into_iter().collect();

    // Newest first; ties break on (source, text before code, category) so the
    // "latest benchmark" is deterministic within one document — generic
    // quality prefers the `text` leaderboard (review nit).
    let rows_sql = format!(
        "SELECT model_id, source, category, score, ci, rank, fetched_at FROM benchmarks \
         WHERE model_id IN ({placeholders}) AND scale = 'elo' \
         ORDER BY fetched_at DESC, source, (category = 'text') DESC, category"
    );
    let mut rows_query = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            f64,
            Option<f64>,
            Option<i64>,
            String,
        ),
    >(&rows_sql);
    for model_id in model_ids {
        rows_query = rows_query.bind(model_id);
    }
    let rows = rows_query.fetch_all(pool).await?;

    // The mean each leaderboard's scores shrink toward.
    let means: Vec<(String, String, f64)> = sqlx::query_as(
        "SELECT source, category, AVG(score) FROM benchmarks WHERE scale = 'elo' \
         GROUP BY source, category",
    )
    .fetch_all(pool)
    .await?;
    let means: HashMap<(String, String), f64> = means
        .into_iter()
        .map(|(source, category, mean)| ((source, category), mean))
        .collect();

    let mut latest: HashMap<String, LatestBenchmark> = HashMap::new();
    for (model_id, source, category, score, ci, rank, fetched) in rows {
        latest
            .entry(model_id)
            .or_insert((source, category, score, ci, rank, fetched));
    }

    for model_id in model_ids {
        let benchmark = latest
            .get(model_id)
            .map(|(source, category, score, ci, rank, fetched)| {
                let mean = means
                    .get(&(source.clone(), category.clone()))
                    .copied()
                    .unwrap_or(*score);
                (*score, *ci, mean, category.clone(), *rank, fetched.clone())
            });
        let overridden = overrides.get(model_id).copied();
        let (resolved, category, rank, fetched_at) = match benchmark {
            Some((score, ci, mean, category, rank, fetched)) => (
                routing::resolve_quality(overridden, Some((score, ci)), Some(mean)),
                Some(category),
                rank,
                Some(fetched),
            ),
            None => (
                routing::resolve_quality(overridden, None, None),
                None,
                None,
                None,
            ),
        };
        quality.insert(
            model_id.clone(),
            QualityDetail {
                score: resolved.score,
                source: resolved.source,
                imputed: resolved.imputed,
                category,
                ci: benchmark_ci(&latest, model_id),
                rank,
                fetched_at,
                overridden: overridden.is_some(),
            },
        );
    }
    Ok(quality)
}

/// One selected benchmark row: source, category, score, CI, rank, fetched_at.
type LatestBenchmark = (String, String, f64, Option<f64>, Option<i64>, String);

/// The CI of the selected (latest) benchmark, if any.
fn benchmark_ci(latest: &HashMap<String, LatestBenchmark>, model_id: &str) -> Option<f64> {
    latest.get(model_id).and_then(|row| row.3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::catalog::ModelInput;
    use crate::test_support::TestApp;

    const FIXTURE: &str = include_str!("../tests/fixtures/leaderboard.json");

    struct Seeded {
        app: TestApp,
        uid: String,
        /// canonical_name -> model id.
        models: HashMap<String, String>,
    }

    async fn seeded() -> Seeded {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let mut models = HashMap::new();
        for name in [
            "gemini-2.5-pro",
            "claude-sonnet-4.5",
            "gpt-4o",
            "deepseek-v3",
        ] {
            let row = catalog::create_model(
                &app.state.pool,
                &Actor {
                    user_id: &uid,
                    ip: "127.0.0.1",
                },
                &ModelInput {
                    canonical_name: name.to_string(),
                    family: None,
                    modalities_input: None,
                    modalities_output: None,
                    context_length: None,
                    max_output_tokens: None,
                    capabilities: None,
                },
            )
            .await
            .expect("model");
            models.insert(name.to_string(), row.id);
        }
        Seeded { app, uid, models }
    }

    fn actor(uid: &str) -> Actor<'_> {
        Actor {
            user_id: uid,
            ip: "127.0.0.1",
        }
    }

    /// `quality.override_clear` audit rows written so far.
    async fn clear_audits(s: &Seeded) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'quality.override_clear'")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("audit count")
    }

    #[tokio::test]
    async fn clearing_without_an_override_is_not_audited() {
        let s = seeded().await;
        let claude = &s.models["claude-sonnet-4.5"];

        // Nothing existed: the clear is a no-op and writes no audit row.
        assert!(
            !clear_override(&s.app.state.pool, &actor(&s.uid), claude)
                .await
                .expect("clear")
        );
        assert_eq!(clear_audits(&s).await, 0, "a no-op clear is not audited");

        // With an override, the clear is audited exactly once.
        set_override(&s.app.state.pool, &actor(&s.uid), claude, 1450.0)
            .await
            .expect("override");
        assert!(
            clear_override(&s.app.state.pool, &actor(&s.uid), claude)
                .await
                .expect("clear")
        );
        assert_eq!(clear_audits(&s).await, 1, "a real clear is audited");
    }

    #[test]
    fn the_fixture_parses_with_six_realistic_entries() {
        let board = parse_leaderboard(FIXTURE).expect("fixture parses");
        assert_eq!(board.entries.len(), 6);
        assert_eq!(board.source.as_deref(), Some("lmarena"));
        assert!(board.entries.iter().any(|e| e.category == "code"));
        assert!(
            board
                .entries
                .iter()
                .all(|e| e.rank.is_some() && e.votes.is_some())
        );
    }

    #[test]
    fn malformed_documents_are_rejected_by_name() {
        let err = parse_leaderboard(r#"{"entries": []}"#).expect_err("empty");
        assert!(err.to_string().contains("entries"), "{err}");
        let err =
            parse_leaderboard(r#"{"entries": [{"model": "m", "score": 1.0, "category": "math"}]}"#)
                .expect_err("category");
        assert!(err.to_string().contains("category"), "{err}");
        let err =
            parse_leaderboard(r#"{"entries": [{"model": "", "score": 1.0, "category": "text"}]}"#)
                .expect_err("model");
        assert!(err.to_string().contains("model"), "{err}");
        let err = parse_leaderboard("not json").expect_err("json");
        assert!(err.to_string().contains("JSON"), "{err}");
    }

    #[tokio::test]
    async fn import_seeds_benchmarks_and_is_idempotent() {
        let s = seeded().await;
        let first = run_import(&s.app.state.pool, &actor(&s.uid), FIXTURE)
            .await
            .expect("first import");
        assert_eq!(first.inserted, 6);
        assert_eq!(first.updated, 0);
        assert_eq!(first.unchanged, 0);
        assert_eq!(first.skipped_unknown_model, 0);

        let second = run_import(&s.app.state.pool, &actor(&s.uid), FIXTURE)
            .await
            .expect("second import");
        assert_eq!(second, first_but_unchanged());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM benchmarks")
            .fetch_one(&s.app.state.pool)
            .await
            .expect("count");
        assert_eq!(count, 6, "no duplicate rows");

        let (score, ci, rank): (f64, Option<f64>, Option<i64>) = sqlx::query_as(
            "SELECT score, ci, rank FROM benchmarks WHERE model_id = ? AND category = 'text'",
        )
        .bind(&s.models["gemini-2.5-pro"])
        .fetch_one(&s.app.state.pool)
        .await
        .expect("gemini row");
        assert_eq!(score, 1432.5);
        assert_eq!(ci, Some(45.0));
        assert_eq!(rank, Some(1));
    }

    fn first_but_unchanged() -> ImportOutcome {
        ImportOutcome {
            inserted: 0,
            updated: 0,
            unchanged: 6,
            skipped_unknown_model: 0,
        }
    }

    #[tokio::test]
    async fn a_quality_override_survives_a_later_import_and_wins() {
        let s = seeded().await;
        let claude = &s.models["claude-sonnet-4.5"];
        // Elo-scale override values everywhere (decision D25).
        set_override(&s.app.state.pool, &actor(&s.uid), claude, 1450.0)
            .await
            .expect("override");
        run_import(&s.app.state.pool, &actor(&s.uid), FIXTURE)
            .await
            .expect("import");
        run_import(&s.app.state.pool, &actor(&s.uid), FIXTURE)
            .await
            .expect("re-import");

        let row: Option<f64> =
            sqlx::query_scalar("SELECT score FROM model_quality_overrides WHERE model_id = ?")
                .bind(claude)
                .fetch_one(&s.app.state.pool)
                .await
                .expect("override row");
        assert_eq!(row, Some(1450.0), "the import never touches overrides");

        let quality = quality_for_models(&s.app.state.pool, std::slice::from_ref(claude))
            .await
            .expect("quality");
        let claude_quality = &quality[claude];
        assert_eq!(claude_quality.source, routing::QualitySource::Override);
        assert_eq!(claude_quality.score, 1450.0);

        // Clearing falls back to the synced benchmark again.
        assert!(
            clear_override(&s.app.state.pool, &actor(&s.uid), claude)
                .await
                .expect("clear")
        );
        let quality = quality_for_models(&s.app.state.pool, std::slice::from_ref(claude))
            .await
            .expect("quality");
        assert_ne!(quality[claude].source, routing::QualitySource::Override);
    }

    #[tokio::test]
    async fn mixed_quality_scales_are_rejected_before_they_can_rank() {
        // The M4 regression: a `0.99` override against Elo benchmarks used to
        // be accepted and then silently ranked last after min-max. Decision
        // D25 rejects likely scale errors up front, naming the range.
        let s = seeded().await;
        let claude = &s.models["claude-sonnet-4.5"];

        // Before any import there is no scale to check against (D25).
        set_override(&s.app.state.pool, &actor(&s.uid), claude, 0.99)
            .await
            .expect("without benchmarks any finite score is accepted");
        clear_override(&s.app.state.pool, &actor(&s.uid), claude)
            .await
            .expect("clear");

        run_import(&s.app.state.pool, &actor(&s.uid), FIXTURE)
            .await
            .expect("import");
        for wrong_scale in [0.99, 0.5, 3000.0] {
            let err = set_override(&s.app.state.pool, &actor(&s.uid), claude, wrong_scale)
                .await
                .expect_err("a different scale is rejected");
            let message = err.to_string();
            assert!(message.contains("scale"), "{message}");
            assert!(
                message.contains("1351.4"),
                "the message names the imported range: {message}"
            );
        }

        // An in-scale override is accepted and ranks by its score — 1450 sits
        // above every imported entry, so claude leads the quality order.
        set_override(&s.app.state.pool, &actor(&s.uid), claude, 1450.0)
            .await
            .expect("in-scale override");
        let ids = vec![claude.clone(), s.models["gemini-2.5-pro"].clone()];
        let quality = quality_for_models(&s.app.state.pool, &ids)
            .await
            .expect("quality");
        assert_eq!(quality[claude].source, routing::QualitySource::Override);
        assert!(quality[claude].score > quality[&s.models["gemini-2.5-pro"]].score);
    }

    #[tokio::test]
    async fn generic_quality_prefers_the_text_leaderboard_on_ties() {
        // Review nit: claude-sonnet-4.5 has `text` and `code` rows in one
        // document (same fetched_at). Generic quality prefers `text`.
        let s = seeded().await;
        run_import(&s.app.state.pool, &actor(&s.uid), FIXTURE)
            .await
            .expect("import");
        let claude = &s.models["claude-sonnet-4.5"];
        let quality = quality_for_models(&s.app.state.pool, std::slice::from_ref(claude))
            .await
            .expect("quality");
        let claude_quality = &quality[claude];
        assert_eq!(
            claude_quality.source,
            routing::QualitySource::BenchmarkShrunk
        );

        // The text entry (1412.7, ci 11.4) shrunk toward the text mean — not
        // the code entry (1420.5).
        let text_mean = (1432.5 + 1412.7 + 1401.2 + 1351.4) / 4.0;
        let expected = routing::shrink(1412.7, Some(11.4), text_mean);
        assert!(
            (claude_quality.score - expected).abs() < 1e-9,
            "score {} != {expected}",
            claude_quality.score
        );
    }

    #[tokio::test]
    async fn wide_cis_shrink_toward_the_leaderboard_mean() {
        let s = seeded().await;
        run_import(&s.app.state.pool, &actor(&s.uid), FIXTURE)
            .await
            .expect("import");
        let gemini = &s.models["gemini-2.5-pro"];
        let quality = quality_for_models(&s.app.state.pool, std::slice::from_ref(gemini))
            .await
            .expect("quality");
        let gemini_quality = &quality[gemini];
        assert_eq!(
            gemini_quality.source,
            routing::QualitySource::BenchmarkShrunk
        );

        // The text leaderboard mean over the four text entries.
        let mean = (1432.5 + 1412.7 + 1401.2 + 1351.4) / 4.0;
        let expected = routing::shrink(1432.5, Some(45.0), mean);
        assert!(
            (gemini_quality.score - expected).abs() < 1e-9,
            "score {} != {expected}",
            gemini_quality.score
        );
        assert!(
            gemini_quality.score < 1432.5 && gemini_quality.score > mean,
            "shrunk toward {mean}: {}",
            gemini_quality.score
        );
    }

    #[tokio::test]
    async fn unknown_models_are_skipped_and_counted() {
        let s = seeded().await;
        let raw = r#"{
            "source": "lmarena",
            "fetched_at": "2026-11-10T00:00:00.000Z",
            "entries": [
                {"rank": 1, "model": "no-such-model", "vendor": "Acme", "score": 1000.0, "ci": 5.0, "votes": 10, "category": "text"}
            ]
        }"#;
        let outcome = run_import(&s.app.state.pool, &actor(&s.uid), raw)
            .await
            .expect("import");
        assert_eq!(outcome.skipped_unknown_model, 1);
        assert_eq!(outcome.inserted, 0);
    }

    #[tokio::test]
    async fn models_without_any_benchmark_get_the_imputed_default() {
        let s = seeded().await;
        run_import(&s.app.state.pool, &actor(&s.uid), FIXTURE)
            .await
            .expect("import");
        // gpt-4o has a text entry but no code entry; the latest overall row
        // still resolves. A model absent from the leaderboard gets the
        // neutral default.
        let gpt = &s.models["gpt-4o"];
        let quality = quality_for_models(&s.app.state.pool, std::slice::from_ref(gpt))
            .await
            .expect("quality");
        assert!(!quality[gpt].imputed, "gpt-4o has a benchmark");

        let missing_id = "model-without-benchmarks".to_string();
        let quality = quality_for_models(&s.app.state.pool, std::slice::from_ref(&missing_id))
            .await
            .expect("quality");
        let missing = &quality[&missing_id];
        assert_eq!(missing.score, 0.5);
        assert!(missing.imputed);
        assert_eq!(missing.source, routing::QualitySource::ModelDefault);
    }

    #[tokio::test]
    async fn the_lists_return_what_the_write_paths_stored_newest_first() {
        let s = seeded().await;
        assert!(
            list_scores(&s.app.state.pool)
                .await
                .expect("scores")
                .is_empty(),
            "nothing imported yet"
        );
        assert!(
            list_overrides(&s.app.state.pool)
                .await
                .expect("overrides")
                .is_empty(),
            "no override set yet"
        );

        run_import(&s.app.state.pool, &actor(&s.uid), FIXTURE)
            .await
            .expect("import");
        set_override(
            &s.app.state.pool,
            &actor(&s.uid),
            &s.models["claude-sonnet-4.5"],
            1450.0,
        )
        .await
        .expect("override");

        // A second document with a later fetch time must list first.
        let later = r#"{
            "source": "alt",
            "fetched_at": "2026-11-12T00:00:00.000Z",
            "entries": [{"model": "gpt-4o", "score": 1410.0, "category": "text"}]
        }"#;
        run_import(&s.app.state.pool, &actor(&s.uid), later)
            .await
            .expect("second import");

        let scores = list_scores(&s.app.state.pool).await.expect("scores");
        assert_eq!(scores.len(), 7);
        assert_eq!(scores[0].source, "alt", "the newer document lists first");
        assert_eq!(scores[0].model_name, "gpt-4o");
        assert_eq!(scores[0].fetched_at, "2026-11-12T00:00:00.000Z");
        assert!(scores[1..].iter().all(|row| row.source == "lmarena"));
        let gemini = scores
            .iter()
            .find(|row| row.model_name == "gemini-2.5-pro" && row.category == "text")
            .expect("gemini text row");
        assert_eq!(gemini.model_id, s.models["gemini-2.5-pro"]);
        assert_eq!(gemini.score, 1432.5);
        assert_eq!(gemini.ci, Some(45.0));
        assert_eq!(gemini.rank, Some(1));
        assert_eq!(
            gemini.source_url.as_deref(),
            Some("https://lmarena.ai/leaderboard")
        );

        let overrides = list_overrides(&s.app.state.pool).await.expect("overrides");
        assert_eq!(overrides.len(), 1);
        let claude = &overrides[0];
        assert_eq!(claude.model_id, s.models["claude-sonnet-4.5"]);
        assert_eq!(claude.model_name, "claude-sonnet-4.5");
        assert_eq!(claude.score, 1450.0);
        assert_eq!(claude.updated_by.as_deref(), Some(s.uid.as_str()));
        assert!(!claude.updated_at.is_empty());

        // Newest first by `updated_at`: pin both rows' timestamps and check
        // the later one lists first.
        set_override(
            &s.app.state.pool,
            &actor(&s.uid),
            &s.models["gpt-4o"],
            1300.0,
        )
        .await
        .expect("second override");
        for (model_id, at) in [
            (
                s.models["claude-sonnet-4.5"].as_str(),
                "2026-01-01T00:00:00.000Z",
            ),
            (s.models["gpt-4o"].as_str(), "2026-01-02T00:00:00.000Z"),
        ] {
            sqlx::query("UPDATE model_quality_overrides SET updated_at = ? WHERE model_id = ?")
                .bind(at)
                .bind(model_id)
                .execute(&s.app.state.pool)
                .await
                .expect("pin updated_at");
        }
        let overrides = list_overrides(&s.app.state.pool).await.expect("overrides");
        let names: Vec<&str> = overrides
            .iter()
            .map(|row| row.model_name.as_str())
            .collect();
        assert_eq!(names, ["gpt-4o", "claude-sonnet-4.5"], "newest first");
    }
}
