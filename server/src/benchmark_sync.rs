//! Automatic benchmark download (decision D31).
//!
//! Quality scores arrive from three public sources instead of a manual import:
//!
//! | Source | Endpoint | Category | Native scale |
//! | --- | --- | --- | --- |
//! | [`SourceId::Arena`] | Hugging Face rows API for `lmarena-ai/leaderboard-dataset`, config `webdev` | `code` | Elo (`elo`) |
//! | [`SourceId::BenchmarkList`] | `benchmarklist.com/directory-data/models.json` | `text` | anchored capability (`capability`) |
//! | [`SourceId::BenchLm`] | `benchlm.ai/_next/data/{build_id}/models.json` | `text` | percentage (`percent`) |
//!
//! Arena's WebDev leaderboard is CC BY 4.0: every stored row keeps the
//! dataset URL in `benchmarks.source_url`, and the admin UI must show it as
//! attribution. BenchmarkList and BenchLM rows carry their site URL.
//!
//! **Scale boundary (D25).** Routing quality lives on one Elo-like scale.
//! Only `scale = 'elo'` rows feed
//! [`crate::benchmarks::quality_for_models`]; the capability and percentage
//! sources are downloaded, matched, stored with their own `scale`, and shown
//! in the UI, but they are never mixed into the Elo ranking. A later decision
//! can calibrate them onto the Elo scale; guessing a mapping here would
//! silently corrupt ranking.
//!
//! **Pinned field names (inspected live 2026-11-16).**
//! * Arena rows API: each item is `{"row_idx", "row": {...}}`; the row has
//!   `model_name`, `organization`, `license`, `rating`, `rating_lower`,
//!   `rating_upper`, `variance`, `vote_count`, `rank`, `category`,
//!   `leaderboard_publish_date`. Only `category == "overall"` rows are used,
//!   `score` is `rating`, and `ci` is `(rating_upper - rating_lower) / 2`.
//!   The dataset holds 676 rows (seven pages of 100); the download is capped
//!   at [`ARENA_MAX_PAGES`] pages, i.e. the top of the leaderboard.
//! * BenchmarkList `directory-data/models.json` is `{"rows": [...]}`; the
//!   published rows currently carry identity metadata only (`id`, `name`, …).
//!   A row contributes when it also carries `capability_score` (0–100, a
//!   numeric string or `capability.score` is tolerated) and optionally
//!   `capability_rank`; unknown fields are ignored. `published` counts every
//!   row seen and `unscored` the rows with no score, which are never guessed
//!   from another column.
//! * BenchLM's page HTML carries `"buildId":"…"`; the data document is
//!   `{"pageProps":{"directory":{"rows":[[name, slug, …, score, …, rank, …]]}}}`.
//!   The positional layout pinned on 2026-11-16 is `[0]` name, `[1]` slug,
//!   `[6]` score, `[9]` rank. This source is optional and brittle: any shape
//!   change fails that one source, is recorded as `status = 'error'`, and
//!   never stops the other sources.
//!
//! **Fetching.** Every request identifies the gateway (`pareto-ai/<version>`),
//! asks for JSON with identity encoding, and is retried up to
//! [`FETCH_ATTEMPTS`] times with exponential backoff (0.5 s, 1 s, 2 s, plus
//! jitter) on transport errors, timeouts, body decode failures, 5xx and 429;
//! the retries downgrade to HTTP/1.1 then HTTP/1.0 with `Connection: close`.
//! A body is read chunk by chunk under [`MAX_BODY_BYTES`], and a body that
//! ended early is still used when it is a complete JSON document: the recorded
//! BenchmarkList failure reached a 2xx, transferred for seconds and then broke
//! while decoding the body, which is the shape of a dropped chunk terminator
//! (curl writes the file and exits non-zero). A paged source that cannot read
//! a page after every retry fails with [`SyncError::Page`], never a silent
//! truncation.
//!
//! **Matching (D38).** Both sides are normalized
//! ([`crate::model_identity::normalize_model_key`]): lower case, vendor prefix
//! (`vendor/`) dropped, non-alphanumerics collapsed to `-`, parenthetical tier
//! markers removed. The *identity* used for matching then drops trailing tier
//! markers (`-latest`, `-preview`, `-thinking`, `-nothinking`,
//! `-xhigh|-high|-medium|-low`), trailing date and build stamps (`-1017`,
//! `-0731`, `-20250807`, `-2025-08-07`), and a leading vendor token from the
//! hyphen shape BenchmarkList publishes (`openai-gpt-5.4`). Vendor spellings
//! are aligned through [`crate::model_identity::canonical_vendor`]
//! (`deepseek-ai`↔`deepseek`, `z-ai`↔`zai-org`,
//! `x-ai`↔`xai`, `moonshotai`↔`moonshot`, `google-ai`↔`google`) for matching
//! only; stored names are never rewritten. `:free`/`:batch` stay distinct
//! offerings.
//!
//! A hint resolves to **every catalog model that shares its identity** — one
//! benchmark row per model, because a published score is model-level and two
//! providers of the same model both deserve it. Zero models is
//! `skipped_unknown`, and more than one *distinct* candidate (candidates that
//! differ by more than a vendor or date suffix) is `ambiguous` — never
//! guessed. A generic tail (`flash`, `pro`, `auto`) is skipped unless an exact
//! full id matches. Two entries of one run resolving to the same model keep
//! the better rank.
//!
//! When the external identity registry has rows, the resolved set is widened
//! by [`crate::model_identity::ModelIndex::with_registry`] to every catalog
//! row sharing a registry canonical key (D38 amendment); a source that reports
//! several keys for one alias is skipped, and an absent registry leaves the
//! spelling behaviour untouched.
//!
//! **Run log.** `benchmark_sync_runs` holds one row per source per attempt
//! with status, counts, duration, a secret-free `error`, and `source_url`
//! (pruned to [`MAX_RUNS`] per source, the D19 pattern). **Scheduling.** The
//! supervisor runs [`spawn_sync_job`] at boot after [`JOB_START_DELAY`]; every
//! [`JOB_INTERVAL`] it syncs only sources whose last success is older than
//! [`SYNC_TTL`] (5 h). The job and the admin trigger share
//! `AppState::benchmark_sync`, a mutex, so two runs never overlap.
//!
//! Network access is isolated behind the [`Fetcher`] trait; tests drive every
//! source from recorded fixtures under `server/tests/fixtures/benchmarks/`.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sqlx::{SqliteConnection, SqlitePool};

use crate::catalog::{self, Actor};
use crate::model_identity::{Matching, ModelIndex};

/// Freshness window of one source's downloaded scores.
pub const SYNC_TTL: Duration = Duration::from_secs(5 * 60 * 60);

/// Hard cap for one source (all its HTTP requests and parsing together).
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(60);

/// Delay before the first background sync after boot.
pub const JOB_START_DELAY: Duration = Duration::from_secs(60);

/// How often the background job checks which sources are due.
pub const JOB_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Per-HTTP-request timeout (a source can issue several requests). Three
/// attempts of this plus the backoff fit inside [`SOURCE_TIMEOUT`], so a page
/// that keeps failing is reported as a failed page rather than as a source
/// timeout.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Attempts per HTTP request, including the first. The Arena rows API
/// returns 502/503 under load and sends early-closed bodies, so a request
/// gets two more tries before its page counts as failed.
const FETCH_ATTEMPTS: usize = 3;

/// First retry delay; each later attempt doubles it (0.5 s, 1 s, 2 s) with
/// up to 25% jitter. The whole source still runs under [`SOURCE_TIMEOUT`].
const RETRY_BASE_DELAY: Duration = Duration::from_millis(500);

/// Upper bound on one response body. The BenchmarkList document is ~11 MiB;
/// the cap only stops a runaway transfer.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Runs kept per source before pruning (D19).
const MAX_RUNS: i64 = 30;

/// Arena rows endpoint; `offset` and `length` are appended per page.
pub const ARENA_ROWS_URL: &str = "https://datasets-server.huggingface.co/rows?\
dataset=lmarena-ai%2Fleaderboard-dataset&config=webdev&split=latest";
/// Arena dataset page, recorded as the CC BY 4.0 attribution for every row.
pub const ARENA_ATTRIBUTION_URL: &str =
    "https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset";
/// Arena page size accepted by the rows API.
pub const ARENA_PAGE_SIZE: usize = 100;
/// Page cap: 300 rows (the top of the WebDev leaderboard) per sync.
pub const ARENA_MAX_PAGES: usize = 3;

/// BenchmarkList's model directory.
pub const BENCHMARKLIST_URL: &str = "https://benchmarklist.com/directory-data/models.json";
/// BenchmarkList attribution target.
pub const BENCHMARKLIST_ATTRIBUTION_URL: &str = "https://benchmarklist.com/";

/// BenchLM's models page (the data document's build id is read from it).
pub const BENCHLM_PAGE_URL: &str = "https://benchlm.ai/models";
/// BenchLM attribution target.
pub const BENCHLM_ATTRIBUTION_URL: &str = "https://benchlm.ai/models";

/// The three supported sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceId {
    Arena,
    BenchmarkList,
    BenchLm,
}

impl SourceId {
    /// Every source, in scheduling and listing order.
    pub const ALL: [SourceId; 3] = [Self::Arena, Self::BenchmarkList, Self::BenchLm];

    /// The stable name recorded in `benchmarks.source` and
    /// `benchmark_sync_runs.source`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Arena => "arena",
            Self::BenchmarkList => "benchmarklist",
            Self::BenchLm => "benchlm",
        }
    }

    /// Parses a request/CLI name; unknown names are `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "arena" => Some(Self::Arena),
            "benchmarklist" => Some(Self::BenchmarkList),
            "benchlm" => Some(Self::BenchLm),
            _ => None,
        }
    }

    /// The `benchmarks.category` this source fills.
    pub fn category(self) -> &'static str {
        match self {
            Self::Arena => "code",
            Self::BenchmarkList | Self::BenchLm => "text",
        }
    }

    /// The `benchmarks.scale` this source's scores use. Only `elo` feeds
    /// routing quality (D25).
    pub fn scale(self) -> &'static str {
        match self {
            Self::Arena => "elo",
            Self::BenchmarkList => "capability",
            Self::BenchLm => "percent",
        }
    }

    /// The URL stored in `benchmarks.source_url` for this source's rows.
    pub fn source_url(self) -> &'static str {
        match self {
            Self::Arena => ARENA_ATTRIBUTION_URL,
            Self::BenchmarkList => BENCHMARKLIST_ATTRIBUTION_URL,
            Self::BenchLm => BENCHLM_ATTRIBUTION_URL,
        }
    }

    /// Human-readable licence note for the admin UI.
    pub fn license(self) -> &'static str {
        match self {
            Self::Arena => "CC BY 4.0",
            Self::BenchmarkList => "see benchmarklist.com",
            Self::BenchLm => "see benchlm.ai",
        }
    }
}

/// Benchmark sync failures. A *source* failure (network, shape, timeout) is
/// not an `Err`: it becomes a `status = 'error'` run row so the other sources
/// still run. `Err` is reserved for database and audit failures.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("fetching {url} failed: {detail}")]
    Fetch { url: String, detail: String },
    #[error("{what} page at offset {offset} failed: {detail}")]
    Page {
        what: &'static str,
        offset: usize,
        detail: String,
    },
    #[error("the {what} response is malformed: {detail}")]
    Parse { what: &'static str, detail: String },
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit write failed: {0}")]
    Audit(#[from] catalog::CatalogError),
}

impl SyncError {
    /// Pages that failed after all retries; `1` for [`SyncError::Page`] and
    /// `0` for every other failure. A paged source aborts on the first page
    /// it cannot read, so it never truncates silently.
    pub fn failed_pages(&self) -> usize {
        match self {
            Self::Page { .. } => 1,
            _ => 0,
        }
    }

    /// The uniform API error for this failure.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        match self {
            Self::Db(e) => {
                crate::error::ApiError::internal("benchmark sync", e).with_request_id(rid)
            }
            Self::Audit(e) => e.into_api_error(rid),
            // Source failures are outcomes, never propagated this far.
            Self::Fetch { .. } | Self::Page { .. } | Self::Parse { .. } => {
                crate::error::ApiError::internal("benchmark source", self).with_request_id(rid)
            }
        }
    }
}

/// Reads one URL. Implementations fail on transport errors and non-2xx
/// responses; tests implement it from fixtures.
pub trait Fetcher {
    fn get(&self, url: &str) -> impl Future<Output = Result<String, SyncError>> + Send;
}

/// The production fetcher: a pooled HTTP client with [`FETCH_TIMEOUT`],
/// [`FETCH_ATTEMPTS`] bounded retries, and a body read that tolerates a
/// chunked or early-closed transfer.
#[derive(Clone)]
pub struct HttpFetcher {
    client: reqwest::Client,
    retry_base: Duration,
}

impl HttpFetcher {
    /// Builds the HTTP client. Fails only when the TLS/HTTP stack cannot be
    /// initialized.
    pub fn new() -> Result<Self, SyncError> {
        // Identify the gateway on every request (`pareto-ai/<version>`); some
        // CDNs close the connection early for clients that do not, and a
        // named client is what an operator reads in the source's logs.
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("pareto-ai/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| SyncError::Fetch {
                url: "http client".to_string(),
                detail: e.to_string(),
            })?;
        Ok(Self {
            client,
            retry_base: RETRY_BASE_DELAY,
        })
    }

    /// Test seam: the same client with a shorter retry base so retry tests do
    /// not sleep for seconds.
    #[cfg(test)]
    fn with_retry_base(retry_base: Duration) -> Result<Self, SyncError> {
        let mut fetcher = Self::new()?;
        fetcher.retry_base = retry_base;
        Ok(fetcher)
    }
}

/// The delay before retry `attempt` (0-based): exponential from `base`, plus
/// up to 25% jitter so parallel retries do not synchronise.
fn retry_delay(base: Duration, attempt: usize) -> Duration {
    let factor: u32 = 1u32 << u32::try_from(attempt.min(6)).unwrap_or(6);
    let step = base.saturating_mul(factor);
    step + step.mul_f64(rand::random::<f64>() * 0.25)
}

/// Reads a response body chunk by chunk until EOF, under [`MAX_BODY_BYTES`].
/// Returns the bytes read so far and, when the transfer broke, why. An early
/// close is never treated as a short read of a complete document by accident;
/// [`HttpFetcher`] decides whether a broken transfer still holds a whole JSON
/// document. Failure mode: the detail string names how many bytes were read
/// before the transfer broke.
async fn read_body(mut response: reqwest::Response) -> Result<Vec<u8>, (Vec<u8>, String)> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err((
                        body,
                        format!(
                            "the body exceeds the {} MiB cap",
                            MAX_BODY_BYTES / (1024 * 1024)
                        ),
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(error) => {
                let detail = format!("the transfer broke after {} bytes: {error}", body.len());
                return Err((body, detail));
            }
        }
    }
}

/// True when a body that arrived with a broken transfer is nevertheless a
/// complete JSON document. The recorded benchmarklist.com failure reached a
/// 2xx, transferred for seconds and then reported a body decode error, while
/// `curl` wrote the whole file: a dropped chunk terminator looks exactly like
/// that. Parsing the bytes is the only honest test — a truncated JSON document
/// does not parse.
fn is_complete_json(body: &[u8]) -> bool {
    !body.is_empty() && serde_json::from_slice::<serde_json::Value>(body).is_ok()
}

impl Fetcher for HttpFetcher {
    fn get(&self, url: &str) -> impl Future<Output = Result<String, SyncError>> + Send {
        let client = self.client.clone();
        let url = url.to_string();
        let retry_base = self.retry_base;
        async move {
            // Bounded retries with exponential backoff. Retried: transport
            // errors, timeouts, body decode failures (an interrupted
            // transfer), 5xx, and 429. Not retried: any other 4xx, which a
            // fresh connection cannot fix.
            //
            // Every request states who it is: a `User-Agent` (some CDNs close
            // the connection early without one), `Accept: application/json`,
            // and `Accept-Encoding: identity` (a compressed early close is
            // harder to diagnose than a plain one). It also asks the server to
            // close the connection, and the retries downgrade to HTTP/1.1 and
            // then HTTP/1.0, because an HTTP/2 stream reset mid-body surfaces
            // as a body decode error.
            let mut last_error: Option<SyncError> = None;
            for attempt in 0..FETCH_ATTEMPTS {
                let started = Instant::now();
                let mut request = client
                    .get(&url)
                    .header(reqwest::header::ACCEPT, "application/json")
                    .header(reqwest::header::ACCEPT_ENCODING, "identity");
                // Attempt 0 may negotiate HTTP/2, where `Connection` is
                // forbidden (hyper strips it with a warning). The retries are
                // pinned to HTTP/1.x, so they ask for a close.
                request = match attempt {
                    0 => request,
                    1 => request
                        .version(reqwest::Version::HTTP_11)
                        .header(reqwest::header::CONNECTION, "close"),
                    _ => request
                        .version(reqwest::Version::HTTP_10)
                        .header(reqwest::header::CONNECTION, "close"),
                };
                let response = match request.send().await {
                    Ok(response) => response,
                    Err(error) => {
                        tracing::debug!(
                            url = %url,
                            attempt,
                            elapsed_ms = started.elapsed().as_millis(),
                            error = %error,
                            layer = "send",
                            "benchmark source request failed"
                        );
                        last_error = Some(SyncError::Fetch {
                            url: url.clone(),
                            detail: format!(
                                "attempt {} of {FETCH_ATTEMPTS} (send): {error}",
                                attempt + 1
                            ),
                        });
                        if attempt + 1 < FETCH_ATTEMPTS {
                            tokio::time::sleep(retry_delay(retry_base, attempt)).await;
                        }
                        continue;
                    }
                };
                let status = response.status();
                let headers = response.headers().clone();
                let header = |name: &str| {
                    headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("")
                        .to_string()
                };
                if !status.is_success() {
                    let retryable = status.is_server_error()
                        || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                    last_error = Some(SyncError::Fetch {
                        url: url.clone(),
                        detail: format!(
                            "attempt {} of {FETCH_ATTEMPTS}: HTTP {status}",
                            attempt + 1
                        ),
                    });
                    tracing::debug!(
                        url = %url,
                        status = %status,
                        attempt,
                        retryable,
                        "benchmark source returned a non-success status"
                    );
                    if !retryable {
                        break;
                    }
                    if attempt + 1 < FETCH_ATTEMPTS {
                        tokio::time::sleep(retry_delay(retry_base, attempt)).await;
                    }
                    continue;
                }
                match read_body(response).await {
                    Ok(bytes) => {
                        tracing::debug!(
                            url = %url,
                            status = %status,
                            content_type = %header("content-type"),
                            content_encoding = %header("content-encoding"),
                            content_length = %header("content-length"),
                            bytes = bytes.len(),
                            first_bytes = ?String::from_utf8_lossy(&bytes[..bytes.len().min(120)]),
                            attempt,
                            "benchmark source fetched"
                        );
                        return Ok(String::from_utf8_lossy(&bytes).into_owned());
                    }
                    Err((bytes, detail)) => {
                        // The document arrived whole but the transfer broke
                        // (a dropped chunk terminator is the live shape). The
                        // bytes are the source's answer; the parser judges
                        // them, and a truncated JSON body does not parse.
                        if is_complete_json(&bytes) {
                            tracing::warn!(
                                url = %url,
                                status = %status,
                                bytes = bytes.len(),
                                detail = %detail,
                                "benchmark source body ended early but is a complete JSON document"
                            );
                            return Ok(String::from_utf8_lossy(&bytes).into_owned());
                        }
                        tracing::debug!(
                            url = %url,
                            status = %status,
                            content_type = %header("content-type"),
                            content_encoding = %header("content-encoding"),
                            content_length = %header("content-length"),
                            attempt,
                            detail = %detail,
                            "benchmark source body failed to decode"
                        );
                        last_error = Some(SyncError::Fetch {
                            url: url.clone(),
                            detail: format!(
                                "attempt {} of {FETCH_ATTEMPTS} (body): {detail}",
                                attempt + 1
                            ),
                        });
                        if attempt + 1 < FETCH_ATTEMPTS {
                            tokio::time::sleep(retry_delay(retry_base, attempt)).await;
                        }
                    }
                }
            }
            Err(last_error.unwrap_or_else(|| SyncError::Fetch {
                url,
                detail: "the response body could not be read".to_string(),
            }))
        }
    }
}

/// One normalized source entry, before model matching.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The source's model name or id, as published.
    pub model_hint: String,
    /// `text` or `code` (from the source, not the entry).
    pub category: &'static str,
    /// On the source's native scale ([`SourceId::scale`]).
    pub score: f64,
    /// CI half-width in the source's units, when published.
    pub ci: Option<f64>,
    pub rank: Option<i64>,
}

impl Entry {
    fn new(model_hint: impl Into<String>, category: &'static str, score: f64) -> Self {
        Self {
            model_hint: model_hint.into(),
            category,
            score,
            ci: None,
            rank: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Arena WebDev
// ---------------------------------------------------------------------------

/// One parsed Arena page plus how many rows it held before filtering.
#[derive(Debug, Default)]
pub struct ArenaPage {
    pub entries: Vec<Entry>,
    pub rows_seen: usize,
    pub total: Option<usize>,
}

/// Downloads up to [`ARENA_MAX_PAGES`] Arena pages (the top of the WebDev
/// leaderboard). Each page gets [`FETCH_ATTEMPTS`] tries inside the fetcher;
/// a page that still fails aborts the source with [`SyncError::Page`] (and a
/// `failed_pages` count of 1) instead of silently truncating the result.
/// Failure mode: [`SyncError::Page`].
pub async fn fetch_arena<F: Fetcher>(fetcher: &F) -> Result<Vec<Entry>, SyncError> {
    let mut offset = 0usize;
    let mut entries = Vec::new();
    for _ in 0..ARENA_MAX_PAGES {
        let url = format!("{ARENA_ROWS_URL}&offset={offset}&length={ARENA_PAGE_SIZE}");
        let page_error = |detail: String| SyncError::Page {
            what: SourceId::Arena.as_str(),
            offset,
            detail,
        };
        let raw = fetcher
            .get(&url)
            .await
            .map_err(|error| page_error(error.to_string()))?;
        let page = parse_arena_page(&raw).map_err(|error| page_error(error.to_string()))?;
        entries.extend(page.entries);
        if page.rows_seen == 0 {
            break;
        }
        offset += page.rows_seen;
        if page.total.is_some_and(|total| offset >= total) {
            break;
        }
    }
    Ok(entries)
}

/// Parses one rows-API page. Only `category == "overall"` rows are kept; a
/// missing `rating` is malformed, while missing CI bounds and rank are fine.
/// Failure mode: [`SyncError::Parse`].
pub fn parse_arena_page(raw: &str) -> Result<ArenaPage, SyncError> {
    #[derive(serde::Deserialize)]
    struct Payload {
        rows: Vec<Row>,
        #[serde(default)]
        num_rows_total: Option<usize>,
    }
    #[derive(serde::Deserialize)]
    struct Row {
        row: Fields,
    }
    #[derive(serde::Deserialize)]
    struct Fields {
        model_name: String,
        #[serde(default)]
        rating: Option<f64>,
        #[serde(default)]
        rating_lower: Option<f64>,
        #[serde(default)]
        rating_upper: Option<f64>,
        #[serde(default)]
        rank: Option<i64>,
        #[serde(default)]
        category: Option<String>,
        #[serde(default)]
        #[allow(dead_code)] // pinned field; provenance lives in the run detail
        vote_count: Option<i64>,
        #[serde(default)]
        #[allow(dead_code)] // pinned field; provenance lives in the run detail
        organization: Option<String>,
        #[serde(default)]
        #[allow(dead_code)]
        leaderboard_publish_date: Option<String>,
    }

    let payload: Payload = serde_json::from_str(raw).map_err(|e| SyncError::Parse {
        what: SourceId::Arena.as_str(),
        detail: format!("invalid JSON: {e}"),
    })?;

    let mut page = ArenaPage {
        entries: Vec::new(),
        rows_seen: payload.rows.len(),
        total: payload.num_rows_total,
    };
    for item in payload.rows {
        let fields = item.row;
        if fields
            .category
            .as_deref()
            .is_some_and(|category| category != "overall")
        {
            continue;
        }
        let name = fields.model_name.trim();
        if name.is_empty() {
            continue;
        }
        let Some(score) = fields.rating.filter(|score| score.is_finite()) else {
            continue;
        };
        let ci = match (fields.rating_lower, fields.rating_upper) {
            (Some(lower), Some(upper))
                if lower.is_finite() && upper.is_finite() && upper >= lower =>
            {
                Some((upper - lower) / 2.0)
            }
            _ => None,
        };
        let mut entry = Entry::new(name, SourceId::Arena.category(), score);
        entry.ci = ci;
        entry.rank = fields.rank;
        page.entries.push(entry);
    }
    Ok(page)
}

// ---------------------------------------------------------------------------
// BenchmarkList capability
// ---------------------------------------------------------------------------

/// One parsed BenchmarkList document.
#[derive(Debug, Default)]
pub struct BenchmarkListPage {
    pub entries: Vec<Entry>,
    /// Rows in the document (scored and unscored); diagnostic and test use.
    #[allow(dead_code)]
    pub rows_seen: usize,
    /// Rows that carry no capability score (counted, never guessed).
    pub unscored: usize,
}

/// Downloads and parses BenchmarkList's model directory.
/// Failure mode: [`SyncError::Fetch`] or [`SyncError::Parse`].
pub async fn fetch_benchmarklist<F: Fetcher>(fetcher: &F) -> Result<BenchmarkListPage, SyncError> {
    let raw = fetcher.get(BENCHMARKLIST_URL).await?;
    parse_benchmarklist(&raw)
}

/// Parses `{"rows": [...]}` (a bare array and a `data` wrapper are also
/// accepted). A row contributes when it names a model and has a finite
/// `capability_score` (a numeric string or `capability.score` is tolerated);
/// `capability_rank`, when present, is kept. Unknown fields are ignored and
/// rows without a score are counted in `unscored`, never guessed.
/// Failure mode: [`SyncError::Parse`].
pub fn parse_benchmarklist(raw: &str) -> Result<BenchmarkListPage, SyncError> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| SyncError::Parse {
        what: SourceId::BenchmarkList.as_str(),
        detail: format!("invalid JSON: {e}"),
    })?;
    let rows = value
        .get("rows")
        .or_else(|| value.get("data").and_then(|data| data.get("rows")))
        .or_else(|| value.get("data"))
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())
        .ok_or_else(|| SyncError::Parse {
            what: SourceId::BenchmarkList.as_str(),
            detail: "expected a rows array ({\"rows\": [...]})".to_string(),
        })?;
    if rows.is_empty() {
        return Err(SyncError::Parse {
            what: SourceId::BenchmarkList.as_str(),
            detail: "rows must not be empty".to_string(),
        });
    }

    let mut page = BenchmarkListPage {
        entries: Vec::new(),
        rows_seen: rows.len(),
        unscored: 0,
    };
    for row in rows {
        let hint = row
            .get("id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| row.get("name").and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|hint| !hint.is_empty());
        let score = row
            .get("capability_score")
            .or_else(|| row.get("capability").and_then(|c| c.get("score")))
            .and_then(|value| match value {
                serde_json::Value::Number(number) => number.as_f64(),
                serde_json::Value::String(raw) => raw.trim().parse::<f64>().ok(),
                _ => None,
            })
            .filter(|score| score.is_finite());
        let (Some(hint), Some(score)) = (hint, score) else {
            page.unscored += 1;
            continue;
        };
        let mut entry = Entry::new(hint, SourceId::BenchmarkList.category(), score);
        entry.rank = row
            .get("capability_rank")
            .and_then(serde_json::Value::as_i64);
        page.entries.push(entry);
    }
    Ok(page)
}

// ---------------------------------------------------------------------------
// BenchLM (optional, brittle)
// ---------------------------------------------------------------------------

/// One parsed BenchLM document.
#[derive(Debug, Default)]
pub struct BenchLmPage {
    pub entries: Vec<Entry>,
    /// Rows in the document; diagnostic and test use.
    #[allow(dead_code)]
    pub rows_seen: usize,
}

/// Reads the BenchLM build id out of the models page HTML.
/// Failure mode: [`SyncError::Parse`].
pub fn benclm_build_id(html: &str) -> Result<String, SyncError> {
    let marker = "\"buildId\":\"";
    let start = html.find(marker).ok_or_else(|| SyncError::Parse {
        what: SourceId::BenchLm.as_str(),
        detail: "the models page carries no buildId".to_string(),
    })? + marker.len();
    let rest = &html[start..];
    let end = rest.find('"').ok_or_else(|| SyncError::Parse {
        what: SourceId::BenchLm.as_str(),
        detail: "the buildId is not terminated".to_string(),
    })?;
    let build = &rest[..end];
    if build.is_empty()
        || !build
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(SyncError::Parse {
            what: SourceId::BenchLm.as_str(),
            detail: "the buildId has an unexpected shape".to_string(),
        });
    }
    Ok(build.to_string())
}

/// The Next.js data URL for one build id.
pub fn benchlm_data_url(build_id: &str) -> String {
    format!("https://benchlm.ai/_next/data/{build_id}/models.json")
}

/// Downloads the models page, then its Next.js data document.
/// Failure mode: [`SyncError::Fetch`] or [`SyncError::Parse`].
pub async fn fetch_benchlm<F: Fetcher>(fetcher: &F) -> Result<Vec<Entry>, SyncError> {
    let html = fetcher.get(BENCHLM_PAGE_URL).await?;
    let build_id = benclm_build_id(&html)?;
    let raw = fetcher.get(&benchlm_data_url(&build_id)).await?;
    Ok(parse_benchlm(&raw)?.entries)
}

/// Parses `pageProps.directory.rows`, a list of positional arrays. The pinned
/// layout is `[0]` name, `[1]` slug, `[6]` score, `[9]` rank. Rows that do
/// not fit are skipped; a document without rows is malformed.
/// Failure mode: [`SyncError::Parse`].
pub fn parse_benchlm(raw: &str) -> Result<BenchLmPage, SyncError> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| SyncError::Parse {
        what: SourceId::BenchLm.as_str(),
        detail: format!("invalid JSON: {e}"),
    })?;
    let rows = value
        .pointer("/pageProps/directory/rows")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| SyncError::Parse {
            what: SourceId::BenchLm.as_str(),
            detail: "no pageProps.directory.rows array".to_string(),
        })?;
    if rows.is_empty() {
        return Err(SyncError::Parse {
            what: SourceId::BenchLm.as_str(),
            detail: "rows must not be empty".to_string(),
        });
    }

    let mut page = BenchLmPage {
        entries: Vec::new(),
        rows_seen: rows.len(),
    };
    for row in rows {
        let Some(fields) = row.as_array() else {
            continue;
        };
        let Some(name) = fields
            .first()
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let score = fields
            .get(6)
            .and_then(serde_json::Value::as_f64)
            .filter(|score| score.is_finite());
        let Some(score) = score else {
            continue;
        };
        let mut entry = Entry::new(name, SourceId::BenchLm.category(), score);
        entry.rank = fields.get(9).and_then(serde_json::Value::as_i64);
        page.entries.push(entry);
    }
    Ok(page)
}

/// Loads the catalog index from `models` and `offerings`, including the
/// external identity registry, so a benchmark that matches one spelling also
/// reaches every catalog row the source groups under the same canonical id
/// (D38 amendment). The registry is optional data: with no rows the index
/// behaves exactly as the pure spelling matcher.
/// Failure mode: [`SyncError::Db`].
pub async fn load_model_index(pool: &SqlitePool) -> Result<ModelIndex, SyncError> {
    let mut conn = pool.acquire().await?;
    let models: Vec<(String, String)> = sqlx::query_as("SELECT canonical_name, id FROM models")
        .fetch_all(&mut *conn)
        .await?;
    let offerings: Vec<(String, String)> = sqlx::query_as(
        "SELECT upstream_model_id, model_id FROM offerings WHERE upstream_model_id IS NOT NULL",
    )
    .fetch_all(&mut *conn)
    .await?;
    let registry = crate::catalog::registry::load(&mut conn).await?;
    Ok(ModelIndex::with_registry(models, offerings, registry))
}

// ---------------------------------------------------------------------------
// Run results and persistence
// ---------------------------------------------------------------------------

/// What one source's run published and changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Entries after the source's own filtering (Arena `overall`, scored
    /// BenchmarkList rows, scored BenchLM rows).
    pub published: usize,
    /// Catalog models that received a row (one row per model; a hint can
    /// attach to several models).
    pub matched: usize,
    /// Hints that resolved to at least one model.
    pub matched_hints: usize,
    pub skipped_unknown: usize,
    /// Hints that named several distinct models (never guessed). Duplicate
    /// entries for one model keep the better rank and are not counted here.
    pub ambiguous: usize,
    /// Pages that failed after every retry before the source aborted.
    pub failed_pages: usize,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
}

/// How a source's attempt ended (serialized into the run log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Success,
    Error,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// Result of one source attempt. A dry run carries the counts but writes no
/// run row and no benchmark row.
#[derive(Debug, Clone)]
pub struct SourceOutcome {
    pub source: SourceId,
    pub status: RunStatus,
    pub dry_run: bool,
    pub counts: Counts,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: i64,
    /// Secret-free failure text, `None` on success.
    pub error: Option<String>,
    /// The attribution URL stored on every row of this source.
    pub source_url: String,
}

/// One existing benchmark row for the change detector: `(score, ci, rank,
/// source_url)`.
type ExistingBenchmark = (f64, Option<f64>, Option<i64>, Option<String>);

/// Picks the best rank (lowest number, unranked last) among duplicate hints.
fn better_rank(candidate: &Entry, current: &Entry) -> bool {
    match (candidate.rank, current.rank) {
        (Some(candidate_rank), Some(current_rank)) => candidate_rank < current_rank,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Resolves entries to models and dedupes per model. A hint that names several
/// models of the same identity attaches to all of them; a hint naming several
/// distinct identities is counted as ambiguous, never guessed.
fn resolve(
    entries: Vec<Entry>,
    index: &ModelIndex,
    published: usize,
) -> (Vec<(String, Entry)>, Counts) {
    let mut counts = Counts {
        published,
        ..Counts::default()
    };
    let mut picked: BTreeMap<String, Entry> = BTreeMap::new();
    for entry in entries {
        match index.lookup(&entry.model_hint) {
            Matching::Unknown => counts.skipped_unknown += 1,
            Matching::Ambiguous => counts.ambiguous += 1,
            Matching::Matched(model_ids) => {
                counts.matched_hints += 1;
                for model_id in model_ids {
                    match picked.get(&model_id) {
                        None => {
                            picked.insert(model_id, entry.clone());
                        }
                        Some(current) => {
                            // The same model twice in one run: keep the better
                            // rank. That is a duplicate, not an ambiguity.
                            if better_rank(&entry, current) {
                                picked.insert(model_id, entry.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    counts.matched = picked.len();
    (picked.into_iter().collect(), counts)
}

/// One source's fetched entries plus the number of rows the source published
/// (which can exceed `entries.len()` when unscored rows are counted).
struct Fetched {
    entries: Vec<Entry>,
    published: usize,
}

/// Fetches one source's entries, inside the per-source timeout.
async fn fetch_entries<F: Fetcher>(source: SourceId, fetcher: &F) -> Result<Fetched, SyncError> {
    match source {
        SourceId::Arena => {
            let entries = fetch_arena(fetcher).await?;
            let published = entries.len();
            Ok(Fetched { entries, published })
        }
        SourceId::BenchmarkList => {
            let page = fetch_benchmarklist(fetcher).await?;
            Ok(Fetched {
                entries: page.entries,
                published: page.rows_seen,
            })
        }
        SourceId::BenchLm => {
            let entries = fetch_benchlm(fetcher).await?;
            let published = entries.len();
            Ok(Fetched { entries, published })
        }
    }
}

/// Runs one source: fetch, match, upsert, record the run row (and, for an
/// admin-triggered run, the audit row). A source failure is recorded as
/// `status = 'error'` and returned as an outcome; `Err` means the run row
/// itself could not be written. `actor = None` is the background job, which
/// writes no audit row.
pub async fn run_source<F: Fetcher>(
    pool: &SqlitePool,
    source: SourceId,
    fetcher: &F,
    dry_run: bool,
    actor: Option<&Actor<'_>>,
) -> Result<SourceOutcome, SyncError> {
    let started_at = catalog::now_ts();
    let started = Instant::now();
    let fetched = tokio::time::timeout(SOURCE_TIMEOUT, fetch_entries(source, fetcher)).await;

    let failed_pages = match &fetched {
        Ok(Err(error)) => error.failed_pages(),
        _ => 0,
    };
    let fetch_error = match fetched {
        Ok(Ok(fetched)) => {
            let index = load_model_index(pool).await?;
            let (matched, counts) = resolve(fetched.entries, &index, fetched.published);
            let counts = if dry_run {
                counts
            } else {
                apply(
                    pool,
                    source,
                    &matched,
                    counts,
                    &started_at,
                    elapsed_ms(started),
                    actor,
                )
                .await?
            };
            let finished_at = catalog::now_ts();
            return Ok(SourceOutcome {
                source,
                status: RunStatus::Success,
                dry_run,
                counts,
                started_at,
                finished_at,
                duration_ms: elapsed_ms(started),
                error: None,
                source_url: source.source_url().to_string(),
            });
        }
        Ok(Err(error)) => error.to_string(),
        Err(_) => format!(
            "the source did not answer within {} seconds",
            SOURCE_TIMEOUT.as_secs()
        ),
    };

    let finished_at = catalog::now_ts();
    let outcome = SourceOutcome {
        source,
        status: RunStatus::Error,
        dry_run,
        counts: Counts {
            failed_pages,
            ..Counts::default()
        },
        started_at,
        finished_at,
        duration_ms: elapsed_ms(started),
        error: Some(fetch_error),
        source_url: source.source_url().to_string(),
    };
    if !dry_run {
        record_run(pool, &outcome, actor).await?;
    }
    Ok(outcome)
}

fn elapsed_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// Upserts the matched entries in one transaction with the run row and the
/// audit row. `model_quality_overrides` is never touched.
async fn apply(
    pool: &SqlitePool,
    source: SourceId,
    matched: &[(String, Entry)],
    mut counts: Counts,
    fetched_at: &str,
    duration_ms: i64,
    actor: Option<&Actor<'_>>,
) -> Result<Counts, SyncError> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;
    let now = catalog::now_ts();

    for (model_id, entry) in matched {
        let existing: Option<ExistingBenchmark> = sqlx::query_as(
            "SELECT score, ci, rank, source_url FROM benchmarks \
             WHERE model_id = ? AND source = ? AND category = ?",
        )
        .bind(model_id)
        .bind(source.as_str())
        .bind(entry.category)
        .fetch_optional(&mut *tx)
        .await?;

        let unchanged = existing.as_ref().is_some_and(|(score, ci, rank, url)| {
            (*score - entry.score).abs() < f64::EPSILON
                && *ci == entry.ci
                && *rank == entry.rank
                && url.as_deref() == Some(source.source_url())
        });
        if unchanged {
            counts.unchanged += 1;
            continue;
        }
        match existing {
            Some(_) => counts.updated += 1,
            None => counts.inserted += 1,
        }
        sqlx::query(
            "INSERT INTO benchmarks \
               (id, model_id, source, category, score, ci, rank, source_url, scale, \
                fetched_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (model_id, source, category) DO UPDATE SET \
               score = excluded.score, ci = excluded.ci, rank = excluded.rank, \
               source_url = excluded.source_url, scale = excluded.scale, \
               fetched_at = excluded.fetched_at, updated_at = excluded.updated_at",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(model_id)
        .bind(source.as_str())
        .bind(entry.category)
        .bind(entry.score)
        .bind(entry.ci)
        .bind(entry.rank)
        .bind(source.source_url())
        .bind(source.scale())
        .bind(fetched_at)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }

    let status = "success";
    sqlx::query(
        "INSERT INTO benchmark_sync_runs \
           (id, source, status, started_at, finished_at, duration_ms, published, matched, \
            matched_hints, skipped_unknown, ambiguous, failed_pages, inserted, updated, unchanged, \
            source_url, error) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(&run_id)
    .bind(source.as_str())
    .bind(status)
    .bind(fetched_at)
    .bind(catalog::now_ts())
    .bind(duration_ms)
    .bind(counts.published as i64)
    .bind(counts.matched as i64)
    .bind(counts.matched_hints as i64)
    .bind(counts.skipped_unknown as i64)
    .bind(counts.ambiguous as i64)
    .bind(counts.failed_pages as i64)
    .bind(counts.inserted as i64)
    .bind(counts.updated as i64)
    .bind(counts.unchanged as i64)
    .bind(source.source_url())
    .execute(&mut *tx)
    .await?;

    if let Some(actor) = actor {
        catalog::audit(
            &mut tx,
            actor,
            "sync.benchmark_sources",
            "benchmark_sync_run",
            &run_id,
            &serde_json::json!({
                "source": source.as_str(),
                "published": counts.published,
                "matched": counts.matched,
                "matched_hints": counts.matched_hints,
                "skipped_unknown": counts.skipped_unknown,
                "ambiguous": counts.ambiguous,
                "failed_pages": counts.failed_pages,
                "inserted": counts.inserted,
                "updated": counts.updated,
                "unchanged": counts.unchanged,
            }),
        )
        .await?;
    }
    prune_runs(&mut tx, source).await?;
    tx.commit().await?;
    Ok(counts)
}

/// Writes the `status = 'error'` run row for a failed source attempt.
async fn record_run(
    pool: &SqlitePool,
    outcome: &SourceOutcome,
    actor: Option<&Actor<'_>>,
) -> Result<(), SyncError> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO benchmark_sync_runs \
           (id, source, status, started_at, finished_at, duration_ms, published, matched, \
            matched_hints, skipped_unknown, ambiguous, failed_pages, inserted, updated, unchanged, \
            source_url, error) \
         VALUES (?, ?, ?, ?, ?, ?, 0, 0, 0, 0, 0, ?, 0, 0, 0, ?, ?)",
    )
    .bind(&run_id)
    .bind(outcome.source.as_str())
    .bind(outcome.status.as_str())
    .bind(&outcome.started_at)
    .bind(&outcome.finished_at)
    .bind(outcome.duration_ms)
    .bind(outcome.counts.failed_pages as i64)
    .bind(outcome.source_url.as_str())
    .bind(outcome.error.as_deref())
    .execute(&mut *tx)
    .await?;

    if let Some(actor) = actor {
        catalog::audit(
            &mut tx,
            actor,
            "sync.benchmark_sources",
            "benchmark_sync_run",
            &run_id,
            &serde_json::json!({
                "source": outcome.source.as_str(),
                "status": outcome.status.as_str(),
                "failed_pages": outcome.counts.failed_pages,
                "error": outcome.error,
            }),
        )
        .await?;
    }
    prune_runs(&mut tx, outcome.source).await?;
    tx.commit().await?;
    Ok(())
}

/// Keeps the newest [`MAX_RUNS`] rows of one source.
async fn prune_runs(conn: &mut SqliteConnection, source: SourceId) -> Result<(), SyncError> {
    sqlx::query(
        "DELETE FROM benchmark_sync_runs \
         WHERE source = ? AND id NOT IN ( \
           SELECT id FROM benchmark_sync_runs WHERE source = ? \
           ORDER BY finished_at DESC, id DESC LIMIT ? \
         )",
    )
    .bind(source.as_str())
    .bind(source.as_str())
    .bind(MAX_RUNS)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Runs the requested sources in order. One source's failure never aborts the
/// rest; each outcome (success or error) is returned. `dry_run` writes
/// nothing at all.
pub async fn run_sources<F: Fetcher>(
    pool: &SqlitePool,
    sources: &[SourceId],
    fetcher: &F,
    dry_run: bool,
    actor: Option<&Actor<'_>>,
) -> Vec<SourceOutcome> {
    let mut outcomes = Vec::with_capacity(sources.len());
    for source in sources {
        match run_source(pool, *source, fetcher, dry_run, actor).await {
            Ok(outcome) => {
                if outcome.status == RunStatus::Error {
                    tracing::warn!(
                        source = source.as_str(),
                        error = outcome.error.as_deref().unwrap_or("unknown"),
                        "benchmark source failed; other sources continue"
                    );
                }
                outcomes.push(outcome);
            }
            Err(error) => {
                tracing::error!(
                    source = source.as_str(),
                    error = %error,
                    "benchmark source could not record its run"
                );
                outcomes.push(SourceOutcome {
                    source: *source,
                    status: RunStatus::Error,
                    dry_run,
                    counts: Counts::default(),
                    started_at: catalog::now_ts(),
                    finished_at: catalog::now_ts(),
                    duration_ms: 0,
                    error: Some(error.to_string()),
                    source_url: source.source_url().to_string(),
                });
            }
        }
    }
    outcomes
}

// ---------------------------------------------------------------------------
// Run log reads and freshness
// ---------------------------------------------------------------------------

/// One `benchmark_sync_runs` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunRow {
    pub id: String,
    pub source: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: i64,
    pub published: i64,
    pub matched: i64,
    pub matched_hints: i64,
    pub skipped_unknown: i64,
    pub ambiguous: i64,
    pub failed_pages: i64,
    pub inserted: i64,
    pub updated: i64,
    pub unchanged: i64,
    pub source_url: Option<String>,
    pub error: Option<String>,
}

/// The most recent run of one source, whatever its status.
/// Failure mode: [`SyncError::Db`].
pub async fn last_run(pool: &SqlitePool, source: SourceId) -> Result<Option<RunRow>, SyncError> {
    Ok(sqlx::query_as::<_, RunRow>(
        "SELECT id, source, status, started_at, finished_at, duration_ms, published, matched, \
                matched_hints, skipped_unknown, ambiguous, failed_pages, inserted, updated, \
                unchanged, source_url, error \
         FROM benchmark_sync_runs WHERE source = ? \
         ORDER BY finished_at DESC, id DESC LIMIT 1",
    )
    .bind(source.as_str())
    .fetch_optional(pool)
    .await?)
}

/// When one source last succeeded, if ever.
/// Failure mode: [`SyncError::Db`].
pub async fn last_success_at(
    pool: &SqlitePool,
    source: SourceId,
) -> Result<Option<String>, SyncError> {
    Ok(sqlx::query_scalar(
        "SELECT finished_at FROM benchmark_sync_runs \
         WHERE source = ? AND status = 'success' \
         ORDER BY finished_at DESC, id DESC LIMIT 1",
    )
    .bind(source.as_str())
    .fetch_optional(pool)
    .await?)
}

/// Whether a success at `finished_at` is still inside [`SYNC_TTL`].
pub fn is_fresh(finished_at: Option<&str>, now: &DateTime<Utc>) -> bool {
    finished_at
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| {
            let age = now.signed_duration_since(timestamp.to_utc());
            age >= chrono::Duration::zero() && age.num_seconds() < SYNC_TTL.as_secs() as i64
        })
        .unwrap_or(false)
}

/// The sources whose last success is missing or older than [`SYNC_TTL`].
/// Failure mode: [`SyncError::Db`].
pub async fn sources_due(
    pool: &SqlitePool,
    now: &DateTime<Utc>,
) -> Result<Vec<SourceId>, SyncError> {
    let mut due = Vec::new();
    for source in SourceId::ALL {
        let last = last_success_at(pool, source).await?;
        if !is_fresh(last.as_deref(), now) {
            due.push(source);
        }
    }
    Ok(due)
}

// ---------------------------------------------------------------------------
// Scheduled job
// ---------------------------------------------------------------------------

/// Spawns the periodic benchmark sync: sleeps [`JOB_START_DELAY`], then every
/// [`JOB_INTERVAL`] syncs the sources older than [`SYNC_TTL`]. The shared
/// `gate` makes an overlapping admin run skip this tick instead of running
/// twice. Never panics; source failures are logged and retried next tick.
pub fn spawn_sync_job(
    pool: SqlitePool,
    fetcher: HttpFetcher,
    gate: std::sync::Arc<tokio::sync::Mutex<()>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(JOB_START_DELAY).await;
        loop {
            if let Ok(_guard) = gate.try_lock() {
                match sources_due(&pool, &Utc::now()).await {
                    Ok(sources) if !sources.is_empty() => {
                        let outcomes = run_sources(&pool, &sources, &fetcher, false, None).await;
                        for outcome in outcomes {
                            tracing::info!(
                                source = outcome.source.as_str(),
                                status = outcome.status.as_str(),
                                published = outcome.counts.published,
                                matched = outcome.counts.matched,
                                "benchmark sync finished"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::error!(error = %error, "reading benchmark sync freshness failed");
                    }
                }
            } else {
                tracing::info!("benchmark sync already running; skipping this tick");
            }
            tokio::time::sleep(JOB_INTERVAL).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ModelInput;
    use crate::model_identity::{canonical_vendor, normalize_model_key};
    use crate::test_support::TestApp;

    const ARENA_PAGE_1: &str = include_str!("../tests/fixtures/benchmarks/arena_page_1.json");
    const ARENA_PAGE_2: &str = include_str!("../tests/fixtures/benchmarks/arena_page_2.json");
    const BENCHMARKLIST: &str =
        include_str!("../tests/fixtures/benchmarks/benchmarklist_models.json");
    const BENCHLM: &str = include_str!("../tests/fixtures/benchmarks/benchlm_models.json");
    const BENCHLM_HTML: &str = include_str!("../tests/fixtures/benchmarks/benchlm_page.html");

    /// A fixture-driven fetcher: the first route whose key is contained in
    /// the requested URL answers; anything else is a transport error.
    struct FixtureFetcher {
        routes: Vec<(&'static str, &'static str)>,
    }

    impl FixtureFetcher {
        fn new(routes: Vec<(&'static str, &'static str)>) -> Self {
            Self { routes }
        }
    }

    impl Fetcher for FixtureFetcher {
        fn get(&self, url: &str) -> impl Future<Output = Result<String, SyncError>> + Send {
            let found = self
                .routes
                .iter()
                .find(|(key, _)| url.contains(key))
                .map(|(_, body)| body.to_string());
            let url = url.to_string();
            async move {
                found.ok_or_else(|| SyncError::Fetch {
                    url,
                    detail: "no fixture for this URL".to_string(),
                })
            }
        }
    }

    fn arena_fetcher() -> FixtureFetcher {
        FixtureFetcher::new(vec![
            ("offset=0&length=100", ARENA_PAGE_1),
            ("offset=3&length=100", ARENA_PAGE_2),
        ])
    }

    async fn admin(app: &TestApp) -> String {
        app.create_user("admin@example.test", crate::auth::Role::Admin, true)
            .await
    }

    async fn seed_model(app: &TestApp, uid: &str, canonical_name: &str) -> String {
        catalog::create_model(
            &app.state.pool,
            &Actor {
                user_id: uid,
                ip: "127.0.0.1",
            },
            &ModelInput {
                canonical_name: canonical_name.to_string(),
                family: None,
                modalities_input: None,
                modalities_output: None,
                context_length: None,
                max_output_tokens: None,
                capabilities: None,
            },
        )
        .await
        .expect("model")
        .id
    }

    // -----------------------------------------------------------------------
    // Parsing and pagination
    // -----------------------------------------------------------------------

    #[test]
    fn arena_pages_are_followed_until_the_page_cap() {
        // `fetch_arena` is async; drive it with a tiny runtime.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let entries = runtime
            .block_on(fetch_arena(&arena_fetcher()))
            .expect("fetch");
        assert_eq!(entries.len(), 4, "two pages, two overall rows each");
        assert_eq!(entries[0].model_hint, "Claude Sonnet 5.5 (xHigh)");
        assert_eq!(entries[0].score, 1772.2);
        assert!(
            entries[0].ci.is_some_and(|ci| (ci - 9.7).abs() < 1e-9),
            "{:?}",
            entries[0].ci
        );
        assert_eq!(entries[0].rank, Some(3));
        assert_eq!(entries[0].category, "code");
        assert_eq!(entries[1].model_hint, "GPT-5.4");
        assert_eq!(entries[2].model_hint, "Gemini 3.1 Pro");
        // The second page's row without CI bounds keeps `None`.
        assert_eq!(entries[3].ci, None);
    }

    #[test]
    fn arena_rows_missing_ci_votes_and_rank_are_tolerated() {
        let page = parse_arena_page(ARENA_PAGE_2).expect("page");
        assert_eq!(page.rows_seen, 3, "the non-overall row still counts");
        assert_eq!(page.total, Some(4));
        assert_eq!(page.entries[1].model_hint, "Bare Model");
        assert_eq!(page.entries[1].ci, None);
        assert_eq!(page.entries[1].rank, None);
    }

    #[test]
    fn malformed_arena_pages_name_the_source() {
        let error = parse_arena_page("not json").expect_err("malformed");
        assert!(error.to_string().contains("arena"), "{error}");
        assert!(error.to_string().contains("JSON"), "{error}");
    }

    #[test]
    fn benchmarklist_shape_parses_scored_rows_and_counts_the_rest() {
        let page = parse_benchmarklist(BENCHMARKLIST).expect("parse");
        assert_eq!(page.rows_seen, 3);
        assert_eq!(page.unscored, 1, "one row has no capability score");
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].model_hint, "openai-gpt-5.4");
        assert_eq!(page.entries[0].score, 87.5);
        assert_eq!(page.entries[0].rank, Some(2));
        assert_eq!(page.entries[0].category, "text");
        assert_eq!(page.entries[1].model_hint, "anthropic-claude-sonnet-4.6");
    }

    #[test]
    fn benchmarklist_without_rows_is_malformed() {
        let error = parse_benchmarklist(r#"{"rows":[]}"#).expect_err("empty");
        assert!(error.to_string().contains("rows"), "{error}");
    }

    #[test]
    fn benchmarklist_accepts_the_live_metadata_shape_and_counts_unscored_rows() {
        // The live payload carries identity metadata only (no capability
        // score in any row). It must parse, with every row counted as seen
        // and unscored, instead of failing the whole source.
        let raw = r#"{
            "rows": [
                {
                    "originalIndex": 0,
                    "id": "google-gemini-3.1-pro-preview",
                    "name": "Gemini 3.1 Pro Preview",
                    "href": "/models/google-gemini-3.1-pro-preview/",
                    "family": "Gemini",
                    "developer": "Google",
                    "sourceKey": "closed_api",
                    "sourceFilter": "closed_api",
                    "releaseDate": "2026-02-19",
                    "count": 496,
                    "icon": {"label": "Gemini / Google", "initials": "GE"},
                    "unknown_future_field": true
                },
                {"id": "acme/thing", "name": "Thing", "capability": {"score": 71.5}}
            ]
        }"#;
        let page = parse_benchmarklist(raw).expect("permissive parse");
        assert_eq!(page.rows_seen, 2);
        assert_eq!(page.unscored, 1);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].model_hint, "acme/thing");
        assert_eq!(page.entries[0].score, 71.5);

        // A bare array and a `data` wrapper are tolerated too.
        let bare =
            parse_benchmarklist(r#"[{"id": "x", "capability_score": 50}]"#).expect("bare array");
        assert_eq!(bare.entries.len(), 1);
        let wrapped =
            parse_benchmarklist(r#"{"data": {"rows": [{"id": "x"}]}}"#).expect("data wrapper");
        assert_eq!(wrapped.rows_seen, 1);
        assert_eq!(wrapped.unscored, 1);
    }

    #[tokio::test]
    async fn benchmarklist_published_counts_rows_seen_not_just_scored_rows() {
        let app = TestApp::new().await;
        admin(&app).await;
        let fetcher = FixtureFetcher::new(vec![(
            "directory-data/models.json",
            r#"{"rows": [{"id": "a"}, {"id": "b"}, {"id": "c"}]}"#,
        )]);
        let outcome = run_source(
            &app.state.pool,
            SourceId::BenchmarkList,
            &fetcher,
            true,
            None,
        )
        .await
        .expect("run");
        assert_eq!(outcome.status, RunStatus::Success);
        assert_eq!(outcome.counts.published, 3, "published = rows seen");
        assert_eq!(outcome.counts.matched, 0);
    }

    #[test]
    fn benchlm_build_id_and_positional_rows_parse() {
        let build = benclm_build_id(BENCHLM_HTML).expect("build id");
        assert_eq!(build, "9_nDLJ0LVx00Ej6ZoA-I6");
        assert_eq!(
            benchlm_data_url(&build),
            "https://benchlm.ai/_next/data/9_nDLJ0LVx00Ej6ZoA-I6/models.json"
        );

        let page = parse_benchlm(BENCHLM).expect("parse");
        assert_eq!(page.rows_seen, 3);
        assert_eq!(page.entries.len(), 3);
        assert_eq!(page.entries[0].model_hint, "Claude Fable 5.2");
        assert_eq!(page.entries[0].score, 93.84);
        assert_eq!(page.entries[0].rank, Some(1));
        assert_eq!(page.entries[0].category, "text");
    }

    #[test]
    fn benchlm_without_the_data_pointer_is_malformed() {
        let error = parse_benchlm(r#"{"pageProps":{}}"#).expect_err("shape");
        assert!(error.to_string().contains("pageProps"), "{error}");
        let error = benclm_build_id("<html></html>").expect_err("no build id");
        assert!(error.to_string().contains("buildId"), "{error}");
    }

    // -----------------------------------------------------------------------
    // Normalization and matching
    // -----------------------------------------------------------------------

    #[test]
    fn model_keys_keep_exact_variants_and_strip_only_in_the_fallback() {
        assert_eq!(normalize_model_key("openai/gpt-4o"), "gpt-4o");
        // `:free`/`:batch` are distinct offerings, never stripped.
        assert_eq!(normalize_model_key("OpenAI/GPT-4O:free"), "gpt-4o-free");
        assert_eq!(normalize_model_key("openai/gpt-4o:batch"), "gpt-4o-batch");
        assert_eq!(
            normalize_model_key("claude-sonnet-4.5"),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            normalize_model_key("Claude Sonnet 4.5"),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            normalize_model_key("Claude Sonnet 5.5 (xHigh)"),
            "claude-sonnet-5-5"
        );
        assert_eq!(
            normalize_model_key("deepseek-v3-latest"),
            "deepseek-v3-latest"
        );
        assert_eq!(
            normalize_model_key("qwen3-coder-2025-08-07"),
            "qwen3-coder-2025-08-07"
        );
        assert_eq!(
            normalize_model_key("qwen3-coder-20250807"),
            "qwen3-coder-20250807"
        );
        assert_eq!(normalize_model_key("kimi-k3-thinking"), "kimi-k3-thinking");
        assert_eq!(normalize_model_key("!!!"), "");
    }

    /// `Matching::Matched` from plain ids, for readable assertions.
    fn matched(ids: &[&str]) -> Matching {
        Matching::Matched(ids.iter().map(|id| (*id).to_string()).collect())
    }

    #[test]
    fn matching_follows_the_decision_table() {
        let index = ModelIndex::new(vec![
            ("openai/gpt-4o".to_string(), "model-gpt".to_string()),
            ("claude-sonnet-4.5".to_string(), "model-claude".to_string()),
            ("acme-widget-7b".to_string(), "model-widget".to_string()),
        ]);
        assert_eq!(index.lookup("GPT-4o"), matched(&["model-gpt"]));
        // A variant the catalog does not have is unknown, never the base.
        assert_eq!(index.lookup("openai/gpt-4o:free"), Matching::Unknown);
        assert_eq!(
            index.lookup("Claude Sonnet 4.5"),
            matched(&["model-claude"])
        );
        // BenchmarkList writes the vendor with a hyphen; that token is
        // stripped for matching. Unknown hints are skipped, never guessed.
        assert_eq!(index.lookup("openai-gpt-4o"), matched(&["model-gpt"]));
        assert_eq!(index.lookup("totally-unknown"), Matching::Unknown);
        // A generic tail is never matched, even when a model ends with it.
        assert_eq!(index.lookup("flash"), Matching::Unknown);
        assert_eq!(index.lookup("pro"), Matching::Unknown);

        // Two catalog entries of the same model under different spellings
        // (vendor difference only) resolve to both, not to a guess.
        let shared = ModelIndex::new(vec![
            ("gpt-4o".to_string(), "model-gpt".to_string()),
            ("openai/gpt-4o".to_string(), "model-other".to_string()),
        ]);
        assert_eq!(
            shared.lookup("gpt-4o"),
            matched(&["model-gpt", "model-other"])
        );

        // Date-stamped variants of one name are the same identity too.
        let dated = ModelIndex::new(vec![
            ("qwen3-coder-2025-08-07".to_string(), "model-a".to_string()),
            ("qwen3-coder-2025-09-01".to_string(), "model-b".to_string()),
        ]);
        assert_eq!(
            dated.lookup("qwen3-coder"),
            matched(&["model-a", "model-b"])
        );

        // Distinct identities that both suffix the hint stay ambiguous.
        let nested = ModelIndex::new(vec![
            (
                "acme-labs/qwen3-coder-30b".to_string(),
                "model-a".to_string(),
            ),
            ("beta-labs/coder-30b".to_string(), "model-b".to_string()),
        ]);
        assert_eq!(
            nested.lookup("some-vendor-x-qwen3-coder-30b"),
            Matching::Ambiguous,
            "two distinct identities is a guess, never a write"
        );
    }

    #[test]
    fn batch_and_free_variants_do_not_collide_with_the_base_model() {
        // Live regression: the inflated catalog held `gpt-4o` and
        // `gpt-4o:batch` under one normalized key, so a benchmark row for
        // `gpt-4o` was reported ambiguous.
        let index = ModelIndex::new(vec![
            ("openai/gpt-4o".to_string(), "base".to_string()),
            ("openai/gpt-4o:batch".to_string(), "batch".to_string()),
            ("openai/gpt-4o:free".to_string(), "free".to_string()),
        ]);
        assert_eq!(index.lookup("openai/gpt-4o"), matched(&["base"]));
        assert_eq!(index.lookup("openai/gpt-4o:batch"), matched(&["batch"]));
        assert_eq!(index.lookup("openai/gpt-4o:free"), matched(&["free"]));
        assert_eq!(index.lookup("openai-gpt-4o"), matched(&["base"]));

        // Two providers of the base model attach to both; the `:free`
        // offering still stays on its own row.
        let two_providers = ModelIndex::new(vec![
            ("openrouter/openai/gpt-4o".to_string(), "or".to_string()),
            ("azure/openai/gpt-4o".to_string(), "az".to_string()),
            (
                "openrouter/openai/gpt-4o:free".to_string(),
                "free".to_string(),
            ),
        ]);
        assert_eq!(
            two_providers.lookup("GPT-4o"),
            matched(&["az", "or"]),
            "same identity, two providers"
        );
        assert_eq!(
            two_providers.lookup("GPT-4o:free"),
            matched(&["free"]),
            "an offering tag is not merged with the base model"
        );
    }

    const MATCHING_SHAPES: &str =
        include_str!("../tests/fixtures/benchmarks/matching_live_shapes.json");

    #[test]
    fn live_shaped_ids_and_benchmark_names_match_the_same_models() {
        // Catalog shapes observed live (OpenRouter ids + SailResearch/Sail
        // canonical names) against the benchmark names Arena/BenchLM publish.
        #[derive(serde::Deserialize)]
        struct Shapes {
            catalog: Vec<CatalogModel>,
            expectations: Vec<Expectation>,
            expected_counts: ExpectedCounts,
        }
        #[derive(serde::Deserialize)]
        struct CatalogModel {
            id: String,
            name: String,
        }
        #[derive(serde::Deserialize)]
        struct Expectation {
            hint: String,
            #[serde(default)]
            matched: Option<Vec<String>>,
            #[serde(default)]
            skipped: bool,
            #[serde(default)]
            ambiguous: bool,
        }
        #[derive(serde::Deserialize)]
        struct ExpectedCounts {
            published: usize,
            matched_models: usize,
            matched_hints: usize,
            skipped_unknown: usize,
            ambiguous: usize,
        }

        let shapes: Shapes = serde_json::from_str(MATCHING_SHAPES).expect("fixture parses");
        assert!(shapes.catalog.len() >= 10, "real-shape catalog ids");
        assert!(shapes.expectations.len() >= 10, "real-shape hints");

        let index = ModelIndex::new(
            shapes
                .catalog
                .iter()
                .map(|model| (model.name.clone(), model.id.clone())),
        );
        for expectation in &shapes.expectations {
            let actual = index.lookup(&expectation.hint);
            let expected = if let Some(ids) = &expectation.matched {
                matched(&ids.iter().map(String::as_str).collect::<Vec<_>>())
            } else if expectation.ambiguous {
                Matching::Ambiguous
            } else {
                assert!(expectation.skipped, "fixture row declares no outcome");
                Matching::Unknown
            };
            assert_eq!(actual, expected, "hint {:?}", expectation.hint);
        }

        // The counts the status endpoint reports for this run.
        let entries: Vec<Entry> = shapes
            .expectations
            .iter()
            .map(|expectation| Entry::new(expectation.hint.clone(), "text", 50.0))
            .collect();
        let (_, counts) = resolve(entries, &index, shapes.expectations.len());
        assert_eq!(counts.published, shapes.expected_counts.published);
        assert_eq!(counts.matched, shapes.expected_counts.matched_models);
        assert_eq!(counts.matched_hints, shapes.expected_counts.matched_hints);
        assert_eq!(
            counts.skipped_unknown,
            shapes.expected_counts.skipped_unknown
        );
        assert_eq!(counts.ambiguous, shapes.expected_counts.ambiguous);
        assert_eq!(counts.matched, 11, "one row per model, two for deepseek");

        // Vendor aliases normalize for matching only.
        assert_eq!(canonical_vendor("deepseek-ai"), "deepseek");
        assert_eq!(canonical_vendor("z-ai"), "zai-org");
        assert_eq!(canonical_vendor("x-ai"), "xai");
        assert_eq!(canonical_vendor("moonshotai"), "moonshot");
        assert_eq!(canonical_vendor("google-ai"), "google");
    }

    #[test]
    fn one_hint_attaches_to_every_model_with_its_identity() {
        let index = ModelIndex::new(vec![
            (
                "deepseek-ai/DeepSeek-V4.1-Flash-1017".to_string(),
                "sail".to_string(),
            ),
            (
                "deepseek/deepseek-v4.1-flash".to_string(),
                "openrouter".to_string(),
            ),
            ("openai/gpt-5.4".to_string(), "gpt".to_string()),
        ]);
        let entries = vec![
            Entry::new("deepseek-v4.1-flash", "text", 88.0),
            Entry::new("openai-gpt-5.4", "text", 87.0),
            Entry::new("some-unknown-model", "text", 60.0),
        ];
        let (picked, counts) = resolve(entries, &index, 3);
        assert_eq!(counts.published, 3);
        assert_eq!(counts.matched_hints, 2, "two hints resolved");
        assert_eq!(
            counts.matched, 3,
            "the deepseek hint attached to two models"
        );
        assert_eq!(counts.skipped_unknown, 1);
        assert_eq!(counts.ambiguous, 0);
        let ids: Vec<&str> = picked.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["gpt", "openrouter", "sail"]);
        assert!(picked.iter().all(|(_, entry)| entry.score != 60.0));

        // A hint written as one provider's full id reaches the other
        // provider of the same model too.
        assert_eq!(
            index.lookup("deepseek/deepseek-v4.1-flash"),
            matched(&["openrouter", "sail"])
        );
    }

    #[test]
    fn duplicate_entries_keep_the_best_rank_without_reading_as_ambiguous() {
        let index = ModelIndex::new(vec![("m".to_string(), "model-m".to_string())]);
        let mut first = Entry::new("m", "text", 90.0);
        first.rank = Some(8);
        let mut second = Entry::new("m (xHigh)", "text", 95.0);
        second.rank = Some(2);
        let (picked, counts) = resolve(vec![first, second], &index, 2);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].1.score, 95.0);
        assert_eq!(counts.matched, 1);
        assert_eq!(counts.matched_hints, 2);
        assert_eq!(counts.ambiguous, 0, "a duplicate is not an ambiguity");
        assert_eq!(counts.published, 2);
    }

    // -----------------------------------------------------------------------
    // Runs and persistence
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_fixture_sync_upserts_benchmarks_and_is_idempotent() {
        let app = TestApp::new().await;
        let uid = admin(&app).await;
        for name in ["claude-sonnet-5-5", "gpt-5-4", "gemini-3-1-pro"] {
            seed_model(&app, &uid, name).await;
        }
        let actor = Actor {
            user_id: &uid,
            ip: "127.0.0.1",
        };

        let first = run_source(
            &app.state.pool,
            SourceId::Arena,
            &arena_fetcher(),
            false,
            Some(&actor),
        )
        .await
        .expect("first sync");
        assert_eq!(first.status, RunStatus::Success);
        assert_eq!(first.counts.published, 4);
        assert_eq!(first.counts.inserted, 3);
        assert_eq!(first.counts.skipped_unknown, 1);
        assert_eq!(first.counts.updated, 0);

        let second = run_source(
            &app.state.pool,
            SourceId::Arena,
            &arena_fetcher(),
            false,
            Some(&actor),
        )
        .await
        .expect("second sync");
        assert_eq!(second.counts.inserted, 0);
        assert_eq!(second.counts.updated, 0);
        assert_eq!(second.counts.unchanged, 3);

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM benchmarks")
            .fetch_one(&app.state.pool)
            .await
            .expect("count");
        assert_eq!(rows, 3, "one row per model, never duplicated");

        let (source, category, scale, score, ci, rank, url): (
            String,
            String,
            String,
            f64,
            Option<f64>,
            Option<i64>,
            String,
        ) = sqlx::query_as(
            "SELECT source, category, scale, score, ci, rank, source_url \
             FROM benchmarks WHERE model_id = ?",
        )
        .bind(
            sqlx::query_scalar::<_, String>(
                "SELECT id FROM models WHERE canonical_name = 'claude-sonnet-5-5'",
            )
            .fetch_one(&app.state.pool)
            .await
            .expect("model id"),
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("row");
        assert_eq!(source, "arena");
        assert_eq!(category, "code");
        assert_eq!(scale, "elo");
        assert_eq!(score, 1772.2);
        assert!(
            ci.is_some_and(|ci| (ci - 9.7).abs() < 1e-9),
            "CI half-width: {ci:?}"
        );
        assert_eq!(rank, Some(3));
        assert_eq!(url, ARENA_ATTRIBUTION_URL);

        let runs: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT source, status, published, matched FROM benchmark_sync_runs \
             ORDER BY finished_at, id",
        )
        .fetch_all(&app.state.pool)
        .await
        .expect("runs");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].0, "arena");
        assert_eq!(runs[0].1, "success");
        assert_eq!(runs[0].2, 4);
        assert_eq!(runs[0].3, 3);

        let audits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'sync.benchmark_sources'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("audit count");
        assert_eq!(audits, 2, "each admin-triggered source run is audited");
    }

    #[tokio::test]
    async fn a_dry_run_writes_nothing() {
        let app = TestApp::new().await;
        let uid = admin(&app).await;
        seed_model(&app, &uid, "claude-sonnet-5-5").await;
        let outcome = run_source(
            &app.state.pool,
            SourceId::Arena,
            &arena_fetcher(),
            true,
            None,
        )
        .await
        .expect("dry run");
        assert!(outcome.dry_run);
        assert_eq!(outcome.counts.matched, 1);

        // `seed_model` itself audits; the dry run must add nothing.
        let audits_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&app.state.pool)
            .await
            .expect("audit count");
        for table in ["benchmarks", "benchmark_sync_runs"] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&app.state.pool)
                .await
                .expect("count");
            assert_eq!(count, 0, "{table} must stay empty on a dry run");
        }
        let audits_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&app.state.pool)
            .await
            .expect("audit count");
        assert_eq!(audits_before, audits_after, "a dry run is not audited");
    }

    #[tokio::test]
    async fn a_failing_source_records_an_error_run_while_others_succeed() {
        let app = TestApp::new().await;
        let uid = admin(&app).await;
        for name in ["claude-sonnet-5-5", "gpt-5-4", "gemini-3-1-pro"] {
            seed_model(&app, &uid, name).await;
        }
        let fetcher = FixtureFetcher::new(vec![
            ("offset=0&length=100", ARENA_PAGE_1),
            ("offset=3&length=100", ARENA_PAGE_2),
            // No route for BenchmarkList: that source fails.
            ("benchlm.ai/models", BENCHLM_HTML),
            ("_next/data", BENCHLM),
        ]);
        let outcomes = run_sources(
            &app.state.pool,
            &[SourceId::Arena, SourceId::BenchmarkList, SourceId::BenchLm],
            &fetcher,
            false,
            None,
        )
        .await;
        assert_eq!(outcomes.len(), 3);
        assert_eq!(outcomes[0].status, RunStatus::Success);
        assert_eq!(outcomes[1].status, RunStatus::Error);
        assert!(
            outcomes[1]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("benchmarklist")),
            "{:?}",
            outcomes[1].error
        );
        assert_eq!(outcomes[2].status, RunStatus::Success);

        let statuses: Vec<(String, String)> =
            sqlx::query_as("SELECT source, status FROM benchmark_sync_runs ORDER BY source")
                .fetch_all(&app.state.pool)
                .await
                .expect("runs");
        assert_eq!(
            statuses,
            vec![
                ("arena".to_string(), "success".to_string()),
                ("benchlm".to_string(), "success".to_string()),
                ("benchmarklist".to_string(), "error".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn an_override_still_wins_after_a_sync() {
        let app = TestApp::new().await;
        let uid = admin(&app).await;
        let model_id = seed_model(&app, &uid, "claude-sonnet-5-5").await;
        crate::benchmarks::set_override(
            &app.state.pool,
            &Actor {
                user_id: &uid,
                ip: "127.0.0.1",
            },
            &model_id,
            1900.0,
        )
        .await
        .expect("override");

        run_source(
            &app.state.pool,
            SourceId::Arena,
            &arena_fetcher(),
            false,
            None,
        )
        .await
        .expect("sync");

        let quality =
            crate::benchmarks::quality_for_models(&app.state.pool, std::slice::from_ref(&model_id))
                .await
                .expect("quality");
        assert_eq!(
            quality[&model_id].source,
            crate::routing::QualitySource::Override
        );
        assert_eq!(quality[&model_id].score, 1900.0);
        let stored: f64 =
            sqlx::query_scalar("SELECT score FROM model_quality_overrides WHERE model_id = ?")
                .bind(&model_id)
                .fetch_one(&app.state.pool)
                .await
                .expect("override row");
        assert_eq!(stored, 1900.0, "the sync never writes overrides");
    }

    #[tokio::test]
    async fn non_elo_scores_are_stored_but_do_not_feed_quality() {
        let app = TestApp::new().await;
        let uid = admin(&app).await;
        let model_id = seed_model(&app, &uid, "openai-gpt-5-4").await;
        let fetcher = FixtureFetcher::new(vec![(BENCHMARKLIST_URL, BENCHMARKLIST)]);
        run_source(
            &app.state.pool,
            SourceId::BenchmarkList,
            &fetcher,
            false,
            None,
        )
        .await
        .expect("sync");

        let quality =
            crate::benchmarks::quality_for_models(&app.state.pool, std::slice::from_ref(&model_id))
                .await
                .expect("quality");
        assert!(
            quality[&model_id].imputed,
            "a capability score is not on the Elo scale (D25)"
        );
        let scale: String = sqlx::query_scalar("SELECT scale FROM benchmarks WHERE model_id = ?")
            .bind(&model_id)
            .fetch_one(&app.state.pool)
            .await
            .expect("row");
        assert_eq!(scale, "capability");
    }

    #[test]
    fn freshness_follows_the_five_hour_ttl() {
        let now = DateTime::parse_from_rfc3339("2026-11-16T12:00:00.000Z")
            .expect("ts")
            .to_utc();
        assert!(!is_fresh(None, &now));
        assert!(is_fresh(Some("2026-11-16T11:00:00.000Z"), &now));
        assert!(is_fresh(Some("2026-11-16T07:00:01.000Z"), &now));
        assert!(!is_fresh(Some("2026-11-16T06:59:59.000Z"), &now));
        assert!(!is_fresh(Some("2026-11-16T13:00:00.000Z"), &now));
    }

    #[tokio::test]
    async fn only_stale_sources_are_due() {
        let app = TestApp::new().await;
        let now = Utc::now();
        let fresh = (now - chrono::Duration::hours(1))
            .format(crate::auth::TS_FMT)
            .to_string();
        let stale = (now - chrono::Duration::hours(6))
            .format(crate::auth::TS_FMT)
            .to_string();
        for (source, at) in [(SourceId::Arena, &fresh), (SourceId::BenchmarkList, &stale)] {
            sqlx::query(
                "INSERT INTO benchmark_sync_runs \
                   (id, source, status, started_at, finished_at, duration_ms) \
                 VALUES (?, ?, 'success', ?, ?, 1)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(source.as_str())
            .bind(at)
            .bind(at)
            .execute(&app.state.pool)
            .await
            .expect("run row");
        }
        let due = sources_due(&app.state.pool, &now).await.expect("due");
        assert_eq!(due, vec![SourceId::BenchmarkList, SourceId::BenchLm]);
    }

    #[tokio::test]
    async fn run_rows_are_pruned_to_the_newest_thirty_per_source() {
        let app = TestApp::new().await;
        for i in 0..35 {
            let ts = format!("2026-01-01T00:{:02}:{:02}.000Z", i / 60, i % 60);
            sqlx::query(
                "INSERT INTO benchmark_sync_runs \
                   (id, source, status, started_at, finished_at, duration_ms) \
                 VALUES (?, 'arena', 'success', ?, ?, 1)",
            )
            .bind(format!("run-{i:02}"))
            .bind(&ts)
            .bind(&ts)
            .execute(&app.state.pool)
            .await
            .expect("synthetic run");
        }
        let uid = admin(&app).await;
        seed_model(&app, &uid, "claude-sonnet-5-5").await;
        run_source(
            &app.state.pool,
            SourceId::Arena,
            &arena_fetcher(),
            false,
            None,
        )
        .await
        .expect("sync");
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM benchmark_sync_runs WHERE source = 'arena'")
                .fetch_one(&app.state.pool)
                .await
                .expect("count");
        assert_eq!(total, 30);
    }

    // -----------------------------------------------------------------------
    // HttpFetcher hardening (live finding 2026-11-17: BenchmarkList failed
    // from the gateway while curl succeeded; Arena 502/503 under load)
    // -----------------------------------------------------------------------

    use std::sync::{Arc, Mutex as StdMutex};

    /// One scripted HTTP server: one response per accepted connection, in
    /// order, plus the request head each connection sent.
    struct Scripted {
        addr: std::net::SocketAddr,
        seen: Arc<StdMutex<Vec<String>>>,
    }

    impl Scripted {
        fn url(&self) -> String {
            format!("http://{}/models", self.addr)
        }

        fn requests(&self) -> Vec<String> {
            self.seen.lock().expect("request log").clone()
        }
    }

    async fn spawn_scripted(responses: Vec<String>) -> Scripted {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scripted listener");
        let addr = listener.local_addr().expect("scripted address");
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let read = stream.read(&mut buf).await.unwrap_or(0);
                log.lock()
                    .expect("request log")
                    .push(String::from_utf8_lossy(&buf[..read]).into_owned());
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        Scripted { addr, seen }
    }

    fn ok_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn status_response(status: &str) -> String {
        format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
    }

    /// A 200 whose `Content-Length` promises more than the server sends before
    /// closing: reqwest reports a body decode error mid-transfer.
    fn truncated_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len() + 500
        )
    }

    /// A chunked 200 that sends one complete chunk and closes without the
    /// terminating `0\r\n\r\n`: the whole document arrived, the framing broke.
    fn unterminated_chunked_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
             transfer-encoding: chunked\r\nconnection: close\r\n\r\n{:x}\r\n{body}\r\n",
            body.len()
        )
    }

    fn fast_fetcher() -> HttpFetcher {
        HttpFetcher::with_retry_base(Duration::from_millis(1)).expect("http client")
    }

    #[tokio::test]
    async fn every_request_states_a_user_agent_and_asks_for_json() {
        // benchmarklist.com's CDN closes TLS on requests without a
        // `User-Agent`; the gateway used to send reqwest's default (none).
        let scripted = spawn_scripted(vec![ok_response("{\"rows\":[]}")]).await;
        let body = fast_fetcher().get(&scripted.url()).await.expect("fetch");
        assert_eq!(body, "{\"rows\":[]}");

        let head = scripted.requests().join("\n").to_lowercase();
        assert!(head.contains("user-agent: pareto-ai/"), "{head}");
        assert!(head.contains("accept: application/json"), "{head}");
        assert!(head.contains("accept-encoding: identity"), "{head}");
    }

    #[tokio::test]
    async fn a_user_agent_is_required_not_optional() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ua listener");
        let addr = listener.local_addr().expect("ua address");
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let read = stream.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..read]).to_lowercase();
                let response = if head.contains("user-agent: pareto-ai/") {
                    ok_response("{\"data\":[]}")
                } else {
                    status_response("403 Forbidden")
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        let body = fast_fetcher()
            .get(&format!("http://{addr}/models"))
            .await
            .expect("the gateway identifies itself, so the server answers");
        assert_eq!(body, "{\"data\":[]}");
    }

    #[tokio::test]
    async fn a_failed_send_is_retried_and_succeeds() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind flaky listener");
        let addr = listener.local_addr().expect("flaky address");
        let body = "{\"data\":[]}";
        tokio::spawn(async move {
            let mut first = true;
            while let Ok((mut stream, _)) = listener.accept().await {
                if first {
                    first = false;
                    // Closing without a byte makes `send()` fail.
                    let _ = stream.shutdown().await;
                    continue;
                }
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let response = ok_response(body);
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        let fetched = fast_fetcher()
            .get(&format!("http://{addr}/models"))
            .await
            .expect("an attempt succeeds");
        assert_eq!(fetched, body);
    }

    #[tokio::test]
    async fn an_early_closed_body_is_retried_and_counted_by_bytes() {
        // The live failure: the response head arrives, the body is cut short.
        // That is an interrupted transfer, not a JSON shape problem, so a
        // fresh connection gets the whole document.
        let scripted = spawn_scripted(vec![
            truncated_response("{\"rows\": [{\"id\": \"a\"}"),
            ok_response("{\"rows\": []}"),
        ])
        .await;
        let body = fast_fetcher()
            .get(&scripted.url())
            .await
            .expect("retry wins");
        assert_eq!(body, "{\"rows\": []}");
        let requests = scripted.requests();
        assert_eq!(requests.len(), 2);
        let retry = requests[1].to_lowercase();
        assert!(retry.contains("connection: close"), "{retry}");
        assert!(
            retry.starts_with("get /models http/1.1"),
            "the retry is pinned to HTTP/1.1: {retry}"
        );
    }

    #[tokio::test]
    async fn a_5xx_is_retried_with_backoff_and_succeeds() {
        let scripted = spawn_scripted(vec![
            status_response("503 Service Unavailable"),
            status_response("502 Bad Gateway"),
            ok_response("{\"rows\": [1]}"),
        ])
        .await;
        let body = fast_fetcher()
            .get(&scripted.url())
            .await
            .expect("third try");
        assert_eq!(body, "{\"rows\": [1]}");
        assert_eq!(scripted.requests().len(), 3, "three attempts, no more");
    }

    #[tokio::test]
    async fn a_permanent_5xx_reports_every_attempt() {
        let scripted = spawn_scripted(vec![
            status_response("503 Service Unavailable"),
            status_response("503 Service Unavailable"),
            status_response("503 Service Unavailable"),
        ])
        .await;
        let error = fast_fetcher()
            .get(&scripted.url())
            .await
            .expect_err("three 503s are a failure");
        let message = error.to_string();
        assert!(message.contains("attempt 3 of 3"), "{message}");
        assert!(message.contains("503"), "{message}");
        assert_eq!(scripted.requests().len(), 3);
    }

    #[tokio::test]
    async fn a_complete_document_with_a_broken_chunk_terminator_is_used() {
        // The shape that fits the recorded live failure: the whole 10 MiB
        // document arrives and the terminator is dropped. curl writes the
        // file; reqwest reported "error decoding response body". The bytes are
        // the answer, so they are used, and no retry is spent on them.
        let body = "{\"rows\": [{\"id\": \"live-shape\"}]}";
        let scripted = spawn_scripted(vec![unterminated_chunked_response(body)]).await;
        let fetched = fast_fetcher().get(&scripted.url()).await.expect("salvaged");
        assert_eq!(fetched, body);
        assert_eq!(scripted.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_truncated_document_is_not_salvaged() {
        // Half a document is never returned as if it were whole, on any
        // attempt.
        let truncated = truncated_response("{\"rows\": [{\"id\": \"a\"}");
        let scripted = spawn_scripted(vec![truncated.clone(), truncated.clone(), truncated]).await;
        let error = fast_fetcher()
            .get(&scripted.url())
            .await
            .expect_err("every attempt is truncated");
        let message = error.to_string();
        assert!(message.contains("attempt 3 of 3"), "{message}");
        assert!(message.contains("(body)"), "{message}");
        assert!(message.contains("bytes"), "{message}");
        assert_eq!(scripted.requests().len(), 3);
    }

    #[tokio::test]
    async fn a_client_error_is_not_retried() {
        let scripted = spawn_scripted(vec![status_response("404 Not Found")]).await;
        let error = fast_fetcher()
            .get(&scripted.url())
            .await
            .expect_err("404 is final");
        assert!(error.to_string().contains("404"), "{error}");
        assert_eq!(scripted.requests().len(), 1);
    }

    #[tokio::test]
    async fn an_unreachable_host_reports_the_last_attempt() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind probe listener");
        let addr = listener.local_addr().expect("probe address");
        drop(listener);

        let error = fast_fetcher()
            .get(&format!("http://{addr}/models"))
            .await
            .expect_err("no server listens");
        let message = error.to_string();
        assert!(message.contains("attempt 3 of 3"), "{message}");
    }

    #[tokio::test]
    async fn a_failed_arena_page_fails_the_source_instead_of_truncating() {
        let app = TestApp::new().await;
        let fetcher = FixtureFetcher::new(vec![("offset=0&length=100", ARENA_PAGE_1)]);
        let outcome = run_source(&app.state.pool, SourceId::Arena, &fetcher, false, None)
            .await
            .expect("run");
        assert_eq!(outcome.status, RunStatus::Error);
        assert_eq!(outcome.counts.failed_pages, 1);
        let error = outcome.error.expect("error text");
        assert!(error.contains("offset 3"), "{error}");

        let stored: (i64, i64) = sqlx::query_as(
            "SELECT failed_pages, published FROM benchmark_sync_runs WHERE source = 'arena'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("run row");
        assert_eq!(stored, (1, 0), "the failed page is recorded, not hidden");
    }

    #[tokio::test]
    async fn one_benchmark_row_attaches_to_every_model_of_its_identity() {
        let app = TestApp::new().await;
        let uid = admin(&app).await;
        let sail = seed_model(&app, &uid, "deepseek-ai/DeepSeek-V4.1-Flash-1017").await;
        let openrouter = seed_model(&app, &uid, "deepseek/deepseek-v4.1-flash").await;
        let fetcher = FixtureFetcher::new(vec![(
            "directory-data/models.json",
            r#"{"rows": [
                {"id": "deepseek-v4.1-flash", "capability_score": 88.0},
                {"id": "no-such-model", "capability_score": 60.0}
            ]}"#,
        )]);
        let outcome = run_source(
            &app.state.pool,
            SourceId::BenchmarkList,
            &fetcher,
            false,
            None,
        )
        .await
        .expect("sync");
        assert_eq!(outcome.status, RunStatus::Success);
        assert_eq!(outcome.counts.published, 2);
        assert_eq!(outcome.counts.matched_hints, 1);
        assert_eq!(outcome.counts.matched, 2, "one row per model");
        assert_eq!(outcome.counts.skipped_unknown, 1);
        assert_eq!(outcome.counts.inserted, 2);

        let rows: Vec<(String, f64, String)> = sqlx::query_as(
            "SELECT model_id, score, scale FROM benchmarks WHERE source = 'benchmarklist' \
             ORDER BY model_id",
        )
        .fetch_all(&app.state.pool)
        .await
        .expect("rows");
        assert_eq!(rows.len(), 2);
        let ids: Vec<&str> = rows.iter().map(|(id, _, _)| id.as_str()).collect();
        assert!(ids.contains(&openrouter.as_str()), "{ids:?}");
        assert!(ids.contains(&sail.as_str()), "{ids:?}");
        assert!(
            rows.iter()
                .all(|(_, score, scale)| *score == 88.0 && scale == "capability")
        );

        let run: (i64, i64, i64) = sqlx::query_as(
            "SELECT matched, matched_hints, failed_pages FROM benchmark_sync_runs \
             WHERE source = 'benchmarklist'",
        )
        .fetch_one(&app.state.pool)
        .await
        .expect("run row");
        assert_eq!(run, (2, 1, 0));
    }

    #[tokio::test]
    async fn a_registry_group_sibling_inherits_the_benchmark_score() {
        let app = TestApp::new().await;
        let uid = admin(&app).await;
        let base = seed_model(&app, &uid, "openai/gpt-6-luna").await;
        let pro = seed_model(&app, &uid, "openai/gpt-6-luna-pro").await;
        // The external source groups both spellings under the base canonical
        // id; without it the benchmark only knows `GPT-6 Luna`.
        for (alias, canonical) in [
            ("openai/gpt-6-luna", "openai/gpt-6-luna"),
            ("openai/gpt-6-luna-pro", "openai/gpt-6-luna"),
        ] {
            sqlx::query(
                "INSERT INTO model_registry (source, alias, canonical, fetched_at) \
                 VALUES ('models_dev', ?, ?, ?)",
            )
            .bind(alias)
            .bind(canonical)
            .bind(catalog::now_ts())
            .execute(&app.state.pool)
            .await
            .expect("registry row");
        }
        let fetcher = FixtureFetcher::new(vec![(
            "offset=0&length=100",
            r#"{"rows":[{"row":{"model_name":"GPT-6 Luna (Max)","rating":1578.0,
                "category":"overall","rank":1}}],"num_rows_total":1}"#,
        )]);
        let outcome = run_source(&app.state.pool, SourceId::Arena, &fetcher, false, None)
            .await
            .expect("sync");
        assert_eq!(outcome.status, RunStatus::Success);
        assert_eq!(outcome.counts.published, 1);
        assert_eq!(outcome.counts.matched_hints, 1);
        assert_eq!(
            outcome.counts.matched, 2,
            "the registry sibling inherits the row"
        );

        let rows: Vec<(String, f64, String)> = sqlx::query_as(
            "SELECT model_id, score, scale FROM benchmarks WHERE source = 'arena' \
             ORDER BY model_id",
        )
        .fetch_all(&app.state.pool)
        .await
        .expect("rows");
        assert_eq!(rows.len(), 2, "one arena row per model");
        let mut ids: Vec<&str> = rows.iter().map(|(id, _, _)| id.as_str()).collect();
        ids.sort();
        let mut expected = vec![base.as_str(), pro.as_str()];
        expected.sort();
        assert_eq!(ids, expected);
        assert!(
            rows.iter()
                .all(|(_, score, scale)| (*score - 1578.0).abs() < 1e-9 && scale == "elo")
        );
    }
}
