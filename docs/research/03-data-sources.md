# Data sources: price, speed, quality

The routing feature is only as good as its inputs. This file records what each
input actually measures and where it can be obtained.

## 1. The distinction that drives the schema

- **Model quality** is a property of the model (e.g. `claude-sonnet-4.5`).
  Benchmarks and Elo measure this.
- **Price, speed, context, reliability** are properties of the
  **provider×model offering** (e.g. the same model on Provider A vs B).
  They must be stored per offering, with a timestamp and a source.

Conflating the two produces a router that compares a benchmark score for a model
against a price for a different provider's hosting of it.

## 2. Price

### OpenRouter (primary, verified in code)

- Endpoint: `GET https://openrouter.ai/api/v1/models` (public).
- Fields used by OmniRoute: `id`, `name`, `context_length`, `pricing.prompt`,
  `pricing.completion`, `pricing.image`, `pricing.request`,
  `top_provider.max_completion_tokens`, `architecture.modality`,
  `architecture.input_modalities`, `architecture.output_modalities`,
  `supported_parameters`.
- **Model identity (D60).** `hugging_face_id` is the exact open-weight
  repository id (185 of 476 entries at the 2026-11-22 read) and is the
  secondary registry anchor; the price sync refreshes it. `canonical_slug`
  groups OpenRouter's own variants but is not used for catalog identity because
  it would also collapse dated snapshots.
- Prices are strings, normally USD per token. Normalize to USD per 1M tokens and
  keep the raw value.
- Caching policy in OmniRoute: 24h TTL, persisted JSON, stale-if-error.
- Limitation: it is OpenRouter's price for OpenRouter's routing, not the price
  the same model costs on the provider directly. It is a good market reference
  and a good default when the provider has no public price API.

### models.dev (primary, verified in code)

- Consumed by `src/lib/modelsDevSync/transform.ts`.
- Provides per-provider model metadata: cost (`input`, `output`, `reasoning`,
  `cache_read`, `cache_write`, `input_audio`, `output_audio`), limits
  (`context`, `input`, `output`), modalities, tool call, structured output,
  reasoning options, knowledge cutoff, release date, open weights, status.
- OmniRoute keeps a mapping table from models.dev provider IDs to its own IDs
  because naming drifts. That table is worth reusing as data.
- **Model identity (D60).** Each model entry may carry `canonical_model_id`, a
  grouping key shared across providers (8,616 of 10,021 entries at the
  2026-11-22 read). The gateway stores provider id -> canonical id in
  `model_registry` and uses it before spelling normalization.
- **UNVERIFIED:** exact license of the models.dev dataset and its API terms.
  Check the upstream repository and site before shipping derived data.

### Direct provider APIs

Most OpenAI-compatible providers do not expose prices. Where they do, add an
adapter with a declared extraction rule and a schema version.

### Manual override

Always allow an admin to override the price for an offering. Record
`source = manual`, `updated_by`, and `updated_at`, and prefer manual values over
synced ones until the override is cleared.

## 3. Speed

Speed must be measured, not imported, because it depends on the network path,
the credential's quota tier, the time of day, and the specific deployment.

Passive measurement (recommended):

- Record per successful request: TTFT, total latency, output tokens, tokens/sec,
  and whether the stream completed.
- Keep a bounded rolling window per offering (e.g. last 200 samples) and
  compute p50/p95, mean, and standard deviation.
- Record failures and error classes separately; a fast provider that fails 20%
  of the time is not fast.
- Bucket by hour-of-day later if traffic patterns justify it.

Active probes (optional, off by default):

- A tiny fixed prompt against each offering on a schedule.
- Costs money and pollutes usage reports; make it explicit and separable.

Caveats:

- TTFT is dominated by queueing and prompt length. Normalize by input size or
  compare only within similar request shapes.
- Tokens/sec from providers is often approximate. Prefer
  `output_tokens / (last_chunk_time - first_token_time)` from the stream.
- Network location matters: measurements are only valid for this gateway's
  deployment region.

OmniRoute's `autoCombo/speedRanking.ts` is a useful reference formula:
weighted TTFT, TPS, end-to-end, p95, health, reliability, and latency stability,
with missing metrics treated as the pool median. Default weights: TTFT 0.25,
TPS 0.20, E2E 0.18, p95 0.12, health 0.05, reliability 0.15, stability 0.05.

## 4. Quality and benchmarks

### LMArena / Arena Elo

- OmniRoute syncs from `https://api.wulong.dev/arena-ai-leaderboards/v1/leaderboard`
  (categories `text`, `code`) with a Hugging Face dataset fallback
  (`arenaEloHfSource.ts`). Entries carry rank, model, vendor, score, confidence
  interval, votes, and license.
- It maps `text` to default/review/documentation/debugging and `code` to coding.
- **UNVERIFIED:** the license and stability of the wulong endpoint and the exact
  Hugging Face dataset used; both must be checked before production use.
- Better long-term source: the official LMArena leaderboard dataset on Hugging
  Face, pinned by revision, so results are reproducible.

### Artificial Analysis

- Provides quality, speed, and price indices per model; has an API.
- **UNVERIFIED:** current API tiers, price, and redistribution terms. Treat as a
  paid option and confirm before depending on it.

### Provider-published evals

Vendor benchmark tables are marketing material. They can be stored as a labeled
source but should not dominate the score.

### Own evaluations

The most trustworthy signal for a team is your own traffic: success rate,
retry rate, user feedback, and task-specific acceptance. Start collecting these
from day one even if the scoring does not use them yet.

## 5. Normalization rules

1. Store every metric with `source`, `source_url`, `fetched_at`, and `revision`
   (where the source provides one).
2. Keep raw values and normalized values separately.
3. Never overwrite a manual override with a sync.
4. Reject data older than a configured staleness limit for routing decisions;
   fall back to a neutral score instead of a stale one.
5. Benchmark scores are model-level. When an offering has no data, inherit the
   model score and mark it inherited.
6. Confidence matters: an Elo with a wide confidence interval should be
   shrunk toward the mean rather than trusted at face value.

## 6. Legal and operational notes

- Respect each source's rate limits and terms; cache aggressively.
- Do not resell benchmark data unless the license allows it.
- Provider terms may restrict reselling access. For an internal team gateway
  this is usually fine, but confirm per provider.
- Keep a provenance page in the UI ("price from OpenRouter, fetched 2h ago") so
  users can judge the numbers.
