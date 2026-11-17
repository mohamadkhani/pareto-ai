//! Reports (M4): the `usage_daily` rollup and the read queries behind
//! `GET /api/usage`, `GET /api/usage/me`, and `GET /api/logs`.
//!
//! `usage_daily` is keyed by (local day in `GATEWAY_TIMEZONE`, user, offering)
//! and is maintained in two ways that are guaranteed to agree:
//!
//! * **incrementally**, one UPSERT inside every settlement transaction (so a
//!   report is never stale — the acceptance curl sequence reads the spend
//!   right after the request that spent it), and
//! * **rebuilt**, idempotently, from `usage_events` by the background job
//!   ([`spawn_rollup_job`]) and the `gateway rebuild-rollups` command. The
//!   rebuild is the source of truth: it deletes and re-derives every row, so
//!   it can be run at any time, any number of times.
//!
//! Both use the same attribution: the day key comes from the request's
//! `started_at` (the period it started in — `occurred_at` for pre-M4 rows),
//! and `COALESCE` keeps rows of deleted users/offering visible as `''`
//! instead of silently dropping spend from the totals.
//!
//! Counter semantics (migration 0006): `requests` counts every `usage_events`
//! row — including pre-send refusals (402 budget, 429 token cap, 403 access),
//! which consumed nothing and were never priced. Refusals have their own
//! `refused_requests` counter and are excluded from `priced_requests`;
//! `refused_requests` + `priced_requests` + `unpriced_requests` +
//! `partial_requests` partition `requests` exactly.

use std::collections::HashMap;

use chrono::DateTime;
use chrono_tz::Tz;
use serde::Serialize;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnection;
use utoipa::ToSchema;

use crate::money;

/// How often the background rollup job rebuilds `usage_daily`.
pub const ROLLUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// The counters of one report group; identical shape for totals and groups.
#[derive(Debug, Clone, Default, Serialize, ToSchema, sqlx::FromRow)]
pub struct UsageTotals {
    /// Every request that reached the chat pipeline — refusals included
    /// (see `refused_requests`). The four request counters below partition it.
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Sum of the priced spend; requests with `cost_usd` NULL (unpriced) are
    /// excluded and counted in `unpriced_requests` — never added as 0.
    pub cost_usd: f64,
    /// Requests whose cost is complete (every used component priced).
    /// Refusals are excluded: they were never priced (see `refused_requests`).
    pub priced_requests: i64,
    /// Requests that could not be priced at all.
    pub unpriced_requests: i64,
    /// Requests whose cost is present but understated (`priced = 0`: a used
    /// component had no applicable price and contributed 0). Together with
    /// `refused_requests` the four counters partition `requests` (review m2).
    pub partial_requests: i64,
    /// Requests answered before anything was sent upstream (402 budget, 429
    /// token cap, 403 access, ...). They consumed nothing and were never
    /// priced, so they are counted here and never in `priced_requests`.
    pub refused_requests: i64,
}

/// One `usage_daily` group as `GET /api/usage` returns it. Which key fields
/// are set depends on `group_by` (`day`/`user`/`offering`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageGroup {
    pub day: Option<String>,
    pub user_id: Option<String>,
    /// Email of the user, when the group is by user and the account still
    /// exists.
    pub email: Option<String>,
    pub offering_id: Option<String>,
    #[serde(flatten)]
    pub totals: UsageTotals,
}

/// One row of `GET /api/usage/me`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageDay {
    pub day: String,
    #[serde(flatten)]
    pub totals: UsageTotals,
}

/// The self-service usage report.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MyUsageReport {
    pub totals: UsageTotals,
    /// One entry per local day with usage, ascending.
    pub days: Vec<UsageDay>,
}

/// The admin aggregate report.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageReport {
    pub totals: UsageTotals,
    pub groups: Vec<UsageGroup>,
}

/// How `GET /api/usage` groups its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Day,
    User,
    Offering,
}

impl GroupBy {
    /// Parses the `group_by` query value; `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "day" => Some(Self::Day),
            "user" => Some(Self::User),
            "offering" => Some(Self::Offering),
            _ => None,
        }
    }
}

/// The values one settlement books into `usage_daily`.
#[derive(Debug, Clone)]
pub struct UsageRow<'a> {
    /// Local day key (from the request's start time).
    pub day: &'a str,
    /// `''` when the user was deleted.
    pub user_id: &'a str,
    /// `''` when the offering was deleted.
    pub offering_id: &'a str,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// `None` when unpriced.
    pub cost_usd: Option<f64>,
    /// Whether the cost is complete.
    pub priced: bool,
    /// The request was answered before anything was sent upstream: it
    /// consumed nothing and was never priced (counted in `refused_requests`).
    pub refused: bool,
}

/// One UPSERT into `usage_daily`, run inside the settlement transaction so
/// reports are fresh the moment the response leaves. Failure mode:
/// [`sqlx::Error`], which rolls the settlement back with everything else.
pub async fn record_usage(
    tx: &mut SqliteConnection,
    row: &UsageRow<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO usage_daily \
           (day, user_id, offering_id, input_tokens, output_tokens, requests, cost_usd, \
            unpriced_requests, priced_requests, partial_requests, refused_requests) \
         VALUES (?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?) \
         ON CONFLICT (day, user_id, offering_id) DO UPDATE SET \
           input_tokens = input_tokens + excluded.input_tokens, \
           output_tokens = output_tokens + excluded.output_tokens, \
           requests = requests + excluded.requests, \
           cost_usd = cost_usd + excluded.cost_usd, \
           unpriced_requests = unpriced_requests + excluded.unpriced_requests, \
           priced_requests = priced_requests + excluded.priced_requests, \
           partial_requests = partial_requests + excluded.partial_requests, \
           refused_requests = refused_requests + excluded.refused_requests",
    )
    .bind(row.day)
    .bind(row.user_id)
    .bind(row.offering_id)
    .bind(row.input_tokens)
    .bind(row.output_tokens)
    .bind(row.cost_usd.unwrap_or(0.0))
    // The four counters partition `requests`: a refusal is counted only in
    // `refused_requests`, everything else lands in exactly one of the other
    // three (migration 0006).
    .bind(i64::from(!row.refused && row.cost_usd.is_none()))
    .bind(i64::from(
        !row.refused && row.cost_usd.is_some() && row.priced,
    ))
    .bind(i64::from(
        !row.refused && row.cost_usd.is_some() && !row.priced,
    ))
    .bind(i64::from(row.refused))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// What one [`rebuild_rollups`] run did.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct RebuildStats {
    /// `usage_events` rows folded into the rollup.
    pub events: i64,
    /// `usage_daily` rows written.
    pub rows: i64,
}

/// Rebuilds `usage_daily` from `usage_events`, idempotently: the whole table
/// is deleted and re-derived in one transaction, so running it twice (or
/// after incremental updates) always converges on the same state. Pre-M4
/// rows without `started_at` attribute to their `occurred_at`.
///
/// The `usage_events` read happens **inside** the transaction, after its
/// write-first `DELETE` (review m1): the transaction holds SQLite's write
/// lock from the delete, so no settlement can commit between the read and
/// the rewrite — a settlement can never vanish from the rollup for the
/// duration of a rebuild.
///
/// Failure mode: [`sqlx::Error`]; the transaction keeps the old rollup when
/// the rebuild fails.
pub async fn rebuild_rollups(pool: &SqlitePool, tz: Tz) -> Result<RebuildStats, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Event {
        user_id: Option<String>,
        offering_id: Option<String>,
        occurred_at: String,
        started_at: Option<String>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        cost_usd: Option<f64>,
        priced: i64,
        refused: i64,
    }

    #[derive(Default)]
    struct Agg {
        input_tokens: i64,
        output_tokens: i64,
        requests: i64,
        cost_usd: f64,
        unpriced_requests: i64,
        priced_requests: i64,
        partial_requests: i64,
        refused_requests: i64,
    }

    let mut tx = pool.begin().await?;
    // Write-first (review m1): from the DELETE on, the transaction owns the
    // write lock and reads a snapshot no settlement can slip past.
    sqlx::query("DELETE FROM usage_daily")
        .execute(&mut *tx)
        .await?;
    let events = sqlx::query_as::<_, Event>(
        "SELECT user_id, offering_id, occurred_at, started_at, input_tokens, output_tokens, \
                cost_usd, priced, refused \
         FROM usage_events",
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut rollup: HashMap<(String, String, String), Agg> = HashMap::new();
    for event in &events {
        let stamp = event.started_at.as_deref().unwrap_or(&event.occurred_at);
        let day = DateTime::parse_from_rfc3339(stamp)
            .map(|value| money::day_key(tz, value.to_utc()))
            // An unparseable stamp cannot be attributed to a local day; the
            // UTC date is the least surprising fallback.
            .unwrap_or_else(|_| event.occurred_at.chars().take(10).collect());
        let key = (
            day,
            event.user_id.clone().unwrap_or_default(),
            event.offering_id.clone().unwrap_or_default(),
        );
        let agg = rollup.entry(key).or_default();
        agg.input_tokens += event.input_tokens.unwrap_or(0);
        agg.output_tokens += event.output_tokens.unwrap_or(0);
        agg.requests += 1;
        agg.cost_usd += event.cost_usd.unwrap_or(0.0);
        // The same four-way partition as `record_usage` (migration 0006): a
        // refusal lands only in `refused_requests`, never in `priced_requests`.
        let refused = event.refused != 0;
        agg.refused_requests += i64::from(refused);
        agg.unpriced_requests += i64::from(!refused && event.cost_usd.is_none());
        agg.priced_requests += i64::from(!refused && event.cost_usd.is_some() && event.priced != 0);
        agg.partial_requests +=
            i64::from(!refused && event.cost_usd.is_some() && event.priced == 0);
    }

    for ((day, user_id, offering_id), agg) in &rollup {
        sqlx::query(
            "INSERT INTO usage_daily \
               (day, user_id, offering_id, input_tokens, output_tokens, requests, cost_usd, \
                unpriced_requests, priced_requests, partial_requests, refused_requests) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(day)
        .bind(user_id)
        .bind(offering_id)
        .bind(agg.input_tokens)
        .bind(agg.output_tokens)
        .bind(agg.requests)
        .bind(agg.cost_usd)
        .bind(agg.unpriced_requests)
        .bind(agg.priced_requests)
        .bind(agg.partial_requests)
        .bind(agg.refused_requests)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(RebuildStats {
        events: events.len() as i64,
        rows: rollup.len() as i64,
    })
}

/// The background rollup job: rebuilds [`ROLLUP_INTERVAL`]-ly. The
/// incremental UPSERTs keep reports fresh between runs; this heals any drift
/// (for example rows written while the process was down).
pub fn spawn_rollup_job(pool: SqlitePool, tz: Tz) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(ROLLUP_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match rebuild_rollups(&pool, tz).await {
                Ok(stats) => tracing::debug!(events = stats.events, "usage rollup rebuilt"),
                Err(e) => tracing::error!(error = %e, "usage rollup rebuild failed"),
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

const GROUP_COLUMNS: &str = "SUM(requests), SUM(input_tokens), SUM(output_tokens), \
     SUM(cost_usd), SUM(priced_requests), SUM(unpriced_requests), SUM(partial_requests), \
     SUM(refused_requests)";

/// Flat decode shapes for the aggregate queries (the fixed column order of
/// [`GROUP_COLUMNS`], optionally preceded by the group key).
type GroupRow = (String, i64, i64, i64, f64, i64, i64, i64, i64);
type UserGroupRow = (
    String,
    Option<String>,
    i64,
    i64,
    i64,
    f64,
    i64,
    i64,
    i64,
    i64,
);

/// Builds totals from the fixed column order of [`GROUP_COLUMNS`].
#[allow(clippy::too_many_arguments)]
fn totals_of(
    requests: i64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    priced_requests: i64,
    unpriced_requests: i64,
    partial_requests: i64,
    refused_requests: i64,
) -> UsageTotals {
    UsageTotals {
        requests,
        input_tokens,
        output_tokens,
        cost_usd,
        priced_requests,
        unpriced_requests,
        partial_requests,
        refused_requests,
    }
}

fn sum_totals(totals: &[UsageTotals]) -> UsageTotals {
    totals.iter().fold(UsageTotals::default(), |mut acc, row| {
        acc.requests += row.requests;
        acc.input_tokens += row.input_tokens;
        acc.output_tokens += row.output_tokens;
        acc.cost_usd += row.cost_usd;
        acc.priced_requests += row.priced_requests;
        acc.unpriced_requests += row.unpriced_requests;
        acc.partial_requests += row.partial_requests;
        acc.refused_requests += row.refused_requests;
        acc
    })
}

/// The self-service report: totals plus one row per local day. `from`/`to`
/// are inclusive `YYYY-MM-DD` day keys. Failure mode: [`sqlx::Error`].
pub async fn usage_for_user(
    pool: &SqlitePool,
    user_id: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<MyUsageReport, sqlx::Error> {
    let rows: Vec<GroupRow> = sqlx::query_as(&format!(
        "SELECT day, {GROUP_COLUMNS} FROM usage_daily \
             WHERE user_id = ?1 \
               AND (?2 IS NULL OR day >= ?2) AND (?3 IS NULL OR day <= ?3) \
             GROUP BY day ORDER BY day"
    ))
    .bind(user_id)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;

    let days: Vec<UsageDay> = rows
        .iter()
        .map(
            |(day, requests, input, output, cost, priced, unpriced, partial, refused)| UsageDay {
                day: day.clone(),
                totals: totals_of(
                    *requests, *input, *output, *cost, *priced, *unpriced, *partial, *refused,
                ),
            },
        )
        .collect();
    let totals = sum_totals(
        &days
            .iter()
            .map(|day| day.totals.clone())
            .collect::<Vec<_>>(),
    );
    Ok(MyUsageReport { totals, days })
}

/// The admin aggregate, grouped by day, user, or offering (optionally
/// narrowed to one user). `from`/`to` are inclusive `YYYY-MM-DD` day keys.
/// Failure mode: [`sqlx::Error`].
pub async fn usage_admin(
    pool: &SqlitePool,
    user_id: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    group_by: GroupBy,
) -> Result<UsageReport, sqlx::Error> {
    let groups: Vec<UsageGroup> = match group_by {
        GroupBy::Day => {
            let rows: Vec<GroupRow> = sqlx::query_as(&format!(
                "SELECT day, {GROUP_COLUMNS} FROM usage_daily \
                     WHERE (?1 IS NULL OR user_id = ?1) \
                       AND (?2 IS NULL OR day >= ?2) AND (?3 IS NULL OR day <= ?3) \
                     GROUP BY day ORDER BY day"
            ))
            .bind(user_id)
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await?;
            rows.iter()
                .map(
                    |(day, requests, input, output, cost, priced, unpriced, partial, refused)| {
                        UsageGroup {
                            day: Some(day.clone()),
                            user_id: None,
                            email: None,
                            offering_id: None,
                            totals: totals_of(
                                *requests, *input, *output, *cost, *priced, *unpriced, *partial,
                                *refused,
                            ),
                        }
                    },
                )
                .collect()
        }
        GroupBy::User => {
            let rows: Vec<UserGroupRow> = sqlx::query_as(&format!(
                "SELECT d.user_id, u.email, {GROUP_COLUMNS} FROM usage_daily d \
                         LEFT JOIN users u ON u.id = d.user_id \
                         WHERE (?1 IS NULL OR d.user_id = ?1) \
                           AND (?2 IS NULL OR d.day >= ?2) AND (?3 IS NULL OR d.day <= ?3) \
                         GROUP BY d.user_id, u.email ORDER BY d.user_id"
            ))
            .bind(user_id)
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await?;
            rows.iter()
                .map(
                    |(
                        id,
                        email,
                        requests,
                        input,
                        output,
                        cost,
                        priced,
                        unpriced,
                        partial,
                        refused,
                    )| {
                        UsageGroup {
                            day: None,
                            user_id: Some(id.clone()),
                            email: email.clone(),
                            offering_id: None,
                            totals: totals_of(
                                *requests, *input, *output, *cost, *priced, *unpriced, *partial,
                                *refused,
                            ),
                        }
                    },
                )
                .collect()
        }
        GroupBy::Offering => {
            let rows: Vec<GroupRow> = sqlx::query_as(&format!(
                "SELECT offering_id, {GROUP_COLUMNS} FROM usage_daily \
                     WHERE (?1 IS NULL OR user_id = ?1) \
                       AND (?2 IS NULL OR day >= ?2) AND (?3 IS NULL OR day <= ?3) \
                     GROUP BY offering_id ORDER BY offering_id"
            ))
            .bind(user_id)
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await?;
            rows.iter()
                .map(
                    |(
                        offering_id,
                        requests,
                        input,
                        output,
                        cost,
                        priced,
                        unpriced,
                        partial,
                        refused,
                    )| {
                        UsageGroup {
                            day: None,
                            user_id: None,
                            email: None,
                            offering_id: Some(offering_id.clone()),
                            totals: totals_of(
                                *requests, *input, *output, *cost, *priced, *unpriced, *partial,
                                *refused,
                            ),
                        }
                    },
                )
                .collect()
        }
    };
    let totals = sum_totals(
        &groups
            .iter()
            .map(|group| group.totals.clone())
            .collect::<Vec<_>>(),
    );
    Ok(UsageReport { totals, groups })
}

// ---------------------------------------------------------------------------
// Request logs (metadata only, decision D11)
// ---------------------------------------------------------------------------

/// One request-log entry: request metadata plus the settled usage counters.
/// Never carries request or response bodies.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
pub struct RequestLogEntry {
    pub request_id: String,
    pub client_request_id: Option<String>,
    pub occurred_at: String,
    pub method: Option<String>,
    pub path: Option<String>,
    pub requested_model: Option<String>,
    pub resolved_offering_id: Option<String>,
    pub decision_json: Option<String>,
    pub status: Option<i64>,
    pub error_class: Option<String>,
    pub bytes_in: Option<i64>,
    pub bytes_out: Option<i64>,
    pub user_id: Option<String>,
    pub api_key_id: Option<String>,
    /// Usage counters settled for this request, when a ledger row exists.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub tokens_estimated: Option<i64>,
    pub latency_ms: Option<i64>,
}

/// The exclusive cursor of the last entry of a page.
#[derive(Debug, Serialize, ToSchema)]
pub struct LogCursor {
    /// The last entry's `occurred_at`, RFC-3339 UTC with milliseconds.
    pub before: String,
    /// The last entry's `request_id`, breaking ties with `before`.
    pub before_request_id: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LogListResponse {
    pub entries: Vec<RequestLogEntry>,
    /// Cursor for the next (older) page; see [`LogCursor`].
    pub next_cursor: Option<LogCursor>,
}

/// One page of request logs, newest first, with the same exclusive
/// `(occurred_at, request_id)` cursor pattern as `/api/audit`. `user_id`
/// narrows to one account. Failure mode: [`sqlx::Error`].
pub async fn list_request_logs(
    pool: &SqlitePool,
    user_id: Option<&str>,
    limit: i64,
    before: Option<&str>,
    before_request_id: Option<&str>,
) -> Result<LogListResponse, sqlx::Error> {
    let entries = sqlx::query_as::<_, RequestLogEntry>(
        "SELECT l.request_id, l.client_request_id, l.occurred_at, l.method, l.path, \
                l.requested_model, l.resolved_offering_id, l.decision_json, l.status, \
                l.error_class, l.bytes_in, l.bytes_out, l.user_id, l.api_key_id, \
                u.input_tokens, u.output_tokens, u.cost_usd, u.tokens_estimated, u.latency_ms \
         FROM request_logs l \
         LEFT JOIN usage_events u ON u.request_id = l.request_id \
         WHERE (?1 IS NULL OR l.user_id = ?1) \
           AND (?2 IS NULL OR l.occurred_at < ?2 \
                OR (?3 IS NOT NULL AND l.occurred_at = ?2 AND l.request_id < ?3)) \
         ORDER BY l.occurred_at DESC, l.request_id DESC \
         LIMIT ?4",
    )
    .bind(user_id)
    .bind(before)
    .bind(before_request_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let next_cursor = match (entries.len() as i64 == limit, entries.last()) {
        (true, Some(last)) => Some(LogCursor {
            before: last.occurred_at.clone(),
            before_request_id: last.request_id.clone(),
        }),
        _ => None,
    };
    Ok(LogListResponse {
        entries,
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `usage_daily` rows as the tests compare them.
    type RollupRow = (
        String,
        String,
        String,
        i64,
        i64,
        i64,
        f64,
        i64,
        i64,
        i64,
        i64,
    );
    use crate::catalog::PriceRow;
    use crate::money::compute_cost;
    use crate::upstream::sse::Usage;

    async fn pool() -> (std::path::PathBuf, SqlitePool) {
        let dir = std::env::temp_dir().join(format!("gateway-reports-{}", uuid::Uuid::new_v4()));
        let pool = crate::db::open(&dir.join("gateway.db"))
            .await
            .expect("open");
        (dir, pool)
    }

    async fn seed_user(pool: &SqlitePool) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active) \
             VALUES (?, ?, 'R', 'x', 'user', 1)",
        )
        .bind(&id)
        .bind(format!("{id}@example.com"))
        .execute(pool)
        .await
        .expect("insert user");
        id
    }

    /// Creates one offering (with its provider and model) and returns its id.
    async fn seed_offering(pool: &SqlitePool, name: &str) -> String {
        let provider_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO providers (id, name, base_url) VALUES (?, ?, 'https://x.example/v1')",
        )
        .bind(&provider_id)
        .bind(format!("{name}-provider"))
        .execute(pool)
        .await
        .expect("insert provider");
        let model_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO models (id, canonical_name) VALUES (?, ?)")
            .bind(&model_id)
            .bind(format!("{name}-model"))
            .execute(pool)
            .await
            .expect("insert model");
        let offering_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO offerings (id, provider_id, model_id, upstream_model_id) \
             VALUES (?, ?, ?, 'up')",
        )
        .bind(&offering_id)
        .bind(&provider_id)
        .bind(&model_id)
        .execute(pool)
        .await
        .expect("insert offering");
        offering_id
    }

    /// Inserts one `usage_events` row the way settlement would.
    #[allow(clippy::too_many_arguments)]
    async fn seed_event(
        pool: &SqlitePool,
        request_id: &str,
        user_id: Option<&str>,
        offering_id: Option<&str>,
        started_at: &str,
        input: i64,
        output: i64,
        cost: Option<f64>,
        priced: bool,
    ) {
        sqlx::query(
            "INSERT INTO usage_events \
               (request_id, user_id, offering_id, model_id, input_tokens, output_tokens, \
                tokens_estimated, cost_usd, priced, started_at, occurred_at) \
             VALUES (?, ?, ?, NULL, ?, ?, 0, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(user_id)
        .bind(offering_id)
        .bind(input)
        .bind(output)
        .bind(cost)
        .bind(i64::from(priced))
        .bind(started_at)
        .bind(started_at)
        .execute(pool)
        .await
        .expect("insert event");
    }

    /// Inserts one `usage_events` row the way settlement books a pre-send
    /// refusal: nothing consumed (tokens NULL), cost 0.0, fully "priced",
    /// `refused = 1` (migration 0006).
    async fn seed_refusal(
        pool: &SqlitePool,
        request_id: &str,
        user_id: Option<&str>,
        offering_id: Option<&str>,
        started_at: &str,
    ) {
        sqlx::query(
            "INSERT INTO usage_events \
               (request_id, user_id, offering_id, input_tokens, output_tokens, \
                tokens_estimated, cost_usd, priced, refused, started_at, occurred_at) \
             VALUES (?, ?, ?, NULL, NULL, 0, 0.0, 1, 1, ?, ?)",
        )
        .bind(request_id)
        .bind(user_id)
        .bind(offering_id)
        .bind(started_at)
        .bind(started_at)
        .execute(pool)
        .await
        .expect("insert refusal event");
    }

    #[tokio::test]
    async fn rollup_rebuild_is_idempotent() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let off_a = seed_offering(&pool, "a").await;
        let off_b = seed_offering(&pool, "b").await;
        seed_event(
            &pool,
            "r1",
            Some(&user),
            Some(&off_a),
            "2026-11-16T10:00:00.000Z",
            100,
            50,
            Some(0.25),
            true,
        )
        .await;
        seed_event(
            &pool,
            "r2",
            Some(&user),
            Some(&off_a),
            "2026-11-16T11:00:00.000Z",
            10,
            5,
            Some(0.02),
            true,
        )
        .await;
        seed_event(
            &pool,
            "r3",
            Some(&user),
            Some(&off_b),
            "2026-11-15T10:00:00.000Z",
            7,
            3,
            None,
            false,
        )
        .await;

        let first = rebuild_rollups(&pool, Tz::UTC).await.expect("rebuild");
        assert_eq!(first.events, 3);
        assert_eq!(first.rows, 2, "two (day, user, offering) buckets");

        let rows: Vec<RollupRow> = sqlx::query_as(
            "SELECT day, user_id, offering_id, input_tokens, output_tokens, requests, \
                        cost_usd, unpriced_requests, priced_requests, partial_requests, refused_requests \
                 FROM usage_daily ORDER BY day, offering_id",
        )
        .fetch_all(&pool)
        .await
        .expect("rollup rows");
        assert_eq!(rows[0].0, "2026-11-15");
        assert_eq!(rows[0].5, 1);
        assert!((rows[0].6 - 0.0).abs() < 1e-9);
        assert_eq!(rows[0].7, 1, "the unpriced request is counted");
        assert_eq!(rows[1].0, "2026-11-16");
        assert_eq!(rows[1].3, 110);
        assert_eq!(rows[1].4, 55);
        assert_eq!(rows[1].5, 2);
        assert!((rows[1].6 - 0.27).abs() < 1e-9);
        assert_eq!(rows[1].8, 2);

        // Rebuilding again converges on the same rows, and so does rebuilding
        // after incremental updates.
        let before = format!("{rows:?}");
        rebuild_rollups(&pool, Tz::UTC)
            .await
            .expect("second rebuild");
        rebuild_rollups(&pool, Tz::UTC)
            .await
            .expect("third rebuild");
        let after: Vec<RollupRow> = sqlx::query_as(
            "SELECT day, user_id, offering_id, input_tokens, output_tokens, requests, \
                    cost_usd, unpriced_requests, priced_requests, partial_requests, refused_requests \
             FROM usage_daily ORDER BY day, offering_id",
        )
        .fetch_all(&pool)
        .await
        .expect("rollup rows");
        assert_eq!(format!("{after:?}"), before, "the rebuild is idempotent");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn reports_reconcile_with_usage_events_totals() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let off_a = seed_offering(&pool, "a").await;
        let off_b = seed_offering(&pool, "b").await;
        seed_event(
            &pool,
            "r1",
            Some(&user),
            Some(&off_a),
            "2026-11-16T10:00:00.000Z",
            100,
            50,
            Some(0.25),
            true,
        )
        .await;
        seed_event(
            &pool,
            "r2",
            Some(&user),
            Some(&off_a),
            "2026-11-16T11:00:00.000Z",
            10,
            5,
            Some(0.02),
            true,
        )
        .await;
        seed_event(
            &pool,
            "r3",
            Some(&user),
            Some(&off_b),
            "2026-11-15T10:00:00.000Z",
            7,
            3,
            None,
            false,
        )
        .await;
        seed_event(
            &pool,
            "r4",
            None,
            None,
            "2026-11-14T10:00:00.000Z",
            1,
            1,
            Some(0.0),
            true,
        )
        .await;

        rebuild_rollups(&pool, Tz::UTC).await.expect("rebuild");

        // The raw ledger totals, as the acceptance requires them asserted.
        let (events, input, output, cost, unpriced): (i64, i64, i64, Option<f64>, i64) =
            sqlx::query_as(
                "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), \
                        SUM(cost_usd), COALESCE(SUM(cost_usd IS NULL), 0) FROM usage_events",
            )
            .fetch_one(&pool)
            .await
            .expect("ledger totals");

        let report = usage_admin(&pool, None, None, None, GroupBy::User)
            .await
            .expect("report");
        assert_eq!(report.totals.requests, events);
        assert_eq!(report.totals.input_tokens, input);
        assert_eq!(report.totals.output_tokens, output);
        assert!((report.totals.cost_usd - cost.unwrap_or(0.0)).abs() < 1e-9);
        assert_eq!(report.totals.unpriced_requests, unpriced);

        // Grouped by day and by offering the totals must agree exactly.
        for group_by in [GroupBy::Day, GroupBy::Offering] {
            let report = usage_admin(&pool, None, None, None, group_by)
                .await
                .expect("report");
            assert_eq!(report.totals.requests, events, "{group_by:?}");
            assert!((report.totals.cost_usd - cost.unwrap_or(0.0)).abs() < 1e-9);
        }

        // The deleted-user bucket is still visible by user, as `''`.
        let report = usage_admin(&pool, None, None, None, GroupBy::User)
            .await
            .expect("report");
        assert!(
            report
                .groups
                .iter()
                .any(|group| group.user_id.as_deref() == Some(""))
        );

        // The self-service report only sees its own rows and sums to the
        // user's share of the ledger.
        let mine = usage_for_user(&pool, &user, None, None)
            .await
            .expect("mine");
        assert_eq!(mine.totals.requests, 3);
        assert!((mine.totals.cost_usd - 0.27).abs() < 1e-9);
        assert_eq!(mine.days.len(), 2);
        assert_eq!(mine.days[0].day, "2026-11-15");

        // Day-range filters are inclusive day keys.
        let ranged = usage_for_user(&pool, &user, Some("2026-11-16"), Some("2026-11-16"))
            .await
            .expect("ranged");
        assert_eq!(ranged.totals.requests, 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn incremental_updates_match_a_full_rebuild() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let off_a = seed_offering(&pool, "a").await;
        // Two settlements through the incremental UPSERT...
        for (request_id, cost, priced) in [("s1", Some(1.5), true), ("s2", None, false)] {
            let mut tx = pool.begin().await.expect("tx");
            sqlx::query(
                "INSERT INTO usage_events \
                   (request_id, user_id, offering_id, input_tokens, output_tokens, tokens_estimated, \
                    cost_usd, priced, started_at, occurred_at) \
                 VALUES (?, ?, ?, 10, 5, 0, ?, ?, ?, ?)",
            )
            .bind(request_id)
            .bind(&user)
            .bind(&off_a)
            .bind(cost)
            .bind(i64::from(priced))
            .bind("2026-11-16T10:00:00.000Z")
            .bind("2026-11-16T10:00:01.000Z")
            .execute(&mut *tx)
            .await
            .expect("event");
            record_usage(
                &mut tx,
                &UsageRow {
                    day: "2026-11-16",
                    user_id: &user,
                    offering_id: &off_a,
                    input_tokens: 10,
                    output_tokens: 5,
                    cost_usd: cost,
                    priced,
                    refused: false,
                },
            )
            .await
            .expect("record");
            tx.commit().await.expect("commit");
        }

        let incremental: Vec<RollupRow> = sqlx::query_as(
            "SELECT day, user_id, offering_id, input_tokens, output_tokens, requests, \
                        cost_usd, unpriced_requests, priced_requests, partial_requests, refused_requests \
                 FROM usage_daily",
        )
        .fetch_all(&pool)
        .await
        .expect("rows");

        rebuild_rollups(&pool, Tz::UTC).await.expect("rebuild");
        let rebuilt: Vec<RollupRow> = sqlx::query_as(
            "SELECT day, user_id, offering_id, input_tokens, output_tokens, requests, \
                    cost_usd, unpriced_requests, priced_requests, partial_requests, refused_requests \
             FROM usage_daily",
        )
        .fetch_all(&pool)
        .await
        .expect("rows");

        assert_eq!(format!("{incremental:?}"), format!("{rebuilt:?}"));
        assert_eq!(incremental.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn request_logs_paginate_with_the_composite_cursor() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        for i in 0..5 {
            sqlx::query(
                "INSERT INTO request_logs (request_id, occurred_at, method, path, status, user_id) \
                 VALUES (?, ?, 'POST', '/v1/chat/completions', 200, ?)",
            )
            .bind(format!("log-{i}"))
            .bind("2026-11-16T10:00:00.000Z") // one shared millisecond
            .bind(&user)
            .execute(&pool)
            .await
            .expect("log row");
        }

        let mut collected = Vec::new();
        let mut before = None;
        let mut before_request_id = None;
        loop {
            let page = list_request_logs(
                &pool,
                None,
                2,
                before.as_deref(),
                before_request_id.as_deref(),
            )
            .await
            .expect("page");
            for entry in &page.entries {
                assert_eq!(entry.user_id.as_deref(), Some(user.as_str()));
                collected.push(entry.request_id.clone());
            }
            match page.next_cursor {
                Some(cursor) => {
                    before = Some(cursor.before);
                    before_request_id = Some(cursor.before_request_id);
                }
                None => break,
            }
        }
        assert_eq!(collected.len(), 5, "no gaps and no repeats: {collected:?}");
        let unique: std::collections::HashSet<_> = collected.iter().collect();
        assert_eq!(unique.len(), 5, "the tie group must not duplicate rows");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_prices_understate_the_cost_and_flag_it() {
        // The report's `cost_usd` carries the priced sum; unpriced rows are
        // counted, never added as 0, and a used component without price
        // marks the row incomplete.
        let price = PriceRow {
            offering_id: "off".to_string(),
            input_per_m: Some(1.0),
            output_per_m: None,
            cache_read_per_m: None,
            cache_write_per_m: None,
            reasoning_per_m: None,
            request_fee: None,
            variable_price: 0,
            currency: "USD".to_string(),
            source: "manual".to_string(),
            source_url: None,
            fetched_at: None,
            updated_by: None,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        };
        let cost = compute_cost(
            Some(&price),
            Some(&Usage {
                input_tokens: Some(1_000_000),
                output_tokens: Some(500_000),
                ..Usage::default()
            }),
        );
        assert_eq!(
            cost.cost_usd,
            Some(1.0),
            "the unpriced output contributes 0"
        );
        assert!(!cost.priced, "a used component without price is incomplete");
    }

    #[tokio::test]
    async fn rollup_counters_partition_requests_by_price_completeness() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let off = seed_offering(&pool, "a").await;
        // One fully priced, one unpriced (NULL cost), one partial
        // (priced = 0 with a non-NULL understated cost) — review m2.
        for (request_id, cost, priced) in [
            ("full", Some(0.25), true),
            ("none", None, false),
            ("part", Some(0.1), false),
        ] {
            seed_event(
                &pool,
                request_id,
                Some(&user),
                Some(&off),
                "2026-11-16T10:00:00.000Z",
                10,
                5,
                cost,
                priced,
            )
            .await;
            let mut tx = pool.begin().await.expect("tx");
            record_usage(
                &mut tx,
                &UsageRow {
                    day: "2026-11-16",
                    user_id: &user,
                    offering_id: &off,
                    input_tokens: 10,
                    output_tokens: 5,
                    cost_usd: cost,
                    priced,
                    refused: false,
                },
            )
            .await
            .expect("record");
            tx.commit().await.expect("commit");
        }

        // The incremental UPSERT counts the split...
        let incremental = usage_admin(&pool, None, None, None, GroupBy::Day)
            .await
            .expect("report");
        assert_eq!(incremental.totals.requests, 3);
        assert_eq!(incremental.totals.priced_requests, 1);
        assert_eq!(incremental.totals.unpriced_requests, 1);
        assert_eq!(
            incremental.totals.partial_requests, 1,
            "a priced=0 row with a non-NULL understated cost"
        );
        assert_eq!(
            incremental.totals.priced_requests
                + incremental.totals.unpriced_requests
                + incremental.totals.partial_requests
                + incremental.totals.refused_requests,
            incremental.totals.requests,
            "the four counters partition requests"
        );

        // ...and the rebuild derives the same split from `usage_events`.
        rebuild_rollups(&pool, Tz::UTC).await.expect("rebuild");
        let rebuilt = usage_admin(&pool, None, None, None, GroupBy::Day)
            .await
            .expect("report");
        assert_eq!(rebuilt.totals.partial_requests, 1);
        assert_eq!(rebuilt.totals.priced_requests, 1);
        assert_eq!(rebuilt.totals.unpriced_requests, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A pre-send refusal (nothing consumed, never priced) gets its own
    /// counter and never inflates `priced_requests` (M4 UI-alignment finding
    /// 1) — incrementally and after a full rollup rebuild.
    #[tokio::test]
    async fn refusals_get_their_own_counter_and_flow_through_the_rebuild() {
        /// The counter split one report must show, however it was derived.
        async fn assert_counters(pool: &SqlitePool) {
            let report = usage_admin(pool, None, None, None, GroupBy::Day)
                .await
                .expect("report");
            assert_eq!(report.totals.requests, 2, "requests counts every row");
            assert_eq!(report.totals.priced_requests, 1);
            assert_eq!(
                report.totals.refused_requests, 1,
                "the refusal has its own counter"
            );
            assert_eq!(report.totals.unpriced_requests, 0);
            assert_eq!(report.totals.partial_requests, 0);
            assert_eq!(
                report.totals.priced_requests
                    + report.totals.unpriced_requests
                    + report.totals.partial_requests
                    + report.totals.refused_requests,
                report.totals.requests,
                "the four counters partition requests"
            );
        }

        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let off = seed_offering(&pool, "a").await;
        // One billable (fully priced) request and one refusal.
        seed_event(
            &pool,
            "sent",
            Some(&user),
            Some(&off),
            "2026-11-16T10:00:00.000Z",
            10,
            5,
            Some(0.25),
            true,
        )
        .await;
        seed_refusal(
            &pool,
            "refused",
            Some(&user),
            Some(&off),
            "2026-11-16T11:00:00.000Z",
        )
        .await;

        // The incremental UPSERT books the refusal the same way settlement
        // does (cost 0.0, "priced", nothing consumed)...
        for row in [
            UsageRow {
                day: "2026-11-16",
                user_id: &user,
                offering_id: &off,
                input_tokens: 10,
                output_tokens: 5,
                cost_usd: Some(0.25),
                priced: true,
                refused: false,
            },
            UsageRow {
                day: "2026-11-16",
                user_id: &user,
                offering_id: &off,
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: Some(0.0),
                priced: true,
                refused: true,
            },
        ] {
            let mut tx = pool.begin().await.expect("tx");
            record_usage(&mut tx, &row).await.expect("record");
            tx.commit().await.expect("commit");
        }
        assert_counters(&pool).await;

        // ...and the rebuild derives the same split from `usage_events`.
        rebuild_rollups(&pool, Tz::UTC).await.expect("rebuild");
        assert_counters(&pool).await;
        std::fs::remove_dir_all(&dir).ok();
    }
}
