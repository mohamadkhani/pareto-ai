//! External model-identity registry (D60): published id -> canonical key.
//!
//! Two public sources are supported:
//!
//! * **models.dev** (`https://models.dev/api.json`) publishes a
//!   `canonical_model_id` for most entries, shared across providers. It is the
//!   primary source because it covers providers the gateway does not route to.
//! * **OpenRouter** (`https://openrouter.ai/api/v1/models`) publishes
//!   `hugging_face_id`, the exact repository id of an open-weight model. The
//!   price sync already downloads this document, so its aliases are refreshed
//!   with every price sync.
//!
//! This module only stores mappings. It never touches prices, credentials, or
//! routing. [`crate::catalog::identity::load_index`] reads the table into
//! [`crate::model_identity::CatalogIndex`], and discovery/sync consult it
//! before falling back to spelling normalization.
//!
//! A refresh replaces one source atomically. A malformed payload is rejected
//! before any delete, so a bad fetch never empties the registry. Tests exercise
//! the parsers and the store from recorded documents; the network path is
//! operator-triggered (`gateway sync-model-registry`) and never runs in tests.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Duration;

use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};

use crate::model_identity::Registry;

/// The only accepted `model_registry.source` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrySource {
    ModelsDev,
    OpenRouter,
}

impl RegistrySource {
    /// Every source, in resolution precedence order.
    pub const ALL: [Self; 2] = [Self::ModelsDev, Self::OpenRouter];

    /// The stored `model_registry.source` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelsDev => "models_dev",
            Self::OpenRouter => "openrouter",
        }
    }

    fn url(self) -> &'static str {
        match self {
            Self::ModelsDev => MODELS_DEV_URL,
            Self::OpenRouter => OPENROUTER_MODELS_URL,
        }
    }
}

/// The models.dev catalog (public, no authentication).
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// The OpenRouter model list the price sync already consumes.
pub const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound on one registry body. The models.dev document is tens of MiB;
/// the cap only stops a runaway transfer.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("the {0} registry payload is malformed: {1}")]
    Malformed(&'static str, String),
    #[error("fetching the {0} registry failed: {1}")]
    Fetch(&'static str, String),
    #[error("the {0} registry would shrink from {1} to {2} aliases; refusing to replace it")]
    Shrink(&'static str, usize, usize),
}

/// Loads the registry into the pure index used by [`crate::model_identity`].
/// Failure mode: [`sqlx::Error`] from the read.
pub(crate) async fn load(conn: &mut SqliteConnection) -> Result<Registry, sqlx::Error> {
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT source, alias, canonical FROM model_registry")
            .fetch_all(conn)
            .await?;
    let mut models_dev = Vec::new();
    let mut openrouter = Vec::new();
    for (source, alias, canonical) in rows {
        match source.as_str() {
            "models_dev" => models_dev.push((alias, canonical)),
            "openrouter" => openrouter.push((alias, canonical)),
            // The CHECK constraint makes this unreachable; an unknown value is
            // ignored rather than aborting every write.
            _ => {}
        }
    }
    Ok(Registry::new(models_dev, openrouter))
}

/// Replaces one source's rows in the caller's transaction and returns how many
/// were inserted. Duplicate `(alias, canonical)` pairs collapse.
///
/// Failure mode: [`RegistryError`]. When the new payload would keep less than
/// half of the distinct aliases already stored for the source, it is rejected
/// with [`RegistryError::Shrink`] **before** any delete, so a bad upstream
/// document cannot empty the registry.
pub(crate) async fn replace_in(
    conn: &mut SqliteConnection,
    source: RegistrySource,
    rows: &[(String, String)],
    fetched_at: &str,
) -> Result<usize, RegistryError> {
    let mut seen = HashSet::new();
    let mut next_aliases = HashSet::new();
    let mut accepted = Vec::new();
    for (alias, canonical) in rows {
        let alias = alias.trim().to_ascii_lowercase();
        let canonical = canonical.trim().to_ascii_lowercase();
        if alias.is_empty() || canonical.is_empty() {
            continue;
        }
        next_aliases.insert(alias.clone());
        if !seen.insert((alias.clone(), canonical.clone())) {
            continue;
        }
        accepted.push((alias, canonical));
    }

    let previous: i64 =
        sqlx::query_scalar("SELECT COUNT(DISTINCT alias) FROM model_registry WHERE source = ?")
            .bind(source.as_str())
            .fetch_one(&mut *conn)
            .await?;
    let previous = previous.max(0) as usize;
    if previous > 0 && next_aliases.len() * 2 < previous {
        return Err(RegistryError::Shrink(
            source.as_str(),
            previous,
            next_aliases.len(),
        ));
    }

    sqlx::query("DELETE FROM model_registry WHERE source = ?")
        .bind(source.as_str())
        .execute(&mut *conn)
        .await?;
    let mut inserted = 0;
    for (alias, canonical) in accepted {
        sqlx::query(
            "INSERT INTO model_registry (source, alias, canonical, fetched_at) \
             VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(source.as_str())
        .bind(&alias)
        .bind(&canonical)
        .bind(fetched_at)
        .execute(&mut *conn)
        .await?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Replaces one source's rows in its own transaction.
///
/// Failure mode: [`RegistryError`]; a database failure rolls the replacement
/// back and leaves the previous rows in place.
pub async fn replace(
    pool: &SqlitePool,
    source: RegistrySource,
    rows: &[(String, String)],
) -> Result<usize, RegistryError> {
    let fetched_at = crate::catalog::now_ts();
    let mut tx = pool.begin().await?;
    let inserted = replace_in(&mut tx, source, rows, &fetched_at).await?;
    tx.commit().await?;
    Ok(inserted)
}

/// Fetches and replaces one source. The only network path in this module; the
/// operator command calls it, and tests never do.
pub async fn refresh(pool: &SqlitePool, source: RegistrySource) -> Result<usize, RegistryError> {
    let raw = fetch(source).await?;
    let rows = match source {
        RegistrySource::ModelsDev => parse_models_dev(&raw)?,
        RegistrySource::OpenRouter => parse_openrouter(&raw)?,
    };
    replace(pool, source, &rows).await
}

/// Fetches a registry document with a bounded timeout and identity encoding.
async fn fetch(source: RegistrySource) -> Result<String, RegistryError> {
    let url = source.url();
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent("pareto-ai-gateway/0.1 model-registry")
        .build()
        .map_err(|error| RegistryError::Fetch(source.as_str(), error.to_string()))?;
    let mut response = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|error| RegistryError::Fetch(source.as_str(), error.to_string()))?;
    if !response.status().is_success() {
        return Err(RegistryError::Fetch(
            source.as_str(),
            format!("HTTP {} from {url}", response.status()),
        ));
    }
    if let Some(length) = response.content_length()
        && length > MAX_BODY_BYTES as u64
    {
        return Err(RegistryError::Fetch(
            source.as_str(),
            format!("the body exceeds {MAX_BODY_BYTES} bytes"),
        ));
    }
    // Stream the body so the cap stops the transfer as soon as it is crossed,
    // instead of buffering a runaway document first.
    let mut body: Vec<u8> = Vec::with_capacity(
        response
            .content_length()
            .map(|length| length.min(MAX_BODY_BYTES as u64) as usize)
            .unwrap_or(0),
    );
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(RegistryError::Fetch(
                        source.as_str(),
                        format!("the body exceeds {MAX_BODY_BYTES} bytes"),
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                return Err(RegistryError::Fetch(source.as_str(), error.to_string()));
            }
        }
    }
    String::from_utf8(body).map_err(|error| {
        RegistryError::Fetch(source.as_str(), format!("the body is not UTF-8: {error}"))
    })
}

/// Parses `models.dev/api.json` into `(alias, canonical_model_id)` pairs.
///
/// Every provider's `models` object is walked; entries without a non-empty
/// `canonical_model_id` are skipped, and so are tagged variants (see
/// [`is_tagged_variant`]). An `hf:` id also becomes an alias without the
/// prefix, because providers publish the bare repository id.
/// Failure mode: [`RegistryError::Malformed`] for invalid JSON, a non-object
/// top level, or a document with no canonical id at all (which would otherwise
/// wipe the registry on a schema change).
pub fn parse_models_dev(raw: &str) -> Result<Vec<(String, String)>, RegistryError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| RegistryError::Malformed("models.dev", error.to_string()))?;
    let providers = value
        .as_object()
        .ok_or_else(|| malformed("models.dev", "expected an object of providers"))?;
    let mut aliases = BTreeMap::<String, BTreeSet<String>>::new();
    for provider in providers.values() {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (id, model) in models {
            // A tagged variant (`:free`, `:batch`, `-free`, `-batch`,
            // `-thinking`) is a mode or price tier, not the base model.
            // Models.dev sometimes groups it under the base canonical id; the
            // catalog keeps them apart, so the alias is not stored. The `hf:`
            // prefix only marks a Hugging Face repository id and is stripped
            // before the check.
            if is_tagged_variant(id) {
                continue;
            }
            let bare = id.strip_prefix("hf:").unwrap_or(id);
            let Some(canonical) = model.get("canonical_model_id").and_then(Value::as_str) else {
                continue;
            };
            add_alias(&mut aliases, bare, canonical);
        }
    }
    if aliases.is_empty() {
        return Err(malformed("models.dev", "no canonical_model_id entries"));
    }
    Ok(flatten(aliases))
}

/// Parses `openrouter.ai/api/v1/models` into `(alias, hugging_face_id)` pairs.
///
/// An entry contributes only when it names an open-weight repository and is a
/// base id: tagged variants (see [`is_tagged_variant`]) are distinct offerings
/// and must not collapse onto the base. Both the OpenRouter id and the repository
/// id become aliases of one canonical key, so a provider that publishes the
/// repository id (SailResearch does) joins the OpenRouter row.
/// Failure mode: [`RegistryError::Malformed`] for invalid JSON, a missing
/// `data` array, or a document with no `hugging_face_id` at all.
pub fn parse_openrouter(raw: &str) -> Result<Vec<(String, String)>, RegistryError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| RegistryError::Malformed("openrouter", error.to_string()))?;
    let entries = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("openrouter", "expected a data array"))?;
    let mut aliases = BTreeMap::<String, BTreeSet<String>>::new();
    for entry in entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(hugging_face_id) = entry.get("hugging_face_id").and_then(Value::as_str) else {
            continue;
        };
        let hugging_face_id = hugging_face_id.trim();
        if hugging_face_id.is_empty() || is_tagged_variant(id) {
            continue;
        }
        add_alias(&mut aliases, hugging_face_id, hugging_face_id);
        add_alias(&mut aliases, id, hugging_face_id);
    }
    if aliases.is_empty() {
        return Err(malformed("openrouter", "no hugging_face_id entries"));
    }
    Ok(flatten(aliases))
}

fn malformed(source: &'static str, message: impl Into<String>) -> RegistryError {
    RegistryError::Malformed(source, message.into())
}

/// True when `id` names a tagged variant rather than a base model id.
///
/// A tagged variant carries a mode or price tier (`:free`, `:batch`,
/// `-free`, `-batch`, `-thinking`), so the catalog keeps it apart from
/// the base model. A leading `hf:` marker is stripped first, and a bare
/// id is untagged.
pub(crate) fn is_tagged_variant(id: &str) -> bool {
    let bare = id.strip_prefix("hf:").unwrap_or(id);
    bare.contains(':')
        || bare.ends_with("-free")
        || bare.ends_with("-batch")
        || bare.ends_with("-thinking")
}

fn add_alias(aliases: &mut BTreeMap<String, BTreeSet<String>>, alias: &str, canonical: &str) {
    let alias = alias.trim().to_ascii_lowercase();
    let canonical = canonical.trim();
    if alias.is_empty() || canonical.is_empty() {
        return;
    }
    aliases
        .entry(alias)
        .or_default()
        .insert(canonical.to_string());
}

fn flatten(aliases: BTreeMap<String, BTreeSet<String>>) -> Vec<(String, String)> {
    aliases
        .into_iter()
        .flat_map(|(alias, canonicals)| {
            canonicals
                .into_iter()
                .map(move |canonical| (alias.clone(), canonical))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_dev_canonical_ids_cover_providers_and_hf_prefixes() {
        let raw = r#"{
            "openrouter": {"models": {
                "deepseek/deepseek-v4.1-flash-1017": {
                    "canonical_model_id": "deepseek/deepseek-v4.1-flash-1017"
                },
                "z-ai/glm-5.3": {"canonical_model_id": "zhipuai/glm-5.3"}
            }},
            "synthetic": {"models": {
                "hf:deepseek-ai/DeepSeek-V4.1-Flash-1017": {
                    "canonical_model_id": "deepseek/deepseek-v4.1-flash-1017"
                }
            }},
            "tagged": {"models": {
                "google/gemma-4-31b-it:free": {
                    "canonical_model_id": "google/gemma-4-31b-it"
                },
                "z-ai/glm-5.3-flash-free": {
                    "canonical_model_id": "zhipuai/glm-5.3-flash"
                },
                "z-ai/glm-5.3-flash-batch": {
                    "canonical_model_id": "zhipuai/glm-5.3-flash"
                }
            }},
            "broken": {"models": {"no-canonical": {}}},
            "not-a-provider": {}
        }"#;
        let rows = parse_models_dev(raw).expect("parses");
        for expected in [
            (
                "deepseek/deepseek-v4.1-flash-1017",
                "deepseek/deepseek-v4.1-flash-1017",
            ),
            ("z-ai/glm-5.3", "zhipuai/glm-5.3"),
            (
                "deepseek-ai/deepseek-v4.1-flash-1017",
                "deepseek/deepseek-v4.1-flash-1017",
            ),
        ] {
            assert!(
                rows.contains(&(expected.0.to_string(), expected.1.to_string())),
                "missing {expected:?}: {rows:?}"
            );
        }
        assert!(
            rows.iter().all(|(alias, _)| !alias.contains(':')
                && !alias.ends_with("-free")
                && !alias.ends_with("-batch")
                && !alias.ends_with("-thinking")),
            "tagged variants are not identities: {rows:?}"
        );
    }

    #[test]
    fn openrouter_maps_the_base_id_and_repository_id_but_not_tagged_variants() {
        let raw = r#"{"data":[
            {"id":"deepseek/deepseek-v4.1-flash-1017",
             "hugging_face_id":"deepseek-ai/DeepSeek-V4.1-Flash-1017"},
            {"id":"deepseek/deepseek-v4.1-flash-1017:batch",
             "hugging_face_id":"deepseek-ai/DeepSeek-V4.1-Flash-1017"},
            {"id":"deepseek/deepseek-v4.1-flash-1017-free",
             "hugging_face_id":"deepseek-ai/DeepSeek-V4.1-Flash-1017"},
            {"id":"deepseek/deepseek-v4.1-flash-1017-thinking",
             "hugging_face_id":"deepseek-ai/DeepSeek-V4.1-Flash-1017"},
            {"id":"google/gemma-4-31b-it",
             "hugging_face_id":"google/gemma-4-31B-it"},
            {"id":"google/gemma-4-31b-it:free",
             "hugging_face_id":"google/gemma-4-31B-it"},
            {"id":"openai/gpt-6.1-terra","hugging_face_id":null},
            {"id":"z-ai/glm-5.3-flashx","hugging_face_id":""}
        ]}"#;
        let rows = parse_openrouter(raw).expect("parses");
        assert!(rows.contains(&(
            "deepseek/deepseek-v4.1-flash-1017".to_string(),
            "deepseek-ai/DeepSeek-V4.1-Flash-1017".to_string()
        )));
        assert_eq!(
            rows.iter()
                .filter(|(alias, _)| alias.starts_with("deepseek"))
                .count(),
            2,
            "base id + repository id, no tagged variants: {rows:?}"
        );
        assert!(
            rows.iter().all(|(alias, _)| !alias.contains(':')
                && !alias.ends_with("-free")
                && !alias.ends_with("-batch")
                && !alias.ends_with("-thinking")),
            "tagged variants never join the base: {rows:?}"
        );
    }

    #[test]
    fn a_payload_without_any_mapping_is_rejected_and_writes_nothing() {
        for raw in ["not json", "[]", r#"{"providers":{}}"#, r#"{"data":[]}"#] {
            assert!(
                matches!(
                    parse_models_dev(raw),
                    Err(RegistryError::Malformed("models.dev", _))
                ),
                "models.dev: {raw}"
            );
            assert!(
                matches!(
                    parse_openrouter(raw),
                    Err(RegistryError::Malformed("openrouter", _))
                ),
                "openrouter: {raw}"
            );
        }
    }

    #[tokio::test]
    async fn replace_is_per_source_and_atomic() {
        let app = crate::test_support::TestApp::new().await;
        let models_dev = vec![
            (
                "deepseek-ai/DeepSeek-V4.1-Flash-1017".to_string(),
                "deepseek/deepseek-v4.1-flash-1017".to_string(),
            ),
            ("zai-org/GLM-5.3".to_string(), "zhipuai/glm-5.3".to_string()),
        ];
        let openrouter = vec![(
            "deepseek-ai/DeepSeek-V4.1-Flash-1017".to_string(),
            "deepseek-ai/DeepSeek-V4.1-Flash-1017".to_string(),
        )];
        assert_eq!(
            replace(&app.state.pool, RegistrySource::ModelsDev, &models_dev)
                .await
                .expect("models.dev"),
            2
        );
        assert_eq!(
            replace(&app.state.pool, RegistrySource::OpenRouter, &openrouter)
                .await
                .expect("openrouter"),
            1
        );

        let mut conn = app.state.pool.acquire().await.expect("conn");
        let registry = load(&mut conn).await.expect("load");
        assert_eq!(
            registry.keys_for("ZAI-ORG/GLM-5.3"),
            vec!["zhipuai/glm-5.3"],
            "aliases are case-insensitive"
        );
        assert_eq!(
            registry.keys_for("deepseek-ai/deepseek-v4.1-flash-1017"),
            vec![
                "deepseek/deepseek-v4.1-flash-1017",
                "deepseek-ai/deepseek-v4.1-flash-1017",
            ],
            "every source's key is returned, models.dev first and lower-cased"
        );

        // Replacing models.dev leaves the openrouter layer alone and clears
        // the old models.dev rows.
        assert_eq!(
            replace(
                &app.state.pool,
                RegistrySource::ModelsDev,
                &[("other/model".to_string(), "canonical".to_string())],
            )
            .await
            .expect("replace"),
            1
        );
        let mut conn = app.state.pool.acquire().await.expect("conn");
        let registry = load(&mut conn).await.expect("load");
        assert_eq!(
            registry.keys_for("deepseek-ai/deepseek-v4.1-flash-1017"),
            vec!["deepseek-ai/deepseek-v4.1-flash-1017"],
            "only the openrouter layer remains; keys are lower-cased"
        );
        assert_eq!(
            registry.keys_for("zai-org/glm-5.3"),
            Vec::<String>::new(),
            "the old models.dev rows are gone"
        );
    }

    #[tokio::test]
    async fn a_replace_that_would_shrink_more_than_half_is_refused() {
        let app = crate::test_support::TestApp::new().await;
        let rows: Vec<(String, String)> = (0..4)
            .map(|i| (format!("alias-{i}"), format!("canonical-{i}")))
            .collect();
        assert_eq!(
            replace(&app.state.pool, RegistrySource::ModelsDev, &rows)
                .await
                .expect("seed"),
            4
        );

        let error = replace(
            &app.state.pool,
            RegistrySource::ModelsDev,
            &[("alias-new".to_string(), "canonical-new".to_string())],
        )
        .await
        .expect_err("1 alias is less than half of 4");
        assert!(
            matches!(error, RegistryError::Shrink("models_dev", 4, 1)),
            "{error}"
        );

        // The guard fired before the delete: the old rows are still there.
        let stored: i64 =
            sqlx::query_scalar("SELECT COUNT(DISTINCT alias) FROM model_registry WHERE source = ?")
                .bind("models_dev")
                .fetch_one(&app.state.pool)
                .await
                .expect("count");
        assert_eq!(stored, 4);

        // Exactly half is allowed, and a normal replace still works.
        let half: Vec<(String, String)> = (0..2)
            .map(|i| (format!("alias-{i}"), format!("canonical-{i}")))
            .collect();
        assert_eq!(
            replace(&app.state.pool, RegistrySource::ModelsDev, &half)
                .await
                .expect("half is allowed"),
            2
        );
    }
}
