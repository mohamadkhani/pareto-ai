//! Provider presets: one entry per known upstream that can configure its
//! price, balance, and health connectors automatically (decision D52).
//!
//! A preset is **code**, not a database row: it names the default base URL and
//! reuses the shipped connector templates from [`crate::connectors::templates`]
//! so there is one source of truth for pointers and units. `providers.preset`
//! stores only the id (see `0018_provider_presets.sql`); `providers.kind`
//! stays the wire protocol (`openai_compatible`).
//!
//! Seeded connector rows carry `updated_by = NULL`. That is how
//! [`apply_preset`] tells a still-template-derived connector (safe to update)
//! from one the operator edited (kept unless `force`).

use serde::Serialize;
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};
use utoipa::ToSchema;

use crate::catalog::{self, Actor};
use crate::connectors::{
    self, ConnectorConfig, ConnectorError, ConnectorKind, ConnectorTemplateConfigs,
};

/// The shipped preset ids; the migration CHECK lists the same names.
pub const PRESET_IDS: [&str; 3] = ["openrouter", "sailresearch", "generic"];

/// Behaviour switches a preset carries for discovery and price parsing.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct PresetFlags {
    /// Discovery already drops `:batch` model ids globally; the flag records
    /// the intent so a future per-provider rule can read it.
    pub skip_batch_variants: bool,
    /// The provider publishes a negative sentinel for variable prices (D37).
    pub variable_price_sentinel: bool,
    /// Run the catalog price refresh after a successful discovery apply.
    pub apply_on_discovery: bool,
}

/// One selectable provider preset.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Prefilled base URL; the operator may override it on create.
    pub base_url: &'static str,
    /// How the credential is sent (`bearer` for every shipped preset).
    pub auth: &'static str,
    /// The connectors a create with this preset seeds. `None` means none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configs: Option<ConnectorTemplateConfigs>,
    pub flags: PresetFlags,
}

/// Every shipped preset, in picker order.
pub fn all() -> Vec<ProviderPreset> {
    vec![openrouter(), sailresearch(), generic()]
}

/// Looks one preset up by id (`None` for an unknown or absent id).
pub fn get(id: &str) -> Option<ProviderPreset> {
    all().into_iter().find(|preset| preset.id == id)
}

fn openrouter() -> ProviderPreset {
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        description: "One key, many models. Catalog prices are USD per token (×1,000,000); balance is total credits minus lifetime usage.",
        base_url: "https://openrouter.ai/api/v1",
        auth: "bearer",
        configs: template_configs("openrouter_per_token"),
        flags: PresetFlags {
            skip_batch_variants: true,
            variable_price_sentinel: true,
            apply_on_discovery: false,
        },
    }
}

fn sailresearch() -> ProviderPreset {
    ProviderPreset {
        id: "sailresearch",
        label: "SailResearch",
        description: "Balanced-tier catalog prices and the /v2 usage balance, both quoted in cents (×0.01). Verify against an invoice.",
        base_url: "https://api.sailresearch.com/v1",
        auth: "bearer",
        configs: template_configs("sailresearch"),
        flags: PresetFlags {
            skip_batch_variants: false,
            variable_price_sentinel: true,
            apply_on_discovery: true,
        },
    }
}

fn generic() -> ProviderPreset {
    ProviderPreset {
        id: "generic",
        label: "Generic OpenAI-compatible",
        description: "Any upstream that speaks the OpenAI wire protocol. No connectors are seeded; configure prices and balance by hand when needed.",
        base_url: "",
        auth: "bearer",
        configs: None,
        flags: PresetFlags {
            skip_batch_variants: false,
            variable_price_sentinel: true,
            apply_on_discovery: false,
        },
    }
}

/// Reuses an existing connector template instead of duplicating pointers.
fn template_configs(template_id: &str) -> Option<ConnectorTemplateConfigs> {
    connectors::templates()
        .into_iter()
        .find(|template| template.id == template_id)
        .map(|template| template.configs)
}

/// The preset's connectors as `(kind, config)` pairs.
pub fn connector_configs(preset: &ProviderPreset) -> Vec<(ConnectorKind, ConnectorConfig)> {
    let Some(configs) = preset.configs.as_ref() else {
        return Vec::new();
    };
    [
        (ConnectorKind::Catalog, configs.catalog.clone()),
        (ConnectorKind::Balance, configs.balance.clone()),
        (ConnectorKind::Health, configs.health.clone()),
    ]
    .into_iter()
    .filter_map(|(kind, config)| config.map(|config| (kind, config)))
    .collect()
}

/// Inserts preset connector rows inside the caller's transaction. Seeded rows
/// keep `updated_by = NULL`, the marker that they are still unmodified.
pub async fn seed_connectors(
    conn: &mut SqliteConnection,
    provider_id: &str,
    scoped: &[(ConnectorKind, ConnectorConfig)],
    now: &str,
) -> Result<Vec<&'static str>, ConnectorError> {
    let mut seeded = Vec::new();
    for (kind, config) in scoped {
        config.validate(*kind)?;
        let config_json = serde_json::to_string(config)
            .map_err(|_| ConnectorError::Invalid("config could not be serialized".to_string()))?;
        sqlx::query(
            "INSERT INTO provider_connectors \
               (provider_id, kind, config_json, enabled, updated_by, updated_at) \
             VALUES (?, ?, ?, 1, NULL, ?) \
             ON CONFLICT (provider_id, kind) DO NOTHING",
        )
        .bind(provider_id)
        .bind(kind.as_str())
        .bind(&config_json)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        seeded.push(kind.as_str());
    }
    Ok(seeded)
}

/// One connector's outcome in an apply-preset run.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PresetChange {
    pub kind: String,
    /// `created`, `updated`, `overwritten`, `unchanged`, or `skipped`.
    pub action: String,
}

/// `POST /api/providers/{id}/apply-preset`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApplyPresetView {
    pub provider_id: String,
    pub preset: String,
    pub force: bool,
    /// Connectors actually written (created + updated + overwritten).
    pub applied: usize,
    /// One entry per connector the preset defines, in catalog/balance/health
    /// order, including the ones the operator's edits kept.
    pub changes: Vec<PresetChange>,
}

#[derive(sqlx::FromRow)]
struct StoredConnector {
    config_json: String,
    updated_by: Option<String>,
}

/// Re-applies a preset's connector defaults to an existing provider.
///
/// A connector whose stored document equals the preset is `unchanged`; one that
/// still carries `updated_by = NULL` is replaced (`updated`); one the operator
/// saved is `skipped` unless `force` asks for `overwritten`. The provider's
/// `base_url` is never touched, so a mock or proxy override survives.
pub async fn apply_preset(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    provider_id: &str,
    requested: Option<&str>,
    force: bool,
) -> Result<ApplyPresetView, ConnectorError> {
    let provider = catalog::get_provider(pool, provider_id)
        .await?
        .ok_or(ConnectorError::NotFound("provider"))?;
    let preset_id = requested
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or(provider.preset.as_deref())
        .ok_or_else(|| {
            ConnectorError::Invalid(
                "this provider has no preset; send one in the body (openrouter, sailresearch, or generic)"
                    .to_string(),
            )
        })?;
    let preset = get(preset_id).ok_or_else(|| {
        ConnectorError::Invalid(format!(
            "unknown preset {preset_id:?}; expected one of {}",
            PRESET_IDS.join(", ")
        ))
    })?;
    let configs = connector_configs(&preset);
    let now = catalog::now_ts();
    let mut changes = Vec::with_capacity(configs.len());
    let mut applied = 0usize;

    let mut tx = pool.begin().await?;
    for (kind, config) in &configs {
        config.validate(*kind)?;
        let preset_json = serde_json::to_value(config)
            .map_err(|_| ConnectorError::Invalid("config could not be serialized".to_string()))?;
        let existing: Option<StoredConnector> = sqlx::query_as(
            "SELECT config_json, updated_by FROM provider_connectors \
             WHERE provider_id = ? AND kind = ?",
        )
        .bind(provider_id)
        .bind(kind.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let action = match existing {
            None => {
                seed_connectors(&mut tx, provider_id, &[(*kind, config.clone())], &now).await?;
                "created"
            }
            Some(row) => {
                let stored: Value = serde_json::from_str(&row.config_json).unwrap_or(Value::Null);
                if stored == preset_json {
                    "unchanged"
                } else if row.updated_by.is_none() || force {
                    let config_json = preset_json.to_string();
                    sqlx::query(
                        "UPDATE provider_connectors SET config_json = ?, enabled = 1, \
                           updated_by = ?, updated_at = ? \
                         WHERE provider_id = ? AND kind = ?",
                    )
                    .bind(&config_json)
                    .bind(actor.user_id)
                    .bind(&now)
                    .bind(provider_id)
                    .bind(kind.as_str())
                    .execute(&mut *tx)
                    .await?;
                    if row.updated_by.is_none() {
                        "updated"
                    } else {
                        "overwritten"
                    }
                } else {
                    "skipped"
                }
            }
        };
        if matches!(action, "created" | "updated" | "overwritten") {
            applied += 1;
        }
        changes.push(PresetChange {
            kind: kind.as_str().to_string(),
            action: action.to_string(),
        });
    }

    sqlx::query("UPDATE providers SET preset = ?, updated_at = ? WHERE id = ?")
        .bind(preset.id)
        .bind(&now)
        .bind(provider_id)
        .execute(&mut *tx)
        .await?;

    let detail = serde_json::json!({
        "preset": preset.id,
        "force": force,
        "applied": applied,
        "changes": changes,
    });
    catalog::audit(
        &mut tx,
        actor,
        "provider.apply_preset",
        "provider",
        provider_id,
        &detail,
    )
    .await?;
    tx.commit().await?;

    Ok(ApplyPresetView {
        provider_id: provider_id.to_string(),
        preset: preset.id.to_string(),
        force,
        applied,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_names_three_presets_and_their_defaults() {
        let presets = all();
        let ids: Vec<&str> = presets.iter().map(|preset| preset.id).collect();
        assert_eq!(ids, ["openrouter", "sailresearch", "generic"]);

        let openrouter = get("openrouter").expect("openrouter");
        assert_eq!(openrouter.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(openrouter.auth, "bearer");
        let balance = openrouter
            .configs
            .as_ref()
            .and_then(|configs| configs.balance.as_ref())
            .and_then(|config| config.balance.as_ref())
            .expect("openrouter balance");
        assert_eq!(balance.balance, "$.data.total_credits - $.data.total_usage");
        assert_eq!(balance.unit.multiplier, 1.0);

        let sail = get("sailresearch").expect("sailresearch");
        assert_eq!(sail.base_url, "https://api.sailresearch.com/v1");
        let sail_balance = sail
            .configs
            .as_ref()
            .and_then(|configs| configs.balance.as_ref())
            .and_then(|config| config.balance.as_ref())
            .expect("sail balance");
        assert_eq!(sail_balance.unit.multiplier, 0.01);
        assert_eq!(
            sail_balance.unit.note.as_deref(),
            Some("quoted in cents — verify against an invoice")
        );
        assert_eq!(
            sail_balance.balance, "$.balance",
            "only the path syntax differs; the unit is the same"
        );
        let sail_catalog = sail
            .configs
            .as_ref()
            .and_then(|configs| configs.catalog.as_ref())
            .and_then(|config| config.catalog.as_ref())
            .expect("sail catalog");
        assert_eq!(sail_catalog.unit.multiplier, 0.01);

        let generic = get("generic").expect("generic");
        assert!(generic.configs.is_none(), "generic seeds no connectors");
        assert!(connector_configs(&generic).is_empty());
        assert!(get("unknown").is_none());
    }

    #[test]
    fn preset_connectors_are_valid_and_preset_id_order_is_stable() {
        for preset in all() {
            for (kind, config) in connector_configs(&preset) {
                config.validate(kind).unwrap_or_else(|error| {
                    panic!("preset {} {} invalid: {error}", preset.id, kind.as_str())
                });
            }
        }
        let openrouter = get("openrouter").expect("preset");
        let kinds: Vec<&str> = connector_configs(&openrouter)
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect();
        assert_eq!(kinds, ["catalog", "balance"]);
    }
}
