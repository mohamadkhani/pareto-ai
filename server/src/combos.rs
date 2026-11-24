//! Combo models (M5): operator-defined routing policies, addressable as the
//! model id `combo:<name>` in `POST /v1/chat/completions` (spec M5 acceptance
//! 5 — the model-id namespace was chosen over a separate `/v1/chat/combos`
//! endpoint so an OpenAI SDK addresses a combo exactly like a model).
//!
//! A combo expands to its member **offerings** (the routable unit: provider +
//! model + credential), then the router applies the combo's strategy
//! (decision D6):
//!
//! * `ordered` — members in their stored order, a fixed fallback list;
//! * `weighted` — [`crate::routing::weighted_order`] with the combo's weights
//!   (defaults or a preset) and per-member weight multipliers;
//! * `round_robin` — a stable rotation over the members, persisted in
//!   `combo_models.rr_cursor` and advanced once per request that actually
//!   dispatches upstream (a request refused before dispatch does not consume
//!   the rotation — review nit).
//!
//! An unknown or disabled combo answers `404 model_not_found`, exactly like an
//! unknown model. Enabled combos are advertised by `GET /v1/models` as
//! `combo:<name>` entries ([`list_v1_entries`]) so clients can discover them.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::catalog::{self, Actor};
use crate::routing::{self, RoutingError, Strategy, Weights};

/// The model-id namespace: `combo:<name>`.
pub const NAMESPACE: &str = "combo:";

/// Minimum accepted stickiness TTL (60 seconds).
pub const MIN_STICKY_TTL_SECONDS: i64 = 60;
/// Maximum accepted stickiness TTL (24 hours).
pub const MAX_STICKY_TTL_SECONDS: i64 = 86_400;
/// Upper bound on sticky pins kept per combo (D35): bounds memory even when a
/// caller sends an endless stream of session ids.
pub const MAX_STICKY_PER_COMBO: i64 = 10_000;

/// Where a combo's candidates come from (D35).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pool {
    /// The explicit `members` list.
    Members,
    /// Every eligible offering in the catalog that satisfies the constraints.
    All,
}

impl Pool {
    /// Parses the stored pool name. Failure mode: [`ComboError::Invalid`].
    pub fn parse(value: &str) -> Result<Self, ComboError> {
        match value {
            "members" => Ok(Self::Members),
            "all" => Ok(Self::All),
            other => Err(ComboError::Invalid(format!(
                "unknown pool {other:?}; expected members or all"
            ))),
        }
    }

    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Members => "members",
            Self::All => "all",
        }
    }
}

/// Which key a sticky combo pins on (D35).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// `X-Session-Id` when present, otherwise the stable conversation hash.
    Auto,
    /// `X-Session-Id` only; requests without the header are not pinned.
    Header,
}

impl KeySource {
    /// Parses the stored key-source name. Failure mode: [`ComboError::Invalid`].
    pub fn parse(value: &str) -> Result<Self, ComboError> {
        match value {
            "auto" => Ok(Self::Auto),
            "header" => Ok(Self::Header),
            other => Err(ComboError::Invalid(format!(
                "unknown stickiness key_source {other:?}; expected auto or header"
            ))),
        }
    }

    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Header => "header",
        }
    }
}

/// Combo storage/validation failures. Failure mode: `400`-worthy
/// [`ComboError::Invalid`] on bad input, `500`-worthy storage errors.
#[derive(Debug, thiserror::Error)]
pub enum ComboError {
    #[error("combo configuration is invalid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Routing(#[from] RoutingError),
    #[error("combo storage failed: {0}")]
    Db(#[from] sqlx::Error),
    #[error("combo audit failed: {0}")]
    Audit(#[from] catalog::CatalogError),
}

impl ComboError {
    /// The uniform API error for this failure.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        match self {
            Self::Invalid(message) => {
                crate::error::ApiError::bad_request(message).with_request_id(rid)
            }
            Self::Routing(e) => {
                crate::error::ApiError::bad_request(e.to_string()).with_request_id(rid)
            }
            Self::Db(e) => {
                crate::error::ApiError::internal("combo storage", e).with_request_id(rid)
            }
            Self::Audit(e) => e.into_api_error(rid),
        }
    }
}

/// One member of a combo: an offering with an optional per-member weight
/// (the `weighted` strategy multiplies its score with it).
#[derive(Debug, Clone, PartialEq)]
pub struct ComboMember {
    pub offering_id: String,
    pub weight: f64,
}

/// A loaded combo row.
#[derive(Debug, Clone, PartialEq)]
pub struct Combo {
    pub id: String,
    pub name: String,
    pub strategy: Strategy,
    pub weights: Weights,
    /// The named preset behind `weights`, when `weights_json` named one.
    pub preset: Option<&'static str>,
    /// The canonical id of the combo's token-mix profile, when set (D34). It
    /// overrides the global default.
    pub mix_profile: Option<String>,
    /// Explicit members or the whole-catalog pool (D35).
    pub pool: Pool,
    /// The minimum resolved quality for a `pool = all` combo (inclusive).
    pub min_quality: Option<f64>,
    /// The maximum mix price per million tokens for a `pool = all` combo
    /// (inclusive).
    pub max_mix_price_per_m: Option<f64>,
    pub members: Vec<ComboMember>,
    pub enabled: bool,
    /// The persisted round-robin rotation counter.
    pub rr_cursor: i64,
    /// Whether the chosen offering is pinned per session (D35).
    pub stickiness_enabled: bool,
    pub stickiness_ttl_seconds: i64,
    pub stickiness_key_source: KeySource,
}

/// One member as created.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
pub struct MemberInput {
    pub offering_id: String,
    /// Score multiplier (`weighted` strategy); defaults to 1.0.
    pub weight: Option<f64>,
}

/// Per-session stickiness configuration (D35).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
pub struct StickinessInput {
    pub enabled: Option<bool>,
    /// Pin lifetime in seconds (60 s – 24 h; default 1800).
    pub ttl_seconds: Option<i64>,
    /// `auto` (default) or `header`.
    pub key_source: Option<String>,
}

/// A combo as created (or replaced, on update).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
pub struct ComboInput {
    /// The name behind the `combo:<name>` namespace (immutable after create).
    pub name: String,
    /// `ordered`, `weighted`, or `round_robin`.
    pub strategy: String,
    /// Raw `weights_json`: null, `{"preset": "..."}`, or a partial object.
    /// An optional `mix_profile` inside the object names the combo's token-mix
    /// profile (id or name, canonicalised to the id on save; D34). The
    /// top-level `mix_profile` wins when both are present.
    pub weights: Option<serde_json::Value>,
    #[serde(default)]
    pub members: Vec<MemberInput>,
    pub enabled: Option<bool>,
    /// `members` (default) or `all` (D35).
    pub pool: Option<String>,
    /// Minimum resolved quality for `pool = all` (inclusive).
    pub min_quality: Option<f64>,
    /// Maximum mix price per million tokens for `pool = all` (inclusive).
    pub max_mix_price_per_m: Option<f64>,
    /// The token-mix profile id or name (D34).
    pub mix_profile: Option<String>,
    /// Session stickiness (D35).
    pub stickiness: Option<StickinessInput>,
}

/// One combo member as returned by the admin API.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MemberView {
    pub offering_id: String,
    pub weight: f64,
}

/// Session stickiness as returned by the admin API.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct StickinessView {
    pub enabled: bool,
    pub ttl_seconds: i64,
    pub key_source: String,
}

/// A combo as returned by the admin API.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ComboView {
    pub id: String,
    /// The `combo:<name>` name.
    pub name: String,
    pub strategy: String,
    /// The effective scoring weights.
    pub weights: serde_json::Value,
    /// The named preset behind `weights`, when one.
    pub preset: Option<String>,
    /// The combo's token-mix profile id, when set (overrides the global
    /// default for this combo's requests).
    pub mix_profile: Option<String>,
    /// `members` or `all`.
    pub pool: String,
    /// The inclusive minimum quality for a catalog pool.
    pub min_quality: Option<f64>,
    /// The inclusive maximum mix price for a catalog pool.
    pub max_mix_price_per_m: Option<f64>,
    pub members: Vec<MemberView>,
    pub enabled: bool,
    /// The persisted round-robin rotation counter.
    pub rr_cursor: i64,
    pub stickiness: StickinessView,
}

impl From<&Combo> for ComboView {
    fn from(combo: &Combo) -> Self {
        Self {
            id: combo.id.clone(),
            name: combo.name.clone(),
            strategy: combo.strategy.as_str().to_string(),
            weights: combo.weights.to_json(),
            preset: combo.preset.map(str::to_owned),
            mix_profile: combo.mix_profile.clone(),
            pool: combo.pool.as_str().to_string(),
            min_quality: combo.min_quality,
            max_mix_price_per_m: combo.max_mix_price_per_m,
            members: combo
                .members
                .iter()
                .map(|member| MemberView {
                    offering_id: member.offering_id.clone(),
                    weight: member.weight,
                })
                .collect(),
            enabled: combo.enabled,
            rr_cursor: combo.rr_cursor,
            stickiness: StickinessView {
                enabled: combo.stickiness_enabled,
                ttl_seconds: combo.stickiness_ttl_seconds,
                key_source: combo.stickiness_key_source.as_str().to_string(),
            },
        }
    }
}

/// Parses `members_json`: `[{"offering_id": "...", "weight": 1.0}, ...]`.
/// Failure mode: [`ComboError::Invalid`].
fn parse_members(raw: &str) -> Result<Vec<ComboMember>, ComboError> {
    let values: Vec<serde_json::Value> = serde_json::from_str(raw)
        .map_err(|e| ComboError::Invalid(format!("members_json is not a list: {e}")))?;
    let mut members = Vec::with_capacity(values.len());
    for value in &values {
        let offering_id = value
            .get("offering_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                ComboError::Invalid("member offering_id must be a string".to_string())
            })?;
        let weight = match value.get("weight") {
            None | Some(serde_json::Value::Null) => 1.0,
            Some(weight) => weight.as_f64().ok_or_else(|| {
                ComboError::Invalid(format!("member {offering_id} weight must be a number"))
            })?,
        };
        members.push(ComboMember {
            offering_id,
            weight,
        });
    }
    // Stored members may be empty for a catalog pool; shape checks still run.
    validate_member_entries(&members)?;
    Ok(members)
}

/// Members must be a non-empty list of unique offerings with positive finite
/// weights. Failure mode: [`ComboError::Invalid`].
fn validate_members(members: &[ComboMember]) -> Result<(), ComboError> {
    if members.is_empty() {
        return Err(ComboError::Invalid("members must not be empty".to_string()));
    }
    validate_member_entries(members)
}

/// Shape checks shared by input validation and stored-row parsing (empty is
/// allowed here because a catalog pool has no explicit members).
fn validate_member_entries(members: &[ComboMember]) -> Result<(), ComboError> {
    for member in members {
        if member.offering_id.trim().is_empty() {
            return Err(ComboError::Invalid(
                "member offering_id must not be empty".to_string(),
            ));
        }
        if !member.weight.is_finite() || member.weight <= 0.0 {
            return Err(ComboError::Invalid(format!(
                "member {} weight must be a positive number",
                member.offering_id
            )));
        }
    }
    for (index, member) in members.iter().enumerate() {
        if members[..index]
            .iter()
            .any(|earlier| earlier.offering_id == member.offering_id)
        {
            return Err(ComboError::Invalid(format!(
                "member {} is listed twice",
                member.offering_id
            )));
        }
    }
    Ok(())
}

/// Serializes members back to the stored `members_json` shape.
fn members_json(members: &[ComboMember]) -> String {
    serde_json::Value::Array(
        members
            .iter()
            .map(|member| {
                serde_json::json!({
                    "offering_id": member.offering_id,
                    "weight": member.weight,
                })
            })
            .collect(),
    )
    .to_string()
}

/// The validated shape of a combo input, ready to persist.
struct Validated {
    name: String,
    strategy: Strategy,
    weights: Weights,
    preset: Option<&'static str>,
    /// The canonical profile id stored in `combo_models.mix_profile`.
    mix_profile: Option<String>,
    weights_raw: Option<String>,
    pool: Pool,
    min_quality: Option<f64>,
    max_mix_price_per_m: Option<f64>,
    members: Vec<ComboMember>,
    enabled: bool,
    stickiness_enabled: bool,
    stickiness_ttl_seconds: i64,
    stickiness_key_source: KeySource,
}

/// Reads the top-level `mix_profile`, falling back to
/// `weights_json.mix_profile` (null/absent or a non-empty string), and
/// resolves it to a profile id. Failure mode: [`ComboError::Invalid`] for a
/// non-string value or an unknown profile.
async fn validate_mix_profile(
    pool: &sqlx::SqlitePool,
    explicit: Option<&str>,
    weights: Option<&serde_json::Value>,
) -> Result<Option<String>, ComboError> {
    let value = match explicit {
        Some(value) => Some(serde_json::Value::String(value.to_string())),
        None => weights
            .and_then(|weights| weights.get("mix_profile"))
            .cloned(),
    };
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let selector = value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ComboError::Invalid("mix_profile must be a non-empty string".to_string()))?;
    let profile = crate::mix::MixProfiles::resolve_selector(pool, selector)
        .await
        .map_err(combo_mix_error)?
        .ok_or_else(|| ComboError::Invalid(format!("mix profile {selector:?} does not exist")))?;
    Ok(Some(profile.id))
}

/// Maps a mix-domain failure onto the combo error vocabulary: storage and
/// audit failures stay internal, invalid or missing selectors are `400`.
fn combo_mix_error(error: crate::mix::MixError) -> ComboError {
    match error {
        crate::mix::MixError::Db(error) => ComboError::Db(error),
        crate::mix::MixError::Audit(error) => ComboError::Audit(error),
        crate::mix::MixError::Invalid(message) => ComboError::Invalid(message),
        crate::mix::MixError::NotFound(what) => ComboError::Invalid(format!("{what} not found")),
    }
}

/// Validates a combo input (name, strategy, weights, members, and that every
/// member offering exists). Failure mode: [`ComboError::Invalid`] naming the
/// offending field.
async fn validate(pool: &SqlitePool, input: &ComboInput) -> Result<Validated, ComboError> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ComboError::Invalid("name must not be empty".to_string()));
    }
    if name.contains(':') {
        return Err(ComboError::Invalid(
            "name must not contain ':' (the combo: namespace separator)".to_string(),
        ));
    }
    let strategy = Strategy::parse(&input.strategy)?;
    let pool_kind = match input.pool.as_deref() {
        None => Pool::Members,
        Some(value) => Pool::parse(value.trim())?,
    };
    validate_constraints(
        pool_kind,
        strategy,
        input.min_quality,
        input.max_mix_price_per_m,
    )?;
    let (stickiness_enabled, stickiness_ttl_seconds, stickiness_key_source) =
        validate_stickiness(input.stickiness.as_ref())?;
    let mix_profile =
        validate_mix_profile(pool, input.mix_profile.as_deref(), input.weights.as_ref()).await?;
    // Canonicalise the stored selector to the profile id so a profile rename
    // cannot silently detach a combo, and keep the column authoritative by
    // dropping the legacy key from `weights_json`.
    let mut weights_value = input.weights.clone();
    if let Some(object) = weights_value
        .as_mut()
        .and_then(|value| value.as_object_mut())
    {
        object.remove("mix_profile");
    }
    let weights_raw = weights_value.as_ref().map(|value| value.to_string());
    let (weights, preset) = Weights::from_json(weights_raw.as_deref())?;
    let members: Vec<ComboMember> = input
        .members
        .iter()
        .map(|member| ComboMember {
            offering_id: member.offering_id.clone(),
            weight: member.weight.unwrap_or(1.0),
        })
        .collect();
    // A catalog pool needs no explicit list; an explicit-list combo does.
    if pool_kind == Pool::Members || !members.is_empty() {
        validate_members(&members)?;
    }
    for member in &members {
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM offerings WHERE id = ?")
            .bind(&member.offering_id)
            .fetch_optional(pool)
            .await?;
        if exists.is_none() {
            return Err(ComboError::Invalid(format!(
                "member offering {} does not exist",
                member.offering_id
            )));
        }
    }
    Ok(Validated {
        name: name.to_string(),
        strategy,
        weights,
        preset,
        mix_profile,
        weights_raw,
        pool: pool_kind,
        min_quality: input.min_quality,
        max_mix_price_per_m: input.max_mix_price_per_m,
        members,
        enabled: input.enabled.unwrap_or(true),
        stickiness_enabled,
        stickiness_ttl_seconds,
        stickiness_key_source,
    })
}

/// A catalog pool (`pool = all`) needs at least one constraint and the
/// `weighted` strategy; constraint values must be usable numbers and the
/// stickiness TTL must be between 60 seconds and 24 hours. Failure mode:
/// [`ComboError::Invalid`] naming the offending field.
fn validate_constraints(
    pool: Pool,
    strategy: Strategy,
    min_quality: Option<f64>,
    max_mix_price_per_m: Option<f64>,
) -> Result<(), ComboError> {
    if let Some(value) = min_quality
        && (!value.is_finite() || value < 0.0)
    {
        return Err(ComboError::Invalid(
            "min_quality must be a finite, non-negative number".to_string(),
        ));
    }
    if let Some(value) = max_mix_price_per_m
        && (!value.is_finite() || value <= 0.0)
    {
        return Err(ComboError::Invalid(
            "max_mix_price_per_m must be a finite number greater than zero".to_string(),
        ));
    }
    if pool == Pool::All {
        if min_quality.is_none() && max_mix_price_per_m.is_none() {
            return Err(ComboError::Invalid(
                "pool = all requires at least one constraint (min_quality or \
                 max_mix_price_per_m); an unconstrained catalog pool is almost always a mistake"
                    .to_string(),
            ));
        }
        if strategy != Strategy::Weighted {
            return Err(ComboError::Invalid(
                "pool = all only supports the weighted strategy: ordered and round_robin need an \
                 explicit member list"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// Validates the stickiness block; absent means disabled with the defaults.
/// Failure mode: [`ComboError::Invalid`] for a bad TTL or key source.
fn validate_stickiness(
    input: Option<&StickinessInput>,
) -> Result<(bool, i64, KeySource), ComboError> {
    let enabled = input.and_then(|value| value.enabled).unwrap_or(false);
    let ttl = input.and_then(|value| value.ttl_seconds).unwrap_or(1800);
    if !(MIN_STICKY_TTL_SECONDS..=MAX_STICKY_TTL_SECONDS).contains(&ttl) {
        return Err(ComboError::Invalid(format!(
            "stickiness.ttl_seconds must be between {MIN_STICKY_TTL_SECONDS} and \
             {MAX_STICKY_TTL_SECONDS} seconds"
        )));
    }
    let key_source = match input.and_then(|value| value.key_source.as_deref()) {
        None => KeySource::Auto,
        Some(value) => KeySource::parse(value.trim())?,
    };
    Ok((enabled, ttl, key_source))
}

/// Validates a draft combo without writing it, for the preview endpoints.
/// Returns a transient [`Combo`] whose `id` is `preview`. Failure mode:
/// [`ComboError::Invalid`] for bad input, [`ComboError::Db`] on storage
/// errors while resolving members or the mix profile.
pub async fn validate_config(pool: &SqlitePool, input: &ComboInput) -> Result<Combo, ComboError> {
    let validated = validate(pool, input).await?;
    Ok(Combo {
        id: "preview".to_string(),
        name: validated.name,
        strategy: validated.strategy,
        weights: validated.weights,
        preset: validated.preset,
        mix_profile: validated.mix_profile,
        pool: validated.pool,
        min_quality: validated.min_quality,
        max_mix_price_per_m: validated.max_mix_price_per_m,
        members: validated.members,
        enabled: validated.enabled,
        rr_cursor: 0,
        stickiness_enabled: validated.stickiness_enabled,
        stickiness_ttl_seconds: validated.stickiness_ttl_seconds,
        stickiness_key_source: validated.stickiness_key_source,
    })
}

// ---------------------------------------------------------------------------
// Catalog-pool resolution (D35)
// ---------------------------------------------------------------------------

/// The constraints that turn the catalog into a `pool = all` candidate set.
/// Both are inclusive; `None` means "not constrained".
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PoolConstraints {
    pub min_quality: Option<f64>,
    pub max_mix_price_per_m: Option<f64>,
}

impl PoolConstraints {
    /// The constraints of a stored combo.
    pub fn of(combo: &Combo) -> Self {
        Self {
            min_quality: combo.min_quality,
            max_mix_price_per_m: combo.max_mix_price_per_m,
        }
    }
}

/// One offering ready for pool resolution: the routing signal plus the
/// resolved quality and mix price the constraints are checked against.
#[derive(Debug, Clone)]
pub struct PoolItem {
    pub offering_id: String,
    /// The model's canonical name (the display `model` field).
    pub model: String,
    pub provider: String,
    pub quality: routing::QualityScore,
    pub mix_price: Option<crate::mix::MixPrice>,
    pub signal: routing::CandidateSignal,
}

/// Why a catalog pool excluded an offering (D35's one-line vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolReason {
    Disabled,
    NoAccess,
    Cooldown,
    BreakerOpen,
    NoCredential,
    CapabilityMismatch,
    ContextExceeded,
    BudgetExhausted,
    Unpriced,
    BelowMinQuality,
    AboveMaxPrice,
}

impl PoolReason {
    /// The stable string the preview and `decision_json` report.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NoAccess => "no_access",
            Self::Cooldown => "cooldown",
            Self::BreakerOpen => "breaker_open",
            Self::NoCredential => "no_credential",
            Self::CapabilityMismatch => "capability",
            Self::ContextExceeded => "context",
            Self::BudgetExhausted => "budget",
            Self::Unpriced => "unpriced",
            Self::BelowMinQuality => "below_min_quality",
            Self::AboveMaxPrice => "above_max_price",
        }
    }
}

impl From<routing::Exclusion> for PoolReason {
    fn from(reason: routing::Exclusion) -> Self {
        match reason {
            routing::Exclusion::Disabled => Self::Disabled,
            routing::Exclusion::NoCredential => Self::NoCredential,
            routing::Exclusion::AccessDenied => Self::NoAccess,
            routing::Exclusion::CapabilityMismatch => Self::CapabilityMismatch,
            routing::Exclusion::ContextExceeded => Self::ContextExceeded,
            routing::Exclusion::BudgetExhausted => Self::BudgetExhausted,
            routing::Exclusion::BreakerOpen => Self::BreakerOpen,
        }
    }
}

/// One excluded offering with a one-line reason and detail.
#[derive(Debug, Clone)]
pub struct PoolExclusion {
    pub offering_id: String,
    pub model: String,
    pub provider: String,
    pub reason: PoolReason,
    pub detail: String,
}

/// One ordered, scored catalog-pool candidate.
#[derive(Debug, Clone)]
pub struct PoolScored {
    pub item: PoolItem,
    pub score: routing::Score,
    pub explanation: String,
}

/// The full catalog-pool outcome: ordered candidates best-first and the
/// exclusions with one-line reasons.
#[derive(Debug, Clone)]
pub struct PoolOutcome {
    pub ordered: Vec<PoolScored>,
    pub excluded: Vec<PoolExclusion>,
}

/// The shared resolution engine behind the runtime `weighted` path and the
/// preview endpoints (D35): applies access/enabled/cooldown/breaker filters,
/// then the inclusive quality and mix-price constraints, then
/// [`routing::weighted_order`] with the combo's weights. Deterministic — the
/// same items, constraints, and weights always produce the same order.
pub fn resolve_pool(
    items: Vec<PoolItem>,
    constraints: &PoolConstraints,
    weights: &Weights,
) -> PoolOutcome {
    let mut kept: Vec<PoolItem> = Vec::with_capacity(items.len());
    let mut excluded: Vec<PoolExclusion> = Vec::new();
    for item in items {
        if let Some((reason, detail)) = pool_exclusion(&item, constraints) {
            excluded.push(PoolExclusion {
                offering_id: item.offering_id,
                model: item.model,
                provider: item.provider,
                reason,
                detail,
            });
        } else {
            kept.push(item);
        }
    }

    let signals: Vec<routing::CandidateSignal> =
        kept.iter().map(|item| item.signal.clone()).collect();
    let outcome = routing::weighted_order(signals, weights);
    let mut by_id: HashMap<String, PoolItem> = kept
        .into_iter()
        .map(|item| (item.offering_id.clone(), item))
        .collect();
    let mut ordered = Vec::with_capacity(outcome.ordered.len());
    for scored in outcome.ordered {
        if let Some(item) = by_id.remove(&scored.id) {
            ordered.push(PoolScored {
                item,
                score: scored.score,
                explanation: scored.explanation,
            });
        }
    }
    for (id, reason) in outcome.excluded {
        if let Some(item) = by_id.remove(&id) {
            excluded.push(PoolExclusion {
                offering_id: item.offering_id,
                model: item.model,
                provider: item.provider,
                reason: reason.into(),
                detail: reason.explain(),
            });
        }
    }
    PoolOutcome { ordered, excluded }
}

/// The first constraint/filter failure of a pool item, in pipeline order.
fn pool_exclusion(item: &PoolItem, constraints: &PoolConstraints) -> Option<(PoolReason, String)> {
    let signal = &item.signal;
    if !signal.enabled {
        return Some((
            PoolReason::Disabled,
            "the offering or its provider is disabled".to_string(),
        ));
    }
    if !signal.granted {
        return Some((
            PoolReason::NoAccess,
            "access denied by grants for this caller".to_string(),
        ));
    }
    if signal.credential_cooled {
        return Some((
            PoolReason::Cooldown,
            "the credential is in cooldown after an upstream auth/quota/rate-limit failure"
                .to_string(),
        ));
    }
    if signal.breaker_open {
        return Some((
            PoolReason::BreakerOpen,
            "the circuit breaker is open for this credential on this offering".to_string(),
        ));
    }
    if let Some(max) = constraints.max_mix_price_per_m
        && item.mix_price.is_none()
    {
        return Some((
            PoolReason::Unpriced,
            format!("no mix price is computable and the combo caps it at ${max:.4}/M"),
        ));
    }
    if let Some(min) = constraints.min_quality
        && item.quality.score < min
    {
        return Some((
            PoolReason::BelowMinQuality,
            format!(
                "quality {:.4} is below the minimum {min:.4}",
                item.quality.score
            ),
        ));
    }
    if let (Some(max), Some(mix)) = (constraints.max_mix_price_per_m, &item.mix_price)
        && mix.per_m > max
    {
        return Some((
            PoolReason::AboveMaxPrice,
            format!(
                "mix price ${:.4}/M is above the maximum ${max:.4}/M",
                mix.per_m
            ),
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// Preview views (D35)
// ---------------------------------------------------------------------------

/// The score breakdown the preview returns (a serialisable mirror of
/// [`routing::Score`]).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct PreviewScore {
    pub price: f64,
    pub speed: f64,
    pub quality: f64,
    pub reliability: f64,
    pub penalties: f64,
    pub total: f64,
    pub imputed: Vec<String>,
    pub shrunk: Vec<String>,
}

impl From<&routing::Score> for PreviewScore {
    fn from(score: &routing::Score) -> Self {
        Self {
            price: score.price,
            speed: score.speed,
            quality: score.quality,
            reliability: score.reliability,
            penalties: score.penalties,
            total: score.total,
            imputed: score.imputed.iter().map(|s| (*s).to_string()).collect(),
            shrunk: score.shrunk.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

/// One ranked preview candidate.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct PreviewCandidate {
    pub offering_id: String,
    pub model: String,
    pub provider: String,
    pub quality: f64,
    pub quality_source: String,
    pub mix_price: Option<f64>,
    pub score: PreviewScore,
    pub explanation: String,
}

/// One preview exclusion with its reason.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct PreviewExcluded {
    pub offering_id: String,
    pub model: String,
    pub provider: String,
    pub reason: String,
    pub detail: String,
}

/// The current pin for a supplied/example session key.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct PreviewSticky {
    pub enabled: bool,
    pub key_source: String,
    pub session_key: Option<String>,
    pub pinned: Option<String>,
    pub age_s: Option<i64>,
}

/// The preview response: the profile used, ranked candidates, exclusions,
/// and (for sticky combos) the current pin for the supplied session key.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct PreviewResult {
    pub profile: crate::mix::MixProfileRef,
    /// How many offerings passed every filter and were scored. `candidates`
    /// may hold fewer than this (`limit` truncates the list, the score count
    /// does not); the UI prints both so "#1 of 90" and "showing 50" agree.
    pub scored: usize,
    pub candidates: Vec<PreviewCandidate>,
    pub excluded: Vec<PreviewExcluded>,
    pub sticky: Option<PreviewSticky>,
}

// ---------------------------------------------------------------------------
// Session stickiness (D35)
// ---------------------------------------------------------------------------

/// A stored sticky pin.
#[derive(Debug, Clone, PartialEq)]
pub struct StickyChoice {
    pub offering_id: String,
    pub chosen_at: String,
    pub expires_at: String,
}

/// The stable session key recipe (D35):
///
/// * `X-Session-Id` when present and non-empty (both key sources);
/// * with `key_source = auto` and no header, `sha256("combo-session-v1\0" +
///   subject + "\0" + system_prompt + "\0" + first_user_message)`, hex
///   encoded. `subject` is the API-key id when the request carries one, else
///   the user id. The NUL separators keep concatenations unambiguous.
/// * with `key_source = header` and no header, `None` (the request is not
///   pinned).
///
/// Returns `None` when no key can be derived.
pub fn session_key(
    key_source: KeySource,
    header: Option<&str>,
    subject: &str,
    system_prompt: Option<&str>,
    first_user_message: Option<&str>,
) -> Option<String> {
    const MAX_KEY_CHARS: usize = 512;
    if let Some(header) = header.map(str::trim).filter(|value| !value.is_empty()) {
        let key: String = header.chars().take(MAX_KEY_CHARS).collect();
        return Some(key);
    }
    if key_source == KeySource::Header {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"combo-session-v1\0");
    hasher.update(subject.as_bytes());
    hasher.update(b"\0");
    hasher.update(system_prompt.unwrap_or("").as_bytes());
    hasher.update(b"\0");
    hasher.update(first_user_message.unwrap_or("").as_bytes());
    Some(format!("{:x}", hasher.finalize()))
}

/// The age of a pin in whole seconds, clamped at zero. `None` for an
/// unparseable timestamp.
pub fn age_seconds(chosen_at: &str, now: &str) -> Option<i64> {
    let chosen = chrono::DateTime::parse_from_rfc3339(chosen_at).ok()?;
    let now = chrono::DateTime::parse_from_rfc3339(now).ok()?;
    Some((now - chosen).num_seconds().max(0))
}

/// Loads a non-expired pin for `(combo, session)` at `now`. Expired rows are
/// ignored (and removed opportunistically by the prune job). Failure mode:
/// [`ComboError::Db`].
pub async fn load_pin(
    pool: &SqlitePool,
    combo_id: &str,
    session_key: &str,
    now: &str,
) -> Result<Option<StickyChoice>, ComboError> {
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT offering_id, chosen_at, expires_at FROM combo_sticky_choices \
         WHERE combo_id = ? AND session_key = ? AND expires_at > ?",
    )
    .bind(combo_id)
    .bind(session_key)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.map(|(offering_id, chosen_at, expires_at)| StickyChoice {
            offering_id,
            chosen_at,
            expires_at,
        }),
    )
}

/// Loads a pin including an expired one, so the caller can distinguish
/// "expired" from "never pinned" in `decision_json`. Failure mode:
/// [`ComboError::Db`].
pub async fn load_pin_any(
    pool: &SqlitePool,
    combo_id: &str,
    session_key: &str,
) -> Result<Option<StickyChoice>, ComboError> {
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT offering_id, chosen_at, expires_at FROM combo_sticky_choices \
         WHERE combo_id = ? AND session_key = ?",
    )
    .bind(combo_id)
    .bind(session_key)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.map(|(offering_id, chosen_at, expires_at)| StickyChoice {
            offering_id,
            chosen_at,
            expires_at,
        }),
    )
}

/// Writes or replaces the pin for `(combo, session)` with `ttl_seconds` of
/// life, then prunes this combo's expired rows and caps it at
/// [`MAX_STICKY_PER_COMBO`]. Failure mode: [`ComboError::Db`].
pub async fn upsert_pin(
    pool: &SqlitePool,
    combo_id: &str,
    session_key: &str,
    offering_id: &str,
    ttl_seconds: i64,
    now: &str,
) -> Result<StickyChoice, ComboError> {
    let ttl = ttl_seconds.clamp(MIN_STICKY_TTL_SECONDS, MAX_STICKY_TTL_SECONDS);
    let chosen = chrono::DateTime::parse_from_rfc3339(now)
        .map_err(|e| ComboError::Invalid(format!("invalid timestamp {now:?}: {e}")))?;
    let expires_at = (chosen + chrono::Duration::seconds(ttl))
        .format(crate::auth::TS_FMT)
        .to_string();
    sqlx::query(
        "INSERT INTO combo_sticky_choices \
           (combo_id, session_key, offering_id, chosen_at, expires_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (combo_id, session_key) DO UPDATE SET \
           offering_id = excluded.offering_id, chosen_at = excluded.chosen_at, \
           expires_at = excluded.expires_at",
    )
    .bind(combo_id)
    .bind(session_key)
    .bind(offering_id)
    .bind(now)
    .bind(&expires_at)
    .execute(pool)
    .await?;
    // Opportunistic upkeep: one combo's rows are bounded and expired pins are
    // useless to the next request.
    sqlx::query("DELETE FROM combo_sticky_choices WHERE combo_id = ? AND expires_at <= ?")
        .bind(combo_id)
        .bind(now)
        .execute(pool)
        .await?;
    cap_combo_pins(pool, combo_id).await?;
    Ok(StickyChoice {
        offering_id: offering_id.to_string(),
        chosen_at: now.to_string(),
        expires_at,
    })
}

/// Deletes expired pins (all combos). Returns the number removed. Failure
/// mode: [`ComboError::Db`].
pub async fn prune_expired_pins(pool: &SqlitePool, now: &str) -> Result<u64, ComboError> {
    let result = sqlx::query("DELETE FROM combo_sticky_choices WHERE expires_at <= ?")
        .bind(now)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Caps one combo's pins at [`MAX_STICKY_PER_COMBO`], keeping the newest.
/// Returns the number removed. Failure mode: [`ComboError::Db`].
pub async fn cap_combo_pins(pool: &SqlitePool, combo_id: &str) -> Result<u64, ComboError> {
    let result = sqlx::query(
        "DELETE FROM combo_sticky_choices WHERE combo_id = ? AND rowid NOT IN ( \
           SELECT rowid FROM combo_sticky_choices WHERE combo_id = ? \
           ORDER BY chosen_at DESC, rowid DESC LIMIT ?)",
    )
    .bind(combo_id)
    .bind(combo_id)
    .bind(MAX_STICKY_PER_COMBO)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// The background prune job: expired pins are removed every `interval`.
/// Failures are logged and retried on the next tick.
pub fn spawn_prune_job(
    pool: SqlitePool,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let now = catalog::now_ts();
            match prune_expired_pins(&pool, &now).await {
                Ok(removed) => {
                    tracing::debug!(removed, "combo sticky pins pruned");
                }
                Err(e) => tracing::error!(error = %e, "combo sticky prune failed"),
            }
        }
    })
}

/// Validates and creates a combo (audited as `combo.create`). Failure mode:
/// [`ComboError::Invalid`] for bad input (unknown strategy, bad weights,
/// duplicate/unknown offerings, taken name), [`ComboError::Db`] on storage
/// errors.
pub async fn create(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    input: &ComboInput,
) -> Result<Combo, ComboError> {
    let validated = validate(pool, input).await?;
    let mut tx = pool.begin().await?;
    let taken: Option<String> = sqlx::query_scalar("SELECT id FROM combo_models WHERE name = ?")
        .bind(&validated.name)
        .fetch_optional(&mut *tx)
        .await?;
    if taken.is_some() {
        return Err(ComboError::Invalid(format!(
            "combo name {:?} is already in use",
            validated.name
        )));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = catalog::now_ts();
    sqlx::query(
        "INSERT INTO combo_models \
           (id, name, strategy, weights_json, members_json, enabled, rr_cursor, \
            pool, min_quality, max_mix_price_per_m, mix_profile, \
            stickiness_enabled, stickiness_ttl_seconds, stickiness_key_source, \
            created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&validated.name)
    .bind(validated.strategy.as_str())
    .bind(&validated.weights_raw)
    .bind(members_json(&validated.members))
    .bind(i64::from(validated.enabled))
    .bind(validated.pool.as_str())
    .bind(validated.min_quality)
    .bind(validated.max_mix_price_per_m)
    .bind(&validated.mix_profile)
    .bind(i64::from(validated.stickiness_enabled))
    .bind(validated.stickiness_ttl_seconds)
    .bind(validated.stickiness_key_source.as_str())
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    catalog::audit(
        &mut tx,
        actor,
        "combo.create",
        "combo",
        &id,
        &serde_json::json!({
            "name": validated.name,
            "strategy": validated.strategy.as_str(),
            "members": validated.members.len(),
        }),
    )
    .await?;
    tx.commit().await?;

    Ok(Combo {
        id,
        name: validated.name,
        strategy: validated.strategy,
        weights: validated.weights,
        preset: validated.preset,
        mix_profile: validated.mix_profile,
        pool: validated.pool,
        min_quality: validated.min_quality,
        max_mix_price_per_m: validated.max_mix_price_per_m,
        members: validated.members,
        enabled: validated.enabled,
        rr_cursor: 0,
        stickiness_enabled: validated.stickiness_enabled,
        stickiness_ttl_seconds: validated.stickiness_ttl_seconds,
        stickiness_key_source: validated.stickiness_key_source,
    })
}

type ComboRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    i64,
    i64,
    String,
    Option<f64>,
    Option<f64>,
    Option<String>,
    i64,
    i64,
    String,
);

const SELECT_COMBO_COLUMNS: &str = "id, name, strategy, weights_json, members_json, enabled, \
     rr_cursor, pool, min_quality, max_mix_price_per_m, mix_profile, stickiness_enabled, \
     stickiness_ttl_seconds, stickiness_key_source";

/// Parses one combo row (storage shape → [`Combo`]).
fn parse_row(row: ComboRow) -> Result<Combo, ComboError> {
    let (
        id,
        name,
        strategy,
        weights_json,
        members_json,
        enabled,
        rr_cursor,
        pool,
        min_quality,
        max_mix_price_per_m,
        mix_profile,
        stickiness_enabled,
        stickiness_ttl_seconds,
        stickiness_key_source,
    ) = row;
    let strategy = Strategy::parse(&strategy)?;
    let pool = Pool::parse(&pool)?;
    let (weights, preset) = Weights::from_json(weights_json.as_deref())?;
    let members = parse_members(&members_json)?;
    Ok(Combo {
        id,
        name,
        strategy,
        weights,
        preset,
        mix_profile,
        pool,
        min_quality,
        max_mix_price_per_m,
        members,
        enabled: enabled == 1,
        rr_cursor,
        stickiness_enabled: stickiness_enabled == 1,
        stickiness_ttl_seconds,
        stickiness_key_source: KeySource::parse(&stickiness_key_source)?,
    })
}

/// Loads one combo by id. Failure mode: [`ComboError`].
pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<Combo>, ComboError> {
    let row = sqlx::query_as::<_, ComboRow>(&format!(
        "SELECT {SELECT_COMBO_COLUMNS} FROM combo_models WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(parse_row).transpose()
}

/// Lists every combo, by name. Failure mode: [`ComboError`].
pub async fn list(pool: &SqlitePool) -> Result<Vec<Combo>, ComboError> {
    let rows = sqlx::query_as::<_, ComboRow>(&format!(
        "SELECT {SELECT_COMBO_COLUMNS} FROM combo_models ORDER BY name"
    ))
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(parse_row).collect()
}

/// Replaces a combo's strategy, weights, members, and enabled flag (its name
/// and rotation cursor are immutable). Audited as `combo.update`. `None` when
/// the id is unknown. Failure mode: [`ComboError`].
pub async fn update(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    id: &str,
    input: &ComboInput,
) -> Result<Option<Combo>, ComboError> {
    let Some(current) = get(pool, id).await? else {
        return Ok(None);
    };
    // The name is immutable: validate under the stored name.
    let merged = ComboInput {
        name: current.name.clone(),
        ..input.clone()
    };
    let validated = validate(pool, &merged).await?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE combo_models SET strategy = ?, weights_json = ?, members_json = ?, \
         enabled = ?, pool = ?, min_quality = ?, max_mix_price_per_m = ?, mix_profile = ?, \
         stickiness_enabled = ?, stickiness_ttl_seconds = ?, stickiness_key_source = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(validated.strategy.as_str())
    .bind(&validated.weights_raw)
    .bind(members_json(&validated.members))
    .bind(i64::from(validated.enabled))
    .bind(validated.pool.as_str())
    .bind(validated.min_quality)
    .bind(validated.max_mix_price_per_m)
    .bind(&validated.mix_profile)
    .bind(i64::from(validated.stickiness_enabled))
    .bind(validated.stickiness_ttl_seconds)
    .bind(validated.stickiness_key_source.as_str())
    .bind(catalog::now_ts())
    .bind(id)
    .execute(&mut *tx)
    .await?;
    catalog::audit(
        &mut tx,
        actor,
        "combo.update",
        "combo",
        id,
        &serde_json::json!({
            "strategy": validated.strategy.as_str(),
            "members": validated.members.len(),
            "enabled": validated.enabled,
        }),
    )
    .await?;
    tx.commit().await?;

    Ok(Some(Combo {
        id: current.id,
        name: current.name,
        strategy: validated.strategy,
        weights: validated.weights,
        preset: validated.preset,
        mix_profile: validated.mix_profile,
        pool: validated.pool,
        min_quality: validated.min_quality,
        max_mix_price_per_m: validated.max_mix_price_per_m,
        members: validated.members,
        enabled: validated.enabled,
        rr_cursor: current.rr_cursor,
        stickiness_enabled: validated.stickiness_enabled,
        stickiness_ttl_seconds: validated.stickiness_ttl_seconds,
        stickiness_key_source: validated.stickiness_key_source,
    }))
}

/// Deletes a combo (audited as `combo.delete`). Returns whether it existed.
/// Failure mode: [`ComboError`].
pub async fn delete(pool: &SqlitePool, actor: &Actor<'_>, id: &str) -> Result<bool, ComboError> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query("DELETE FROM combo_models WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() > 0 {
        catalog::audit(
            &mut tx,
            actor,
            "combo.delete",
            "combo",
            id,
            &serde_json::json!({}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

/// Loads a combo by its `combo:<name>` name (without the prefix), whether
/// enabled or not (the caller turns a disabled combo into `404
/// model_not_found`). Failure mode: [`ComboError`] on storage errors or a
/// corrupt row.
pub async fn load_by_name(pool: &SqlitePool, name: &str) -> Result<Option<Combo>, ComboError> {
    let row = sqlx::query_as::<_, ComboRow>(&format!(
        "SELECT {SELECT_COMBO_COLUMNS} FROM combo_models WHERE name = ?"
    ))
    .bind(name)
    .fetch_optional(pool)
    .await?;
    row.map(parse_row).transpose()
}

/// Advances a combo's round-robin cursor and returns its **new** value (the
/// first call returns 1). Called once per request that actually dispatches
/// upstream: refusals before dispatch (budget, context, breaker) do not
/// consume the rotation. Failure mode: [`ComboError::Db`].
pub async fn advance_cursor(pool: &SqlitePool, combo_id: &str) -> Result<i64, ComboError> {
    let cursor: i64 = sqlx::query_scalar(
        "UPDATE combo_models SET rr_cursor = rr_cursor + 1, updated_at = ? \
         WHERE id = ? RETURNING rr_cursor",
    )
    .bind(catalog::now_ts())
    .bind(combo_id)
    .fetch_one(pool)
    .await?;
    Ok(cursor)
}

/// One advertised combo for `GET /v1/models` (review m4 — combos must be
/// discoverable, not a hidden namespace): the `combo:<name>` entry, its pool
/// kind, and its members' `(model_id, offering_id)` pairs for access filtering.
#[derive(Debug)]
pub struct ComboV1Entry {
    pub model: catalog::V1Model,
    /// Where the candidates come from (D35).
    pub pool: Pool,
    /// `(model_id, offering_id)` of every member that still exists. Empty for
    /// [`Pool::All`], whose candidates are the whole catalog.
    pub members: Vec<(String, String)>,
}

/// One member offering as joined for the `GET /v1/models` combo entry: the
/// row ids access control needs plus the effective context and enabled flags
/// that decide whether it may lower the advertised combo context.
#[derive(Debug, sqlx::FromRow)]
struct ComboMemberRow {
    offering_id: String,
    model_id: String,
    context_override: Option<i64>,
    context_length: Option<i64>,
    offering_enabled: i64,
    provider_enabled: i64,
    model_enabled: i64,
}

/// The enabled combos as advertised by `GET /v1/models`: one `combo:<name>`
/// entry per enabled combo. `context_length` is the minimum effective context
/// (the offering override wins over the model value, like [`catalog::V1Model`])
/// over the combo's **eligible** members — an enabled offering, provider, and
/// model — so a client sizes its prompt for the smallest window it may be
/// routed to; members with an unknown context are ignored, and when none is
/// known the field stays null. `max_output_tokens` and price are null — they
/// vary per member and the chat pipeline checks them per candidate. A
/// [`Pool::All`] combo has no explicit members, so its context stays null; it
/// is advertised with an empty `members` list and the route decides its
/// visibility against the caller's catalog-wide access. Failure mode:
/// [`ComboError`].
pub async fn list_v1_entries(pool: &SqlitePool) -> Result<Vec<ComboV1Entry>, ComboError> {
    let mut entries = Vec::new();
    for combo in list(pool).await? {
        if !combo.enabled {
            continue;
        }
        let mut members = Vec::with_capacity(combo.members.len());
        let mut min_context: Option<i64> = None;
        for member in &combo.members {
            let row: Option<ComboMemberRow> = sqlx::query_as(
                "SELECT o.id AS offering_id, o.model_id, o.context_override, m.context_length, \
                        o.enabled AS offering_enabled, pr.enabled AS provider_enabled, \
                        m.enabled AS model_enabled \
                 FROM offerings o \
                 JOIN models m ON m.id = o.model_id \
                 JOIN providers pr ON pr.id = o.provider_id \
                 WHERE o.id = ?",
            )
            .bind(&member.offering_id)
            .fetch_optional(pool)
            .await?;
            let Some(row) = row else {
                continue;
            };
            members.push((row.model_id.clone(), row.offering_id.clone()));
            let eligible =
                row.offering_enabled == 1 && row.provider_enabled == 1 && row.model_enabled == 1;
            if eligible && let Some(context) = row.context_override.or(row.context_length) {
                min_context = Some(min_context.map_or(context, |current| current.min(context)));
            }
        }
        entries.push(ComboV1Entry {
            pool: combo.pool,
            model: catalog::V1Model {
                id: format!("{NAMESPACE}{}", combo.name),
                name: combo.name.clone(),
                context_length: min_context,
                max_output_tokens: None,
                modalities: catalog::V1Modalities {
                    input: Vec::new(),
                    output: Vec::new(),
                },
                price: None,
            },
            members,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::catalog::{CredentialInput, ModelInput, OfferingInput, ProviderInput};
    use crate::test_support::TestApp;

    struct Seeded {
        app: TestApp,
        uid: String,
        offering_ids: Vec<String>,
    }

    async fn seeded() -> Seeded {
        let app = TestApp::new().await;
        let uid = app
            .create_user("admin@example.test", Role::Admin, true)
            .await;
        let actor = Actor {
            user_id: &uid,
            ip: "127.0.0.1",
        };
        let provider = catalog::create_provider(
            &app.state.pool,
            &actor,
            &ProviderInput {
                name: "p1".to_string(),
                base_url: "http://127.0.0.1:1".to_string(),
                kind: None,
                enabled: None,
                preset: None,
                connectors: None,
            },
        )
        .await
        .expect("provider");
        catalog::create_credential(
            &app.state.pool,
            &app.state.secrets,
            &actor,
            &provider.id,
            &CredentialInput {
                label: "k".to_string(),
                secret: "sk-test".to_string(),
                headers: None,
                max_concurrency: None,
                balance_usd: None,
                balance_source: None,
                balance_low_threshold: None,
                balance_gate: None,
            },
        )
        .await
        .expect("credential");
        let model = catalog::create_model(
            &app.state.pool,
            &actor,
            &ModelInput {
                canonical_name: "m".to_string(),
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
        let mut offering_ids = Vec::new();
        for i in 0..2 {
            let offering = catalog::create_offering(
                &app.state.pool,
                &app.state.secrets,
                &actor,
                &OfferingInput {
                    provider_id: provider.id.clone(),
                    model_id: model.id.clone(),
                    upstream_model_id: format!("up-{i}"),
                    context_override: None,
                    max_output_override: None,
                    enabled: None,
                    headers: None,
                },
            )
            .await
            .expect("offering");
            offering_ids.push(offering.id);
        }
        Seeded {
            app,
            uid,
            offering_ids,
        }
    }

    fn input<S: AsRef<str>>(name: &str, strategy: &str, members: &[S]) -> ComboInput {
        ComboInput {
            name: name.to_string(),
            strategy: strategy.to_string(),
            weights: None,
            members: members
                .iter()
                .map(|offering_id| MemberInput {
                    offering_id: offering_id.as_ref().to_string(),
                    weight: None,
                })
                .collect(),
            enabled: None,
            pool: None,
            min_quality: None,
            max_mix_price_per_m: None,
            mix_profile: None,
            stickiness: None,
        }
    }

    #[tokio::test]
    async fn a_combo_round_trips_with_default_weights_and_members() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let created = create(
            &s.app.state.pool,
            &actor,
            &input("test", "ordered", &s.offering_ids),
        )
        .await
        .expect("create");
        assert_eq!(created.strategy, Strategy::Ordered);
        assert_eq!(created.preset, Some("balanced"));
        assert_eq!(created.weights, Weights::default());

        let loaded = load_by_name(&s.app.state.pool, "test")
            .await
            .expect("load")
            .expect("combo exists");
        assert_eq!(loaded, created);
        assert_eq!(
            loaded.members,
            s.offering_ids
                .iter()
                .map(|id| ComboMember {
                    offering_id: id.clone(),
                    weight: 1.0,
                })
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn weights_accept_presets_and_partial_overrides() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let mut preset_input = input("cheap", "weighted", &s.offering_ids[..1]);
        preset_input.weights = Some(serde_json::json!({"preset": "cheapest"}));
        let cheap = create(&s.app.state.pool, &actor, &preset_input)
            .await
            .expect("create");
        assert_eq!(cheap.preset, Some("cheapest"));
        assert_eq!(cheap.weights.price, 1.0);

        let mut partial_input = input("custom", "weighted", &s.offering_ids[..1]);
        partial_input.weights = Some(serde_json::json!({"price": 0.9}));
        partial_input.members[0].weight = Some(2.5);
        let custom = create(&s.app.state.pool, &actor, &partial_input)
            .await
            .expect("create");
        assert_eq!(custom.preset, None);
        assert_eq!(custom.weights.price, 0.9);
        assert_eq!(custom.weights.speed, 0.30, "missing keys keep defaults");
        assert_eq!(custom.members[0].weight, 2.5);
    }

    #[tokio::test]
    async fn mix_profile_is_validated_and_canonicalised_in_weights_json() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let pool = &s.app.state.pool;

        // A name resolves and is stored as the canonical profile id.
        let mut by_name = input("mix-by-name", "weighted", &s.offering_ids[..1]);
        by_name.weights = Some(serde_json::json!({"preset": "cheapest", "mix_profile": "chat"}));
        let created = create(pool, &actor, &by_name).await.expect("create");
        assert_eq!(created.mix_profile.as_deref(), Some("mix-chat"));
        assert_eq!(created.preset, Some("cheapest"));
        let loaded = load_by_name(pool, "mix-by-name")
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(loaded.mix_profile.as_deref(), Some("mix-chat"));

        // An id resolves too.
        let mut by_id = input("mix-by-id", "ordered", &s.offering_ids[..1]);
        by_id.weights = Some(serde_json::json!({"mix_profile": "mix-long-context"}));
        let created = create(pool, &actor, &by_id).await.expect("create");
        assert_eq!(created.mix_profile.as_deref(), Some("mix-long-context"));
        assert_eq!(created.preset, None, "an explicit object names no preset");

        // A null/absent selector means "use the global default".
        let plain = create(
            pool,
            &actor,
            &input("mix-none", "ordered", &s.offering_ids[..1]),
        )
        .await
        .expect("create");
        assert!(plain.mix_profile.is_none());

        // Unknown and non-string selectors are 400-worthy.
        let mut unknown = input("mix-unknown", "ordered", &s.offering_ids[..1]);
        unknown.weights = Some(serde_json::json!({"mix_profile": "nope"}));
        let err = create(pool, &actor, &unknown)
            .await
            .expect_err("unknown profile");
        assert!(err.to_string().contains("does not exist"), "{err}");

        let mut bad_type = input("mix-type", "ordered", &s.offering_ids[..1]);
        bad_type.weights = Some(serde_json::json!({"mix_profile": 7}));
        let err = create(pool, &actor, &bad_type)
            .await
            .expect_err("non-string selector");
        assert!(
            err.to_string().contains("must be a non-empty string"),
            "{err}"
        );

        // Update validates it too.
        let mut update_input = input("mix-by-name", "ordered", &s.offering_ids[..1]);
        update_input.weights = Some(serde_json::json!({"mix_profile": "nope"}));
        let err = update(pool, &actor, &loaded.id, &update_input)
            .await
            .expect_err("unknown on update");
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[tokio::test]
    async fn invalid_combos_are_rejected_by_reason() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let pool = &s.app.state.pool;

        let err = create(pool, &actor, &input("x", "zoo", &s.offering_ids[..1]))
            .await
            .expect_err("strategy");
        assert!(err.to_string().contains("round_robin"), "{err}");

        let err = create(pool, &actor, &input("x", "ordered", &[] as &[&str]))
            .await
            .expect_err("members");
        assert!(err.to_string().contains("members"), "{err}");

        let err = create(
            pool,
            &actor,
            &input("x", "ordered", &[&s.offering_ids[0], &s.offering_ids[0]]),
        )
        .await
        .expect_err("duplicates");
        assert!(err.to_string().contains("twice"), "{err}");

        let err = create(pool, &actor, &input("x", "ordered", &["missing-offering"]))
            .await
            .expect_err("offering");
        assert!(err.to_string().contains("does not exist"), "{err}");

        let mut bad_weight = input("x", "weighted", &s.offering_ids[..1]);
        bad_weight.members[0].weight = Some(-1.0);
        let err = create(pool, &actor, &bad_weight).await.expect_err("weight");
        assert!(err.to_string().contains("weight"), "{err}");

        let mut bad_name = input("bad:name", "ordered", &s.offering_ids[..1]);
        bad_name.name = "bad:name".to_string();
        let err = create(pool, &actor, &bad_name).await.expect_err("name");
        assert!(err.to_string().contains("namespace separator"), "{err}");

        create(
            pool,
            &actor,
            &input("taken", "ordered", &s.offering_ids[..1]),
        )
        .await
        .expect("first");
        let err = create(
            pool,
            &actor,
            &input("taken", "ordered", &s.offering_ids[..1]),
        )
        .await
        .expect_err("name taken");
        assert!(err.to_string().contains("already in use"), "{err}");
    }

    #[tokio::test]
    async fn the_round_robin_cursor_persists_across_requests() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let combo = create(
            &s.app.state.pool,
            &actor,
            &input("rr", "round_robin", &s.offering_ids),
        )
        .await
        .expect("create");
        assert_eq!(combo.rr_cursor, 0);

        assert_eq!(
            advance_cursor(&s.app.state.pool, &combo.id)
                .await
                .expect("1"),
            1
        );
        assert_eq!(
            advance_cursor(&s.app.state.pool, &combo.id)
                .await
                .expect("2"),
            2
        );
        let reloaded = load_by_name(&s.app.state.pool, "rr")
            .await
            .expect("load")
            .expect("combo");
        assert_eq!(reloaded.rr_cursor, 2, "persisted in the row");
    }

    #[tokio::test]
    async fn unknown_names_load_none() {
        let s = seeded().await;
        assert!(
            load_by_name(&s.app.state.pool, "nope")
                .await
                .expect("load")
                .is_none(),
            "unknown combos are not found"
        );
    }

    // -----------------------------------------------------------------------
    // D35: catalog-pool constraints, preview engine, stickiness
    // -----------------------------------------------------------------------

    fn mix_at(per_m: f64) -> crate::mix::MixPrice {
        let profile = crate::mix::builtin_profile(crate::mix::CHAT).expect("built-in chat profile");
        crate::mix::mix_price_from(
            crate::mix::ComponentPrices {
                input_per_m: Some(per_m),
                output_per_m: Some(per_m),
                ..crate::mix::ComponentPrices::default()
            },
            &profile,
        )
        .expect("computable mix")
    }

    fn pool_item(id: &str, quality: f64, per_m: Option<f64>) -> PoolItem {
        let mut signal = routing::CandidateSignal::neutral(id);
        signal.quality = Some(quality);
        signal.price_per_m_usd = per_m;
        PoolItem {
            offering_id: id.to_string(),
            model: id.to_string(),
            provider: "p".to_string(),
            quality: routing::QualityScore {
                score: quality,
                source: routing::QualitySource::Override,
                imputed: false,
            },
            mix_price: per_m.map(mix_at),
            signal,
        }
    }

    fn pool_ids(outcome: &PoolOutcome) -> Vec<&str> {
        outcome
            .ordered
            .iter()
            .map(|scored| scored.item.offering_id.as_str())
            .collect()
    }

    #[test]
    fn pool_constraints_are_inclusive_at_both_boundaries() {
        let items = vec![
            pool_item("at", 0.5, Some(1.0)),
            pool_item("below", 0.499, Some(1.0001)),
        ];
        let outcome = resolve_pool(
            items,
            &PoolConstraints {
                min_quality: Some(0.5),
                max_mix_price_per_m: Some(1.0),
            },
            &Weights::default(),
        );
        assert_eq!(pool_ids(&outcome), ["at"], ">= quality and <= price");
        assert_eq!(outcome.excluded.len(), 1);
        assert_eq!(outcome.excluded[0].reason, PoolReason::BelowMinQuality);
    }

    #[test]
    fn an_unknown_mix_price_is_excluded_only_when_a_price_cap_is_set() {
        let capped = resolve_pool(
            vec![
                pool_item("priced", 0.6, Some(1.0)),
                pool_item("unpriced", 0.6, None),
            ],
            &PoolConstraints {
                min_quality: None,
                max_mix_price_per_m: Some(2.0),
            },
            &Weights::default(),
        );
        assert_eq!(pool_ids(&capped), ["priced"]);
        assert_eq!(capped.excluded[0].reason, PoolReason::Unpriced);

        let uncapped = resolve_pool(
            vec![
                pool_item("priced", 0.6, Some(1.0)),
                pool_item("unpriced", 0.6, None),
            ],
            &PoolConstraints {
                min_quality: Some(0.5),
                max_mix_price_per_m: None,
            },
            &Weights::default(),
        );
        assert_eq!(uncapped.ordered.len(), 2, "no cap, unpriced is allowed");
    }

    #[test]
    fn the_default_quality_of_half_passes_a_half_minimum() {
        let mut item = pool_item("default", 0.5, Some(1.0));
        item.quality = routing::QualityScore {
            score: 0.5,
            source: routing::QualitySource::ModelDefault,
            imputed: true,
        };
        item.signal.quality = None;
        let outcome = resolve_pool(
            vec![item],
            &PoolConstraints {
                min_quality: Some(0.5),
                max_mix_price_per_m: None,
            },
            &Weights::default(),
        );
        assert_eq!(pool_ids(&outcome), ["default"]);
        assert_eq!(outcome.ordered[0].score.quality, 0.5);
        assert!(outcome.ordered[0].score.imputed.contains(&"quality"));
    }

    #[test]
    fn a_weight_change_reorders_the_catalog_pool() {
        let items = || {
            vec![
                pool_item("cheap", 0.4, Some(1.0)),
                pool_item("great", 0.9, Some(4.0)),
            ]
        };
        let (_, cheapest) = Weights::preset("cheapest").expect("preset");
        let (_, best) = Weights::preset("best").expect("preset");
        let constraints = PoolConstraints::default();
        assert_eq!(
            pool_ids(&resolve_pool(items(), &constraints, &cheapest)),
            ["cheap", "great"]
        );
        assert_eq!(
            pool_ids(&resolve_pool(items(), &constraints, &best)),
            ["great", "cheap"]
        );
    }

    #[test]
    fn pool_resolution_is_deterministic_across_input_permutations() {
        let constraints = PoolConstraints {
            min_quality: Some(0.3),
            max_mix_price_per_m: Some(5.0),
        };
        let build = |reverse: bool| {
            let mut items = vec![
                pool_item("a", 0.5, Some(1.0)),
                pool_item("b", 0.9, Some(4.0)),
                pool_item("c", 0.7, Some(2.0)),
            ];
            if reverse {
                items.reverse();
            }
            items
        };
        let first = resolve_pool(build(false), &constraints, &Weights::default());
        let second = resolve_pool(build(true), &constraints, &Weights::default());
        assert_eq!(pool_ids(&first), pool_ids(&second));
    }

    #[test]
    fn pool_exclusions_carry_their_one_line_reason() {
        let mut disabled = pool_item("disabled", 0.9, Some(1.0));
        disabled.signal.enabled = false;
        let mut denied = pool_item("denied", 0.9, Some(1.0));
        denied.signal.granted = false;
        let mut cooled = pool_item("cooled", 0.9, Some(1.0));
        cooled.signal.credential_cooled = true;
        let mut breaker = pool_item("breaker", 0.9, Some(1.0));
        breaker.signal.breaker_open = true;
        let outcome = resolve_pool(
            vec![disabled, denied, cooled, breaker],
            &PoolConstraints::default(),
            &Weights::default(),
        );
        let reasons: Vec<&str> = outcome
            .excluded
            .iter()
            .map(|excluded| excluded.reason.as_str())
            .collect();
        assert_eq!(
            reasons,
            ["disabled", "no_access", "cooldown", "breaker_open"]
        );
        for excluded in &outcome.excluded {
            assert!(!excluded.detail.is_empty());
        }
    }

    #[tokio::test]
    async fn catalog_pool_validation_names_the_offending_rule() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let pool = &s.app.state.pool;

        let mut unconstrained = input("all-empty", "weighted", &[] as &[&str]);
        unconstrained.pool = Some("all".to_string());
        let err = create(pool, &actor, &unconstrained)
            .await
            .expect_err("no constraint");
        assert!(err.to_string().contains("at least one constraint"), "{err}");

        let mut ordered = input("all-ordered", "ordered", &[] as &[&str]);
        ordered.pool = Some("all".to_string());
        ordered.min_quality = Some(0.5);
        let err = create(pool, &actor, &ordered)
            .await
            .expect_err("ordered pool");
        assert!(err.to_string().contains("weighted strategy"), "{err}");

        let mut bad_ttl = input("bad-ttl", "weighted", &s.offering_ids[..1]);
        bad_ttl.stickiness = Some(StickinessInput {
            enabled: Some(true),
            ttl_seconds: Some(30),
            key_source: None,
        });
        let err = create(pool, &actor, &bad_ttl).await.expect_err("bad ttl");
        assert!(err.to_string().contains("ttl_seconds"), "{err}");

        let mut bad_profile = input("bad-profile", "weighted", &s.offering_ids[..1]);
        bad_profile.mix_profile = Some("nope".to_string());
        let err = create(pool, &actor, &bad_profile)
            .await
            .expect_err("bad profile");
        assert!(err.to_string().contains("does not exist"), "{err}");

        let mut good = input("all-good", "weighted", &[] as &[&str]);
        good.pool = Some("all".to_string());
        good.max_mix_price_per_m = Some(2.5);
        good.stickiness = Some(StickinessInput {
            enabled: Some(true),
            ttl_seconds: Some(600),
            key_source: Some("header".to_string()),
        });
        let created = create(pool, &actor, &good).await.expect("valid pool");
        assert_eq!(created.pool, Pool::All);
        assert_eq!(created.max_mix_price_per_m, Some(2.5));
        assert!(created.stickiness_enabled);
        assert_eq!(created.stickiness_ttl_seconds, 600);
        assert_eq!(created.stickiness_key_source, KeySource::Header);
    }

    #[test]
    fn session_keys_use_the_header_then_the_documented_hash() {
        let header = session_key(
            KeySource::Auto,
            Some(" sess-1 "),
            "u1",
            Some("s"),
            Some("hi"),
        );
        assert_eq!(header.as_deref(), Some("sess-1"));

        let hashed = session_key(KeySource::Auto, None, "u1", Some("system"), Some("hello"))
            .expect("auto key");
        assert_eq!(hashed.len(), 64);
        assert_eq!(
            session_key(
                KeySource::Auto,
                Some("   "),
                "u1",
                Some("system"),
                Some("hello")
            ),
            Some(hashed.clone()),
            "an empty header falls back to the hash"
        );
        assert_ne!(
            session_key(KeySource::Auto, None, "u1", Some("system"), Some("bye")),
            Some(hashed.clone()),
            "a different opening is a different session"
        );
        assert_ne!(
            session_key(KeySource::Auto, None, "u2", Some("system"), Some("hello")),
            Some(hashed),
            "a different caller is a different session"
        );
        assert_eq!(
            session_key(KeySource::Header, None, "u1", Some("s"), Some("h")),
            None,
            "header mode requires X-Session-Id"
        );
    }

    #[tokio::test]
    async fn pins_round_trip_expire_prune_and_cap() {
        let s = seeded().await;
        let actor = Actor {
            user_id: &s.uid,
            ip: "127.0.0.1",
        };
        let pool = &s.app.state.pool;
        let combo = create(
            pool,
            &actor,
            &input("sticky", "ordered", &s.offering_ids[..1]),
        )
        .await
        .expect("combo");
        let now = "2026-11-16T12:00:00.000Z";

        let pin = upsert_pin(pool, &combo.id, "sess", &s.offering_ids[0], 1800, now)
            .await
            .expect("pin");
        assert_eq!(pin.offering_id, s.offering_ids[0]);
        assert_eq!(
            load_pin(pool, &combo.id, "sess", now)
                .await
                .expect("load")
                .expect("pin")
                .offering_id,
            s.offering_ids[0]
        );
        assert!(
            load_pin(pool, &combo.id, "other", now)
                .await
                .expect("load")
                .is_none(),
            "different sessions do not share a pin"
        );
        assert!(
            load_pin(pool, &combo.id, "sess", "2026-11-16T12:31:00.000Z")
                .await
                .expect("expired")
                .is_none(),
            "an expired pin is not used"
        );

        // Pruning removes expired rows across combos.
        sqlx::query(
            "INSERT INTO combo_sticky_choices \
               (combo_id, session_key, offering_id, chosen_at, expires_at) \
             VALUES (?, 'old', ?, '2020-01-01T00:00:00.000Z', '2020-01-01T00:01:00.000Z')",
        )
        .bind(&combo.id)
        .bind(&s.offering_ids[0])
        .execute(pool)
        .await
        .expect("old pin");
        let pruned = prune_expired_pins(pool, now).await.expect("prune");
        assert!(pruned >= 1);

        // The cap keeps the newest MAX_STICKY_PER_COMBO rows.
        sqlx::query(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < ?) \
             INSERT INTO combo_sticky_choices \
               (combo_id, session_key, offering_id, chosen_at, expires_at) \
             SELECT ?, 'bulk-' || n, ?, ?, '2099-01-01T00:00:00.000Z' FROM seq",
        )
        .bind(MAX_STICKY_PER_COMBO + 5)
        .bind(&combo.id)
        .bind(&s.offering_ids[0])
        .bind(now)
        .execute(pool)
        .await
        .expect("bulk pins");
        let removed = cap_combo_pins(pool, &combo.id).await.expect("cap");
        assert_eq!(removed, 6, "the pre-existing pin plus the bulk rows");
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM combo_sticky_choices WHERE combo_id = ?")
                .bind(&combo.id)
                .fetch_one(pool)
                .await
                .expect("count");
        assert_eq!(count, MAX_STICKY_PER_COMBO);
    }
}
