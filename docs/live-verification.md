# Live provider verification

The build network cannot reach `openrouter.ai` or `api.sailresearch.com`
(checked 2026-11-16/17: `curl` returns `000` / timeout). Everything about these
providers is therefore proven against fixtures and a scripted upstream, and the
live path is an explicit pending item.

This page describes how to close it on a machine with network access.

## What is already proven without network

- OpenAI-compatible streaming, timeouts, cancellation, failover, usage
  accounting, and the ledger: scripted upstream (`server/src/upstream/stub.rs`).
- OpenRouter catalog/price parsing: recorded fixture
  (`server/tests/fixtures/openrouter_models.json`).
- Benchmark sources: Arena via the HF datasets API (runs live from this host),
  BenchmarkList (shape changed, currently yields no scores), BenchLM (optional).
- Discovery, overview, combos, mix pricing, budgets: integration tests + a live
  gateway with a mock upstream.

## What needs network

1. OpenRouter: discovery (`GET /api/v1/models`), price sync, and a real chat
   completion.
2. SailResearch: discovery and a real chat completion.
3. Cost accuracy against a real provider's reported usage.

## How to run it

```bash
# 1. Start the gateway (any host with network access)
cd server
export GATEWAY_DATA_DIR=./data
export GATEWAY_MASTER_KEY=$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=')
export GATEWAY_ADMIN_EMAIL=admin@example.com
export GATEWAY_ADMIN_PASSWORD='<a strong password>'
cargo run --release

# 2. Log in once in the UI and complete the forced password change
#    (the script refuses to run while the rotation flag is set).

# 3. Run the smoke test with keys from the environment only
export ADMIN_PASSWORD='<the password you set>'
export OPENROUTER_API_KEY='...'
export SAIL_API_KEY='...'
just live-smoke
```

The script (`scripts/live-smoke.sh`) logs in with CSRF, creates or reuses one
provider + credential per vendor, runs discovery in preview then apply mode,
prints the discovered model candidates, sends one non-streaming and one
streaming `POST /v1/chat/completions`, and finally prints the last request-log
rows so you can compare `cost_usd`, tokens and `error_class`.

Useful overrides: `GATEWAY_URL`, `OPENROUTER_BASE`, `SAIL_BASE`,
`OR_MODEL`, `SAIL_MODEL`.

## What to check by hand afterwards

- `/models`: the discovered offerings appear with prices (after a price sync)
  and quality (`models` page → quality column after a benchmark sync).
- `/logs`: open the decision panel for the two requests — provider, score
  components, mix price and `tokens_estimated` should be sensible.
- `/usage`: cost for the requests is non-zero and matches the provider's
  dashboard within rounding.
- `/providers`: the credential "Test" action passes for both vendors.

## Known catalog repairs (documented, not run automatically)

- **Variable prices.** OpenRouter publishes `pricing.prompt`/`completion` as
  `"-1"` for router models (`openrouter/auto`, `nvidia/switchyard`, six more).
  That is a sentinel, not a price: since 2026-11-17 the sync stores the
  component as NULL and sets `prices.variable_price`; an offering whose
  components are all variable is unpriced for routing and shown as `variable`
  in the UI (D37). `0012_variable_price.sql` repairs rows written before the
  fix (it nulls every negative component and sets the flag).
- **Duplicate models across providers.** Discovery and the price sync resolve
  an upstream id by exact name (ignoring ASCII case), then by a unique external
  canonical id from `model_registry` (models.dev `canonical_model_id`, then
  OpenRouter `hugging_face_id`), then by a unique spelling identity (D60). Run
  `gateway sync-model-registry --data-dir <dir>` after adding a provider; a
  price sync also refreshes the OpenRouter layer. Check
  `SELECT source, alias, canonical FROM model_registry` and then the Models
  page: `deepseek-ai/DeepSeek-V4.1-Flash-1017` (Sail) and
  `deepseek/deepseek-v4.1-flash-1017` (OpenRouter) should be one row with two
  provider chips. The registry affects current and future writes only. A
  one-time repair at startup merges pre-existing rows when their spellings
  normalize alike: the oldest row is kept, references are repointed, and
  conflicting benchmarks or overrides keep the keeper's value. The run is
  guarded by `db_meta.model_identity_repair_v1`; the older manual cleanup SQL in
  the header of `0013_discovery_state.sql` is no longer needed for these pairs.
  A malformed registry payload is rejected before any delete, so the previous
  rows survive; a payload that would shrink a source by more than half is also
  refused and the old rows stay. A missing registry only loses the extra join,
  never discovery.

## Security

Keys must come from the environment; the script never writes them to disk or
logs. Any key pasted into a chat, ticket, or commit message should be rotated
once the test is done.
