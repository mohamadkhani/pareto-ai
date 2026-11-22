//! Catalog-side use of [`crate::model_identity`]: the write-time index that
//! joins a provider's upstream id to an existing model, and the one-time
//! repair that merges rows created before the rule existed.
//!
//! Discovery and the OpenRouter price sync both call [`load_index`] once per
//! transaction, then resolve each discovered id with
//! [`crate::model_identity::CatalogIndex::resolve`]. Exact names win, a unique
//! strict identity joins, and several candidates stay separate — the operator
//! may have meant them as different rows.
//!
//! The repair is not part of routing: it runs once after migrations, guarded
//! by a `db_meta` key, and only merges models whose `canonical_name` collapses
//! to the same [`crate::model_identity::catalog_identity`]. Everything that
//! points at a duplicate is repointed to the oldest row (keeper): offerings,
//! benchmarks, quality overrides, token caps, grants, and usage events. Where
//! a single-row constraint would collide, the keeper wins (benchmarks and
//! overrides), token-cap usage is summed, and duplicate grants are dropped in
//! favour of the keeper's rule. The duplicate model row is deleted last.

use sqlx::{SqliteConnection, SqlitePool};

use crate::model_identity::{CatalogIndex, catalog_identity};

use super::CatalogError;

/// `db_meta` key recording that the one-time repair already ran.
const REPAIR_META_KEY: &str = "model_identity_repair_v1";

/// Loads the write-time index over every catalog model.
///
/// Failure mode: [`sqlx::Error`] from the `models` read.
pub(crate) async fn load_index(conn: &mut SqliteConnection) -> Result<CatalogIndex, sqlx::Error> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, canonical_name FROM models")
        .fetch_all(&mut *conn)
        .await?;
    let registry = super::registry::load(conn).await?;
    Ok(CatalogIndex::with_registry(rows, registry))
}

#[derive(Debug, sqlx::FromRow)]
struct ModelRow {
    id: String,
    canonical_name: String,
    family: Option<String>,
    modalities_input: Option<String>,
    modalities_output: Option<String>,
    context_length: Option<i64>,
    max_output_tokens: Option<i64>,
    capabilities_json: Option<String>,
    created_at: String,
}

/// Merges the model rows that share a strict catalog identity, once.
///
/// Returns the number of rows deleted (0 when there was nothing to merge or
/// the repair already ran). Failure mode: [`CatalogError`]; the whole merge
/// and its audit row are one transaction, so a failure leaves the catalog
/// untouched.
pub async fn repair_duplicate_models(pool: &SqlitePool) -> Result<usize, CatalogError> {
    let done: Option<String> = sqlx::query_scalar("SELECT value FROM db_meta WHERE key = ?")
        .bind(REPAIR_META_KEY)
        .fetch_optional(pool)
        .await?;
    if done.is_some() {
        return Ok(0);
    }

    let mut tx = pool.begin().await?;
    let rows: Vec<ModelRow> = sqlx::query_as(
        "SELECT id, canonical_name, family, modalities_input, modalities_output, \
                context_length, max_output_tokens, capabilities_json, created_at \
         FROM models ORDER BY created_at, id",
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut merged = 0usize;
    let mut merged_rows: Vec<serde_json::Value> = Vec::new();

    // First pass: names that differ only by ASCII case (`Auto` / `auto`) are
    // the same row, even when the spelling identity refuses a bare word.
    let mut case_groups: std::collections::HashMap<String, Vec<ModelRow>> =
        std::collections::HashMap::new();
    for row in rows {
        case_groups
            .entry(row.canonical_name.trim().to_ascii_lowercase())
            .or_default()
            .push(row);
    }
    merged += merge_groups(&mut tx, case_groups, &mut merged_rows).await?;

    // Second pass: reload after the case merges, then group by the strict
    // spelling identity.
    let rows: Vec<ModelRow> = sqlx::query_as(
        "SELECT id, canonical_name, family, modalities_input, modalities_output, \
                context_length, max_output_tokens, capabilities_json, created_at \
         FROM models ORDER BY created_at, id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut identity_groups: std::collections::HashMap<String, Vec<ModelRow>> =
        std::collections::HashMap::new();
    for row in rows {
        let Some(identity) = catalog_identity(&row.canonical_name) else {
            continue;
        };
        identity_groups.entry(identity).or_default().push(row);
    }
    merged += merge_groups(&mut tx, identity_groups, &mut merged_rows).await?;

    if merged > 0 {
        let detail = serde_json::json!({
            "merged": merged,
            "rows": merged_rows,
        });
        crate::auth::audit(
            &mut *tx,
            None,
            "catalog.merge_duplicate_models",
            Some("catalog"),
            None,
            Some(detail.to_string().as_str()),
            None,
        )
        .await?;
    }

    sqlx::query(
        "INSERT INTO db_meta (key, value) VALUES (?, ?) \
         ON CONFLICT (key) DO NOTHING",
    )
    .bind(REPAIR_META_KEY)
    .bind(crate::catalog::now_ts())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    if merged > 0 {
        tracing::info!(merged, "merged duplicate catalog models by identity");
    }
    Ok(merged)
}

/// Merges every group with more than one row into its oldest member. Shared by
/// the case-only and the spelling-identity passes.
async fn merge_groups(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    groups: std::collections::HashMap<String, Vec<ModelRow>>,
    merged_rows: &mut Vec<serde_json::Value>,
) -> Result<usize, CatalogError> {
    let mut merged = 0usize;
    let now = crate::catalog::now_ts();
    for mut group in groups.into_values() {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        let keeper = group.remove(0);
        let merged_fields = merge_fields(&keeper, &group);
        sqlx::query(
            "UPDATE models SET family = ?, modalities_input = ?, modalities_output = ?, \
                context_length = ?, max_output_tokens = ?, capabilities_json = ?, \
                updated_at = ? \
              WHERE id = ?",
        )
        .bind(&merged_fields.family)
        .bind(&merged_fields.modalities_input)
        .bind(&merged_fields.modalities_output)
        .bind(merged_fields.context_length)
        .bind(merged_fields.max_output_tokens)
        .bind(&merged_fields.capabilities_json)
        .bind(&now)
        .bind(&keeper.id)
        .execute(&mut **tx)
        .await?;

        for duplicate in &group {
            repoint_offerings(tx, &duplicate.id, &keeper.id).await?;
            merge_benchmarks(tx, &duplicate.id, &keeper.id).await?;
            merge_override(tx, &duplicate.id, &keeper.id).await?;
            merge_token_caps(tx, &duplicate.id, &keeper.id).await?;
            merge_grants(tx, &duplicate.id, &keeper.id).await?;
            sqlx::query("UPDATE usage_events SET model_id = ? WHERE model_id = ?")
                .bind(&keeper.id)
                .bind(&duplicate.id)
                .execute(&mut **tx)
                .await?;
            // Every reference now points at the keeper; cascades would have
            // nothing left to remove.
            sqlx::query("DELETE FROM models WHERE id = ?")
                .bind(&duplicate.id)
                .execute(&mut **tx)
                .await?;
            merged_rows.push(serde_json::json!({
                "duplicate": duplicate.id,
                "keeper": keeper.id,
                "canonical_name": duplicate.canonical_name,
            }));
            merged += 1;
        }
    }
    Ok(merged)
}

struct MergedFields {
    family: Option<String>,
    modalities_input: Option<String>,
    modalities_output: Option<String>,
    context_length: Option<i64>,
    max_output_tokens: Option<i64>,
    capabilities_json: Option<String>,
}

/// Fills the keeper's empty fields from the duplicates; a non-NULL keeper
/// value always wins, so the oldest row's operator data is never replaced.
/// An empty modalities array (`"[]"`) counts as empty, like NULL.
fn merge_fields(keeper: &ModelRow, duplicates: &[ModelRow]) -> MergedFields {
    let mut merged = MergedFields {
        family: non_empty(&keeper.family),
        modalities_input: non_empty(&keeper.modalities_input),
        modalities_output: non_empty(&keeper.modalities_output),
        context_length: keeper.context_length,
        max_output_tokens: keeper.max_output_tokens,
        capabilities_json: non_empty(&keeper.capabilities_json),
    };
    for row in duplicates {
        merged.family = merged.family.clone().or_else(|| non_empty(&row.family));
        merged.modalities_input = merged
            .modalities_input
            .clone()
            .or_else(|| non_empty(&row.modalities_input));
        merged.modalities_output = merged
            .modalities_output
            .clone()
            .or_else(|| non_empty(&row.modalities_output));
        merged.context_length = merged.context_length.or(row.context_length);
        merged.max_output_tokens = merged.max_output_tokens.or(row.max_output_tokens);
        merged.capabilities_json = merged
            .capabilities_json
            .clone()
            .or_else(|| non_empty(&row.capabilities_json));
    }
    merged
}

/// A string field that carries information; NULL and an empty JSON array
/// (`"[]"`, what an earlier sync wrote for "no modalities") are both empty.
fn non_empty(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .filter(|value| value.as_str() != "[]")
        .cloned()
}

async fn repoint_offerings(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    duplicate: &str,
    keeper: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE offerings SET model_id = ? WHERE model_id = ?")
        .bind(keeper)
        .bind(duplicate)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn merge_benchmarks(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    duplicate: &str,
    keeper: &str,
) -> Result<(), sqlx::Error> {
    // UNIQUE(model_id, source, category): the keeper's row for the same
    // source/category wins; the duplicate's copy is dropped first.
    sqlx::query(
        "DELETE FROM benchmarks \
          WHERE model_id = ? \
            AND (source, category) IN (SELECT source, category FROM benchmarks WHERE model_id = ?)",
    )
    .bind(duplicate)
    .bind(keeper)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE benchmarks SET model_id = ? WHERE model_id = ?")
        .bind(keeper)
        .bind(duplicate)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn merge_override(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    duplicate: &str,
    keeper: &str,
) -> Result<(), sqlx::Error> {
    // model_quality_overrides is keyed by model_id: keep the keeper's manual
    // score when both rows have one, otherwise move the duplicate's.
    sqlx::query(
        "DELETE FROM model_quality_overrides \
          WHERE model_id = ? AND EXISTS (SELECT 1 FROM model_quality_overrides WHERE model_id = ?)",
    )
    .bind(duplicate)
    .bind(keeper)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE model_quality_overrides SET model_id = ? WHERE model_id = ?")
        .bind(keeper)
        .bind(duplicate)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn merge_token_caps(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    duplicate: &str,
    keeper: &str,
) -> Result<(), sqlx::Error> {
    // UNIQUE(user_id, model_id, period, period_start): usage from the
    // duplicate's colliding bucket is added to the keeper's; the keeper's
    // limit stays. Non-colliding caps are simply repointed.
    sqlx::query(
        "UPDATE token_caps AS keeper \
            SET used_tokens = keeper.used_tokens + COALESCE(( \
                  SELECT dup.used_tokens FROM token_caps AS dup \
                   WHERE dup.model_id = ? AND dup.user_id = keeper.user_id \
                     AND dup.period = keeper.period \
                     AND dup.period_start = keeper.period_start), 0) \
          WHERE keeper.model_id = ? \
            AND EXISTS (SELECT 1 FROM token_caps AS dup \
                         WHERE dup.model_id = ? AND dup.user_id = keeper.user_id \
                           AND dup.period = keeper.period \
                           AND dup.period_start = keeper.period_start)",
    )
    .bind(duplicate)
    .bind(keeper)
    .bind(duplicate)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "DELETE FROM token_caps \
          WHERE model_id = ? \
            AND EXISTS (SELECT 1 FROM token_caps AS keeper \
                         WHERE keeper.model_id = ? \
                           AND keeper.user_id = token_caps.user_id \
                           AND keeper.period = token_caps.period \
                           AND keeper.period_start = token_caps.period_start)",
    )
    .bind(duplicate)
    .bind(keeper)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE token_caps SET model_id = ? WHERE model_id = ?")
        .bind(keeper)
        .bind(duplicate)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn merge_grants(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    duplicate: &str,
    keeper: &str,
) -> Result<(), sqlx::Error> {
    // UNIQUE(subject, resource): a duplicate grant for a subject already
    // granted/denied on the keeper is dropped; the keeper's mode wins.
    sqlx::query(
        "DELETE FROM grants \
          WHERE resource_type = 'model' AND resource_id = ? \
            AND EXISTS (SELECT 1 FROM grants AS keeper \
                         WHERE keeper.resource_type = 'model' AND keeper.resource_id = ? \
                           AND keeper.subject_type = grants.subject_type \
                           AND keeper.subject_id = grants.subject_id)",
    )
    .bind(duplicate)
    .bind(keeper)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE grants SET resource_id = ? \
          WHERE resource_type = 'model' AND resource_id = ?",
    )
    .bind(keeper)
    .bind(duplicate)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::test_support::TestApp;

    const NOW: &str = "2026-11-21T00:00:00.000Z";

    async fn seed_model(app: &TestApp, id: &str, name: &str, created_at: &str) {
        sqlx::query(
            "INSERT INTO models (id, canonical_name, created_at, updated_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(created_at)
        .bind(created_at)
        .execute(&app.state.pool)
        .await
        .expect("model");
    }

    async fn seed_offering(
        app: &TestApp,
        id: &str,
        provider_id: &str,
        model_id: &str,
        upstream: &str,
    ) {
        sqlx::query(
            "INSERT INTO offerings \
               (id, provider_id, model_id, upstream_model_id, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(id)
        .bind(provider_id)
        .bind(model_id)
        .bind(upstream)
        .bind(NOW)
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("offering");
    }

    #[tokio::test]
    async fn the_repair_merges_one_identity_and_repoints_every_reference() {
        let app = TestApp::new().await;
        let user_id = app
            .create_user("admin@example.com", Role::Admin, true)
            .await;
        sqlx::query("DELETE FROM db_meta WHERE key = ?")
            .bind(REPAIR_META_KEY)
            .execute(&app.state.pool)
            .await
            .expect("clear guard");
        sqlx::query(
            "INSERT INTO providers (id, name, base_url, enabled, created_at, updated_at) \
             VALUES ('prov-a', 'alpha', 'https://a.test/v1', 1, ?, ?), \
                    ('prov-b', 'beta', 'https://b.test/v1', 1, ?, ?)",
        )
        .bind(NOW)
        .bind(NOW)
        .bind(NOW)
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("providers");

        // The oldest row is the keeper; the newer one is the duplicate.
        seed_model(
            &app,
            "keep",
            "deepseek-ai/DeepSeek-V4.1-Flash-1017",
            "2026-11-01T00:00:00.000Z",
        )
        .await;
        seed_model(
            &app,
            "dup",
            "deepseek/deepseek-v4.1-flash-1017",
            "2026-11-02T00:00:00.000Z",
        )
        .await;
        // A distinct snapshot must stay its own row.
        seed_model(
            &app,
            "snapshot",
            "deepseek/deepseek-v4.1-flash",
            "2026-11-03T00:00:00.000Z",
        )
        .await;
        sqlx::query("UPDATE models SET family = 'deepseek' WHERE id = 'dup'")
            .execute(&app.state.pool)
            .await
            .expect("family");

        seed_offering(&app, "off-keep", "prov-a", "keep", "a-1").await;
        seed_offering(&app, "off-dup", "prov-b", "dup", "b-1").await;

        // A conflicting benchmark and a unique one on the duplicate.
        for (id, model, source, category, score) in [
            ("b-keep", "keep", "lmarena", "text", 1400.0),
            ("b-dup", "dup", "lmarena", "text", 1300.0),
            ("b-unique", "dup", "arena", "code", 1500.0),
        ] {
            sqlx::query(
                "INSERT INTO benchmarks \
                   (id, model_id, source, category, score, fetched_at, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(model)
            .bind(source)
            .bind(category)
            .bind(score)
            .bind(NOW)
            .bind(NOW)
            .bind(NOW)
            .execute(&app.state.pool)
            .await
            .expect("benchmark");
        }

        // Override, token cap, grant, and usage on the duplicate.
        sqlx::query(
            "INSERT INTO model_quality_overrides (model_id, score, updated_at) \
             VALUES ('dup', 1900.0, ?)",
        )
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("override");
        for (id, model, used) in [("cap-keep", "keep", 10), ("cap-dup", "dup", 5)] {
            sqlx::query(
                "INSERT INTO token_caps \
                   (id, user_id, model_id, period, period_start, limit_tokens, used_tokens, \
                    created_at, updated_at) \
                 VALUES (?, ?, ?, 'month', '2026-11-01', 100, ?, ?, ?)",
            )
            .bind(id)
            .bind(&user_id)
            .bind(model)
            .bind(used)
            .bind(NOW)
            .bind(NOW)
            .execute(&app.state.pool)
            .await
            .expect("cap");
        }
        sqlx::query(
            "INSERT INTO grants \
               (id, subject_type, subject_id, resource_type, resource_id, mode, created_at) \
             VALUES ('g-keep', 'user', ?, 'model', 'keep', 'allow', ?), \
                    ('g-dup', 'user', ?, 'model', 'dup', 'deny', ?)",
        )
        .bind(&user_id)
        .bind(NOW)
        .bind(&user_id)
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("grants");
        sqlx::query(
            "INSERT INTO usage_events (request_id, model_id, occurred_at) \
             VALUES ('req-1', 'dup', ?)",
        )
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("usage event");

        let merged = repair_duplicate_models(&app.state.pool)
            .await
            .expect("repair");
        assert_eq!(merged, 1);

        let models: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT id, canonical_name, family FROM models ORDER BY id")
                .fetch_all(&app.state.pool)
                .await
                .expect("models");
        assert_eq!(
            models.len(),
            2,
            "the distinct snapshot survives: {models:?}"
        );
        assert_eq!(models[0].0, "keep");
        assert_eq!(models[0].1, "deepseek-ai/DeepSeek-V4.1-Flash-1017");
        assert_eq!(models[0].2.as_deref(), Some("deepseek"), "filled from dup");
        assert_eq!(models[1].0, "snapshot");

        let offerings: Vec<(String, String)> =
            sqlx::query_as("SELECT id, model_id FROM offerings ORDER BY id")
                .fetch_all(&app.state.pool)
                .await
                .expect("offerings");
        assert_eq!(
            offerings,
            [
                ("off-dup".to_string(), "keep".to_string()),
                ("off-keep".to_string(), "keep".to_string())
            ]
        );

        let benchmarks: Vec<(String, String, String)> =
            sqlx::query_as("SELECT id, model_id, source FROM benchmarks ORDER BY id")
                .fetch_all(&app.state.pool)
                .await
                .expect("benchmarks");
        assert_eq!(
            benchmarks,
            [
                (
                    "b-keep".to_string(),
                    "keep".to_string(),
                    "lmarena".to_string()
                ),
                (
                    "b-unique".to_string(),
                    "keep".to_string(),
                    "arena".to_string()
                ),
            ],
            "the keeper's conflicting row wins; the unique one moves"
        );

        let overrides: Vec<(String, f64)> =
            sqlx::query_as("SELECT model_id, score FROM model_quality_overrides")
                .fetch_all(&app.state.pool)
                .await
                .expect("overrides");
        assert_eq!(overrides, [("keep".to_string(), 1900.0)]);

        let caps: Vec<(String, i64)> =
            sqlx::query_as("SELECT model_id, used_tokens FROM token_caps")
                .fetch_all(&app.state.pool)
                .await
                .expect("caps");
        assert_eq!(caps, [("keep".to_string(), 15)], "usage is summed");

        let grants: Vec<(String, String)> = sqlx::query_as("SELECT resource_id, mode FROM grants")
            .fetch_all(&app.state.pool)
            .await
            .expect("grants");
        assert_eq!(
            grants,
            [("keep".to_string(), "allow".to_string())],
            "the keeper's rule wins a collision"
        );

        let usage: Option<String> =
            sqlx::query_scalar("SELECT model_id FROM usage_events WHERE request_id = 'req-1'")
                .fetch_one(&app.state.pool)
                .await
                .expect("usage");
        assert_eq!(usage.as_deref(), Some("keep"));

        // Idempotent: the guard records the run and a second call is a no-op.
        assert_eq!(
            repair_duplicate_models(&app.state.pool)
                .await
                .expect("second repair"),
            0
        );

        let audit: Option<String> = sqlx::query_scalar(
            "SELECT detail_json FROM audit_log WHERE action = 'catalog.merge_duplicate_models'",
        )
        .fetch_optional(&app.state.pool)
        .await
        .expect("audit");
        let audit: serde_json::Value =
            serde_json::from_str(&audit.expect("one audit row")).expect("detail json");
        assert_eq!(audit["merged"], 1);
        assert_eq!(audit["rows"][0]["duplicate"], "dup");
        assert_eq!(audit["rows"][0]["keeper"], "keep");
    }

    #[tokio::test]
    async fn the_repair_merges_case_only_duplicates_and_fills_empty_fields() {
        let app = TestApp::new().await;
        sqlx::query("DELETE FROM db_meta WHERE key = ?")
            .bind(REPAIR_META_KEY)
            .execute(&app.state.pool)
            .await
            .expect("clear guard");

        // A bare word never gets a spelling identity, but the case-only pass
        // still catches `Auto` / `auto`.
        seed_model(&app, "keep", "Auto", "2026-11-01T00:00:00.000Z").await;
        seed_model(&app, "dup", "auto", "2026-11-02T00:00:00.000Z").await;
        sqlx::query("UPDATE models SET modalities_input = '[]' WHERE id = 'keep'")
            .execute(&app.state.pool)
            .await
            .expect("empty modalities");
        sqlx::query("UPDATE models SET modalities_input = '[\"text\"]' WHERE id = 'dup'")
            .execute(&app.state.pool)
            .await
            .expect("filled modalities");
        let before: String = sqlx::query_scalar("SELECT updated_at FROM models WHERE id = 'keep'")
            .fetch_one(&app.state.pool)
            .await
            .expect("keeper updated_at before repair");

        assert_eq!(
            repair_duplicate_models(&app.state.pool)
                .await
                .expect("repair"),
            1
        );

        let row: (String, Option<String>, String, String) = sqlx::query_as(
            "SELECT canonical_name, modalities_input, created_at, updated_at FROM models",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("keeper");
        assert_eq!(row.0, "Auto", "the oldest spelling is kept");
        assert_eq!(
            row.1.as_deref(),
            Some("[\"text\"]"),
            "an empty array does not block the fill"
        );
        assert_eq!(row.2, "2026-11-01T00:00:00.000Z");
        assert!(
            row.3 >= before,
            "the keeper's updated_at does not move backwards: {row:?}"
        );
    }
}
