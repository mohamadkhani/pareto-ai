//! Token-mix profiles and the derived mix price (decision D34).
//!
//! A **mix** is the client's token shape: disjoint shares of input, output,
//! cache read, cache write, and reasoning tokens. A **mix price** blends an
//! offering's component prices at those shares:
//!
//! ```text
//! per_m = input_share       * input_per_m
//!       + output_share      * output_per_m
//!       + cache_read_share  * cache_read_per_m
//!       + cache_write_share * cache_write_per_m
//!       + reasoning_share   * reasoning_per_m
//! ```
//!
//! Shares are normalised to sum to 1 first, so a percentage profile
//! (`8.1/0.4/91.5`) and its fractional equivalent blend identically. The
//! blend is USD per million tokens and **excludes `request_fee`**: the fee is
//! per request, not per token, and has no meaningful share. Routing and the
//! Models page use the mix price; billing never does — `usage_events.cost_usd`
//! keeps D22's component formula.
//!
//! **Fallbacks keep cache-less comparisons honest.** A cache-read share
//! without a published cache price bills at the input price (never dropped
//! and never renormalised), because that is what the provider actually
//! charges for those tokens. The same rule applies to cache writes (fallback
//! `input_per_m`) and reasoning tokens (fallback `output_per_m`, matching
//! D22's billing fallback). A positive share whose component has no price,
//! even after fallback, makes the mix **unpriced** (`None`, never 0): a
//! partial sum would systematically favour the offering with fewer known
//! prices.
//!
//! **Built-ins are immutable.** `code-agent`, `code-agent-nocache`, `chat`,
//! and `long-context` are seeded by `0010_token_mix.sql`. Editing or deleting
//! one is rejected; clone it under a new name instead. A routing number whose
//! meaning silently changed would make old `decision_json` records
//! irreproducible.
//!
//! **Default resolution.** `GATEWAY_TOKEN_MIX_PROFILE` (name or id) wins over
//! the `pricing.default_mix_profile` setting, which wins over the built-in
//! `code-agent`. An unknown selector at one layer falls through to the next;
//! the final embedded `code-agent` preset keeps routing deterministic even if
//! a seed row is missing.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sqlx::SqlitePool;

use crate::catalog::{self, Actor, PriceRow};

/// The default preset name (D34): long agent sessions with a warm cache.
pub const CODE_AGENT: &str = "code-agent";
/// The pessimistic preset: the same token shape with no cache hits.
pub const CODE_AGENT_NOCACHE: &str = "code-agent-nocache";
/// The short-assistant preset.
pub const CHAT: &str = "chat";
/// The one-shot document preset.
pub const LONG_CONTEXT: &str = "long-context";

/// The settings key holding the operator's default selector.
pub const DEFAULT_SETTING: &str = "pricing.default_mix_profile";

/// The four built-in presets as `(name, input, output, cache read)`.
pub const BUILTIN_PRESETS: [(&str, f64, f64, f64); 4] = [
    (CODE_AGENT, 8.1, 0.4, 91.5),
    (CODE_AGENT_NOCACHE, 99.6, 0.4, 0.0),
    (CHAT, 74.0, 26.0, 0.0),
    (LONG_CONTEXT, 97.0, 3.0, 0.0),
];

/// Storage and validation failures of the mix domain.
#[derive(Debug, thiserror::Error)]
pub enum MixError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit write failed: {0}")]
    Audit(#[from] catalog::CatalogError),
    #[error("{0}")]
    Invalid(String),
    #[error("{0} not found")]
    NotFound(&'static str),
}

impl MixError {
    /// Maps the domain error onto the uniform API error shape.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        use crate::error::ApiError;
        match self {
            MixError::Invalid(message) => ApiError::bad_request(message).with_request_id(rid),
            MixError::NotFound(what) => ApiError::not_found(what).with_request_id(rid),
            other => ApiError::internal("mix", other).with_request_id(rid),
        }
    }
}

/// The five disjoint shares of a profile.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct MixShares {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub reasoning: f64,
}

impl MixShares {
    /// Normalises the shares to sum to 1. `None` when any share is negative
    /// or non-finite, or when they sum to zero.
    pub fn normalised(self) -> Option<Self> {
        let parts = [
            self.input,
            self.output,
            self.cache_read,
            self.cache_write,
            self.reasoning,
        ];
        if parts.iter().any(|value| !value.is_finite() || *value < 0.0) {
            return None;
        }
        let sum: f64 = parts.iter().sum();
        if sum <= 0.0 {
            return None;
        }
        Some(Self {
            input: self.input / sum,
            output: self.output / sum,
            cache_read: self.cache_read / sum,
            cache_write: self.cache_write / sum,
            reasoning: self.reasoning / sum,
        })
    }

    /// The human label of **normalised** shares: `8.1/0.4/91.5` for the
    /// research presets, with non-zero write/reasoning shares appended.
    pub fn label(self) -> String {
        let mut parts = vec![
            self.input * 100.0,
            self.output * 100.0,
            self.cache_read * 100.0,
        ];
        if self.cache_write > 0.0 {
            parts.push(self.cache_write * 100.0);
        }
        if self.reasoning > 0.0 {
            parts.push(self.reasoning * 100.0);
        }
        parts
            .iter()
            .map(|value| format!("{value:.1}"))
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// A stored token-mix profile.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct MixProfile {
    pub id: String,
    pub name: String,
    /// `preset`, `custom`, or `measured`.
    pub kind: String,
    pub input_share: f64,
    pub output_share: f64,
    pub cache_read_share: f64,
    pub cache_write_share: f64,
    pub reasoning_share: f64,
    /// Built-ins can be cloned but never edited or deleted.
    pub is_builtin: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl MixProfile {
    /// The profile's shares as stored (not yet normalised).
    pub fn shares(&self) -> MixShares {
        MixShares {
            input: self.input_share,
            output: self.output_share,
            cache_read: self.cache_read_share,
            cache_write: self.cache_write_share,
            reasoning: self.reasoning_share,
        }
    }

    /// The shares normalised to sum to 1; `None` for an invalid profile.
    pub fn normalised_shares(&self) -> Option<MixShares> {
        self.shares().normalised()
    }

    /// `measured` for profiles calibrated from telemetry, `blended` otherwise
    /// (the research presets and operator assumptions).
    pub fn basis(&self) -> &'static str {
        match self.kind.as_str() {
            "measured" => "measured",
            _ => "blended",
        }
    }
}

/// The built-in preset `name` as a profile (seeded id, `is_builtin = true`).
/// `None` for an unknown name.
pub fn builtin_profile(name: &str) -> Option<MixProfile> {
    let (_, input, output, cache_read) = BUILTIN_PRESETS
        .iter()
        .find(|(preset, ..)| *preset == name)?;
    Some(MixProfile {
        id: format!("mix-{name}"),
        name: name.to_string(),
        kind: "preset".to_string(),
        input_share: *input,
        output_share: *output,
        cache_read_share: *cache_read,
        cache_write_share: 0.0,
        reasoning_share: 0.0,
        is_builtin: true,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

/// The embedded `code-agent` preset, the last layer of default resolution.
pub fn code_agent_profile() -> MixProfile {
    builtin_profile(CODE_AGENT).unwrap_or(MixProfile {
        id: "mix-code-agent".to_string(),
        name: CODE_AGENT.to_string(),
        kind: "preset".to_string(),
        input_share: 8.1,
        output_share: 0.4,
        cache_read_share: 91.5,
        cache_write_share: 0.0,
        reasoning_share: 0.0,
        is_builtin: true,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

/// One component of a blended mix price.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct MixComponent {
    /// The component price used, after any fallback.
    pub price_per_m: f64,
    /// The effective (normalised) share.
    pub share: f64,
    /// `share * price_per_m` in USD per million tokens.
    pub contribution_per_m: f64,
    /// The superset whose price was inherited: `input` or `output`.
    pub fallback: Option<String>,
}

/// The five component contributions of a mix price. Components with a zero
/// share are `None`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct MixComponents {
    pub input: Option<MixComponent>,
    pub output: Option<MixComponent>,
    pub cache_read: Option<MixComponent>,
    pub cache_write: Option<MixComponent>,
    pub reasoning: Option<MixComponent>,
}

/// The blended price of one offering at one profile's shares.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct MixPrice {
    /// USD per million tokens over the five components; `request_fee` is not
    /// part of it.
    pub per_m: f64,
    pub components: MixComponents,
    /// The normalised shares the blend used.
    pub effective_shares: MixShares,
}

impl MixPrice {
    /// The shares label used in routing explanations.
    pub fn shares_label(&self) -> String {
        self.effective_shares.label()
    }
}

/// The component prices of one offering, decoupled from the catalog row so
/// the blend is pure and testable without storage.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ComponentPrices {
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub reasoning_per_m: Option<f64>,
}

impl From<&PriceRow> for ComponentPrices {
    fn from(price: &PriceRow) -> Self {
        Self {
            input_per_m: price.input_per_m,
            output_per_m: price.output_per_m,
            cache_read_per_m: price.cache_read_per_m,
            cache_write_per_m: price.cache_write_per_m,
            reasoning_per_m: price.reasoning_per_m,
        }
    }
}

/// Blends one offering's price row at `profile`'s shares.
///
/// Returns `None` (unpriced) when the shares are invalid or when any
/// positive-share component has no price even after its fallback. Cache read
/// and cache write fall back to the input price, reasoning to the output
/// price. `request_fee` is deliberately excluded.
pub fn mix_price(price: &PriceRow, profile: &MixProfile) -> Option<MixPrice> {
    mix_price_from(ComponentPrices::from(price), profile)
}

/// [`mix_price`] over raw component prices.
pub fn mix_price_from(prices: ComponentPrices, profile: &MixProfile) -> Option<MixPrice> {
    let shares = profile.normalised_shares()?;
    let entries: [(f64, Option<f64>, Option<&str>); 5] = [
        (shares.input, prices.input_per_m, None),
        (shares.output, prices.output_per_m, None),
        (shares.cache_read, prices.cache_read_per_m, Some("input")),
        (shares.cache_write, prices.cache_write_per_m, Some("input")),
        (shares.reasoning, prices.reasoning_per_m, Some("output")),
    ];
    let fallback_price = |fallback: &str| match fallback {
        "input" => prices.input_per_m,
        "output" => prices.output_per_m,
        _ => None,
    };

    let mut components = [
        None::<MixComponent>,
        None::<MixComponent>,
        None::<MixComponent>,
        None::<MixComponent>,
        None::<MixComponent>,
    ];
    let mut per_m = 0.0;
    for (index, (share, own_price, fallback)) in entries.into_iter().enumerate() {
        if share <= 0.0 {
            continue;
        }
        let (price_per_m, inherited) = match own_price {
            Some(price) => (price, None),
            None => {
                let inherited = fallback.and_then(fallback_price);
                (inherited?, fallback)
            }
        };
        let contribution = share * price_per_m;
        per_m += contribution;
        components[index] = Some(MixComponent {
            price_per_m,
            share,
            contribution_per_m: contribution,
            fallback: inherited.map(str::to_owned),
        });
    }
    // `entries` and `components` share the same fixed order.
    let [input, output, cache_read, cache_write, reasoning] = components;
    Some(MixPrice {
        per_m,
        components: MixComponents {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        },
        effective_shares: shares,
    })
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// A profile as supplied by the admin API (create or full replace).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
pub struct MixProfileInput {
    pub name: String,
    /// `custom` (default) or `measured`. `preset` is reserved for the seeded
    /// built-ins and is rejected on create/update.
    pub kind: Option<String>,
    pub input_share: f64,
    pub output_share: f64,
    pub cache_read_share: f64,
    /// Defaults to 0.
    pub cache_write_share: Option<f64>,
    /// Defaults to 0.
    pub reasoning_share: Option<f64>,
}

type ProfileRow = (
    String,
    String,
    String,
    f64,
    f64,
    f64,
    f64,
    f64,
    i64,
    String,
    String,
);

const SELECT_PROFILE_COLUMNS: &str = "id, name, kind, input_share, output_share, cache_read_share, \
     cache_write_share, reasoning_share, is_builtin, created_at, updated_at";

fn profile_from_row(row: ProfileRow) -> MixProfile {
    let (
        id,
        name,
        kind,
        input_share,
        output_share,
        cache_read_share,
        cache_write_share,
        reasoning_share,
        is_builtin,
        created_at,
        updated_at,
    ) = row;
    MixProfile {
        id,
        name,
        kind,
        input_share,
        output_share,
        cache_read_share,
        cache_write_share,
        reasoning_share,
        is_builtin: is_builtin != 0,
        created_at,
        updated_at,
    }
}

/// A validated input, ready to persist.
struct ValidatedProfile {
    name: String,
    kind: String,
    input_share: f64,
    output_share: f64,
    cache_read_share: f64,
    cache_write_share: f64,
    reasoning_share: f64,
}

fn validate_profile(input: &MixProfileInput) -> Result<ValidatedProfile, MixError> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(MixError::Invalid("name must not be empty".to_string()));
    }
    let kind = input.kind.as_deref().unwrap_or("custom").trim();
    if !matches!(kind, "custom" | "measured") {
        return Err(MixError::Invalid(
            "kind must be custom or measured; preset is reserved for built-in profiles".to_string(),
        ));
    }
    let cache_write_share = input.cache_write_share.unwrap_or(0.0);
    let reasoning_share = input.reasoning_share.unwrap_or(0.0);
    let shares = MixShares {
        input: input.input_share,
        output: input.output_share,
        cache_read: input.cache_read_share,
        cache_write: cache_write_share,
        reasoning: reasoning_share,
    };
    if [
        shares.input,
        shares.output,
        shares.cache_read,
        shares.cache_write,
        shares.reasoning,
    ]
    .iter()
    .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(MixError::Invalid(
            "shares must be finite and non-negative".to_string(),
        ));
    }
    if shares.normalised().is_none() {
        return Err(MixError::Invalid(
            "shares must sum to more than zero".to_string(),
        ));
    }
    Ok(ValidatedProfile {
        name: name.to_string(),
        kind: kind.to_string(),
        input_share: shares.input,
        output_share: shares.output,
        cache_read_share: shares.cache_read,
        cache_write_share: shares.cache_write,
        reasoning_share: shares.reasoning,
    })
}

/// The mix-profile repository: CRUD, default resolution, and the settings
/// entry for the operator's default selector.
pub struct MixProfiles;

impl MixProfiles {
    /// Every profile, built-ins first and then by name. Failure mode:
    /// [`MixError::Db`].
    pub async fn list(pool: &SqlitePool) -> Result<Vec<MixProfile>, MixError> {
        let rows = sqlx::query_as::<_, ProfileRow>(&format!(
            "SELECT {SELECT_PROFILE_COLUMNS} FROM token_mix_profiles \
             ORDER BY is_builtin DESC, name"
        ))
        .fetch_all(pool)
        .await?;
        Ok(rows.into_iter().map(profile_from_row).collect())
    }

    /// Loads one profile by id. Failure mode: [`MixError::Db`].
    pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<MixProfile>, MixError> {
        let row = sqlx::query_as::<_, ProfileRow>(&format!(
            "SELECT {SELECT_PROFILE_COLUMNS} FROM token_mix_profiles WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(pool)
        .await?;
        Ok(row.map(profile_from_row))
    }

    /// Resolves a selector that may be an id or a name (id wins). Failure
    /// mode: [`MixError::Db`].
    pub async fn resolve_selector(
        pool: &SqlitePool,
        selector: &str,
    ) -> Result<Option<MixProfile>, MixError> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Ok(None);
        }
        if let Some(profile) = Self::get(pool, selector).await? {
            return Ok(Some(profile));
        }
        let row = sqlx::query_as::<_, ProfileRow>(&format!(
            "SELECT {SELECT_PROFILE_COLUMNS} FROM token_mix_profiles WHERE name = ?"
        ))
        .bind(selector)
        .fetch_optional(pool)
        .await?;
        Ok(row.map(profile_from_row))
    }

    /// Resolves the effective default profile: `env_selector` >
    /// `pricing.default_mix_profile` > the built-in `code-agent`. An unknown
    /// selector at one layer falls through to the next. Failure mode:
    /// [`MixError::Db`]; the final layer is embedded, so this never returns
    /// `None`.
    pub async fn resolve_default(
        pool: &SqlitePool,
        env_selector: Option<&str>,
    ) -> Result<MixProfile, MixError> {
        if let Some(selector) = env_selector.map(str::trim).filter(|s| !s.is_empty())
            && let Some(profile) = Self::resolve_selector(pool, selector).await?
        {
            return Ok(profile);
        }
        if let Some(selector) = Self::stored_default(pool).await?
            && let Some(profile) = Self::resolve_selector(pool, &selector).await?
        {
            return Ok(profile);
        }
        match Self::resolve_selector(pool, CODE_AGENT).await? {
            Some(profile) => Ok(profile),
            None => Ok(code_agent_profile()),
        }
    }

    /// Creates a profile, audited as `mix_profile.create`. Failure mode:
    /// [`MixError::Invalid`] for bad shares or a taken name,
    /// [`MixError::Db`] on storage errors.
    pub async fn create(
        pool: &SqlitePool,
        actor: &Actor<'_>,
        input: &MixProfileInput,
    ) -> Result<MixProfile, MixError> {
        let validated = validate_profile(input)?;
        let mut tx = pool.begin().await?;
        let taken: Option<String> =
            sqlx::query_scalar("SELECT id FROM token_mix_profiles WHERE name = ?")
                .bind(&validated.name)
                .fetch_optional(&mut *tx)
                .await?;
        if taken.is_some() {
            return Err(MixError::Invalid(format!(
                "mix profile name {:?} is already in use",
                validated.name
            )));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = catalog::now_ts();
        sqlx::query(
            "INSERT INTO token_mix_profiles \
               (id, name, kind, input_share, output_share, cache_read_share, \
                cache_write_share, reasoning_share, is_builtin, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?)",
        )
        .bind(&id)
        .bind(&validated.name)
        .bind(&validated.kind)
        .bind(validated.input_share)
        .bind(validated.output_share)
        .bind(validated.cache_read_share)
        .bind(validated.cache_write_share)
        .bind(validated.reasoning_share)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        catalog::audit(
            &mut tx,
            actor,
            "mix_profile.create",
            "mix_profile",
            &id,
            &serde_json::json!({
                "name": validated.name,
                "kind": validated.kind,
                "shares": {
                    "input": validated.input_share,
                    "output": validated.output_share,
                    "cache_read": validated.cache_read_share,
                    "cache_write": validated.cache_write_share,
                    "reasoning": validated.reasoning_share,
                },
            }),
        )
        .await?;
        tx.commit().await?;
        Self::get(pool, &id)
            .await?
            .ok_or(MixError::NotFound("mix profile"))
    }

    /// Replaces a profile's name, kind, and shares, audited as
    /// `mix_profile.update`. Built-ins are rejected: clone one under a new
    /// name instead. `None` when the id is unknown. Failure mode:
    /// [`MixError::Invalid`] / [`MixError::Db`].
    pub async fn update(
        pool: &SqlitePool,
        actor: &Actor<'_>,
        id: &str,
        input: &MixProfileInput,
    ) -> Result<Option<MixProfile>, MixError> {
        let Some(current) = Self::get(pool, id).await? else {
            return Ok(None);
        };
        if current.is_builtin {
            return Err(MixError::Invalid(format!(
                "built-in mix profile {:?} cannot be edited; clone it under a new name",
                current.name
            )));
        }
        let validated = validate_profile(input)?;
        let mut tx = pool.begin().await?;
        let taken: Option<String> =
            sqlx::query_scalar("SELECT id FROM token_mix_profiles WHERE name = ? AND id <> ?")
                .bind(&validated.name)
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if taken.is_some() {
            return Err(MixError::Invalid(format!(
                "mix profile name {:?} is already in use",
                validated.name
            )));
        }
        let now = catalog::now_ts();
        sqlx::query(
            "UPDATE token_mix_profiles SET name = ?, kind = ?, input_share = ?, \
             output_share = ?, cache_read_share = ?, cache_write_share = ?, \
             reasoning_share = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&validated.name)
        .bind(&validated.kind)
        .bind(validated.input_share)
        .bind(validated.output_share)
        .bind(validated.cache_read_share)
        .bind(validated.cache_write_share)
        .bind(validated.reasoning_share)
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        catalog::audit(
            &mut tx,
            actor,
            "mix_profile.update",
            "mix_profile",
            id,
            &serde_json::json!({
                "name": validated.name,
                "kind": validated.kind,
                "shares": {
                    "input": validated.input_share,
                    "output": validated.output_share,
                    "cache_read": validated.cache_read_share,
                    "cache_write": validated.cache_write_share,
                    "reasoning": validated.reasoning_share,
                },
            }),
        )
        .await?;
        tx.commit().await?;
        Self::get(pool, id).await
    }

    /// Deletes a profile, audited as `mix_profile.delete`. Built-ins are
    /// protected. `true` when a row was removed. Failure mode:
    /// [`MixError::Invalid`] for a built-in, [`MixError::Db`] otherwise.
    pub async fn delete(pool: &SqlitePool, actor: &Actor<'_>, id: &str) -> Result<bool, MixError> {
        let Some(current) = Self::get(pool, id).await? else {
            return Ok(false);
        };
        if current.is_builtin {
            return Err(MixError::Invalid(format!(
                "built-in mix profile {:?} cannot be deleted",
                current.name
            )));
        }
        let mut tx = pool.begin().await?;
        let result = sqlx::query("DELETE FROM token_mix_profiles WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() > 0 {
            catalog::audit(
                &mut tx,
                actor,
                "mix_profile.delete",
                "mix_profile",
                id,
                &serde_json::json!({ "name": current.name }),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    /// The raw `pricing.default_mix_profile` setting, when set. Failure mode:
    /// [`MixError::Db`].
    pub async fn stored_default(pool: &SqlitePool) -> Result<Option<String>, MixError> {
        Ok(
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(DEFAULT_SETTING)
                .fetch_optional(pool)
                .await?,
        )
    }

    /// Stores the default selector (canonicalised to the profile id) or
    /// clears it when `selector` is absent/blank, audited as
    /// `settings.pricing`. Returns the effective profile afterwards.
    ///
    /// Failure mode: [`MixError::NotFound`] for an unknown selector,
    /// [`MixError::Db`] on storage errors.
    pub async fn set_stored_default(
        pool: &SqlitePool,
        actor: &Actor<'_>,
        selector: Option<&str>,
    ) -> Result<MixProfile, MixError> {
        let selector = selector.map(str::trim).filter(|value| !value.is_empty());
        let profile = match selector {
            Some(selector) => Some(
                Self::resolve_selector(pool, selector)
                    .await?
                    .ok_or(MixError::NotFound("mix profile"))?,
            ),
            None => None,
        };
        let mut tx = pool.begin().await?;
        let previous: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(DEFAULT_SETTING)
                .fetch_optional(&mut *tx)
                .await?;
        let stored = profile.as_ref().map(|profile| profile.id.clone());
        match &stored {
            Some(value) => {
                sqlx::query(
                    "INSERT INTO settings (key, value, updated_at, updated_by) \
                     VALUES (?, ?, ?, ?) \
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value, \
                       updated_at = excluded.updated_at, updated_by = excluded.updated_by",
                )
                .bind(DEFAULT_SETTING)
                .bind(value)
                .bind(catalog::now_ts())
                .bind(actor.user_id)
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM settings WHERE key = ?")
                    .bind(DEFAULT_SETTING)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        catalog::audit(
            &mut tx,
            actor,
            "settings.pricing",
            "settings",
            DEFAULT_SETTING,
            &serde_json::json!({
                "default_mix_profile": stored,
                "previous_default_mix_profile": previous,
            }),
        )
        .await?;
        tx.commit().await?;
        Self::resolve_default(pool, None).await
    }
}

/// One immutable view of the profile table plus the stored default selector.
#[derive(Debug)]
struct MixProfileSnapshot {
    by_id: HashMap<String, MixProfile>,
    by_name: HashMap<String, MixProfile>,
    stored_default: Option<String>,
}

/// In-memory cache of the token-mix profile table (D34).
///
/// The table changes only through the admin API, so the request path loads it
/// once (two SELECTs) and then resolves the default and a combo selector with
/// a hash lookup. [`Self::invalidate`] is called by every profile and pricing
/// mutation; a selector that misses triggers one lazy reload, so an edit made
/// by another process (or before an invalidation) still takes effect on the
/// next request. Only pure in-memory state: cloning the cache is cheap.
#[derive(Debug, Default)]
pub struct MixProfileCache {
    snapshot: RwLock<Option<Arc<MixProfileSnapshot>>>,
}

impl MixProfileCache {
    /// A cache with no snapshot; the first resolution loads it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops the snapshot so the next resolution reloads the table. Called by
    /// every mutation of a profile or of the default setting.
    pub fn invalidate(&self) {
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The current snapshot, loading it on first use. Failure mode:
    /// [`MixError::Db`] (a failed load leaves the cache empty so the next
    /// request retries).
    async fn snapshot(&self, pool: &SqlitePool) -> Result<Arc<MixProfileSnapshot>, MixError> {
        if let Some(snapshot) = self
            .snapshot
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Ok(snapshot);
        }
        self.refresh(pool).await
    }

    /// Reloads the snapshot and stores it. Failure mode: [`MixError::Db`].
    async fn refresh(&self, pool: &SqlitePool) -> Result<Arc<MixProfileSnapshot>, MixError> {
        let profiles = MixProfiles::list(pool).await?;
        let stored_default = MixProfiles::stored_default(pool).await?;
        let mut by_id = HashMap::with_capacity(profiles.len());
        let mut by_name = HashMap::with_capacity(profiles.len());
        for profile in profiles {
            by_id.insert(profile.id.clone(), profile.clone());
            by_name.insert(profile.name.clone(), profile);
        }
        let snapshot = Arc::new(MixProfileSnapshot {
            by_id,
            by_name,
            stored_default,
        });
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Some(snapshot.clone());
        Ok(snapshot)
    }

    fn lookup<'a>(snapshot: &'a MixProfileSnapshot, selector: &str) -> Option<&'a MixProfile> {
        snapshot
            .by_id
            .get(selector)
            .or_else(|| snapshot.by_name.get(selector))
    }

    fn default_from<'a>(
        snapshot: &'a MixProfileSnapshot,
        env_selector: Option<&str>,
    ) -> Option<&'a MixProfile> {
        env_selector
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
            .and_then(|selector| Self::lookup(snapshot, selector))
            .or_else(|| {
                snapshot
                    .stored_default
                    .as_deref()
                    .and_then(|selector| Self::lookup(snapshot, selector))
            })
            .or_else(|| Self::lookup(snapshot, CODE_AGENT))
    }

    /// Resolves the effective default profile from the cache:
    /// `env_selector` > the stored setting > the built-in `code-agent`, with
    /// the embedded preset as the last resort. Failure mode: [`MixError::Db`].
    pub async fn resolve_default(
        &self,
        pool: &SqlitePool,
        env_selector: Option<&str>,
    ) -> Result<MixProfile, MixError> {
        let snapshot = self.snapshot(pool).await?;
        if let Some(profile) = Self::default_from(&snapshot, env_selector) {
            return Ok(profile.clone());
        }
        // A miss can be a stale cache (an out-of-process edit); reload once.
        let snapshot = self.refresh(pool).await?;
        Ok(Self::default_from(&snapshot, env_selector)
            .cloned()
            .unwrap_or_else(code_agent_profile))
    }

    /// Resolves a selector (id wins over name) from the cache; a miss reloads
    /// once before answering `None`. Failure mode: [`MixError::Db`].
    pub async fn resolve_selector(
        &self,
        pool: &SqlitePool,
        selector: &str,
    ) -> Result<Option<MixProfile>, MixError> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Ok(None);
        }
        let snapshot = self.snapshot(pool).await?;
        if let Some(profile) = Self::lookup(&snapshot, selector) {
            return Ok(Some(profile.clone()));
        }
        let snapshot = self.refresh(pool).await?;
        Ok(Self::lookup(&snapshot, selector).cloned())
    }
}

/// A secret-free reference to a profile for API echoes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct MixProfileRef {
    pub profile_id: String,
    pub profile_name: String,
}

impl From<&MixProfile> for MixProfileRef {
    fn from(profile: &MixProfile) -> Self {
        Self {
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        }
    }
}

/// A per-offering or aggregate mix price as the overview API returns it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct MixPriceView {
    pub per_m: f64,
    pub profile_id: String,
    pub profile_name: String,
    /// `measured` when the profile was calibrated from our traffic, else
    /// `blended`.
    pub basis: String,
}

impl MixPriceView {
    pub fn new(per_m: f64, profile: &MixProfile) -> Self {
        Self {
            per_m,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            basis: profile.basis().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::test_support::TestApp;

    fn row(input: Option<f64>, output: Option<f64>, cache_read: Option<f64>) -> PriceRow {
        PriceRow {
            offering_id: "off".to_string(),
            input_per_m: input,
            output_per_m: output,
            cache_read_per_m: cache_read,
            cache_write_per_m: None,
            reasoning_per_m: None,
            request_fee: None,
            variable_price: 0,
            currency: "USD".to_string(),
            source: "manual".to_string(),
            source_url: None,
            fetched_at: None,
            updated_by: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn profile(name: &str) -> MixProfile {
        builtin_profile(name).expect("builtin preset")
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn each_preset_blends_the_reference_price_table() {
        // The reference table from docs/research/12-token-mix.md:
        // $3.00 input / $15.00 output / $0.30 cache read.
        let price = row(Some(3.0), Some(15.0), Some(0.3));
        for (name, expected) in [
            (CODE_AGENT, 0.5775),
            (CODE_AGENT_NOCACHE, 3.048),
            (CHAT, 6.12),
            (LONG_CONTEXT, 3.36),
        ] {
            let mix = mix_price(&price, &profile(name)).expect("priced");
            assert!(
                close(mix.per_m, expected),
                "{name}: {} != {expected}",
                mix.per_m
            );
            assert_eq!(mix.shares_label(), {
                let preset = profile(name);
                preset.normalised_shares().expect("valid shares").label()
            });
        }
        let code_agent = mix_price(&price, &profile(CODE_AGENT)).expect("priced");
        assert_eq!(code_agent.shares_label(), "8.1/0.4/91.5");
        let cache_read = code_agent
            .components
            .cache_read
            .as_ref()
            .expect("cache read component");
        assert_close(cache_read.contribution_per_m, 0.2745);
        assert!(cache_read.fallback.is_none());
        assert!(code_agent.components.cache_write.is_none());
        assert!(code_agent.components.reasoning.is_none());
    }

    fn assert_close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn cache_less_provider_falls_back_to_the_input_price() {
        // A published cached price is used; a missing one bills the cache
        // share at the input price, so `code-agent` on a cache-less provider
        // costs exactly as much as `code-agent-nocache` (never ~5x cheaper).
        let cache_less = row(Some(3.0), Some(15.0), None);
        let with_cache = row(Some(3.0), Some(15.0), Some(0.3));
        let code_agent = mix_price(&cache_less, &profile(CODE_AGENT)).expect("priced");
        let nocache = mix_price(&cache_less, &profile(CODE_AGENT_NOCACHE)).expect("priced");
        assert_close(code_agent.per_m, 3.048);
        assert_close(code_agent.per_m, nocache.per_m);
        let cache_read = code_agent
            .components
            .cache_read
            .as_ref()
            .expect("cache read component");
        assert_eq!(cache_read.price_per_m, 3.0);
        assert_eq!(cache_read.fallback.as_deref(), Some("input"));
        // And the cached price still wins when it exists.
        let cached = mix_price(&with_cache, &profile(CODE_AGENT)).expect("priced");
        assert_close(cached.per_m, 0.5775);
    }

    #[test]
    fn missing_component_fallbacks_cover_cache_write_and_reasoning() {
        let mut price = row(Some(3.0), Some(15.0), Some(0.3));
        price.cache_write_per_m = None;
        price.reasoning_per_m = None;
        let mut write_and_reasoning = profile(CODE_AGENT);
        write_and_reasoning.input_share = 50.0;
        write_and_reasoning.cache_write_share = 25.0;
        write_and_reasoning.reasoning_share = 25.0;
        write_and_reasoning.output_share = 0.0;
        write_and_reasoning.cache_read_share = 0.0;
        let mix = mix_price(&price, &write_and_reasoning).expect("priced");
        // 0.5 * 3 + 0.25 * 3 (cache write -> input) + 0.25 * 15 (reasoning ->
        // output).
        assert_close(mix.per_m, 1.5 + 0.75 + 3.75);
        assert_eq!(
            mix.components
                .cache_write
                .as_ref()
                .expect("write component")
                .fallback
                .as_deref(),
            Some("input")
        );
        assert_eq!(
            mix.components
                .reasoning
                .as_ref()
                .expect("reasoning component")
                .fallback
                .as_deref(),
            Some("output")
        );

        // A published price wins over the fallback.
        price.cache_write_per_m = Some(4.0);
        price.reasoning_per_m = Some(20.0);
        let explicit = mix_price(&price, &write_and_reasoning).expect("priced");
        assert_close(explicit.per_m, 1.5 + 1.0 + 5.0);
        assert!(
            explicit
                .components
                .cache_write
                .as_ref()
                .expect("write")
                .fallback
                .is_none()
        );
    }

    #[test]
    fn unpriced_rows_never_produce_a_zero_mix() {
        // No component at all.
        assert!(mix_price(&row(None, None, None), &profile(CODE_AGENT)).is_none());
        // A positive share whose component and fallback are both missing is
        // unpriced: a partial sum would look artificially cheap.
        assert!(
            mix_price(&row(Some(3.0), None, Some(0.3)), &profile(CODE_AGENT)).is_none(),
            "the output share needs an output price"
        );
        // The same row under a profile with no output share is priced.
        let input_only = mix_price(&row(Some(3.0), None, None), &profile(CODE_AGENT_NOCACHE));
        assert!(
            input_only.is_none(),
            "cache-less still needs the output share"
        );
        let mut input_only_profile = profile(LONG_CONTEXT);
        input_only_profile.input_share = 100.0;
        input_only_profile.output_share = 0.0;
        let priced = mix_price(&row(Some(3.0), None, None), &input_only_profile).expect("priced");
        assert_close(priced.per_m, 3.0);
    }

    #[test]
    fn shares_are_normalised_and_zero_shares_are_ignored() {
        let price = row(Some(3.0), Some(15.0), Some(0.3));
        let reference = mix_price(&price, &profile(CODE_AGENT)).expect("priced");
        let mut doubled = profile(CODE_AGENT);
        doubled.input_share = 16.2;
        doubled.output_share = 0.8;
        doubled.cache_read_share = 183.0;
        let scaled = mix_price(&price, &doubled).expect("priced");
        assert_close(scaled.per_m, reference.per_m);
        assert_close(scaled.effective_shares.input, 0.081);
        assert_close(scaled.effective_shares.output, 0.004);
        assert_close(scaled.effective_shares.cache_read, 0.915);

        // Zero shares are absent from the components: an unpriced component
        // with share 0 never makes the mix unpriced.
        let mut no_output = profile(CHAT);
        no_output.output_share = 0.0;
        let input_only = mix_price(&row(Some(3.0), None, None), &no_output).expect("priced");
        assert_close(input_only.per_m, 3.0);
        assert!(input_only.components.output.is_none());

        // An all-zero profile cannot be normalised.
        let mut zero = profile(CHAT);
        zero.input_share = 0.0;
        zero.output_share = 0.0;
        zero.cache_read_share = 0.0;
        assert!(mix_price(&price, &zero).is_none());
        assert!(
            MixShares {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                reasoning: 0.0
            }
            .normalised()
            .is_none()
        );
    }

    #[test]
    fn request_fee_is_not_part_of_the_per_million_mix() {
        let mut price = row(Some(3.0), Some(15.0), Some(0.3));
        price.request_fee = Some(0.01);
        let mix = mix_price(&price, &profile(CODE_AGENT)).expect("priced");
        assert_close(mix.per_m, 0.5775);
    }

    async fn seeded_app() -> TestApp {
        // The migration seeds four built-ins, so the app is ready to use.
        TestApp::new().await
    }

    #[tokio::test]
    async fn builtins_are_seeded_and_cannot_be_edited_or_deleted() {
        let app = seeded_app().await;
        let pool = &app.state.pool;
        let profiles = MixProfiles::list(pool).await.expect("list");
        assert_eq!(profiles.len(), 4);
        for (name, expected) in [
            (CODE_AGENT, "8.1/0.4/91.5"),
            (CODE_AGENT_NOCACHE, "99.6/0.4/0.0"),
            (CHAT, "74.0/26.0/0.0"),
            (LONG_CONTEXT, "97.0/3.0/0.0"),
        ] {
            let profile = MixProfiles::resolve_selector(pool, name)
                .await
                .expect("resolve")
                .expect("seeded");
            assert!(profile.is_builtin);
            assert_eq!(
                profile.normalised_shares().expect("valid shares").label(),
                expected
            );
        }
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let actor = Actor {
            user_id: &uid,
            ip: "127.0.0.1",
        };
        let error = MixProfiles::update(
            pool,
            &actor,
            "mix-chat",
            &MixProfileInput {
                name: "chat".to_string(),
                kind: None,
                input_share: 70.0,
                output_share: 30.0,
                cache_read_share: 0.0,
                cache_write_share: None,
                reasoning_share: None,
            },
        )
        .await
        .expect_err("built-ins are immutable");
        assert!(error.to_string().contains("built-in"), "{error}");
        let error = MixProfiles::delete(pool, &actor, "mix-chat")
            .await
            .expect_err("built-ins are protected");
        assert!(error.to_string().contains("cannot be deleted"), "{error}");
    }

    #[tokio::test]
    async fn profiles_round_trip_and_validate() {
        let app = seeded_app().await;
        let pool = &app.state.pool;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let actor = Actor {
            user_id: &uid,
            ip: "127.0.0.1",
        };
        let created = MixProfiles::create(
            pool,
            &actor,
            &MixProfileInput {
                name: "team-agent".to_string(),
                kind: Some("measured".to_string()),
                input_share: 5.0,
                output_share: 1.0,
                cache_read_share: 94.0,
                cache_write_share: None,
                reasoning_share: None,
            },
        )
        .await
        .expect("create");
        assert_eq!(created.name, "team-agent");
        assert_eq!(created.kind, "measured");
        assert_eq!(created.basis(), "measured");
        assert!(!created.is_builtin);
        assert_eq!(
            MixProfiles::get(pool, &created.id).await.expect("get"),
            Some(created.clone())
        );

        let updated = MixProfiles::update(
            pool,
            &actor,
            &created.id,
            &MixProfileInput {
                name: "team-agent-v2".to_string(),
                kind: Some("custom".to_string()),
                input_share: 10.0,
                output_share: 2.0,
                cache_read_share: 88.0,
                cache_write_share: Some(0.0),
                reasoning_share: Some(0.0),
            },
        )
        .await
        .expect("update")
        .expect("exists");
        assert_eq!(updated.name, "team-agent-v2");
        assert_eq!(updated.basis(), "blended");
        assert_eq!(
            MixProfiles::resolve_selector(pool, "team-agent-v2")
                .await
                .expect("resolve")
                .expect("found")
                .id,
            created.id
        );

        // Duplicate names (including a built-in's) are rejected.
        let duplicate = MixProfiles::create(
            pool,
            &actor,
            &MixProfileInput {
                name: "chat".to_string(),
                kind: None,
                input_share: 1.0,
                output_share: 1.0,
                cache_read_share: 0.0,
                cache_write_share: None,
                reasoning_share: None,
            },
        )
        .await
        .expect_err("duplicate name");
        assert!(
            duplicate.to_string().contains("already in use"),
            "{duplicate}"
        );

        // `preset` is reserved for the seeded built-ins; operator profiles are
        // `custom` or `measured` only.
        for kind in ["preset", "weird"] {
            let error = MixProfiles::create(
                pool,
                &actor,
                &MixProfileInput {
                    name: "operator".to_string(),
                    kind: Some(kind.to_string()),
                    input_share: 1.0,
                    output_share: 1.0,
                    cache_read_share: 0.0,
                    cache_write_share: None,
                    reasoning_share: None,
                },
            )
            .await
            .expect_err("reserved/invalid kind");
            assert!(error.to_string().contains("custom or measured"), "{error}");
        }

        // Negative and all-zero shares are rejected.
        for (input_share, output_share) in [(-1.0, 2.0), (0.0, 0.0)] {
            let error = MixProfiles::create(
                pool,
                &actor,
                &MixProfileInput {
                    name: "bad".to_string(),
                    kind: None,
                    input_share,
                    output_share,
                    cache_read_share: 0.0,
                    cache_write_share: None,
                    reasoning_share: None,
                },
            )
            .await
            .expect_err("invalid shares");
            assert!(matches!(error, MixError::Invalid(_)), "{error}");
        }

        assert!(
            MixProfiles::delete(pool, &actor, &created.id)
                .await
                .expect("delete")
        );
        assert!(
            MixProfiles::get(pool, &created.id)
                .await
                .expect("get")
                .is_none()
        );
        assert!(
            !MixProfiles::delete(pool, &actor, &created.id)
                .await
                .expect("second delete"),
            "an unknown id is a no-op"
        );
    }

    #[tokio::test]
    async fn default_resolution_prefers_env_then_setting_then_code_agent() {
        let app = seeded_app().await;
        let pool = &app.state.pool;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let actor = Actor {
            user_id: &uid,
            ip: "127.0.0.1",
        };
        assert_eq!(
            MixProfiles::resolve_default(pool, None)
                .await
                .expect("default")
                .name,
            CODE_AGENT
        );
        // Unknown env selectors fall through to the built-in default.
        assert_eq!(
            MixProfiles::resolve_default(pool, Some("nope"))
                .await
                .expect("default")
                .name,
            CODE_AGENT
        );
        // The setting wins over the built-in, by name or id.
        let stored = MixProfiles::set_stored_default(pool, &actor, Some("chat"))
            .await
            .expect("store");
        assert_eq!(stored.name, CHAT);
        assert_eq!(
            MixProfiles::resolve_default(pool, None)
                .await
                .expect("setting")
                .name,
            CHAT
        );
        assert_eq!(
            MixProfiles::stored_default(pool)
                .await
                .expect("stored")
                .as_deref(),
            Some("mix-chat"),
            "the selector is canonicalised to the profile id"
        );
        assert_eq!(
            MixProfiles::resolve_default(pool, Some("long-context"))
                .await
                .expect("env")
                .name,
            LONG_CONTEXT,
            "the environment wins over the setting"
        );
        assert_eq!(
            MixProfiles::resolve_default(pool, Some("mix-chat"))
                .await
                .expect("env id")
                .name,
            CHAT,
            "ids resolve like names"
        );
        // Clearing the setting falls back to code-agent.
        MixProfiles::set_stored_default(pool, &actor, None)
            .await
            .expect("clear");
        assert!(
            MixProfiles::stored_default(pool)
                .await
                .expect("stored")
                .is_none()
        );
        assert_eq!(
            MixProfiles::resolve_default(pool, None)
                .await
                .expect("cleared")
                .name,
            CODE_AGENT
        );
        // Storing an unknown selector is a 404-worthy error.
        let error = MixProfiles::set_stored_default(pool, &actor, Some("nope"))
            .await
            .expect_err("unknown selector");
        assert!(matches!(error, MixError::NotFound(_)), "{error}");
    }
}
