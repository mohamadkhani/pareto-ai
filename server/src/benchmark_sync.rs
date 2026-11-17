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
//! **Matching.** Both sides are normalized ([`normalize_model_key`]): lower
//! case, vendor prefix (`vendor/`) dropped, non-alphanumerics collapsed to
//! `-`, parenthetical tier markers removed, and the suffix markers
//! `:free`, `-free`, `-latest`, `-preview`, `-thinking`, `-nothinking`,
//! `-xhigh`, `-high`, `-medium`, `-low` plus trailing date stamps stripped.
//! A hint is matched against `models.canonical_name` and
//! `offerings.upstream_model_id`; exactly one distinct model is a match, zero
//! is `skipped_unknown`, and more than one is `ambiguous` — never guessed.
//! Two entries of one run resolving to the same model keep the better rank
//! and count the loser as ambiguous.
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

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sqlx::{SqliteConnection, SqlitePool};

use crate::catalog::{self, Actor};

/// Freshness window of one source's downloaded scores.
pub const SYNC_TTL: Duration = Duration::from_secs(5 * 60 * 60);

/// Hard cap for one source (all its HTTP requests and parsing together).
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(60);

/// Delay before the first background sync after boot.
pub const JOB_START_DELAY: Duration = Duration::from_secs(60);

/// How often the background job checks which sources are due.
pub const JOB_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Per-HTTP-request timeout (a source can issue several requests).
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

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
    #[error("the {what} response is malformed: {detail}")]
    Parse { what: &'static str, detail: String },
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit write failed: {0}")]
    Audit(#[from] catalog::CatalogError),
}

impl SyncError {
    /// The uniform API error for this failure.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        match self {
            Self::Db(e) => {
                crate::error::ApiError::internal("benchmark sync", e).with_request_id(rid)
            }
            Self::Audit(e) => e.into_api_error(rid),
            // Source failures are outcomes, never propagated this far.
            Self::Fetch { .. } | Self::Parse { .. } => {
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

/// The production fetcher: a pooled HTTP client with [`FETCH_TIMEOUT`].
#[derive(Clone)]
pub struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    /// Builds the HTTP client. Fails only when the TLS/HTTP stack cannot be
    /// initialized.
    pub fn new() -> Result<Self, SyncError> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent("pareto-ai-gateway/0.1 benchmark-sync")
            .build()
            .map_err(|e| SyncError::Fetch {
                url: "http client".to_string(),
                detail: e.to_string(),
            })?;
        Ok(Self { client })
    }
}

impl Fetcher for HttpFetcher {
    fn get(&self, url: &str) -> impl Future<Output = Result<String, SyncError>> + Send {
        let client = self.client.clone();
        let url = url.to_string();
        async move {
            // Two attempts: a connect failure or a body decode failure (an
            // interrupted transfer, not a JSON shape problem) gets one fresh
            // connection, because `send()` surfaces a mid-body decode error
            // too. Both the send and the body read are inside the loop, and
            // the attempt-level error is kept for the final message. The
            // request uses identity encoding and the body is decoded lossily
            // from raw bytes, so an encoding/charset disagreement cannot
            // masquerade as a parse error. Status, headers, elapsed time and
            // the first bytes are logged at debug level, which is enough to
            // tell transport from serde.
            let mut last_error: Option<SyncError> = None;
            for attempt in 0..2 {
                let started = std::time::Instant::now();
                let mut request = client
                    .get(&url)
                    .header(reqwest::header::ACCEPT_ENCODING, "identity");
                if attempt == 1 {
                    request = request.version(reqwest::Version::HTTP_11);
                }
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
                            detail: format!("attempt {} (send): {error}", attempt + 1),
                        });
                        continue;
                    }
                };
                let status = response.status();
                let headers = response.headers().clone();
                if !status.is_success() {
                    return Err(SyncError::Fetch {
                        url,
                        detail: format!("HTTP {status}"),
                    });
                }
                let header = |name: &str| {
                    headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("")
                        .to_string()
                };
                match response.bytes().await {
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
                    Err(error) => {
                        tracing::debug!(
                            url = %url,
                            status = %status,
                            content_type = %header("content-type"),
                            content_encoding = %header("content-encoding"),
                            content_length = %header("content-length"),
                            attempt,
                            error = %error,
                            "benchmark source body failed to decode"
                        );
                        last_error = Some(SyncError::Fetch {
                            url: url.clone(),
                            detail: format!("attempt {} (body): {error}", attempt + 1),
                        });
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
/// leaderboard). Failure mode: [`SyncError::Fetch`] or [`SyncError::Parse`].
pub async fn fetch_arena<F: Fetcher>(fetcher: &F) -> Result<Vec<Entry>, SyncError> {
    let mut offset = 0usize;
    let mut entries = Vec::new();
    for _ in 0..ARENA_MAX_PAGES {
        let url = format!("{ARENA_ROWS_URL}&offset={offset}&length={ARENA_PAGE_SIZE}");
        let raw = fetcher.get(&url).await?;
        let page = parse_arena_page(&raw)?;
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

// ---------------------------------------------------------------------------
// Model matching
// ---------------------------------------------------------------------------

/// Normalizes a model name or id for matching.
///
/// Lower-cases, drops parenthetical tier groups (`(xHigh)`), replaces every
/// run of non-alphanumerics with `-`, keeps only the part after the last
/// `/` (vendor prefix), and trims dashes. Variant suffixes (`:free`, `:batch`,
/// `-preview`, `-latest`, dates, …) are kept here: the exact key identifies a
/// distinct offering. [`ModelIndex::lookup`] tries the exact key first and
/// only then a suffix-stripped fallback that must identify exactly one model.
/// The result is empty when the input has no alphanumerics.
pub fn normalize_model_key(raw: &str) -> String {
    let mut lowered = raw.trim().to_lowercase();

    // Remove parenthetical groups, e.g. "Claude Sonnet 5.2 (xHigh)".
    while let (Some(start), Some(end)) = (lowered.find('('), lowered.find(')')) {
        if end <= start {
            break;
        }
        lowered.replace_range(start..=end, " ");
    }

    // Vendor prefix: the canonical name and the upstream id share the tail.
    if let Some((_, tail)) = lowered.rsplit_once('/') {
        lowered = tail.to_string();
    }

    let mut key = String::with_capacity(lowered.len());
    let mut last_dash = true;
    for character in lowered.chars() {
        if character.is_ascii_alphanumeric() {
            key.push(character);
            last_dash = false;
        } else if !last_dash {
            key.push('-');
            last_dash = true;
        }
    }
    while key.ends_with('-') {
        key.pop();
    }
    key
}

/// Strips trailing tier markers and date stamps for the *fallback* match
/// only. `free` and `batch` are deliberately absent: they denote distinct
/// offerings, not the same model under another label.
fn strip_variant_suffixes(mut key: String) -> String {
    const SUFFIXES: [&str; 9] = [
        "latest",
        "preview",
        "thinking",
        "nothinking",
        "xhigh",
        "high",
        "medium",
        "low",
        "exp",
    ];
    while let Some((head, tail)) = key.rsplit_once('-') {
        if !SUFFIXES.contains(&tail) {
            break;
        }
        key.truncate(head.len());
    }

    // Trailing date stamps: "-2025-08-07" or a compact "-20250807".
    loop {
        let parts: Vec<&str> = key.rsplitn(4, '-').collect();
        let compact = parts.first().and_then(|last| {
            if last.len() == 8 && last.starts_with("20") && last.bytes().all(|b| b.is_ascii_digit())
            {
                Some(key.len() - last.len() - 1)
            } else {
                None
            }
        });
        if let Some(keep) = compact {
            key.truncate(keep);
            continue;
        }
        let date_suffix = parts.len() == 4
            && parts[0].len() == 2
            && parts[1].len() == 2
            && parts[2].len() == 4
            && parts[2].starts_with("20")
            && parts[..3]
                .iter()
                .all(|part| part.bytes().all(|b| b.is_ascii_digit()));
        if date_suffix {
            key.truncate(key.len() - 11);
            continue;
        }
        break;
    }
    key
}

/// Maps an exact key's model ids to a resolution.
fn best_match(ids: &[String]) -> Matching {
    match ids {
        [model_id] => Matching::Matched(model_id.clone()),
        _ => Matching::Ambiguous,
    }
}

/// How one hint resolved against the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matching {
    Matched(String),
    Unknown,
    Ambiguous,
}

/// A normalized index over `models.canonical_name` and
/// `offerings.upstream_model_id`.
///
/// `keys` is the exact normalized key (distinct offerings stay distinct:
/// `gpt-4o` and `gpt-4o:batch` are two keys). `bases` is the suffix-stripped
/// fallback index, consulted only when the exact key is absent and only when
/// it names exactly one model.
#[derive(Debug, Default)]
pub struct ModelIndex {
    keys: HashMap<String, Vec<String>>,
    bases: HashMap<String, Vec<String>>,
}

impl ModelIndex {
    /// Builds an index from `(hint, model_id)` pairs. Duplicate ids for one
    /// key collapse; distinct ids make that key ambiguous.
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut index = Self::default();
        for (hint, model_id) in pairs {
            let key = normalize_model_key(&hint);
            if key.is_empty() {
                continue;
            }
            let ids = index.keys.entry(key.clone()).or_default();
            if !ids.contains(&model_id) {
                ids.push(model_id.clone());
            }
            let base = strip_variant_suffixes(key);
            if base.is_empty() {
                continue;
            }
            let ids = index.bases.entry(base).or_default();
            if !ids.contains(&model_id) {
                ids.push(model_id);
            }
        }
        index
    }

    /// Resolves one hint: exactly one model, none, or several.
    ///
    /// 1. The exact normalized key wins, so `:free`/`:batch` variants never
    ///    collide with their base model.
    /// 2. Otherwise a suffix-stripped fallback (`-preview`, `-latest`, dates,
    ///    tier markers) is tried; it must identify exactly one model.
    /// 3. Finally a hyphen-boundary vendor prefix match (`openai-gpt-5-4` ->
    ///    `gpt-5-4`, BenchmarkList's id shape) is tried under the same
    ///    uniqueness rule.
    pub fn lookup(&self, hint: &str) -> Matching {
        let key = normalize_model_key(hint);
        if key.is_empty() {
            return Matching::Unknown;
        }
        if let Some(ids) = self.keys.get(&key) {
            return best_match(ids);
        }

        let base = strip_variant_suffixes(key.clone());
        if !base.is_empty()
            && let Some(ids) = self.bases.get(&base)
        {
            return best_match(ids);
        }

        let mut ids: Vec<&str> = Vec::new();
        for (index_key, index_ids) in &self.keys {
            if key.ends_with(&format!("-{index_key}")) {
                for id in index_ids {
                    if !ids.contains(&id.as_str()) {
                        ids.push(id);
                    }
                }
            }
        }
        match ids.as_slice() {
            [model_id] => Matching::Matched((*model_id).to_string()),
            [] => Matching::Unknown,
            _ => Matching::Ambiguous,
        }
    }
}

/// Loads the catalog index from `models` and `offerings`.
/// Failure mode: [`SyncError::Db`].
pub async fn load_model_index(pool: &SqlitePool) -> Result<ModelIndex, SyncError> {
    let models: Vec<(String, String)> = sqlx::query_as("SELECT canonical_name, id FROM models")
        .fetch_all(pool)
        .await?;
    let offerings: Vec<(String, String)> = sqlx::query_as(
        "SELECT upstream_model_id, model_id FROM offerings WHERE upstream_model_id IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    Ok(ModelIndex::new(models.into_iter().chain(offerings)))
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
    /// Entries that resolved to one model and were kept (one per model).
    pub matched: usize,
    pub skipped_unknown: usize,
    /// Multi-model matches plus duplicate entries for one model.
    pub ambiguous: usize,
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

/// Resolves entries to models and dedupes per model. Never guesses: an entry
/// with no unique match is counted, not written.
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
            Matching::Matched(model_id) => match picked.get(&model_id) {
                None => {
                    picked.insert(model_id, entry);
                }
                Some(current) => {
                    counts.ambiguous += 1;
                    if better_rank(&entry, current) {
                        picked.insert(model_id, entry);
                    }
                }
            },
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
        counts: Counts::default(),
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
            skipped_unknown, ambiguous, inserted, updated, unchanged, source_url, error) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(&run_id)
    .bind(source.as_str())
    .bind(status)
    .bind(fetched_at)
    .bind(catalog::now_ts())
    .bind(duration_ms)
    .bind(counts.published as i64)
    .bind(counts.matched as i64)
    .bind(counts.skipped_unknown as i64)
    .bind(counts.ambiguous as i64)
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
                "skipped_unknown": counts.skipped_unknown,
                "ambiguous": counts.ambiguous,
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
            skipped_unknown, ambiguous, inserted, updated, unchanged, source_url, error) \
         VALUES (?, ?, ?, ?, ?, ?, 0, 0, 0, 0, 0, 0, 0, ?, ?)",
    )
    .bind(&run_id)
    .bind(outcome.source.as_str())
    .bind(outcome.status.as_str())
    .bind(&outcome.started_at)
    .bind(&outcome.finished_at)
    .bind(outcome.duration_ms)
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
    pub skipped_unknown: i64,
    pub ambiguous: i64,
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
                skipped_unknown, ambiguous, inserted, updated, unchanged, source_url, error \
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

    #[test]
    fn matching_is_exact_first_then_a_unique_suffix_fallback() {
        let index = ModelIndex::new(vec![
            ("openai/gpt-4o".to_string(), "model-gpt".to_string()),
            ("claude-sonnet-4.5".to_string(), "model-claude".to_string()),
            ("acme-widget-7b".to_string(), "model-widget".to_string()),
        ]);
        assert_eq!(
            index.lookup("GPT-4o"),
            Matching::Matched("model-gpt".into())
        );
        // A variant the catalog does not have is unknown, never the base.
        assert_eq!(index.lookup("openai/gpt-4o:free"), Matching::Unknown);
        assert_eq!(
            index.lookup("Claude Sonnet 4.5"),
            Matching::Matched("model-claude".into())
        );
        // BenchmarkList writes the vendor with a hyphen; the suffix match
        // strips it. Unknown hints are skipped, never guessed.
        assert_eq!(
            index.lookup("openai-gpt-4o"),
            Matching::Matched("model-gpt".into())
        );
        assert_eq!(index.lookup("totally-unknown"), Matching::Unknown);

        // Two distinct models under one key are ambiguous, not a guess.
        let ambiguous = ModelIndex::new(vec![
            ("gpt-4o".to_string(), "model-gpt".to_string()),
            ("openai/gpt-4o".to_string(), "model-other".to_string()),
        ]);
        assert_eq!(ambiguous.lookup("gpt-4o"), Matching::Ambiguous);

        // A suffix fallback matches only when exactly one model has that base.
        let unique = ModelIndex::new(vec![(
            "qwen3-coder-2025-08-07".to_string(),
            "model-qwen".to_string(),
        )]);
        assert_eq!(
            unique.lookup("qwen3-coder"),
            Matching::Matched("model-qwen".into())
        );
        let competing = ModelIndex::new(vec![
            ("qwen3-coder-2025-08-07".to_string(), "model-a".to_string()),
            ("qwen3-coder-2025-09-01".to_string(), "model-b".to_string()),
        ]);
        assert_eq!(competing.lookup("qwen3-coder"), Matching::Ambiguous);
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
        assert_eq!(
            index.lookup("openai/gpt-4o"),
            Matching::Matched("base".into())
        );
        assert_eq!(
            index.lookup("openai/gpt-4o:batch"),
            Matching::Matched("batch".into())
        );
        assert_eq!(
            index.lookup("openai/gpt-4o:free"),
            Matching::Matched("free".into())
        );
        // The vendor-hyphen fallback still lands on one of the variants.
        assert_eq!(
            index.lookup("openai-gpt-4o"),
            Matching::Matched("base".into())
        );

        // A genuinely ambiguous suffix still reports ambiguous.
        let ambiguous = ModelIndex::new(vec![
            ("gpt-4o-preview".to_string(), "model-a".to_string()),
            ("openai/gpt-4o-preview".to_string(), "model-b".to_string()),
        ]);
        assert_eq!(ambiguous.lookup("GPT-4o Preview"), Matching::Ambiguous);
    }

    #[test]
    fn duplicate_entries_keep_the_best_rank_and_count_the_rest() {
        let index = ModelIndex::new(vec![("m".to_string(), "model-m".to_string())]);
        let mut first = Entry::new("m", "text", 90.0);
        first.rank = Some(8);
        let mut second = Entry::new("m (xHigh)", "text", 95.0);
        second.rank = Some(2);
        let (picked, counts) = resolve(vec![first, second], &index, 2);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].1.score, 95.0);
        assert_eq!(counts.matched, 1);
        assert_eq!(counts.ambiguous, 1);
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
    // HttpFetcher retry (live finding 2026-11-17)
    // -----------------------------------------------------------------------

    /// Accepts the first connection and closes it without a response, then
    /// serves `body` on every later connection: `send()` fails once.
    async fn spawn_close_first_server(body: &'static str) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind flaky listener");
        let addr = listener.local_addr().expect("flaky address");
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
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_failed_send_is_retried_and_succeeds() {
        let addr = spawn_close_first_server("{\"data\":[]}").await;
        let fetcher = HttpFetcher::new().expect("http client");
        let body = fetcher
            .get(&format!("http://{addr}/models"))
            .await
            .expect("the second attempt succeeds");
        assert_eq!(body, "{\"data\":[]}");
    }

    #[tokio::test]
    async fn an_unreachable_host_reports_the_attempt_level_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind probe listener");
        let addr = listener.local_addr().expect("probe address");
        drop(listener);

        let fetcher = HttpFetcher::new().expect("http client");
        let error = fetcher
            .get(&format!("http://{addr}/models"))
            .await
            .expect_err("no server listens");
        let message = error.to_string();
        assert!(message.contains("attempt 2"), "{message}");
    }
}
