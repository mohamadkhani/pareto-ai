# Token-usage mixes: what shares to ship as default "mix price" profiles

Started: 2026-11-16. Status: research complete enough to ship presets; calibration
against our own traffic is still open (see the end).

## The question

A blended "mix price" (USD per million tokens) needs three token shares:

- **input** — prompt tokens billed at the full input rate,
- **output** — completion tokens, *including* reasoning/thinking tokens (providers
  bill those as output),
- **cache** — prompt tokens served from a prompt cache (Anthropic
  `cache_read_input_tokens`, OpenAI `cached_tokens`, Gemini
  `total_cached_tokens`).

The three shares are **disjoint** and sum to 1, matching D22's non-overlapping
billing. Two things are deliberately *not* components: cache **writes** (billed
above the input rate) and reasoning tokens (a subset of output). Both distort the
model a little; both are called out below.

`llm-leaders` defines the same three-way mix and currently defaults to 3:1
input:output (75/25). The user's measured coding-agent mix is
8.1 / 0.4 / 91.5 (implied cache hit rate 91.9% of prompt tokens).

## Findings

### Q1. Input:output ratio by workload

| Claim | Number | Source | Date | What exactly it measures | Confidence |
| --- | --- | --- | --- | --- | --- |
| Chat traffic is about 3:1 input:output | **74.2% input / 25.8% output** by tokens (mean request 719, mean response 250; median per-request ratio 3.15:1) | Computed from `data/BurstGPT_1.csv` (1,429,737 requests, Azure OpenAI ChatGPT + GPT-4, 2 months); paper <https://arxiv.org/abs/2401.17644> | data 2023–24; paper v5 2025-05-26 | Summed request tokens vs response tokens of the `Conversation log` rows. Not a per-conversation cumulative figure — the trace does not say whether history is included in "Request tokens" | **High** for that population |
| API-shaped traffic is more output-sparse | **84.7% input / 15.3% output** by tokens; median per-request ratio **17:1** (median request 234, median response 27) | Same file, `API log` rows (1,275,518 requests) | same | Same caveat; the median is dominated by tiny canned responses | Medium |
| Chat is short and per-turn output-heavy | **2.0 turns**, 69.5 tokens per user prompt, 214.5 tokens per response (per turn, output ≈ 3× input) | LMSYS-Chat-1M, Table 1, <https://arxiv.org/abs/2309.11998> | 2024-03-10 | Tokenised conversation text (Llama-2 tokenizer); per-message, not cumulative | **High** (2023 traffic) |
| Coding *tasks* (few turns) are ~3:1 | prompt:completion **1.0:1 to 10.7:1**, median ≈ **3:1** across the published runs | Aider polyglot leaderboard per-run "Prompt tokens"/"Completion tokens", <https://aider.chat/docs/leaderboards/> | runs 2024-12 → 2025-10 | Tokens summed over all requests of 225 benchmark tasks per run. Includes cache reads in "prompt tokens"; cost is computed at full input price, so it is *gross* prompt volume | **High** (published per-run counts) |
| Long agent sessions are 25:1 to 330:1 | 25:1 (modelled 50-turn session: 1M input / 40k output); **83:1** (modelled 30-turn: 1,252,500 prompt / 15,000 output); **330:1** (one real month: 8.24B prompt / 25M output) | <https://www.vantage.sh/blog/agentic-coding-costs>; <https://www.morphllm.com/prompt-caching>; HN item 47933355 via the same Morph page | 2026-04-15 / 2026-09-01 | Modelled sessions (Vantage: "illustrative"; Morph: worked example) except the 330:1 case, which is a reported monthly bill | Low–Medium (2 modelled, 1 self-reported) |
| Batch summarisation output is a few percent | XSum: **median summary 23 tokens** against ~431-word documents → output ≈ **4–5%** of input | <https://arxiv.org/pdf/2504.06275>; corpus stats in <http://www.scielo.org.mx/scielo.php?script=sci_arttext&pid=S1405-55462023000401203> | 2025 | Dataset statistics, not a production trace | Medium |

Why the ratio moves so much: in a multi-turn session the prompt at turn *n* is
the prefix plus everything appended, so summed prompt tokens grow ~O(n²) while
summed output grows ~O(n). A 30-turn session gives 83:1; a 130-turn session gives
~250:1; a 3-request Aider task gives 3:1. **The input:output ratio is a function
of session depth, not of the model.**

### Q2. Cached-input share in practice

| Claim | Number | Source | Date | What exactly it measures | Confidence |
| --- | --- | --- | --- | --- | --- |
| Cache reads cost 10% of input (Anthropic), writes cost 25% more (5m TTL) or 2× (1h) | 0.1× read, 1.25× / 2× write; some 2026 models at 0.05× and 0.025× | <https://platform.claude.com/docs/en/build-with-claude/prompt-caching> | read 2026-11-16 | Published price multipliers | **High** |
| OpenAI cached input is 10% of input on current models, 50% on gpt-4.1/4o; writes 1.25× (30m) / 2.5× (12h) | e.g. gpt-5.5 $5.00 → cached $0.50 | <https://developers.openai.com/api/docs/pricing> | read 2026-11-16 | Published price table | **High** |
| Gemini cached input is 10% of input, plus storage per token-hour for explicit caches | e.g. Gemini 4 Carbon $2.00 → cached $0.10; implicit caching on 2.5+ (min 2,048 / 4,096 tokens) | <https://ai.google.dev/gemini-api/docs/pricing>, <https://ai.google.dev/gemini-api/docs/caching> | read 2026-11-16 | Published price table + cache rules | **High** |
| A well-behaved coding agent reads essentially all of its prompt from cache | Claude Code, 182-message session: **136,640 cache-read tokens vs 677 fresh input (~100% hit)**. Same backend, qwen-code: 75,072 vs 18,489 (**80%**), 82,048 vs 15,836 (**84%**) | <https://github.com/QwenLM/qwen-code/issues/5942> | 2026-06-27 | Captured request/response bodies through a proxy, one session per client. Hit rate here is *cache reads ÷ prompt tokens*, not share of the whole bill | **High** (small n, but real bodies) |
| Anthropic's own docs quote ~91% as an example | Two adjacent examples in the same page: a token line `1.2k input, 5.3k output, 940.0k cache read, 50.0k cache write` (→ 99.9% hit rate on the numbers as printed), and a cache line saying `91% of input tokens from cache` | <https://code.claude.com/docs/en/costs> | read 2026-11-16 | Illustrative sample output in vendor docs. The two blocks do not come from the same session, so neither is a measurement — only the shape (cache reads dwarf everything else) is usable | Low–Medium |
| In general production traffic, caching is mostly *not* happening | only **28%** of LLM call spans show any cached-read tokens, even restricted to cache-capable models; **69%** of all input tokens are system prompts | Datadog, *State of AI Engineering 2026*, <https://www.datadoghq.com/state-of-ai-engineering/> | 2026 (March 2026 data) | 1,000+ customer telemetry; tokens approximated as characters ÷ 4; "cached" = any span with >0 cached-read tokens | **High** (for the population) |
| A real month of agent work with *partially* working caching | 6.47B uncached input, 1.67B cache read, 101M cache write, 25M output → **cache 20.2% of tokens**, output 0.3% | HN item 47933355, tabulated at <https://www.morphllm.com/prompt-caching> | 2026-09-01 (page) | Self-reported monthly bill across an agent client, proxy library, Bedrock and Opus 4.6 | Medium |
| Vendor-measured saving depends on the shape | −90% (100k-token cached prompt), −86% (10k many-shot), **−53%** (10-turn multi-turn conversation) | <https://www.anthropic.com/news/prompt-caching> | 2024-08-14 (page re-dated 2025-08-14) | Vendor case-study cost deltas, not hit rates | **High** for the deltas |
| Third-party agent-session savings cluster at 40–80% | PwC: 41.4–79.6% across 4 models over 500 DeepResearch sessions; LangChain: 49–80% | cited in <https://www.morphllm.com/prompt-caching> | 2026-09-01 | Cost reduction, not token shares; secondary citation | Low–Medium |

**Two different numbers are routinely conflated**, and the difference matters:

- *cache hit rate* = cache ÷ (cache + fresh input). This is what the
  Claude Code and qwen-code rows above report, and what provider dashboards show.
- *cache share of all tokens* = cache ÷ (cache + fresh input + output). This is
  the `cache` component of a mix.

With output at 0.4% of tokens the two are nearly identical: the user's 91.5%
cache *share of all tokens* corresponds to a 91.9% *hit rate*. That is why a
single number can serve both purposes for `code-agent` — and why reusing it for
`chat` would be a mistake (26% output) even at the same hit rate.

### Q3. Do reasoning models shift the output side?

| Claim | Number | Source | Date | What exactly it measures | Confidence |
| --- | --- | --- | --- | --- | --- |
| Reasoning tokens are billed as output | rule, not a ratio | <https://developers.openai.com/api/docs/guides/reasoning>; Gemini pricing lists "Output price (including thinking tokens)" | read 2026-11-16 | Provider billing rules | **High** |
| Reasoning can dominate billed output | OpenAI's own example: `output_tokens 1186`, `reasoning_tokens 1024` → **86%** of output is reasoning | same OpenAI reasoning page | read 2026-11-16 | One illustrative hard task (a bash transpose script) | Low (single example) |
| High effort multiplies output tokens 2–3.4× on the same coding benchmark | gpt-5 on 225 Aider tasks: completion **779,568 (low) → 1,468,799 (medium) → 2,623,429 (high)**. Claude Sonnet 4: **433,373 (no thinking) → 1,271,074 (32k thinking)** | <https://aider.chat/docs/leaderboards/> | 2025-08 / 2025-05 | Same benchmark, same harness, only effort differs. Implies reasoning ≈ **66–70% of billed completion tokens** at high effort | Medium (derived, not reported) |
| Thinking can also *replace* verbose output | Claude Opus 4 on the same benchmark: 380,717 completion tokens without thinking vs 363,142 with 32k thinking | same | 2025-05-25 | Pass rates differed (70.7% vs 72.0%), so not a clean control | Low |

Consequence for pricing: a reasoning model's output line can be 2–3× larger for
the same task. At a 5× output/input price ratio, moving the output share from
0.4% to 1.2% raises the `code-agent` blend by about **20%** ($0.578 → $0.692 on
the reference model below) — not dominant, but not negligible either. For `chat`
(26% output) the same multiplication would be the single largest term.

### Q4. Is 8.1 / 0.4 / 91.5 representative?

Three independent shapes agree on the *output* side, and disagree on the
input/cache split:

| Data point | input | output | cache | Implied hit rate |
| --- | --- | --- | --- | --- |
| User's mix (`--mix input=8.1,output=0.4,cache=91.5`) | 8.1% | 0.4% | 91.5% | 91.9% |
| Anthropic docs example (1.2k / 5.3k / 940k + 50k write) | 0.12% | 0.53% | 94.3% (write 5.0%) | 99.9% |
| One real month, partial cache (HN) | 78.3% | 0.3% | 20.2% (write 1.2%) | 20.5% |
| Claude Code 182-message session (hit rate only) | — | — | — | ~100% |
| qwen-code, same backend (hit rate only) | — | — | — | 80–84% |

Verdict: **the output share (~0.3–0.5%) is robust; the input/cache split is a
property of the client, not the workload.** 91.5% cache is the well-behaved end
of a range whose other end is ~20%. It is representative of a long, cache-warm,
session-affine agent (Claude Code), not of "coding traffic" in general — and
definitely not of a gateway that re-picks an offering per turn, because each
model switch re-prefills the whole context.

Why this matters numerically. Same model at $3.00 input / $15.00 output /
$0.30 cached:

| Assumed mix | Blended $/M | vs `code-agent` |
| --- | ---: | ---: |
| `code-agent` 8.1 / 0.4 / 91.5 | **$0.578** | 1.0× |
| half the hits 49.7 / 0.3 / 50.0 | $1.686 | 2.9× |
| partial cache (HN) 78.0 / 0.3 / 20.0 | $2.445 | 4.2× |
| `code-agent-nocache` 99.6 / 0.4 / 0 | $3.048 | 5.3× |
| `long-context` 97.0 / 3.0 / 0 | $3.360 | 5.8× |
| `chat` 74.0 / 26.0 / 0 | $6.120 | 10.6× |

So the mix is not a cosmetic detail: it reorders models. A model with a cheap
input price and no cache wins on `chat` and loses by 5× on `code-agent`.

### Q5. How cache-less providers distort comparisons

The rule that keeps the comparison honest:

```
blended = (w_input + w_cache) * input_price      # no cache price published
        +  w_output          * output_price
```

i.e. the cache share is billed at the **input** price, not dropped and not
renormalised. `llm-leaders` already does exactly this (`cache_read` falls back to
`input`), and the identity means `code-agent` on a cache-less provider costs the
same as `code-agent-nocache` — which is the point: the provider really does
charge full price for those tokens.

What breaks if you get this wrong:

1. **Renormalising the mix when a cache price is missing.** Dropping the 91.5%
   cache component and rescaling input/output to 95/5 makes a cache-less provider
   look ~5× cheaper than it is. This is the most common error and the reason the
   fallback must be in the price function, not in the mix.
2. **Providers that publish a cached price that never gets realised.** Resellers
   and some OpenAI-compatible endpoints advertise a cached-input rate but never
   report cached tokens (or the client never sends `cache_control` /
   `prompt_cache_key`). The price table then promises a discount the bill does not
   contain. Mitigation: compute cost from *reported* `usage` components when they
   exist; use a preset only for ranking and planning.
3. **Different definitions of `prompt_tokens`.** Anthropic's `input_tokens`
   excludes both cache reads and cache writes; OpenAI's `prompt_tokens` includes
   cached tokens; Gemini reports `total_cached_tokens` separately. A mix computed
   from one convention is wrong under another. Normalise before computing shares:
   `total_prompt = input + cache_read + cache_creation`.
4. **Cache writes are invisible in a three-component mix.** Writes are 1.25×
   (Anthropic 5m, OpenAI 30m) to 2.5× (OpenAI 12h) input, and are a large share
   of prompt tokens on short sessions. Observed: 1.2% of tokens in a long session,
   but 100% of prompt tokens on every first request. A `code-agent` blend assumes
   a warm cache; a 1–2-turn workload pays the surcharge instead of earning the
   discount and can end up **worse than not caching**.
5. **Nonlinear long-context tiers.** OpenAI bills >272k-token requests at 2× input
   / 1.5× output; a single per-million input price is wrong for `long-context`.
   Use the long-context tier's price with that preset.
6. **Batch / flex tiers are orthogonal.** A 50% batch discount stacks with the
   cache discount. Do not compare a batch price against a non-batch mix.

## What this means for defaults

Four presets. `code-agent` is the default because the gateway serves a coding
team; the other three exist so that a report can say which assumption it used.

| Preset | input | output | cache | Intended workload | Rationale | Confidence |
| --- | ---: | ---: | ---: | --- | --- | --- |
| **`code-agent`** (default) | **8.1** | **0.4** | **91.5** | Interactive agentic CLI sessions (Claude Code, Codex CLI, OpenCode) with a working prompt cache and stable session→offering routing | The user's measured mix. Output share is corroborated by the Anthropic docs example (0.53%) and a real monthly bill (0.3%); the 91.9% hit rate sits between qwen-code's measured 80–84% and Claude Code's ~100% | Medium |
| `code-agent-nocache` | 99.6 | 0.4 | 0 | The same sessions when the cache will not hit: provider without cached pricing, no `prompt_cache_key`/session affinity, or a client that never sends `cache_control` | Same token shape with the cache component folded into input. Datadog found only 28% of production calls have any cached reads, so this is the pessimistic bound, and it is the honest number for a router that re-picks a model per turn | Medium–High |
| `chat` | 74.0 | 26.0 | 0 | Short multi-turn assistants, plus one-shot coding tasks (Aider/Copilot-style) | BurstGPT conversation logs: 74.2 / 25.8 by tokens over 1.4M real requests. Also matches the existing 75/25 default, so this preset is a rename rather than a change | **High** |
| `long-context` | 97.0 | 3.0 | 0 | One-shot long-document analysis and batch summarisation: the document is the input, the answer is short | XSum summaries are ~4–5% of input length; a 100k-token document with a 1–2k-token answer is ~1–2%. Output price barely matters, which is the useful signal | Medium |

Notes on the choices:

- **Keep 8.1 / 0.4 / 91.5 as `code-agent`.** Nothing in the evidence contradicts
  it; two independent shapes land within a factor of ~1.3 on the output share.
  The risk is not that it is wrong for Claude-Code-like sessions, it is that it is
  optimistic for anything else — which is what `code-agent-nocache` is for.
- **`chat` should replace 75/25 as the "generic" default** if a workload-agnostic
  number is needed; it is the only preset backed by a million-request trace.
- **Do not ship a preset for repeated Q&A over one long document.** It is
  `code-agent`-shaped (high cache) with output ~2%, not `long-context`-shaped.
  If that workload shows up in our telemetry, add `doc-qa` rather than stretching
  `long-context`.
- **Store the mix on the thing being reported**, not in a global setting. Two
  blended prices computed with different mixes are not comparable, and the report
  row should carry its mix for provenance (the same argument as price-source
  provenance in D4).
- Ranking implication for routing: `cheapest` under `code-agent` rewards a cheap
  *cached* rate, under `chat` a cheap *output* rate. If routing uses a mix, the
  mix belongs to the routing preset, so that "cheapest" means one thing at a time.

## What we still don't know

1. **Nothing here is our traffic.** The strongest coding-agent numbers are one
   user's mix, one 182-message session, and one self-reported monthly bill. A
   multi-user telemetry study of coding agents does not appear to exist publicly.
2. **Whether the output share holds for reasoning-heavy coding.** The 0.3–0.5%
   output figures come from sessions where thinking was a modest part of output.
   With high effort everywhere, output can be 2–3.4× larger; at 1.2% output the
   blend rises about 20%, so this is a second-order risk to the default but a
   first-order one for `chat`.
3. **The cache share distribution, not its endpoints.** We know 20% and ~100%
   occur. We do not know the median for a proxied fleet, which is what a gateway
   default should encode if it is used for ranking rather than per-request cost.
4. **Whether our gateway will break cache hits.** Session affinity, per-turn model
   choice, and tool-schema drift each destroy prefixes. This is testable and worth
   testing: same session routed twice, compare reported cached tokens.
5. **Cache writes.** The three-component model cannot represent them. If our
   traffic is many short sessions, the write surcharge may dominate and the
   `code-agent` blend will be systematically low.
6. **Long-context tier pricing.** Whether to model the >272k tier as a separate
   offering or as a multiplier on the input price is an open modelling decision,
   not a data question.

### Calibrating from our own `usage_events`

The data needed is already in the shape D22 assumes. Per request, per offering,
tagged with client/app and a conversation id:

```
prompt_tokens, cache_read_tokens, cache_creation_tokens,
completion_tokens, reasoning_tokens
```

Then:

1. **Recompute the mix from sums, not from averages of ratios.**
   `input = Σ(prompt − cache_read)`, `output = Σ completion`,
   `cache = Σ cache_read`, each divided by the total (after normalising
   separately-reported components into D22's subset convention). Summing first
   weights long sessions correctly; averaging per-request ratios lets a thousand
   200-token calls outvote one 130k-token session.
2. **Report per session, then p50 / p90.** The distribution is heavy-tailed; a
   single global share hides the fact that a small share of sessions carries most
   of the spend.
3. **Split by client.** Claude Code, Codex CLI, a chat UI and a batch job should
   not share a preset; if their mixes differ by more than ~2× on the output
   component, that is four presets, not one.
4. **Measure the cache hit rate as a first-class metric**, per offering, and alert
   when it drops. Datadog's 28% shows that a low hit rate is the normal failure
   mode, and Morph documents five mechanical causes (dynamic prefix content,
   per-turn model switching, side queries with a different prefix, TTL expiry
   during long tool calls, worker churn without a session key).
5. **Re-derive quarterly.** Anthropic's 2026 cache multipliers already spread from
   0.025× to 0.1× across models; a single "cache share" is only stable while the
   price table behind it is.
