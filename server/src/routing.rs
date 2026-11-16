//! Pure routing core (M5): filters, normalization, weighted scoring,
//! penalties, the deterministic tie-break, quality resolution, and one-line
//! explanations. No I/O and no time — everything here is a pure function of
//! its inputs, so the same pool and weights always produce the same order.
//!
//! The scoring pipeline (docs/research/04-architecture.md §6):
//!
//! 1. **Filters** first: enabled, credential present, access granted,
//!    capability fit, context fit, budget fit, and the circuit breaker. An
//!    excluded candidate is returned with its reason, never silently dropped.
//! 2. **Normalization**: each component is min-max normalized to `[0, 1]`
//!    across the eligible pool (direction-aware: price and failure rate are
//!    lower-is-better). A pool with no spread scores 0.5 — it cannot
//!    discriminate. A **missing** component is imputed at the flat neutral
//!    0.5 (there is no pool median — review 2026-11-16) and marked in the
//!    explanation. Telemetry-derived components (speed, reliability) backed
//!    by fewer than 10 window samples are **shrunk** toward 0.5 by
//!    `n/(n+10)` (decision D27) and marked in the explanation like imputed
//!    ones; 10+ samples are fully trusted.
//! 3. **Weighted sum**: `w_price * price + w_speed * speed + w_quality *
//!    quality + w_rel * reliability` minus the penalties, scaled by the
//!    per-member weight — `total = member_weight * (weighted sum −
//!    penalties)`. `total` is **not** clamped to `[0, 1]`: member weights
//!    above 1 push it over and penalties take it below zero (the four
//!    components are the `[0, 1]` quantities).
//! 4. **Penalties** subtract: recent failures (telemetry failure rate and a
//!    cooled credential), a recently-tripped breaker pair, low quota,
//!    near-context pressure, latency jitter. The architecture lists the
//!    failure rate both as the reliability input and as a penalty; the
//!    penalty constant is deliberately small so it only breaks near-ties
//!    toward recently-clean offerings. **Penalties apply only under
//!    multi-component weights** — see the preset contract below.
//! 5. **Deterministic tie-break**: lower price, then lower p95 latency, then
//!    stable id. Never random, so results are reproducible and explainable.
//!
//! Preset contract (decision D24): `cheapest` = price order, `fastest` =
//! speed order, `best` = quality order — each a **pure order with no
//! penalties**. A cheap-but-unhealthy candidate still wins `cheapest`,
//! because health is the filters' and the breaker's job, not a hidden
//! re-ordering. Penalties exist only under multi-component weights:
//! `balanced` = the default (price 0.25, speed 0.30, quality 0.30,
//! reliability 0.15) and custom `weighted` vectors. A custom
//! single-component vector is a pure order too (see
//! [`Weights::is_pure_order`]).
//!
//! Quality resolution (decision D4/D5/D25): a per-model override wins, then
//! the latest imported benchmark (wide CIs shrunk toward the leaderboard
//! mean), then the neutral model default (0.5, imputed). Benchmarks and
//! overrides share one Elo-like scale.

use serde_json::Value;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Invalid routing configuration (weights, presets). Failure mode: surfaces
/// as a combo-configuration error; the pure math itself cannot fail.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RoutingError {
    #[error("unknown weight preset {0:?}; expected one of cheapest, fastest, balanced, best")]
    UnknownPreset(String),
    #[error("weights must be finite and non-negative, and must not all be zero")]
    InvalidWeights,
    #[error("weights_json must be null, an object, or {{\"preset\": \"...\"}}: {0}")]
    MalformedWeights(String),
    #[error("unknown routing strategy {0:?}; expected one of ordered, weighted, round_robin")]
    UnknownStrategy(String),
}

// ---------------------------------------------------------------------------
// Weights and presets
// ---------------------------------------------------------------------------

/// Scoring weights over the four components. Non-negative and not all zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    pub price: f64,
    pub speed: f64,
    pub quality: f64,
    pub reliability: f64,
}

impl Default for Weights {
    /// The documented defaults: price 0.25, speed 0.30, quality 0.30,
    /// reliability 0.15.
    fn default() -> Self {
        Self {
            price: 0.25,
            speed: 0.30,
            quality: 0.30,
            reliability: 0.15,
        }
    }
}

impl Weights {
    /// The named preset `name`, with its documented name. `None` for unknown
    /// names.
    pub fn preset(name: &str) -> Option<(&'static str, Self)> {
        match name {
            "cheapest" => Some((
                "cheapest",
                Self {
                    price: 1.0,
                    speed: 0.0,
                    quality: 0.0,
                    reliability: 0.0,
                },
            )),
            "fastest" => Some((
                "fastest",
                Self {
                    price: 0.0,
                    speed: 1.0,
                    quality: 0.0,
                    reliability: 0.0,
                },
            )),
            "best" => Some((
                "best",
                Self {
                    price: 0.0,
                    speed: 0.0,
                    quality: 1.0,
                    reliability: 0.0,
                },
            )),
            "balanced" => Some(("balanced", Self::default())),
            _ => None,
        }
    }

    /// Rejects non-finite, negative, or all-zero weights.
    pub fn validate(self) -> Result<(), RoutingError> {
        let parts = [self.price, self.speed, self.quality, self.reliability];
        if parts.iter().any(|w| !w.is_finite() || *w < 0.0) {
            return Err(RoutingError::InvalidWeights);
        }
        if parts.iter().sum::<f64>() <= 0.0 {
            return Err(RoutingError::InvalidWeights);
        }
        Ok(())
    }

    /// Whether these weights form a pure single-component order (decision
    /// D24): exactly one positive component, so that component alone decides.
    /// Pure orders apply **no penalties** — health is enforced by the filters
    /// and the breaker, not by a hidden re-ordering. The named single-component
    /// presets (`cheapest`, `fastest`, `best`) are pure orders.
    pub fn is_pure_order(self) -> bool {
        [self.price, self.speed, self.quality, self.reliability]
            .iter()
            .filter(|weight| **weight > 0.0)
            .count()
            == 1
    }

    /// Parses `combo_models.weights_json`: NULL/absent is the `balanced`
    /// default; `{"preset": "cheapest"}` names a preset; an object of any
    /// subset of `price`/`speed`/`quality`/`reliability` overrides those keys
    /// over the defaults. Failure mode: [`RoutingError`].
    pub fn from_json(raw: Option<&str>) -> Result<(Self, Option<&'static str>), RoutingError> {
        let Some(raw) = raw
            .map(str::trim)
            .filter(|raw| !raw.is_empty() && *raw != "null")
        else {
            return Ok((Self::default(), Some("balanced")));
        };
        let value: Value = serde_json::from_str(raw)
            .map_err(|_| RoutingError::MalformedWeights(raw.to_string()))?;
        let object = value
            .as_object()
            .ok_or_else(|| RoutingError::MalformedWeights(raw.to_string()))?;

        if let Some(preset) = object.get("preset") {
            let name = preset
                .as_str()
                .ok_or_else(|| RoutingError::MalformedWeights(raw.to_string()))?;
            let (preset_name, weights) =
                Self::preset(name).ok_or_else(|| RoutingError::UnknownPreset(name.to_string()))?;
            return Ok((weights, Some(preset_name)));
        }

        let mut weights = Self::default();
        for (key, target) in [
            ("price", &mut weights.price),
            ("speed", &mut weights.speed),
            ("quality", &mut weights.quality),
            ("reliability", &mut weights.reliability),
        ] {
            if let Some(value) = object.get(key) {
                *target = value
                    .as_f64()
                    .ok_or_else(|| RoutingError::MalformedWeights(raw.to_string()))?;
            }
        }
        weights.validate()?;
        Ok((weights, None))
    }

    /// The four weights as a JSON object (for `decision_json`).
    pub fn to_json(self) -> Value {
        serde_json::json!({
            "price": self.price,
            "speed": self.speed,
            "quality": self.quality,
            "reliability": self.reliability,
        })
    }
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// The three combo strategies (decision D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Members in their stored order (fallback list).
    Ordered,
    /// Weighted scoring over the member pool.
    Weighted,
    /// Stable rotation over the members, persisted per combo.
    RoundRobin,
}

impl Strategy {
    /// Parses the stored strategy name. Failure mode: `None`.
    pub fn parse(value: &str) -> Result<Self, RoutingError> {
        match value {
            "ordered" => Ok(Self::Ordered),
            "weighted" => Ok(Self::Weighted),
            "round_robin" => Ok(Self::RoundRobin),
            other => Err(RoutingError::UnknownStrategy(other.to_string())),
        }
    }

    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ordered => "ordered",
            Self::Weighted => "weighted",
            Self::RoundRobin => "round_robin",
        }
    }
}

/// Rotates `items` so index `start` comes first, wrapping around. `start` is
/// taken modulo the length; an empty list stays empty. Pure: the same inputs
/// always produce the same order.
pub fn rotate<T>(items: Vec<T>, start: usize) -> Vec<T> {
    let len = items.len();
    if len == 0 {
        return items;
    }
    let start = start % len;
    let mut iter = items.into_iter();
    let head: Vec<T> = iter.by_ref().take(start).collect();
    let mut rotated: Vec<T> = iter.collect();
    rotated.extend(head);
    rotated
}

// ---------------------------------------------------------------------------
// Candidates in, decisions out
// ---------------------------------------------------------------------------

/// One candidate's routing inputs. `None` metrics are imputed at the neutral
/// 0.5 and marked; the filter flags decide eligibility before any scoring.
#[derive(Debug, Clone)]
pub struct CandidateSignal {
    pub id: String,
    /// Blended list price in USD per million tokens (input + output); lower
    /// is better. Unknown is `None`, never 0 (decision D10).
    pub price_per_m_usd: Option<f64>,
    /// Measured throughput in output tokens/second; higher is better.
    pub tokens_per_sec: Option<f64>,
    /// Model-level quality score; higher is better.
    pub quality: Option<f64>,
    /// Where the quality score came from (override / benchmark / shrunk),
    /// for the explanation. `None` when imputed.
    pub quality_source: Option<&'static str>,
    /// Window failure rate in `[0, 1]`; lower is better.
    pub failure_rate: Option<f64>,
    /// End-to-end latency p95 in ms (tie-break and jitter penalty only).
    pub latency_p95_ms: Option<f64>,
    /// Latency coefficient of variation (stddev / mean) for the jitter
    /// penalty.
    pub latency_cv: Option<f64>,
    /// Window sample count behind `tokens_per_sec`/`failure_rate` (decision
    /// D27): thin windows shrink those components toward 0.5. `None` means
    /// the metrics carry no sample provenance and are not shrunk.
    pub telemetry_samples: Option<usize>,
    /// Per-member weight override (combo members): scales the final score.
    pub member_weight: f64,
    /// The (credential, offering) circuit breaker is open.
    pub breaker_open: bool,
    /// The pair tripped recently but is not open (the breaker's recovery
    /// signal): a small penalty under multi-component weights.
    pub breaker_recovering: bool,
    /// The credential is in cooldown (M3): counts as a recent failure.
    pub credential_cooled: bool,
    /// Remaining credential balance over its low threshold, clamped to
    /// `[0, 1]`; 1.0 is comfortable. `None` when unknown (advisory in v1,
    /// decision D8).
    pub balance_ratio: Option<f64>,
    /// Context pressure from [`context_pressure`], in `[0, 1]`.
    pub context_pressure: f64,
    // Filters — an excluded candidate is reported with its reason.
    pub enabled: bool,
    pub has_credential: bool,
    pub granted: bool,
    pub capability_fit: bool,
    pub context_fit: bool,
    /// Hard budget fit per candidate. In v1 this is always true: the hard
    /// gate is the M4 reservation and credential balance is advisory
    /// (decision D8); the slot exists so a balance sync can start excluding
    /// exhausted credentials without a schema change.
    pub budget_fit: bool,
}

impl CandidateSignal {
    /// A neutral candidate: passes every filter, no metrics (all imputed).
    pub fn neutral(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            price_per_m_usd: None,
            tokens_per_sec: None,
            quality: None,
            quality_source: None,
            failure_rate: None,
            latency_p95_ms: None,
            latency_cv: None,
            telemetry_samples: None,
            member_weight: 1.0,
            breaker_open: false,
            breaker_recovering: false,
            credential_cooled: false,
            balance_ratio: None,
            context_pressure: 0.0,
            enabled: true,
            has_credential: true,
            granted: true,
            capability_fit: true,
            context_fit: true,
            budget_fit: true,
        }
    }
}

/// Why a candidate was filtered out before scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    Disabled,
    NoCredential,
    AccessDenied,
    CapabilityMismatch,
    ContextExceeded,
    BudgetExhausted,
    BreakerOpen,
}

impl Exclusion {
    /// The `decision_json` outcome string (the M3 skip vocabulary).
    pub fn outcome(self) -> &'static str {
        match self {
            Self::Disabled => "skipped_disabled",
            Self::NoCredential => "skipped_no_credential",
            Self::AccessDenied => "skipped_access",
            Self::CapabilityMismatch => "skipped_capability",
            Self::ContextExceeded => "skipped_context",
            Self::BudgetExhausted => "skipped_budget",
            Self::BreakerOpen => "skipped_breaker_open",
        }
    }

    /// The one-line reason for the decision record.
    pub fn explain(self) -> String {
        let text = match self {
            Self::Disabled => "the offering or its provider is disabled",
            Self::NoCredential => "no usable credential for the provider",
            Self::AccessDenied => "access denied by grants for this caller",
            Self::CapabilityMismatch => "the model does not declare the required capability",
            Self::ContextExceeded => "estimated input plus max_tokens exceeds the context window",
            Self::BudgetExhausted => "the credential's remaining budget is exhausted",
            Self::BreakerOpen => "the circuit breaker is open for this credential on this offering",
        };
        format!("excluded: {text}")
    }
}

/// The component scores of one candidate, all in `[0, 1]` before penalties.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub price: f64,
    pub speed: f64,
    pub quality: f64,
    pub reliability: f64,
    /// The unscaled penalty term; the total subtracts `member_weight * penalties`.
    pub penalties: f64,
    /// `member_weight * (weighted sum − penalties)`. **Not clamped to `[0, 1]`**
    /// (review nit): member weights above 1 push it over and penalties take it
    /// below zero. The four components are the `[0, 1]` quantities.
    pub total: f64,
    /// Components imputed at the neutral 0.5 (missing metrics).
    pub imputed: Vec<&'static str>,
    /// Components shrunk toward the neutral 0.5 by thin telemetry windows
    /// (decision D27).
    pub shrunk: Vec<&'static str>,
}

impl Score {
    /// The `decision_json` score object (additive shape: `shrunk` joined
    /// `imputed` in the 2026-11-16 fix round).
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "price": self.price,
            "speed": self.speed,
            "quality": self.quality,
            "reliability": self.reliability,
            "penalties": self.penalties,
            "total": self.total,
            "imputed": self.imputed,
            "shrunk": self.shrunk,
        })
    }
}

/// One scored, ordered candidate.
#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub id: String,
    pub score: Score,
    /// Why this candidate sits where it sits (`#rank of n` included).
    pub explanation: String,
}

/// What the router decided: the eligible pool in order (best first) and the
/// filtered-out candidates with their reasons.
#[derive(Debug, Clone)]
pub struct RoutingOutcome {
    pub ordered: Vec<ScoredCandidate>,
    pub excluded: Vec<(String, Exclusion)>,
}

// ---------------------------------------------------------------------------
// Penalties
// ---------------------------------------------------------------------------

/// Penalty for the window failure rate (and one cooled credential).
pub const P_RECENT_FAILURES: f64 = 0.20;
/// Penalty while the circuit breaker is open (only reachable when scoring an
/// unfiltered pool; [`weighted_order`] filters those candidates out first).
pub const P_BREAKER: f64 = 0.50;
/// Penalty while a pair recently tripped but is not open — the breaker's
/// recovery signal (review m2: `breaker::is_recovering` was dead code with a
/// promise in its doc).
pub const P_RECOVERING: f64 = 0.10;
/// Penalty for a low remaining credential balance (scaled by its shortfall).
pub const P_LOW_QUOTA: f64 = 0.15;
/// Penalty for context pressure (scaled by [`context_pressure`]).
pub const P_NEAR_CONTEXT: f64 = 0.15;
/// Penalty for latency jitter (scaled by the coefficient of variation).
pub const P_LATENCY_JITTER: f64 = 0.10;

/// Context pressure in `[0, 1]`: 0 below the [`NEAR_CONTEXT_KNEE`] of the
/// context window, 1 at (or past) the window itself.
pub const NEAR_CONTEXT_KNEE: f64 = 0.8;

/// Context pressure of a request that wants `requested_tokens` of a
/// `context_length` window. Unknown windows have no pressure.
pub fn context_pressure(requested_tokens: f64, context_length: Option<i64>) -> f64 {
    let Some(context) = context_length.filter(|c| *c > 0) else {
        return 0.0;
    };
    let ratio = requested_tokens / context as f64;
    clamp01((ratio - NEAR_CONTEXT_KNEE) / (1.0 - NEAR_CONTEXT_KNEE))
}

fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

/// The subtracted penalty of one candidate (multi-component weights only —
/// pure orders never apply it, decision D24).
pub fn penalties(signal: &CandidateSignal) -> f64 {
    let recent_failures = signal.failure_rate.unwrap_or(0.0)
        + f64::from(signal.credential_cooled) * (1.0 - signal.failure_rate.unwrap_or(0.0));
    P_RECENT_FAILURES * recent_failures
        + match signal.breaker_open {
            true => P_BREAKER,
            false => 0.0,
        }
        + match signal.breaker_recovering {
            true => P_RECOVERING,
            false => 0.0,
        }
        + P_LOW_QUOTA
            * signal
                .balance_ratio
                .map(|ratio| clamp01(1.0 - ratio))
                .unwrap_or(0.0)
        + P_NEAR_CONTEXT * clamp01(signal.context_pressure)
        + P_LATENCY_JITTER * signal.latency_cv.map(clamp01).unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// The pipeline
// ---------------------------------------------------------------------------

/// Filters, scores, and orders `signals` with `weights`. Deterministic: the
/// same pool and weights always produce the same order (the tie-break ends on
/// the stable id).
pub fn weighted_order(signals: Vec<CandidateSignal>, weights: &Weights) -> RoutingOutcome {
    let mut eligible: Vec<CandidateSignal> = Vec::new();
    let mut excluded: Vec<(String, Exclusion)> = Vec::new();
    for signal in signals {
        match exclusion_of(&signal) {
            Some(reason) => excluded.push((signal.id, reason)),
            None => eligible.push(signal),
        }
    }

    let priced: Vec<Option<f64>> = eligible.iter().map(|s| s.price_per_m_usd).collect();
    let sped: Vec<Option<f64>> = eligible.iter().map(|s| s.tokens_per_sec).collect();
    let quality: Vec<Option<f64>> = eligible.iter().map(|s| s.quality).collect();
    let failure: Vec<Option<f64>> = eligible.iter().map(|s| s.failure_rate).collect();

    let price_scores = normalize(&priced, false);
    let speed_scores = normalize(&sped, true);
    let quality_scores = normalize(&quality, true);
    // Reliability is the inverse of the failure rate: lower failure rate is
    // better, so normalize inverted.
    let reliability_scores = normalize(&failure, false);

    let mut scored: Vec<(CandidateSignal, Score)> = eligible
        .into_iter()
        .zip(price_scores)
        .zip(speed_scores)
        .zip(quality_scores)
        .zip(reliability_scores)
        .map(|((((signal, price), speed), quality), reliability)| {
            let mut imputed = Vec::new();
            let mut shrunk = Vec::new();
            let price = unwrap_or_median(price, "price", &mut imputed);
            // Speed and reliability come from telemetry: thin windows shrink
            // them toward the neutral 0.5 (decision D27).
            let speed = unwrap_or_median(
                shrink_component(speed, "speed", signal.telemetry_samples, &mut shrunk),
                "speed",
                &mut imputed,
            );
            let quality = unwrap_or_median(quality, "quality", &mut imputed);
            let reliability = unwrap_or_median(
                shrink_component(
                    reliability,
                    "reliability",
                    signal.telemetry_samples,
                    &mut shrunk,
                ),
                "reliability",
                &mut imputed,
            );
            // Pure single-component orders carry no penalties (decision D24);
            // the member weight scales the whole (weighted − penalties), so
            // penalties count proportionally (review nit).
            let penalty = match weights.is_pure_order() {
                true => 0.0,
                false => penalties(&signal),
            };
            let weighted = weights.price * price
                + weights.speed * speed
                + weights.quality * quality
                + weights.reliability * reliability;
            let total = signal.member_weight * (weighted - penalty);
            (
                signal,
                Score {
                    price,
                    speed,
                    quality,
                    reliability,
                    penalties: penalty,
                    total,
                    imputed,
                    shrunk,
                },
            )
        })
        .collect();

    // Deterministic tie-break: lower price, then lower p95 latency, then
    // stable id. Unknown metrics sort last (they are not "cheap" or "fast").
    scored.sort_by(|(a, sa), (b, sb)| {
        sb.total
            .total_cmp(&sa.total)
            .then_with(|| tie_f64(a.price_per_m_usd).total_cmp(&tie_f64(b.price_per_m_usd)))
            .then_with(|| tie_f64(a.latency_p95_ms).total_cmp(&tie_f64(b.latency_p95_ms)))
            .then_with(|| a.id.cmp(&b.id))
    });

    let count = scored.len();
    let best = scored
        .first()
        .map(|(_, score)| score.total)
        .unwrap_or_default();
    let ordered = scored
        .into_iter()
        .enumerate()
        .map(|(index, (signal, score))| {
            let rank = index + 1;
            let explanation = explain_score(rank, count, &signal, &score, best);
            ScoredCandidate {
                id: signal.id,
                score,
                explanation,
            }
        })
        .collect();

    RoutingOutcome { ordered, excluded }
}

/// The first filter failure of a candidate, in pipeline order.
fn exclusion_of(signal: &CandidateSignal) -> Option<Exclusion> {
    match (signal.enabled, signal.has_credential, signal.granted) {
        (false, _, _) => Some(Exclusion::Disabled),
        (_, false, _) => Some(Exclusion::NoCredential),
        (_, _, false) => Some(Exclusion::AccessDenied),
        _ if !signal.capability_fit => Some(Exclusion::CapabilityMismatch),
        _ if !signal.context_fit => Some(Exclusion::ContextExceeded),
        _ if !signal.budget_fit => Some(Exclusion::BudgetExhausted),
        _ if signal.breaker_open => Some(Exclusion::BreakerOpen),
        _ => None,
    }
}

/// Min-max normalizes the present values to `[0, 1]`; `higher_is_better`
/// flips the direction. Absent values stay absent, and a pool without spread
/// scores 0.5 (no discrimination).
fn normalize(values: &[Option<f64>], higher_is_better: bool) -> Vec<Option<f64>> {
    let present: Vec<f64> = values.iter().flatten().copied().collect();
    if present.is_empty() {
        return values.iter().map(|_| None).collect();
    }
    let min = present.iter().copied().fold(f64::INFINITY, f64::min);
    let max = present.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    values
        .iter()
        .map(|value| {
            let value = (*value)?;
            if (max - min).abs() <= f64::EPSILON {
                return Some(0.5);
            }
            let normalized = (value - min) / (max - min);
            Some(match higher_is_better {
                true => normalized,
                false => 1.0 - normalized,
            })
        })
        .collect()
}

/// Sample-count trust factor for telemetry-derived components (decision
/// D27): `n/(n+10)` below 10 window samples — one lucky sample keeps only
/// 1/11 of its distance to neutral — and fully trusted at 10+.
fn trust_factor(samples: Option<usize>) -> f64 {
    match samples {
        Some(n) if n < 10 => n as f64 / (n as f64 + 10.0),
        _ => 1.0,
    }
}

/// Shrinks a present normalized component toward the neutral 0.5 by
/// [`trust_factor`] (decision D27) and marks it when it moved. Missing
/// components pass through — they are imputed at 0.5 separately.
fn shrink_component(
    value: Option<f64>,
    name: &'static str,
    samples: Option<usize>,
    shrunk: &mut Vec<&'static str>,
) -> Option<f64> {
    let value = value?;
    let factor = trust_factor(samples);
    if factor >= 1.0 {
        return Some(value);
    }
    shrunk.push(name);
    Some(0.5 + (value - 0.5) * factor)
}

/// A missing component gets the flat neutral 0.5 and is marked.
fn unwrap_or_median(
    value: Option<f64>,
    name: &'static str,
    imputed: &mut Vec<&'static str>,
) -> f64 {
    match value {
        Some(value) => value,
        None => {
            imputed.push(name);
            0.5
        }
    }
}

/// The tie-break projection of an optional metric: unknown sorts last.
fn tie_f64(value: Option<f64>) -> f64 {
    value.unwrap_or(f64::INFINITY)
}

/// The one-line explanation of a scored candidate's rank.
fn explain_score(
    rank: usize,
    count: usize,
    signal: &CandidateSignal,
    score: &Score,
    best: f64,
) -> String {
    let components = format!(
        "price {:.2}, speed {:.2}, quality {:.2}, reliability {:.2}",
        score.price, score.speed, score.quality, score.reliability
    );
    let mut line = match rank {
        1 => format!(
            "scored #1 of {count}: best weighted score {:.4} ({components}, penalties {:.2}, member weight {})",
            score.total, score.penalties, signal.member_weight
        ),
        _ => format!(
            "scored #{rank} of {count}: weighted score {:.4} below the leader {:.4} ({components}, penalties {:.2}, member weight {})",
            score.total, best, score.penalties, signal.member_weight
        ),
    };
    if !score.imputed.is_empty() {
        line.push_str(&format!(
            "; imputed at 0.5 (no data): {}",
            score.imputed.join(", ")
        ));
    }
    if !score.shrunk.is_empty() {
        line.push_str(&format!(
            "; shrunk toward 0.5 (fewer than 10 samples): {}",
            score.shrunk.join(", ")
        ));
    }
    if let Some(source) = signal.quality_source {
        line.push_str(&format!("; quality from {source}"));
    }
    line
}

// ---------------------------------------------------------------------------
// Quality resolution (decision D4/D5)
// ---------------------------------------------------------------------------

/// Where a quality score came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualitySource {
    /// The operator's `model_quality_overrides` row.
    Override,
    /// An imported benchmark with a narrow (or absent) CI.
    Benchmark,
    /// An imported benchmark whose CI was shrunk toward the mean.
    BenchmarkShrunk,
    /// Nothing imported: the neutral model default (imputed).
    ModelDefault,
}

impl QualitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::Benchmark => "benchmark",
            Self::BenchmarkShrunk => "benchmark_shrunk",
            Self::ModelDefault => "model_default",
        }
    }
}

/// The resolved quality score of one model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityScore {
    pub score: f64,
    pub source: QualitySource,
    /// True for [`QualitySource::ModelDefault`]: the neutral 0.5 imputation.
    pub imputed: bool,
}

/// CI half-width (in the source's own units, e.g. Elo points) at which a
/// benchmark keeps half its distance to the mean. Wide CIs shrink hard,
/// narrow CIs barely move.
pub const CI_HALF_WIDTH_SCALE: f64 = 50.0;

/// Shrinks `score` toward `mean` by its confidence: `mean + (score - mean) *
/// scale² / (scale² + ci²)`. A missing CI keeps the score unchanged.
pub fn shrink(score: f64, ci: Option<f64>, mean: f64) -> f64 {
    let Some(ci) = ci else {
        return score;
    };
    let factor = {
        let scale = CI_HALF_WIDTH_SCALE;
        scale * scale / (scale * scale + ci * ci)
    };
    mean + (score - mean) * factor
}

/// Resolves a model's quality: the operator override wins, then the latest
/// imported benchmark (wide CIs shrunk toward `mean`), then the neutral
/// model default of 0.5 (marked imputed).
pub fn resolve_quality(
    override_score: Option<f64>,
    benchmark: Option<(f64, Option<f64>)>,
    mean: Option<f64>,
) -> QualityScore {
    if let Some(score) = override_score {
        return QualityScore {
            score,
            source: QualitySource::Override,
            imputed: false,
        };
    }
    if let Some((score, ci)) = benchmark {
        let shrunk = shrink(score, ci, mean.unwrap_or(score));
        return QualityScore {
            score: shrunk,
            source: match ci.is_some() {
                true => QualitySource::BenchmarkShrunk,
                false => QualitySource::Benchmark,
            },
            imputed: false,
        };
    }
    QualityScore {
        score: 0.5,
        source: QualitySource::ModelDefault,
        imputed: true,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A candidate with the given metrics; everything else neutral.
    fn signal(
        id: &str,
        price: Option<f64>,
        tps: Option<f64>,
        quality: Option<f64>,
        failure_rate: Option<f64>,
    ) -> CandidateSignal {
        CandidateSignal {
            id: id.to_string(),
            price_per_m_usd: price,
            tokens_per_sec: tps,
            quality,
            failure_rate,
            ..CandidateSignal::neutral(id)
        }
    }

    fn ids(outcome: &RoutingOutcome) -> Vec<&str> {
        outcome.ordered.iter().map(|c| c.id.as_str()).collect()
    }

    /// A fixed spread-out pool: a is cheap+slow+mediocre, b is
    // expensive+fast+great, c sits in between.
    fn pool() -> Vec<CandidateSignal> {
        vec![
            signal("a", Some(1.0), Some(10.0), Some(0.4), Some(0.10)),
            signal("b", Some(3.0), Some(80.0), Some(0.9), Some(0.02)),
            signal("c", Some(2.0), Some(40.0), Some(0.6), Some(0.05)),
        ]
    }

    /// A tiny deterministic LCG so the shuffle tests need no RNG crate.
    fn lcg_shuffle<T>(items: &mut [T], seed: u64) {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        for i in (1..items.len()).rev() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let j = ((state >> 33) as usize) % (i + 1);
            items.swap(i, j);
        }
    }

    #[test]
    fn same_pool_and_weights_always_produce_the_same_ordering() {
        // Property-style over a fixed pool: shuffle the input order many
        // times; the output order must never change (determinism).
        let expected_outcome = weighted_order(pool(), &Weights::default());
        let expected = ids(&expected_outcome);
        assert_eq!(expected.len(), 3);
        for seed in 0..50u64 {
            let mut inputs = pool();
            lcg_shuffle(&mut inputs, seed);
            let outcome = weighted_order(inputs, &Weights::default());
            let got = ids(&outcome);
            assert_eq!(got, expected, "input permutation {seed} changed the order");
        }
    }

    #[test]
    fn a_weight_change_flips_the_winner() {
        // `a` is the best price, `b` the best quality.
        let cheap = Weights {
            price: 1.0,
            speed: 0.0,
            quality: 0.0,
            reliability: 0.0,
        };
        let great = Weights {
            price: 0.0,
            speed: 0.0,
            quality: 1.0,
            reliability: 0.0,
        };
        assert_eq!(ids(&weighted_order(pool(), &cheap)), ["a", "c", "b"]);
        assert_eq!(ids(&weighted_order(pool(), &great)), ["b", "c", "a"]);
    }

    /// The D24 flip fixture: `deal` is the cheap, the fast, and the best
    /// candidate at once — but maximally unhealthy (0.60 in penalty inputs:
    /// failed window, cooled credential, empty balance, full context
    /// pressure, jitter). Under multi-component weights the penalties demote
    /// it; a pure order must not (decision D24).
    fn unhealthy_deal(id: &str, price: f64, tps: f64, quality: f64) -> CandidateSignal {
        let mut deal = signal(id, Some(price), Some(tps), Some(quality), Some(1.0));
        deal.credential_cooled = true;
        deal.balance_ratio = Some(0.0);
        deal.context_pressure = 1.0;
        deal.latency_cv = Some(1.0);
        deal
    }

    /// A pool where the unhealthy candidate wins its own component and would
    /// lose under `balanced` — the flip the fixture must be able to expose.
    fn flip_pool(
        deal: CandidateSignal,
        solid: CandidateSignal,
        mid: CandidateSignal,
    ) -> Vec<CandidateSignal> {
        vec![deal, solid, mid]
    }

    #[test]
    fn cheapest_preset_is_price_order() {
        let (name, weights) = Weights::preset("cheapest").expect("preset");
        assert_eq!(name, "cheapest");
        let pool = flip_pool(
            unhealthy_deal("deal", 1.0, 10.0, 0.4),
            signal("solid", Some(3.0), Some(80.0), Some(0.9), Some(0.0)),
            signal("mid", Some(2.0), Some(40.0), Some(0.6), Some(0.0)),
        );
        assert!(
            penalties(&unhealthy_deal("deal", 1.0, 10.0, 0.4)) >= 0.60,
            "the fixture carries enough penalty to flip the order"
        );
        // The cheap-but-unhealthy candidate still wins `cheapest`: health is
        // the filters' and the breaker's job (decision D24).
        let outcome = weighted_order(pool.clone(), &weights);
        assert_eq!(ids(&outcome), ["deal", "mid", "solid"]);
        assert_eq!(
            outcome.ordered[0].score.penalties, 0.0,
            "pure orders carry no penalties"
        );
        // And the same fixture under `balanced` exposes the flip: there the
        // 0.60 of penalties sends `deal` to the last rank.
        let (_, balanced) = Weights::preset("balanced").expect("preset");
        assert_eq!(
            ids(&weighted_order(pool, &balanced)),
            ["solid", "mid", "deal"],
            "multi-component weights still apply penalties"
        );
    }

    #[test]
    fn fastest_preset_is_speed_order() {
        let (name, weights) = Weights::preset("fastest").expect("preset");
        assert_eq!(name, "fastest");
        let pool = flip_pool(
            unhealthy_deal("deal", 3.0, 100.0, 0.4),
            signal("solid", Some(1.0), Some(40.0), Some(0.6), Some(0.0)),
            signal("mid", Some(2.0), Some(70.0), Some(0.9), Some(0.0)),
        );
        let outcome = weighted_order(pool, &weights);
        assert_eq!(ids(&outcome), ["deal", "mid", "solid"]);
        assert_eq!(outcome.ordered[0].score.penalties, 0.0);
    }

    #[test]
    fn best_preset_is_quality_order() {
        let (name, weights) = Weights::preset("best").expect("preset");
        assert_eq!(name, "best");
        let pool = flip_pool(
            unhealthy_deal("deal", 3.0, 10.0, 0.95),
            signal("solid", Some(1.0), Some(80.0), Some(0.4), Some(0.0)),
            signal("mid", Some(2.0), Some(40.0), Some(0.6), Some(0.0)),
        );
        let outcome = weighted_order(pool, &weights);
        assert_eq!(ids(&outcome), ["deal", "mid", "solid"]);
        assert_eq!(outcome.ordered[0].score.penalties, 0.0);
    }

    #[test]
    fn single_component_weights_are_pure_orders() {
        for name in ["cheapest", "fastest", "best"] {
            let (_, weights) = Weights::preset(name).expect("preset");
            assert!(weights.is_pure_order(), "{name} is a pure order");
        }
        assert!(
            !Weights::default().is_pure_order(),
            "balanced applies penalties"
        );
        assert!(
            !Weights {
                price: 0.8,
                ..Weights::default()
            }
            .is_pure_order()
        );
        assert!(
            Weights {
                price: 3.0,
                speed: 0.0,
                quality: 0.0,
                reliability: 0.0,
            }
            .is_pure_order(),
            "a custom single-component vector is a pure order too"
        );
    }

    #[test]
    fn balanced_preset_is_the_documented_default_weighted_sum() {
        let (name, weights) = Weights::preset("balanced").expect("preset");
        assert_eq!(name, "balanced");
        assert_eq!(weights, Weights::default());
        let outcome = weighted_order(pool(), &weights);
        // min-max over the pool: price a=1.0/c=0.5/b=0.0 (inverted), speed
        // b=1.0/c=0.4286/a=0.0, quality b=1.0/c=0.4/a=0.0, reliability
        // b=1.0/c=0.625/a=0.0 (inverted). Manual weighted sums minus the
        // failure-rate penalty must match.
        let by_id = |id: &str| {
            outcome
                .ordered
                .iter()
                .find(|c| c.id == id)
                .expect("candidate")
                .score
                .clone()
        };
        let b = by_id("b");
        let expected_b =
            0.25 * 0.0 + 0.30 * 1.0 + 0.30 * 1.0 + 0.15 * 1.0 - P_RECENT_FAILURES * 0.02;
        assert!(
            (b.total - expected_b).abs() < 1e-9,
            "b total {} != {expected_b}",
            b.total
        );
        let a = by_id("a");
        let expected_a =
            0.25 * 1.0 + 0.30 * 0.0 + 0.30 * 0.0 + 0.15 * 0.0 - P_RECENT_FAILURES * 0.10;
        assert!((a.total - expected_a).abs() < 1e-9);
        assert!(b.total > a.total);
    }

    #[test]
    fn missing_components_are_imputed_at_half_and_marked() {
        let mut signals = pool();
        signals.push(signal("d", None, None, None, None));
        let outcome = weighted_order(signals, &Weights::default());
        let d = outcome
            .ordered
            .iter()
            .find(|c| c.id == "d")
            .expect("candidate d");
        assert_eq!(d.score.price, 0.5);
        assert_eq!(d.score.speed, 0.5);
        assert_eq!(d.score.quality, 0.5);
        assert_eq!(d.score.reliability, 0.5);
        assert_eq!(
            d.score.imputed,
            ["price", "speed", "quality", "reliability"],
            "every missing component is marked imputed"
        );
        assert!(
            d.explanation.contains("imputed at 0.5"),
            "the explanation says so: {}",
            d.explanation
        );
    }

    #[test]
    fn the_tie_break_is_lower_price_then_p95_then_stable_id() {
        // Three identical candidates (all components imputed, equal scores):
        // the order must be price, then p95, then id — never input order.
        let mut a = CandidateSignal::neutral("c3");
        a.price_per_m_usd = Some(2.0);
        a.latency_p95_ms = Some(500.0);
        let mut b = CandidateSignal::neutral("c1");
        b.price_per_m_usd = Some(1.0);
        b.latency_p95_ms = Some(900.0);
        let mut c = CandidateSignal::neutral("c2");
        c.price_per_m_usd = Some(1.0);
        c.latency_p95_ms = Some(100.0);
        for inputs in [
            vec![a.clone(), b.clone(), c.clone()],
            vec![c.clone(), a.clone(), b.clone()],
            vec![b.clone(), c.clone(), a.clone()],
        ] {
            let outcome = weighted_order(inputs, &Weights::default());
            assert_eq!(
                ids(&outcome),
                ["c2", "c1", "c3"],
                "lower price first, then lower p95, then id"
            );
        }
    }

    #[test]
    fn filters_exclude_candidates_with_one_line_reasons() {
        let mut disabled = signal("disabled", Some(1.0), None, None, None);
        disabled.enabled = false;
        let mut no_credential = signal("no-cred", Some(1.0), None, None, None);
        no_credential.has_credential = false;
        let mut denied = signal("denied", Some(1.0), None, None, None);
        denied.granted = false;
        let mut incapable = signal("incapable", Some(1.0), None, None, None);
        incapable.capability_fit = false;
        let mut too_big = signal("too-big", Some(1.0), None, None, None);
        too_big.context_fit = false;
        let mut broke = signal("broke", Some(1.0), None, None, None);
        broke.budget_fit = false;
        let mut breaker = signal("breaker", Some(1.0), None, None, None);
        breaker.breaker_open = true;

        let outcome = weighted_order(
            vec![
                disabled,
                no_credential,
                denied,
                incapable,
                too_big,
                broke,
                breaker,
                signal("ok", Some(1.0), None, None, None),
            ],
            &Weights::default(),
        );
        assert_eq!(ids(&outcome), ["ok"]);
        let reasons: Vec<(&str, &str)> = outcome
            .excluded
            .iter()
            .map(|(id, reason)| (id.as_str(), reason.outcome()))
            .collect();
        assert_eq!(
            reasons,
            [
                ("disabled", "skipped_disabled"),
                ("no-cred", "skipped_no_credential"),
                ("denied", "skipped_access"),
                ("incapable", "skipped_capability"),
                ("too-big", "skipped_context"),
                ("broke", "skipped_budget"),
                ("breaker", "skipped_breaker_open"),
            ]
        );
        for (id, reason) in &outcome.excluded {
            assert!(!reason.explain().is_empty(), "{id} needs an explanation");
        }
    }

    #[test]
    fn penalties_subtract_from_the_weighted_sum() {
        let mut clean = signal("clean", Some(2.0), Some(40.0), Some(0.6), Some(0.0));
        clean.latency_cv = Some(0.0);
        let mut dirty = signal("dirty", Some(2.0), Some(40.0), Some(0.6), Some(0.5));
        dirty.latency_cv = Some(0.8);
        dirty.context_pressure = 1.0;
        dirty.credential_cooled = true;

        let outcome = weighted_order(vec![clean.clone(), dirty.clone()], &Weights::default());
        let clean_score = &outcome
            .ordered
            .iter()
            .find(|c| c.id == "clean")
            .unwrap()
            .score;
        let dirty_score = &outcome
            .ordered
            .iter()
            .find(|c| c.id == "dirty")
            .unwrap()
            .score;
        assert_eq!(clean_score.penalties, 0.0);
        // 0.20 (failures + cooled) + 0.15 (context at 1.0) + 0.08 (jitter).
        assert!(
            (dirty_score.penalties - 0.43).abs() < 1e-9,
            "failures + cooled + context + jitter: {}",
            dirty_score.penalties
        );
        assert!(dirty_score.total < clean_score.total);
        assert_eq!(ids(&outcome), ["clean", "dirty"]);
    }

    #[test]
    fn the_breaker_penalty_only_matters_on_an_unfiltered_pool() {
        // `weighted_order` filters breaker-open candidates out; the penalty
        // still applies when the pool is scored directly.
        let mut open = signal("open", Some(1.0), Some(100.0), Some(1.0), Some(0.0));
        open.breaker_open = true;
        let clean = signal("clean", Some(1.0), Some(100.0), Some(1.0), Some(0.0));
        let outcome = weighted_order(vec![open, clean.clone()], &Weights::default());
        assert_eq!(ids(&outcome), ["clean"], "the open breaker is filtered out");

        let penalty = penalties(&{
            let mut open = signal("open", Some(1.0), None, None, None);
            open.breaker_open = true;
            open
        });
        assert_eq!(penalty, P_BREAKER);
        assert_eq!(penalties(&clean), 0.0);
    }

    #[test]
    fn low_quota_and_near_context_pressures_scale_their_penalties() {
        let mut low = signal("low", None, None, None, None);
        low.balance_ratio = Some(0.0);
        assert!((penalties(&low) - P_LOW_QUOTA).abs() < 1e-12);
        let mut comfortable = signal("ok", None, None, None, None);
        comfortable.balance_ratio = Some(1.0);
        assert_eq!(penalties(&comfortable), 0.0);

        assert_eq!(context_pressure(100.0, Some(1000)), 0.0, "below the knee");
        assert_eq!(context_pressure(800.0, Some(1000)), 0.0, "at the knee");
        assert_eq!(context_pressure(900.0, Some(1000)), 0.5);
        assert_eq!(context_pressure(1000.0, Some(1000)), 1.0);
        assert_eq!(context_pressure(5000.0, None), 0.0, "unknown window");
    }

    #[test]
    fn wide_cis_shrink_toward_the_mean_and_narrow_ones_keep_the_score() {
        let mean = 1400.0;
        let wide = shrink(1500.0, Some(100.0), mean);
        assert!(
            wide > mean && wide < 1500.0,
            "wide CI shrinks between mean and score: {wide}"
        );
        assert!(
            (wide - mean).abs() < (1500.0 - mean) * 0.3,
            "a 100-point CI keeps less than a third of the distance: {wide}"
        );
        let narrow = shrink(1500.0, Some(5.0), mean);
        assert!(
            (narrow - 1500.0).abs() < 5.0,
            "a narrow CI barely moves: {narrow}"
        );
        assert_eq!(shrink(1500.0, None, mean), 1500.0, "no CI, no shrink");
    }

    #[test]
    fn quality_resolution_prefers_override_then_benchmark_then_default() {
        // Fixtures are Elo-scale throughout (decision D25: one quality scale).
        let by_override = resolve_quality(Some(1450.0), Some((1500.0, Some(10.0))), Some(1400.0));
        assert_eq!(by_override.source, QualitySource::Override);
        assert_eq!(by_override.score, 1450.0);
        assert!(!by_override.imputed);

        let by_benchmark = resolve_quality(None, Some((1500.0, Some(45.0))), Some(1400.0));
        assert_eq!(by_benchmark.source, QualitySource::BenchmarkShrunk);
        assert!(
            by_benchmark.score < 1500.0 && by_benchmark.score > 1400.0,
            "shrunk toward the mean: {}",
            by_benchmark.score
        );

        let exact = resolve_quality(None, Some((1500.0, None)), Some(1400.0));
        assert_eq!(exact.source, QualitySource::Benchmark);
        assert_eq!(exact.score, 1500.0);

        let defaulted = resolve_quality(None, None, None);
        assert_eq!(defaulted.source, QualitySource::ModelDefault);
        assert_eq!(defaulted.score, 0.5);
        assert!(defaulted.imputed);
    }

    #[test]
    fn weight_parsing_handles_presets_partial_objects_and_errors() {
        let (weights, preset) = Weights::from_json(None).expect("null weights");
        assert_eq!(weights, Weights::default());
        assert_eq!(preset, Some("balanced"));

        let (weights, preset) =
            Weights::from_json(Some(r#"{"preset": "cheapest"}"#)).expect("preset");
        assert_eq!(weights.price, 1.0);
        assert_eq!(preset, Some("cheapest"));

        let (weights, preset) =
            Weights::from_json(Some(r#"{"price": 0.8, "quality": 0.1}"#)).expect("partial");
        assert_eq!(weights.price, 0.8);
        assert_eq!(weights.quality, 0.1);
        assert_eq!(weights.speed, 0.30, "missing keys keep the defaults");
        assert_eq!(preset, None);

        assert_eq!(
            Weights::from_json(Some(r#"{"preset": "nope"}"#)),
            Err(RoutingError::UnknownPreset("nope".to_string()))
        );
        assert_eq!(
            Weights::from_json(Some(r#"{"price": -1}"#)),
            Err(RoutingError::InvalidWeights)
        );
        assert_eq!(
            Weights::from_json(Some(
                r#"{"price": 0, "speed": 0, "quality": 0, "reliability": 0}"#
            )),
            Err(RoutingError::InvalidWeights),
            "all-zero weights cannot discriminate"
        );
        assert!(matches!(
            Weights::from_json(Some("[1,2]")),
            Err(RoutingError::MalformedWeights(_))
        ));
    }

    #[test]
    fn strategies_parse_and_round_trip() {
        for name in ["ordered", "weighted", "round_robin"] {
            let strategy = Strategy::parse(name).expect("strategy");
            assert_eq!(strategy.as_str(), name);
        }
        assert!(matches!(
            Strategy::parse("zoo"),
            Err(RoutingError::UnknownStrategy(_))
        ));
    }

    #[test]
    fn rotate_is_a_stable_wrapping_rotation() {
        let items = vec!["a", "b", "c"];
        assert_eq!(rotate(items.clone(), 0), ["a", "b", "c"]);
        assert_eq!(rotate(items.clone(), 1), ["b", "c", "a"]);
        assert_eq!(rotate(items.clone(), 2), ["c", "a", "b"]);
        assert_eq!(rotate(items.clone(), 5), ["c", "a", "b"], "modulo length");
        assert_eq!(rotate::<&str>(Vec::new(), 3), Vec::<&str>::new());
    }

    #[test]
    fn member_weights_scale_the_final_score() {
        let mut heavy = signal("heavy", Some(2.0), Some(40.0), Some(0.6), Some(0.05));
        heavy.member_weight = 2.0;
        let light = signal("light", Some(2.0), Some(40.0), Some(0.6), Some(0.05));
        let outcome = weighted_order(vec![light, heavy], &Weights::default());
        assert_eq!(ids(&outcome), ["heavy", "light"]);
    }

    #[test]
    fn a_pool_without_spread_scores_half_for_everyone() {
        let outcome = weighted_order(
            vec![
                signal("a", Some(1.0), None, None, None),
                signal("b", Some(1.0), None, None, None),
            ],
            &Weights::default(),
        );
        for candidate in &outcome.ordered {
            assert_eq!(candidate.score.price, 0.5, "no spread, no discrimination");
        }
        assert_eq!(ids(&outcome), ["a", "b"], "the tie-break is the id");
    }

    #[test]
    fn penalties_scale_with_the_member_weight() {
        // Review nit: the member weight scales the whole (weighted −
        // penalties), so a weighted member's penalty counts proportionally.
        let mut light = signal("light", Some(2.0), Some(40.0), Some(0.6), Some(0.5));
        light.latency_cv = Some(0.8);
        light.context_pressure = 1.0;
        light.credential_cooled = true;
        let mut heavy = light.clone();
        heavy.id = "heavy".to_string();
        heavy.member_weight = 2.0;
        let outcome = weighted_order(vec![light, heavy], &Weights::default());
        let by_id = |id: &str| {
            outcome
                .ordered
                .iter()
                .find(|candidate| candidate.id == id)
                .expect("candidate")
                .score
                .clone()
        };
        let light_score = by_id("light");
        let heavy_score = by_id("heavy");
        assert!(
            (light_score.penalties - 0.43).abs() < 1e-9,
            "{light_score:?}"
        );
        // Identical metrics (no spread → 0.5 each): total = weight·(0.5−0.43).
        assert!(
            (light_score.total - 0.07).abs() < 1e-9,
            "{}",
            light_score.total
        );
        assert!(
            (heavy_score.total - 2.0 * light_score.total).abs() < 1e-9,
            "the penalty doubled with the member weight: {}",
            heavy_score.total
        );
        assert_eq!(ids(&outcome), ["heavy", "light"]);
    }

    #[test]
    fn a_recently_tripped_pair_pays_a_small_recovery_penalty() {
        // Review m2: `breaker::is_recovering` is wired as a small penalty
        // under multi-component weights — and ignored by pure orders (D24).
        let clean = signal("clean", Some(2.0), Some(40.0), Some(0.6), Some(0.0));
        let mut recovering = signal("recovering", Some(2.0), Some(40.0), Some(0.6), Some(0.0));
        recovering.breaker_recovering = true;

        let outcome = weighted_order(vec![clean.clone(), recovering.clone()], &Weights::default());
        assert_eq!(ids(&outcome), ["clean", "recovering"]);
        assert_eq!(
            outcome.ordered[1].score.penalties, P_RECOVERING,
            "the recovery signal is the only difference"
        );

        let (_, cheapest) = Weights::preset("cheapest").expect("preset");
        let outcome = weighted_order(vec![clean, recovering], &cheapest);
        assert_eq!(outcome.ordered[1].score.penalties, 0.0, "pure order");
    }

    #[test]
    fn one_lucky_sample_cannot_outrank_a_200_sample_veteran() {
        // Decision D27: a metric from n < 10 samples keeps only n/(n+10) of
        // its distance to the neutral 0.5, so one lucky sample cannot define
        // an offering. The veteran's 200 samples are fully trusted.
        let mut lucky = signal("lucky", None, Some(100.0), None, None);
        lucky.telemetry_samples = Some(1);
        let mut veteran = signal("veteran", None, Some(75.0), None, None);
        veteran.telemetry_samples = Some(200);
        let mut slow = signal("slow", None, Some(25.0), None, None);
        slow.telemetry_samples = Some(200);

        let outcome = weighted_order(vec![lucky, veteran, slow], &Weights::default());
        assert_eq!(
            ids(&outcome),
            ["veteran", "lucky", "slow"],
            "the lucky sample collapsed toward neutral and lost the lead"
        );
        let score_of = |id: &str| {
            outcome
                .ordered
                .iter()
                .find(|candidate| candidate.id == id)
                .expect("candidate")
                .score
                .clone()
        };
        let lucky_score = score_of("lucky");
        assert!(
            (lucky_score.speed - (0.5 + 0.5 / 11.0)).abs() < 1e-9,
            "1 sample keeps 1/11 of the distance: {}",
            lucky_score.speed
        );
        assert_eq!(lucky_score.shrunk, ["speed"], "and is marked shrunk");
        assert!(
            outcome.ordered[1].explanation.contains("shrunk toward 0.5"),
            "{}",
            outcome.ordered[1].explanation
        );
        let veteran_score = score_of("veteran");
        assert!(veteran_score.shrunk.is_empty(), "10+ samples are trusted");
        assert!(
            (veteran_score.speed - 50.0 / 75.0).abs() < 1e-9,
            "min-max over 25..100: {}",
            veteran_score.speed
        );
        assert_eq!(trust_factor(Some(1)), 1.0 / 11.0);
        assert_eq!(trust_factor(Some(10)), 1.0, "trusted at 10+");
        assert_eq!(trust_factor(Some(200)), 1.0);
        assert_eq!(trust_factor(None), 1.0, "no provenance, no shrink");
    }

    /// A pseudo-random optional metric for the property test: some `None`
    /// (imputed), the rest a small non-negative spread.
    fn metric(bits: u64) -> Option<f64> {
        match bits % 5 {
            0 => None,
            _ => Some((bits % 10_000) as f64 / 100.0),
        }
    }

    #[test]
    fn randomized_pools_keep_the_score_invariants() {
        // Reviewer's suggested strengthening over randomized pools:
        // (a) every component score is in [0, 1] and no NaN,
        // (b) the output is a permutation of the eligible input,
        // (c) identical inputs give identical orders.
        let mut state = 0x243F_6A88_85A3_08D3u64;
        let mut rng = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state >> 33
        };
        for round in 0..200u64 {
            let size = (rng() % 8 + 1) as usize;
            let mut pool = Vec::with_capacity(size);
            for index in 0..size {
                let mut candidate = CandidateSignal::neutral(format!("c{index}"));
                candidate.price_per_m_usd = metric(rng());
                candidate.tokens_per_sec = metric(rng());
                candidate.quality = metric(rng());
                candidate.failure_rate = metric(rng()).map(|rate| rate % 1.0);
                candidate.latency_p95_ms = metric(rng());
                candidate.latency_cv = metric(rng()).map(|cv| cv % 1.0);
                candidate.telemetry_samples = Some((rng() % 30) as usize);
                candidate.member_weight = 0.5 + (rng() % 32) as f64 / 16.0;
                candidate.breaker_recovering = rng() % 4 == 0;
                candidate.credential_cooled = rng() % 4 == 0;
                candidate.balance_ratio = metric(rng()).map(|ratio| ratio % 1.0);
                candidate.context_pressure = (rng() % 101) as f64 / 100.0;
                if rng() % 5 == 0 {
                    candidate.enabled = false;
                }
                if rng() % 7 == 0 {
                    candidate.context_fit = false;
                }
                if rng() % 11 == 0 {
                    candidate.granted = false;
                }
                pool.push(candidate);
            }
            let weights = match round % 5 {
                0 => Weights::default(),
                1 => Weights::preset("cheapest").expect("preset").1,
                2 => Weights::preset("fastest").expect("preset").1,
                3 => Weights::preset("best").expect("preset").1,
                _ => Weights {
                    price: 1.0,
                    speed: 2.0,
                    quality: 0.5,
                    reliability: 1.5,
                },
            };

            // (c) identical inputs, identical order.
            let outcome = weighted_order(pool.clone(), &weights);
            let again = weighted_order(pool.clone(), &weights);
            assert_eq!(ids(&outcome), ids(&again), "round {round}");

            // (b) ordered + excluded is a permutation of the input.
            let mut input_ids: Vec<&str> =
                pool.iter().map(|candidate| candidate.id.as_str()).collect();
            input_ids.sort_unstable();
            let mut output_ids = ids(&outcome);
            output_ids.extend(outcome.excluded.iter().map(|(id, _)| id.as_str()));
            output_ids.sort_unstable();
            assert_eq!(output_ids, input_ids, "round {round}");

            // (a) components stay in [0, 1], everything is finite and no NaN.
            for candidate in &outcome.ordered {
                for (name, value) in [
                    ("price", candidate.score.price),
                    ("speed", candidate.score.speed),
                    ("quality", candidate.score.quality),
                    ("reliability", candidate.score.reliability),
                ] {
                    assert!(
                        value.is_finite() && (0.0..=1.0).contains(&value),
                        "round {round} {} {name} score {value}",
                        candidate.id
                    );
                }
                assert!(
                    candidate.score.penalties.is_finite() && candidate.score.total.is_finite(),
                    "round {round} {} totals are finite",
                    candidate.id
                );
            }
        }
    }
}
