//! Models overview: one query-powered payload for the admin Models page.
//!
//! `GET /api/models/overview?provider=<id|all>` (admin session) merges three
//! sources the page would otherwise join client-side:
//!
//! * `scope=provider` returns one row per offering of that provider, with the
//!   offering identity, effective limits (offering override wins over the
//!   model value), its price row, its **mix price** at the selected token-mix
//!   profile, and the model's resolved quality;
//! * `scope=all` (the default) returns one row per model, aggregating every
//!   offering across providers, its average and minimum component price, its
//!   average and minimum mix price, and quality.
//!
//! **Mix price (decision D34).** `?mix=<profile id|name>` selects the
//! profile; absent means the resolved default
//! (`GATEWAY_TOKEN_MIX_PROFILE` > `pricing.default_mix_profile` > built-in
//! `code-agent`). Every row carries `mix_price` (`per_m`, `profile_id`,
//! `profile_name`, `basis`) or `null` when the blend cannot be computed
//! (unpriced row, or a used component with no price even after its fallback).
//! The top-level `mix` echoes the profile. In `scope=all`, `mix_price` is the
//! **average** and `mix_price_min` the **minimum** over offerings whose mix
//! price is computable; offerings without a mix price are excluded, never
//! averaged toward zero. Component `price_avg`/`price_min` keep their
//! existing per-component rule.
//!
//! **Price aggregation rule (`scope=all`).** An offering is *priced* when its
//! `prices` row has at least one non-null component; an offering without a
//! price row, or with an all-null row, is *unpriced* and counted only in
//! `unpriced_offerings` — it never drags an average toward zero (D10: unknown
//! is never 0). Each aggregate component is the average/minimum over the
//! priced offerings that actually carry that component; a component nobody
//! prices stays `null`. `price_avg`/`price_min` are `null` when the model has
//! no priced offering at all.
//!
//! **Variable prices.** A source may publish `"-1"` instead of a number
//! (OpenRouter does for router models). That is a sentinel, not a price: the
//! sync stores the component as NULL and sets `prices.variable_price`, which
//! `OverviewPrice.variable_price` and `AllModelsRow.variable_offerings`
//! expose. Such an offering is unpriced when every component is variable
//! (`unpriced_offerings`), is never averaged or ranked as cheap, and the UI
//! shows "variable" instead of a number.
//!
//! **Quality** is the existing M5 resolution
//! ([`crate::benchmarks::quality_details_for_models`]): operator override >
//! latest imported benchmark (wide CIs shrunk toward the leaderboard mean;
//! `text` wins ties over `code`) > neutral model default `0.5`. The provenance
//! fields (`category`, `ci`, `rank`, `fetched_at`) come from the selected
//! benchmark even when an override wins; `overridden` marks that case.
//!
//! **Search (decision D31).** `?q=<term>` keeps only offerings whose model
//! `canonical_name`, `family`, `upstream_model_id`, or provider name contains
//! the term, case-insensitively. The term is trimmed; empty or absent means no
//! filter, and more than [`SEARCH_MAX_CHARS`] characters is a 400. The filter
//! runs in SQL *before* aggregation, so `scope=all` aggregates only matching
//! offerings and `scope=provider` filters that provider's offerings.
//!
//! Determinism: provider rows are ordered by `canonical_name`, then
//! `upstream_model_id`, then offering id; model rows by `canonical_name`, then
//! model id. One joined query feeds both scopes (no N+1), and the quality
//! lookup is a fixed number of set queries.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;

use super::types::parse_modalities;
use super::{CatalogError, get_provider};
use crate::benchmarks::{self, QualityDetail};
use crate::mix::{self, ComponentPrices, MixError, MixPriceView, MixProfile, MixProfileRef};

/// The longest accepted `?q=` term, in characters.
pub const SEARCH_MAX_CHARS: usize = 200;

/// Query parameters of `GET /api/models/overview`.
#[derive(Debug, Default, serde::Deserialize, ToSchema)]
pub struct OverviewQuery {
    /// A provider id, or `all`/absent for the cross-provider view.
    pub provider: Option<String>,
    /// A token-mix profile id or name; absent means the resolved default.
    pub mix: Option<String>,
    /// Case-insensitive substring over model name, family, upstream id, or
    /// provider name; absent or blank means no filter.
    pub q: Option<String>,
}

/// Response of `GET /api/models/overview`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ModelsOverview {
    /// `all` or `provider`.
    pub scope: String,
    /// The provider id when `scope = "provider"`, else `null`.
    pub provider_id: Option<String>,
    /// The token-mix profile every `mix_price` was computed with.
    pub mix: MixProfileRef,
    /// One row per offering (`scope=provider`) or per model (`scope=all`).
    pub rows: Vec<OverviewRow>,
}

/// A row of either scope; the shape is determined by the response `scope`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum OverviewRow {
    Provider(ProviderModelRow),
    All(AllModelsRow),
}

/// One offering of a provider (`scope=provider`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderModelRow {
    pub model_id: String,
    pub canonical_name: String,
    pub family: Option<String>,
    pub modalities_input: Option<Vec<String>>,
    pub modalities_output: Option<Vec<String>>,
    /// Effective context: the offering override wins over the model value.
    pub context_length: Option<i64>,
    /// Effective max output: the offering override wins over the model value.
    pub max_output_tokens: Option<i64>,
    pub offering_id: String,
    pub upstream_model_id: String,
    pub enabled: bool,
    /// `null` when the offering is unpriced.
    pub price: Option<OverviewPrice>,
    /// The blend at the selected token-mix profile; `null` when it cannot be
    /// computed.
    pub mix_price: Option<MixPriceView>,
    /// Always present for a catalog row; the neutral default is imputed.
    pub quality: Option<OverviewQuality>,
}

/// One model aggregated across providers (`scope=all`).
#[derive(Debug, Serialize, ToSchema)]
pub struct AllModelsRow {
    pub model_id: String,
    pub canonical_name: String,
    pub family: Option<String>,
    pub modalities_input: Option<Vec<String>>,
    pub modalities_output: Option<Vec<String>>,
    /// Largest effective context across the model's offerings.
    pub context_length: Option<i64>,
    /// Largest effective max output across the model's offerings.
    pub max_output_tokens: Option<i64>,
    /// Every provider that offers the model, ordered by provider name, so the
    /// page can show one chip per provider instead of a bare count.
    pub providers: Vec<OverviewProvider>,
    pub providers_count: usize,
    pub offerings_count: usize,
    /// Offerings with no price row or an all-null price row (never averaged).
    pub unpriced_offerings: usize,
    /// Offerings whose source published a variable price (the `"-1"`
    /// sentinel). Their variable components are NULL, so an all-variable
    /// offering is also counted in `unpriced_offerings`; the UI shows
    /// "variable" instead of a number for it.
    pub variable_offerings: usize,
    /// Average over priced offerings per component; `null` when unpriced.
    pub price_avg: Option<AggregatePrice>,
    /// Minimum over priced offerings per component; `null` when unpriced.
    pub price_min: Option<AggregatePrice>,
    /// Average mix price over offerings whose blend is computable.
    pub mix_price: Option<MixPriceView>,
    /// Minimum mix price over offerings whose blend is computable.
    pub mix_price_min: Option<MixPriceView>,
    /// Always present for a catalog model; the neutral default is imputed.
    pub quality: Option<OverviewQuality>,
}

/// One provider that offers a model (`scope=all`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OverviewProvider {
    pub id: String,
    pub name: String,
}

/// A single offering's price row (`scope=provider`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OverviewPrice {
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
    pub reasoning_per_m: Option<f64>,
    pub request_fee: Option<f64>,
    /// True when the source publishes a variable price (the `"-1"`
    /// sentinel): the affected components are null, and the UI shows
    /// "variable" instead of a fixed number.
    pub variable_price: bool,
    pub currency: String,
    /// `manual`, `openrouter`, `models_dev`, or `custom_api`.
    pub source: String,
    pub fetched_at: Option<String>,
}

/// Average or minimum price across a model's priced offerings (`scope=all`).
#[derive(Debug, Serialize, ToSchema)]
pub struct AggregatePrice {
    pub input_per_m: Option<f64>,
    pub output_per_m: Option<f64>,
    pub cache_read_per_m: Option<f64>,
}

/// The resolved M5 quality of a model with provenance.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OverviewQuality {
    pub score: f64,
    /// `override`, `benchmark`, `benchmark_shrunk`, or `model_default`.
    pub source: String,
    pub category: Option<String>,
    pub ci: Option<f64>,
    pub rank: Option<i64>,
    pub fetched_at: Option<String>,
    /// True when the operator's override supplied `score`.
    pub overridden: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct OverviewRowRaw {
    model_id: String,
    canonical_name: String,
    family: Option<String>,
    modalities_input: Option<String>,
    modalities_output: Option<String>,
    context_length: Option<i64>,
    max_output_tokens: Option<i64>,
    provider_id: String,
    provider_name: String,
    offering_id: String,
    upstream_model_id: String,
    context_override: Option<i64>,
    max_output_override: Option<i64>,
    enabled: i64,
    input_per_m: Option<f64>,
    output_per_m: Option<f64>,
    cache_read_per_m: Option<f64>,
    cache_write_per_m: Option<f64>,
    reasoning_per_m: Option<f64>,
    request_fee: Option<f64>,
    variable_price: Option<i64>,
    currency: Option<String>,
    source: Option<String>,
    fetched_at: Option<String>,
}

/// The joined offering/model/price shape, filtered by provider when asked.
/// `is_priced` follows the documented rule: at least one non-null component.
impl OverviewRowRaw {
    fn is_priced(&self) -> bool {
        [
            self.input_per_m,
            self.output_per_m,
            self.cache_read_per_m,
            self.cache_write_per_m,
            self.reasoning_per_m,
            self.request_fee,
        ]
        .iter()
        .any(Option::is_some)
    }
}

/// Loads the overview with an optional `?q=` search term.
///
/// The term is trimmed; blank is no filter and more than [`SEARCH_MAX_CHARS`]
/// characters is [`CatalogError::Invalid`] (a 400). The match is a
/// case-insensitive substring over the model name, family, upstream id, and
/// provider name, applied before aggregation. Failure modes are
/// [`CatalogError`]; an unknown provider or mix profile is a 404.
pub async fn load_with_search(
    pool: &SqlitePool,
    scope: Option<&str>,
    mix_selector: Option<&str>,
    env_selector: Option<&str>,
    search: Option<&str>,
) -> Result<ModelsOverview, CatalogError> {
    let search = normalize_search(search)?;
    let profile = match mix_selector
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(selector) => mix::MixProfiles::resolve_selector(pool, selector)
            .await
            .map_err(mix_error)?
            .ok_or(CatalogError::NotFound("mix profile"))?,
        None => mix::MixProfiles::resolve_default(pool, env_selector)
            .await
            .map_err(mix_error)?,
    };
    let provider_id = scope
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all");
    let mut rows = fetch_rows(pool, provider_id, search.as_deref()).await?;
    let quality = quality_map(pool, &rows).await?;
    let mix_ref = MixProfileRef::from(&profile);

    match provider_id {
        Some(id) => {
            if get_provider(pool, id).await?.is_none() {
                return Err(CatalogError::NotFound("provider"));
            }
            rows.sort_by(|a, b| {
                (&a.canonical_name, &a.upstream_model_id, &a.offering_id).cmp(&(
                    &b.canonical_name,
                    &b.upstream_model_id,
                    &b.offering_id,
                ))
            });
            Ok(ModelsOverview {
                scope: "provider".to_string(),
                provider_id: Some(id.to_string()),
                mix: mix_ref,
                rows: rows
                    .iter()
                    .map(|row| {
                        OverviewRow::Provider(ProviderModelRow {
                            model_id: row.model_id.clone(),
                            canonical_name: row.canonical_name.clone(),
                            family: row.family.clone(),
                            modalities_input: parse_modalities(row.modalities_input.as_deref()),
                            modalities_output: parse_modalities(row.modalities_output.as_deref()),
                            context_length: row.context_override.or(row.context_length),
                            max_output_tokens: row.max_output_override.or(row.max_output_tokens),
                            offering_id: row.offering_id.clone(),
                            upstream_model_id: row.upstream_model_id.clone(),
                            enabled: row.enabled != 0,
                            price: row.price_view(),
                            mix_price: row.mix_price_view(&profile),
                            quality: quality.get(&row.model_id).map(quality_view),
                        })
                    })
                    .collect(),
            })
        }
        None => {
            let grouped = aggregate(rows, &quality, &profile);
            Ok(ModelsOverview {
                scope: "all".to_string(),
                provider_id: None,
                mix: mix_ref,
                rows: grouped.into_iter().map(OverviewRow::All).collect(),
            })
        }
    }
}

/// Trims the search term and lowercases it for the substring match.
/// `None` for blank so the query skips the filter entirely; a term longer than
/// [`SEARCH_MAX_CHARS`] characters is [`CatalogError::Invalid`].
fn normalize_search(search: Option<&str>) -> Result<Option<String>, CatalogError> {
    let Some(term) = search.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if term.chars().count() > SEARCH_MAX_CHARS {
        return Err(CatalogError::Invalid(format!(
            "search query must be at most {SEARCH_MAX_CHARS} characters"
        )));
    }
    Ok(Some(term.to_lowercase()))
}

/// Flattens a mix-domain error into the catalog error so the overview uses
/// the same API mapping as every other catalog read.
fn mix_error(error: MixError) -> CatalogError {
    match error {
        MixError::Db(error) => CatalogError::Db(error),
        MixError::Audit(error) => error,
        MixError::Invalid(message) => CatalogError::Invalid(message),
        MixError::NotFound(what) => CatalogError::NotFound(what),
    }
}

async fn fetch_rows(
    pool: &SqlitePool,
    provider_id: Option<&str>,
    search: Option<&str>,
) -> Result<Vec<OverviewRowRaw>, CatalogError> {
    let rows = sqlx::query_as::<_, OverviewRowRaw>(
        "SELECT m.id AS model_id, m.canonical_name, m.family, m.modalities_input, \
                m.modalities_output, m.context_length, m.max_output_tokens, \
                o.provider_id, pr.name AS provider_name, o.id AS offering_id, o.upstream_model_id, \
                o.context_override, max_output_override, o.enabled, \
                p.input_per_m, p.output_per_m, p.cache_read_per_m, p.cache_write_per_m, \
                p.reasoning_per_m, p.request_fee, p.variable_price, p.currency, p.source, p.fetched_at \
         FROM offerings o \
         JOIN models m ON m.id = o.model_id \
         JOIN providers pr ON pr.id = o.provider_id \
         LEFT JOIN prices p ON p.offering_id = o.id \
         WHERE (? IS NULL OR o.provider_id = ?) \
           AND (? IS NULL \
                OR instr(lower(m.canonical_name), ?) > 0 \
                OR instr(lower(coalesce(m.family, '')), ?) > 0 \
                OR instr(lower(o.upstream_model_id), ?) > 0 \
                OR instr(lower(pr.name), ?) > 0) \
         ORDER BY m.canonical_name, o.upstream_model_id, o.id",
    )
    .bind(provider_id)
    .bind(provider_id)
    .bind(search)
    .bind(search)
    .bind(search)
    .bind(search)
    .bind(search)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

async fn quality_map(
    pool: &SqlitePool,
    rows: &[OverviewRowRaw],
) -> Result<HashMap<String, QualityDetail>, CatalogError> {
    let mut ids: Vec<String> = rows.iter().map(|row| row.model_id.clone()).collect();
    ids.sort();
    ids.dedup();
    benchmarks::quality_details_for_models(pool, &ids)
        .await
        .map_err(|e| match e {
            benchmarks::BenchmarkError::Db(e) => CatalogError::Db(e),
            benchmarks::BenchmarkError::Audit(e) => e,
            other => CatalogError::Invalid(other.to_string()),
        })
}

impl OverviewRowRaw {
    fn price_view(&self) -> Option<OverviewPrice> {
        let currency = self.currency.clone()?;
        Some(OverviewPrice {
            input_per_m: self.input_per_m,
            output_per_m: self.output_per_m,
            cache_read_per_m: self.cache_read_per_m,
            cache_write_per_m: self.cache_write_per_m,
            reasoning_per_m: self.reasoning_per_m,
            request_fee: self.request_fee,
            variable_price: self.variable_price.is_some_and(|value| value != 0),
            currency,
            source: self.source.clone().unwrap_or_default(),
            fetched_at: self.fetched_at.clone(),
        })
    }

    /// The offering's mix price at `profile`, or `None` when unpriced.
    fn mix_price_view(&self, profile: &MixProfile) -> Option<MixPriceView> {
        let prices = ComponentPrices {
            input_per_m: self.input_per_m,
            output_per_m: self.output_per_m,
            cache_read_per_m: self.cache_read_per_m,
            cache_write_per_m: self.cache_write_per_m,
            reasoning_per_m: self.reasoning_per_m,
        };
        mix::mix_price_from(prices, profile).map(|mix| MixPriceView::new(mix.per_m, profile))
    }
}

fn quality_view(detail: &QualityDetail) -> OverviewQuality {
    OverviewQuality {
        score: detail.score,
        source: detail.source.as_str().to_string(),
        category: detail.category.clone(),
        ci: detail.ci,
        rank: detail.rank,
        fetched_at: detail.fetched_at.clone(),
        overridden: detail.overridden,
    }
}

/// Groups offering rows by model and computes the documented aggregates.
/// The input is already sorted by canonical name; insertion order keeps the
/// output deterministic.
fn aggregate(
    rows: Vec<OverviewRowRaw>,
    quality: &HashMap<String, QualityDetail>,
    profile: &MixProfile,
) -> Vec<AllModelsRow> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<OverviewRowRaw>> = HashMap::new();
    for row in rows {
        if !groups.contains_key(&row.model_id) {
            order.push(row.model_id.clone());
        }
        groups.entry(row.model_id.clone()).or_default().push(row);
    }

    order
        .into_iter()
        .filter_map(|model_id| {
            let offerings = groups.remove(&model_id)?;
            let first = offerings.first()?;
            let priced: Vec<&OverviewRowRaw> =
                offerings.iter().filter(|row| row.is_priced()).collect();
            let mut providers: Vec<OverviewProvider> = Vec::new();
            for row in &offerings {
                if providers
                    .iter()
                    .all(|provider| provider.id != row.provider_id)
                {
                    providers.push(OverviewProvider {
                        id: row.provider_id.clone(),
                        name: row.provider_name.clone(),
                    });
                }
            }
            providers.sort_by_key(|provider| provider.name.to_lowercase());
            let providers_count = providers.len();
            let context_length = offerings
                .iter()
                .filter_map(|row| row.context_override.or(row.context_length))
                .max();
            let max_output_tokens = offerings
                .iter()
                .filter_map(|row| row.max_output_override.or(row.max_output_tokens))
                .max();
            let mix_values: Vec<f64> = priced
                .iter()
                .filter_map(|row| row.mix_price_view(profile))
                .map(|view| view.per_m)
                .collect();
            Some(AllModelsRow {
                model_id: model_id.clone(),
                canonical_name: first.canonical_name.clone(),
                family: first.family.clone(),
                modalities_input: parse_modalities(first.modalities_input.as_deref()),
                modalities_output: parse_modalities(first.modalities_output.as_deref()),
                context_length,
                max_output_tokens,
                providers,
                providers_count,
                offerings_count: offerings.len(),
                unpriced_offerings: offerings.len() - priced.len(),
                variable_offerings: offerings
                    .iter()
                    .filter(|row| row.variable_price.is_some_and(|value| value != 0))
                    .count(),
                price_avg: aggregate_price(&priced, AggregateKind::Average),
                price_min: aggregate_price(&priced, AggregateKind::Minimum),
                mix_price: average(mix_values.iter().copied())
                    .map(|per_m| MixPriceView::new(per_m, profile)),
                mix_price_min: minimum(mix_values.iter().copied())
                    .map(|per_m| MixPriceView::new(per_m, profile)),
                quality: quality.get(&model_id).map(quality_view),
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
enum AggregateKind {
    Average,
    Minimum,
}

/// Per-component average/minimum over the offerings that carry that component.
/// `None` when at least one priced offering exists but nobody priced any of
/// the three aggregated components (e.g. only a request fee is known).
fn aggregate_price(priced: &[&OverviewRowRaw], kind: AggregateKind) -> Option<AggregatePrice> {
    if priced.is_empty() {
        return None;
    }
    let (input, output, cache) = match kind {
        AggregateKind::Average => (
            average(priced.iter().filter_map(|row| row.input_per_m)),
            average(priced.iter().filter_map(|row| row.output_per_m)),
            average(priced.iter().filter_map(|row| row.cache_read_per_m)),
        ),
        AggregateKind::Minimum => (
            minimum(priced.iter().filter_map(|row| row.input_per_m)),
            minimum(priced.iter().filter_map(|row| row.output_per_m)),
            minimum(priced.iter().filter_map(|row| row.cache_read_per_m)),
        ),
    };
    if input.is_none() && output.is_none() && cache.is_none() {
        return None;
    }
    Some(AggregatePrice {
        input_per_m: input,
        output_per_m: output,
        cache_read_per_m: cache,
    })
}

fn average(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for value in values {
        sum += value;
        count += 1;
    }
    (count > 0).then(|| sum / count as f64)
}

fn minimum(values: impl Iterator<Item = f64>) -> Option<f64> {
    values.reduce(f64::min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::test_support::{TestApp, api_request, send};
    use axum::http::{Method, StatusCode};

    const NOW: &str = "2026-11-16T00:00:00.000Z";

    async fn insert_provider(app: &TestApp, id: &str, name: &str) {
        sqlx::query(
            "INSERT INTO providers (id, name, base_url, enabled, created_at, updated_at) \
             VALUES (?, ?, 'https://example.test/v1', 1, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(NOW)
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("provider");
    }

    async fn insert_offering(
        app: &TestApp,
        offering_id: &str,
        provider_id: &str,
        model_id: &str,
        upstream: &str,
        context_override: Option<i64>,
        price: Option<(Option<f64>, Option<f64>, Option<f64>)>,
    ) {
        sqlx::query(
            "INSERT INTO offerings \
               (id, provider_id, model_id, upstream_model_id, context_override, enabled, \
                created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(offering_id)
        .bind(provider_id)
        .bind(model_id)
        .bind(upstream)
        .bind(context_override)
        .bind(NOW)
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("offering");
        if let Some((input, output, cache)) = price {
            sqlx::query(
                "INSERT INTO prices \
                   (id, offering_id, input_per_m, output_per_m, cache_read_per_m, currency, \
                    source, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, 'USD', 'manual', ?, ?)",
            )
            .bind(format!("price-{offering_id}"))
            .bind(offering_id)
            .bind(input)
            .bind(output)
            .bind(cache)
            .bind(NOW)
            .bind(NOW)
            .execute(&app.state.pool)
            .await
            .expect("price");
        }
    }

    async fn insert_model(
        app: &TestApp,
        id: &str,
        name: &str,
        context: Option<i64>,
        max_output: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO models \
               (id, canonical_name, family, context_length, max_output_tokens, created_at, updated_at) \
             VALUES (?, ?, 'test-family', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(context)
        .bind(max_output)
        .bind(NOW)
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("model");
    }

    async fn fixture(app: &TestApp) {
        insert_provider(app, "prov-a", "alpha").await;
        insert_provider(app, "prov-b", "beta").await;
        insert_model(app, "model-a", "a-model", Some(128_000), Some(8_192)).await;
        insert_model(app, "model-b", "b-model", Some(64_000), None).await;
        // a-model across two providers, with one offering per provider plus a
        // third unpriced offering on the first provider.
        insert_offering(
            app,
            "off-a1",
            "prov-a",
            "model-a",
            "a-1",
            Some(200_000),
            Some((Some(1.0), Some(2.0), None)),
        )
        .await;
        insert_offering(
            app,
            "off-a2",
            "prov-b",
            "model-a",
            "a-2",
            None,
            Some((Some(3.0), None, Some(4.0))),
        )
        .await;
        insert_offering(app, "off-a3", "prov-a", "model-a", "a-3", None, None).await;
        // b-model: one unpriced offering.
        insert_offering(app, "off-b1", "prov-a", "model-b", "b-1", None, None).await;
    }

    #[tokio::test]
    async fn all_scope_aggregates_price_only_over_priced_offerings() {
        let app = TestApp::new().await;
        fixture(&app).await;
        let overview = load_with_search(&app.state.pool, None, None, None, None)
            .await
            .expect("overview");
        assert_eq!(overview.scope, "all");
        assert_eq!(overview.provider_id, None);
        assert_eq!(overview.rows.len(), 2);
        let OverviewRow::All(a) = &overview.rows[0] else {
            panic!("all scope row");
        };
        assert_eq!(a.canonical_name, "a-model");
        assert_eq!(a.context_length, Some(200_000), "max effective context");
        assert_eq!(a.max_output_tokens, Some(8_192));
        assert_eq!(a.providers_count, 2);
        assert_eq!(
            a.providers
                .iter()
                .map(|provider| provider.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"],
            "one provider entry per provider, ordered by name"
        );
        assert_eq!(a.offerings_count, 3);
        assert_eq!(a.unpriced_offerings, 1);
        let avg = a.price_avg.as_ref().expect("average");
        assert_eq!(avg.input_per_m, Some(2.0), "(1 + 3) / 2");
        assert_eq!(avg.output_per_m, Some(2.0), "only off-a1 prices output");
        assert_eq!(avg.cache_read_per_m, Some(4.0), "only off-a2 prices cache");
        let min = a.price_min.as_ref().expect("minimum");
        assert_eq!(min.input_per_m, Some(1.0));
        assert_eq!(min.output_per_m, Some(2.0));
        assert_eq!(min.cache_read_per_m, Some(4.0));

        let OverviewRow::All(b) = &overview.rows[1] else {
            panic!("second model");
        };
        assert_eq!(b.canonical_name, "b-model");
        assert_eq!(b.unpriced_offerings, 1);
        assert!(b.price_avg.is_none(), "no priced offering");
        assert!(b.price_min.is_none());
        assert!(b.mix_price.is_none());

        // The default profile is the built-in code-agent. Only off-a1 has a
        // computable blend (off-a2 has no output price, so its output share
        // is unpriceable); the average and minimum are that one offering.
        assert_eq!(overview.mix.profile_name, "code-agent");
        let mix = a.mix_price.as_ref().expect("mix price");
        assert!((mix.per_m - 1.004).abs() < 1e-9, "{}", mix.per_m);
        assert_eq!(mix.basis, "blended");
        assert!((a.mix_price_min.as_ref().expect("min").per_m - 1.004).abs() < 1e-9);
    }

    #[tokio::test]
    async fn all_scope_averages_and_minimises_computable_mix_prices() {
        let app = TestApp::new().await;
        insert_provider(&app, "prov-m", "mix").await;
        insert_model(&app, "model-m", "mix-model", None, None).await;
        // $1/$2/$1 (cache falls back to input) and $3/$5/$0.30.
        insert_offering(
            &app,
            "off-m1",
            "prov-m",
            "model-m",
            "m-1",
            None,
            Some((Some(1.0), Some(2.0), None)),
        )
        .await;
        insert_offering(
            &app,
            "off-m2",
            "prov-m",
            "model-m",
            "m-2",
            None,
            Some((Some(3.0), Some(5.0), Some(0.3))),
        )
        .await;
        // An unpriced offering is excluded from the mix aggregate too.
        insert_offering(&app, "off-m3", "prov-m", "model-m", "m-3", None, None).await;

        let overview = load_with_search(&app.state.pool, None, None, None, None)
            .await
            .expect("overview");
        let OverviewRow::All(row) = &overview.rows[0] else {
            panic!("all row");
        };
        // code-agent: 0.081*1 + 0.004*2 + 0.915*1 = 1.004 and
        // 0.081*3 + 0.004*5 + 0.915*0.3 = 0.5375.
        let average = row.mix_price.as_ref().expect("average mix");
        assert!((average.per_m - (1.004 + 0.5375) / 2.0).abs() < 1e-9);
        let minimum = row.mix_price_min.as_ref().expect("minimum mix");
        assert!((minimum.per_m - 0.5375).abs() < 1e-9);
        assert_eq!(row.unpriced_offerings, 1);
    }

    #[tokio::test]
    async fn provider_scope_mix_prices_follow_the_requested_profile() {
        let app = TestApp::new().await;
        fixture(&app).await;
        let default = load_with_search(&app.state.pool, Some("prov-a"), None, None, None)
            .await
            .expect("default");
        assert_eq!(default.mix.profile_id, "mix-code-agent");
        assert_eq!(default.mix.profile_name, "code-agent");
        let first = |overview: &ModelsOverview| -> ProviderModelRow {
            match &overview.rows[0] {
                OverviewRow::Provider(row) => row.clone(),
                OverviewRow::All(_) => panic!("provider row"),
            }
        };
        // off-a1: $1 input / $2 output, no cache price.
        let row = first(&default);
        let mix = row.mix_price.as_ref().expect("code-agent blend");
        assert!((mix.per_m - 1.004).abs() < 1e-9, "{}", mix.per_m);
        assert_eq!(mix.profile_name, "code-agent");

        let chat = load_with_search(&app.state.pool, Some("prov-a"), Some("chat"), None, None)
            .await
            .expect("chat");
        assert_eq!(chat.mix.profile_name, "chat");
        let row = first(&chat);
        let mix = row.mix_price.as_ref().expect("chat blend");
        assert!((mix.per_m - 1.26).abs() < 1e-9, "{}", mix.per_m);

        // An unknown profile is a 404, not a silent fallback.
        let error = load_with_search(&app.state.pool, Some("prov-a"), Some("missing"), None, None)
            .await
            .expect_err("unknown mix profile");
        assert!(
            matches!(error, CatalogError::NotFound("mix profile")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn provider_scope_lists_offerings_with_effective_limits() {
        let app = TestApp::new().await;
        fixture(&app).await;
        let overview = load_with_search(&app.state.pool, Some("prov-a"), None, None, None)
            .await
            .expect("overview");
        assert_eq!(overview.scope, "provider");
        assert_eq!(overview.provider_id.as_deref(), Some("prov-a"));
        let rows: Vec<&ProviderModelRow> = overview
            .rows
            .iter()
            .map(|row| match row {
                OverviewRow::Provider(row) => row,
                OverviewRow::All(_) => panic!("provider scope row"),
            })
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].offering_id, "off-a1");
        assert_eq!(rows[0].context_length, Some(200_000), "override wins");
        assert_eq!(rows[0].upstream_model_id, "a-1");
        let price = rows[0].price.as_ref().expect("priced");
        assert_eq!(price.input_per_m, Some(1.0));
        assert_eq!(price.source, "manual");
        assert_eq!(rows[1].offering_id, "off-a3");
        assert!(rows[1].price.is_none(), "unpriced offering");
        assert_eq!(rows[1].context_length, Some(128_000), "model fallback");
    }

    #[tokio::test]
    async fn quality_resolution_prefers_override_then_text_benchmark() {
        let app = TestApp::new().await;
        fixture(&app).await;
        for (category, score) in [("code", 1400.0), ("text", 1300.0)] {
            sqlx::query(
                "INSERT INTO benchmarks \
                   (id, model_id, source, category, score, ci, rank, fetched_at, created_at, updated_at) \
                 VALUES (?, 'model-a', 'lmarena', ?, ?, 10.0, 1, ?, ?, ?)",
            )
            .bind(format!("bench-{category}"))
            .bind(category)
            .bind(score)
            .bind(NOW)
            .bind(NOW)
            .bind(NOW)
            .execute(&app.state.pool)
            .await
            .expect("benchmark");
        }
        let overview = load_with_search(&app.state.pool, None, None, None, None)
            .await
            .expect("overview");
        let OverviewRow::All(a) = &overview.rows[0] else {
            panic!("first row");
        };
        let quality = a.quality.as_ref().expect("quality");
        assert_eq!(quality.source, "benchmark_shrunk");
        assert_eq!(quality.category.as_deref(), Some("text"), "text wins ties");
        assert_eq!(quality.ci, Some(10.0));
        assert_eq!(quality.rank, Some(1));
        assert_eq!(quality.fetched_at.as_deref(), Some(NOW));
        assert!(!quality.overridden);

        sqlx::query(
            "INSERT INTO model_quality_overrides (model_id, score, updated_at) \
             VALUES ('model-a', 1450.0, ?)",
        )
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("override");
        let overview = load_with_search(&app.state.pool, None, None, None, None)
            .await
            .expect("overview");
        let OverviewRow::All(a) = &overview.rows[0] else {
            panic!("first row");
        };
        let quality = a.quality.as_ref().expect("quality");
        assert_eq!(quality.source, "override");
        assert_eq!(quality.score, 1450.0);
        assert!(quality.overridden);
        assert_eq!(
            quality.category.as_deref(),
            Some("text"),
            "provenance survives an override"
        );
    }

    #[tokio::test]
    async fn empty_catalog_and_unknown_provider_are_clean() {
        let app = TestApp::new().await;
        let empty = load_with_search(&app.state.pool, None, None, None, None)
            .await
            .expect("empty overview");
        assert!(empty.rows.is_empty());
        assert_eq!(empty.scope, "all");

        let error = load_with_search(&app.state.pool, Some("missing"), None, None, None)
            .await
            .expect_err("404");
        assert!(
            matches!(error, CatalogError::NotFound("provider")),
            "{error}"
        );
    }

    async fn admin(app: &TestApp) -> (axum::Router, String, String) {
        app.create_user("admin@example.com", Role::Admin, true)
            .await;
        let router = app.router();
        let (cookie, csrf) = crate::test_support::login(&router, "admin@example.com").await;
        (router, cookie, csrf)
    }

    #[tokio::test]
    async fn overview_http_route_is_admin_only_and_deterministic() {
        let app = TestApp::new().await;
        let (router, cookie, _csrf) = admin(&app).await;
        fixture(&app).await;

        let response = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/models/overview?provider=all",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert_eq!(response.body["scope"], "all");
        let names: Vec<&str> = response.body["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| row["canonical_name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, ["a-model", "b-model"], "canonical_name order");
        assert_eq!(response.body["mix"]["profile_name"], "code-agent");
        assert_eq!(response.body["mix"]["profile_id"], "mix-code-agent");

        let chat = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/models/overview?provider=prov-a&mix=chat",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(chat.status, StatusCode::OK, "{}", chat.body);
        assert_eq!(chat.body["mix"]["profile_name"], "chat");
        assert_eq!(chat.body["rows"][0]["mix_price"]["profile_name"], "chat");
        assert_eq!(chat.body["rows"][0]["mix_price"]["basis"], "blended");

        let unknown_mix = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/models/overview?mix=missing",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(
            unknown_mix.status,
            StatusCode::NOT_FOUND,
            "{}",
            unknown_mix.body
        );
        assert_eq!(unknown_mix.body["error"]["code"], "not_found");

        let scoped = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/models/overview?provider=prov-b",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(scoped.status, StatusCode::OK, "{}", scoped.body);
        assert_eq!(scoped.body["scope"], "provider");
        assert_eq!(scoped.body["rows"].as_array().expect("rows").len(), 1);

        let missing = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/models/overview?provider=missing",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);
        assert_eq!(missing.body["error"]["code"], "not_found");

        let anonymous = send(
            router,
            api_request(Method::GET, "/api/models/overview", None, None, None),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn overview_static_route_wins_over_the_model_id_route() {
        // `/api/models/overview` must not be treated as `GET /api/models/{id}`
        // (which would answer 404 for a model literally named "overview").
        let app = TestApp::new().await;
        let (router, cookie, _csrf) = admin(&app).await;
        let response = send(
            router,
            api_request(
                Method::GET,
                "/api/models/overview",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert_eq!(response.body["scope"], "all");
        assert!(response.body["rows"].as_array().expect("rows").is_empty());
    }

    #[tokio::test]
    async fn json_shape_matches_the_documented_fields() {
        let app = TestApp::new().await;
        fixture(&app).await;
        let overview = load_with_search(&app.state.pool, Some("prov-a"), None, None, None)
            .await
            .expect("overview");
        let value = serde_json::to_value(&overview).expect("serializes");
        assert_eq!(value["scope"], "provider");
        let row = &value["rows"][0];
        for field in [
            "model_id",
            "canonical_name",
            "family",
            "modalities_input",
            "modalities_output",
            "context_length",
            "max_output_tokens",
            "offering_id",
            "upstream_model_id",
            "enabled",
            "price",
            "mix_price",
            "quality",
        ] {
            assert!(row.get(field).is_some(), "missing {field}: {row}");
        }
        assert_eq!(value["mix"]["profile_name"], "code-agent");
        let all = load_with_search(&app.state.pool, None, None, None, None)
            .await
            .expect("overview");
        let value = serde_json::to_value(&all).expect("serializes");
        assert_eq!(value["mix"]["profile_name"], "code-agent");
        let row = &value["rows"][0];
        for field in [
            "model_id",
            "canonical_name",
            "family",
            "modalities_input",
            "modalities_output",
            "context_length",
            "max_output_tokens",
            "providers",
            "providers_count",
            "offerings_count",
            "unpriced_offerings",
            "variable_offerings",
            "price_avg",
            "price_min",
            "mix_price",
            "mix_price_min",
            "quality",
        ] {
            assert!(row.get(field).is_some(), "missing {field}: {row}");
        }
    }

    #[tokio::test]
    async fn a_variable_price_is_unpriced_and_counted_separately() {
        let app = TestApp::new().await;
        insert_provider(&app, "prov-v", "vendor").await;
        insert_model(&app, "model-v", "router/auto", Some(128_000), None).await;
        insert_offering(&app, "off-v", "prov-v", "model-v", "auto", None, None).await;
        // A row as the sync writes it for a "-1" sentinel: NULL components,
        // variable_price = 1, never a minus one million number.
        sqlx::query(
            "INSERT INTO prices \
               (id, offering_id, input_per_m, output_per_m, cache_read_per_m, \
                variable_price, currency, source, created_at, updated_at) \
             VALUES ('price-v', 'off-v', NULL, NULL, NULL, 1, 'USD', 'openrouter', ?, ?)",
        )
        .bind(NOW)
        .bind(NOW)
        .execute(&app.state.pool)
        .await
        .expect("variable price");

        let overview = load_with_search(&app.state.pool, None, None, None, None)
            .await
            .expect("overview");
        let OverviewRow::All(row) = &overview.rows[0] else {
            panic!("all row");
        };
        assert!(row.price_avg.is_none(), "a variable price is not averaged");
        assert!(row.price_min.is_none(), "and never becomes the minimum");
        assert!(row.mix_price.is_none(), "and cannot feed the mix blend");
        assert_eq!(row.unpriced_offerings, 1);
        assert_eq!(row.variable_offerings, 1);

        let provider = load_with_search(&app.state.pool, Some("prov-v"), None, None, None)
            .await
            .expect("provider overview");
        let OverviewRow::Provider(row) = &provider.rows[0] else {
            panic!("provider row");
        };
        let price = row.price.as_ref().expect("price row");
        assert!(price.variable_price);
        assert!(price.input_per_m.is_none());
        let value = serde_json::to_value(&overview).expect("serializes");
        assert_eq!(value["rows"][0]["variable_offerings"], 1);
    }

    #[tokio::test]
    async fn search_matches_name_family_upstream_and_provider_case_insensitively() {
        let app = TestApp::new().await;
        fixture(&app).await;

        let names = |overview: &ModelsOverview| -> Vec<String> {
            overview
                .rows
                .iter()
                .map(|row| match row {
                    OverviewRow::All(row) => row.canonical_name.clone(),
                    OverviewRow::Provider(row) => row.canonical_name.clone(),
                })
                .collect()
        };

        // Canonical name.
        let by_name = load_with_search(&app.state.pool, None, None, None, Some("a-model"))
            .await
            .expect("by name");
        assert_eq!(names(&by_name), ["a-model"]);

        // Family: every model in the fixture shares `test-family`.
        let by_family = load_with_search(&app.state.pool, None, None, None, Some("TEST-FAMILY"))
            .await
            .expect("by family");
        assert_eq!(names(&by_family), ["a-model", "b-model"]);

        // Upstream id: only off-a2 carries `a-2`, so the all-scope row
        // aggregates that one offering.
        let by_upstream = load_with_search(&app.state.pool, None, None, None, Some("A-2"))
            .await
            .expect("upstream");
        let OverviewRow::All(row) = &by_upstream.rows[0] else {
            panic!("all row");
        };
        assert_eq!(row.canonical_name, "a-model");
        assert_eq!(row.offerings_count, 1);
        assert_eq!(row.providers_count, 1);

        // Provider name: alpha owns off-a1, off-a3, and off-b1.
        let by_provider_name = load_with_search(&app.state.pool, None, None, None, Some("Alpha"))
            .await
            .expect("by provider");
        let OverviewRow::All(a) = &by_provider_name.rows[0] else {
            panic!("all row");
        };
        assert_eq!(a.offerings_count, 2);
        let OverviewRow::All(b) = &by_provider_name.rows[1] else {
            panic!("all row");
        };
        assert_eq!(b.offerings_count, 1);

        // Whitespace is trimmed; an empty term is no filter.
        let trimmed = load_with_search(&app.state.pool, None, None, None, Some("  beta  "))
            .await
            .expect("trimmed");
        assert_eq!(names(&trimmed), ["a-model"], "beta owns only off-a2");
        let blank = load_with_search(&app.state.pool, None, None, None, Some("   "))
            .await
            .expect("blank");
        assert_eq!(names(&blank), ["a-model", "b-model"]);
    }

    #[tokio::test]
    async fn search_no_match_is_an_empty_payload() {
        let app = TestApp::new().await;
        fixture(&app).await;
        let overview = load_with_search(&app.state.pool, None, None, None, Some("nothing-here"))
            .await
            .expect("no match");
        assert_eq!(overview.scope, "all");
        assert!(overview.rows.is_empty());
    }

    #[tokio::test]
    async fn search_filters_before_aggregation_and_combines_with_provider_and_mix() {
        let app = TestApp::new().await;
        fixture(&app).await;

        // All scope with a term that matches only one offering of a-model:
        // the aggregates count that offering alone, not every offering.
        let overview = load_with_search(
            &app.state.pool,
            Some("all"),
            Some("chat"),
            None,
            Some("a-1"),
        )
        .await
        .expect("search");
        assert_eq!(overview.scope, "all");
        assert_eq!(overview.mix.profile_name, "chat");
        assert_eq!(overview.rows.len(), 1);
        let OverviewRow::All(row) = &overview.rows[0] else {
            panic!("all row");
        };
        assert_eq!(row.offerings_count, 1);
        assert_eq!(row.unpriced_offerings, 0);
        assert_eq!(
            row.price_avg.as_ref().and_then(|p| p.input_per_m),
            Some(1.0)
        );

        // Provider scope filters that provider's offerings by the same term.
        let scoped = load_with_search(
            &app.state.pool,
            Some("prov-b"),
            Some("chat"),
            None,
            Some("a-"),
        )
        .await
        .expect("scoped");
        assert_eq!(scoped.scope, "provider");
        assert_eq!(scoped.provider_id.as_deref(), Some("prov-b"));
        let OverviewRow::Provider(row) = &scoped.rows[0] else {
            panic!("provider row");
        };
        assert_eq!(row.offering_id, "off-a2");
        assert_eq!(scoped.rows.len(), 1);

        // A term that matches nothing in the scoped provider is empty, while
        // the provider itself still resolves (not a 404).
        let none = load_with_search(&app.state.pool, Some("prov-b"), None, None, Some("a-1"))
            .await
            .expect("scoped miss");
        assert_eq!(none.scope, "provider");
        assert!(none.rows.is_empty());
    }

    #[tokio::test]
    async fn search_rejects_a_term_longer_than_two_hundred_characters() {
        let app = TestApp::new().await;
        fixture(&app).await;
        let longest = "x".repeat(SEARCH_MAX_CHARS);
        load_with_search(&app.state.pool, None, None, None, Some(&longest))
            .await
            .expect("200 characters is accepted");
        let too_long = "x".repeat(SEARCH_MAX_CHARS + 1);
        let error = load_with_search(&app.state.pool, None, None, None, Some(&too_long))
            .await
            .expect_err("201 characters is rejected");
        assert!(matches!(error, CatalogError::Invalid(_)), "{error}");
    }

    #[tokio::test]
    async fn overview_route_applies_q_and_rejects_an_overlong_term() {
        let app = TestApp::new().await;
        let (router, cookie, _csrf) = admin(&app).await;
        fixture(&app).await;

        let matched = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/models/overview?q=ALPHA",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(matched.status, StatusCode::OK, "{}", matched.body);
        assert_eq!(matched.body["rows"].as_array().expect("rows").len(), 2);

        let missing = send(
            router.clone(),
            api_request(
                Method::GET,
                "/api/models/overview?provider=prov-a&q=zzz",
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(missing.status, StatusCode::OK, "{}", missing.body);
        assert!(missing.body["rows"].as_array().expect("rows").is_empty());

        let too_long = "x".repeat(SEARCH_MAX_CHARS + 1);
        let rejected = send(
            router,
            api_request(
                Method::GET,
                &format!("/api/models/overview?q={too_long}"),
                None,
                Some(&cookie),
                None,
            ),
        )
        .await;
        assert_eq!(
            rejected.status,
            StatusCode::BAD_REQUEST,
            "{}",
            rejected.body
        );
        assert_eq!(rejected.body["error"]["code"], "invalid_request");
    }
}
