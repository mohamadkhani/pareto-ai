//! Money domain (M4): cost computation (decision D10, subset-aware per D22),
//! spend caps and period buckets (decision D9), and the reservation ledger
//! (decision D7).
//!
//! **Cost.** One request's cost is computed from the settled usage tokens and
//! the offering's `prices` row, billing the **non-overlapping** parts of
//! OpenAI-style usage (D22): `cache_read` is a subset of `input` and
//! `reasoning` a subset of `output`, so the billed parts are
//! `(input - cache_read) * input_per_m + cache_read * cache_read_per_m` and
//! `(output - reasoning) * output_per_m + reasoning * reasoning_per_m`,
//! every term clamped at zero, plus `cache_write * cache_write_per_m` and
//! `request_fee`, divided by 1M. A subset whose own price is missing falls
//! back to its superset's price (`cache_read` → `input_per_m`, `reasoning` →
//! `output_per_m`), so a missing subset price neither understates the cost
//! nor flags the row (the old five-term sum double-billed overlapping tokens
//! and falsely flagged such rows — review M3). A used part with no applicable
//! price contributes 0 and marks the cost `priced = 0` (the cost is
//! understated exactly then). A missing or all-NULL price row means the
//! request is **unpriced**: `cost_usd` stays NULL and is reported as such —
//! never silently 0 (D10). A request that consumed nothing costs exactly 0
//! and is fully priced. Usage counts are clamped to be non-negative at parse
//! (`upstream::sse`) and again here, and every accumulation is saturating, so
//! hostile upstream counts can never credit the ledger or overflow (review
//! M2). One documented residual (review m4): the reservation *estimate* is
//! priced on the first priced candidate and the output estimate is capped at
//! 4096 tokens when the request sets no `max_tokens`, so the USD cap is soft
//! against an actual output that overshoots the estimate — the settle books
//! the true actual and per-model token caps meter the overshoot.
//!
//! **Periods.** `day`/`week`/`month`/`total` caps reset at local-midnight
//! boundaries in `GATEWAY_TIMEZONE` (weeks start Monday); `total` never
//! resets. Cap rows are **bucket-scoped**: `period_start` keys the UTC start
//! of the bucket the spend belongs to, so a reset *creates a new row* (the
//! limit is carried forward) while the old row still receives the spend of
//! requests that started inside it. A request is therefore always charged to
//! the bucket derived from its `started_at`, even when that bucket already
//! reset — a period rollover can never hide earlier spend (review M1).
//!
//! **Reservations.** Before forwarding, one `reservations` row is opened in a
//! write-first transaction with the request's estimate. The budget check is
//! `used + open reservations > limit`, where the open-reservation sum already
//! includes the row being opened — equivalent to the spec's `used + open +
//! estimate > limit`, but it cannot double-count under concurrency: the
//! write-first transaction serializes check and insert on SQLite's single
//! writer. Settlement (in the same transaction as the `usage_events` row)
//! books the actuals and marks the row `settled`; requests that consumed
//! nothing mark it `released`; a reconciler releases rows left open past
//! [`reservation_ttl`].
//!
//! **Booking invariant.** Actuals are booked **exactly once** because the
//! `open`/`released` → `settled` transition on the reservation row is the
//! settle marker: it is an atomic `UPDATE ... WHERE status <> 'settled'`
//! (the transaction's first statement when called standalone), so a second
//! settlement — including one racing the first — matches nothing and books
//! nothing (review B1). Booking is keyed on that marker, never on
//! `status = 'open'`: a request whose reservation the reconciler released is
//! still booked, and since the estimate was gated inside the same headroom
//! that the actual is later charged to, releasing can free headroom only for
//! crashed requests — it can never dodge a cap.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnection;

use crate::auth;
use crate::catalog::{self, Actor, PriceRow};
use crate::upstream::sse::Usage;

/// A spend-cap period. `Total` accumulates forever and never resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Period {
    Day,
    Week,
    Month,
    Total,
}

impl Period {
    /// Parses `day`/`week`/`month`/`total`; `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            "total" => Some(Self::Total),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Total => "total",
        }
    }
}

impl std::fmt::Display for Period {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Floor of [`reservation_ttl`]: the default headroom hold for requests whose
/// total timeout is shorter than ten minutes.
pub const MIN_RESERVATION_TTL: Duration = Duration::from_secs(10 * 60);
/// How often the reservation reconciler runs.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);

/// The reservation age after which the reconciler releases an `open` row:
/// `max(10 min, timeout_total + 5 min)` — **strictly greater** than the
/// request's total timeout, so the reconciler can never race a live request
/// (review B1). Releasing only frees the headroom of crashed settlements;
/// actuals are booked even for a released reservation.
pub fn reservation_ttl(timeout_total: Duration) -> Duration {
    timeout_total
        .saturating_add(Duration::from_secs(5 * 60))
        .max(MIN_RESERVATION_TTL)
}

/// The `period_start` of every `total` row: `total` has no buckets and never
/// resets, so all its rows share one sentinel key.
pub const TOTAL_BUCKET: &str = "1970-01-01T00:00:00.000Z";

/// UTC timestamp in the shape this module writes (SQLite `strftime` shape).
fn ts(at: DateTime<Utc>) -> String {
    at.format(auth::TS_FMT).to_string()
}

/// The local-date day key (`YYYY-MM-DD`) of `at` in `tz`; the rollup key.
pub fn day_key(tz: Tz, at: DateTime<Utc>) -> String {
    at.with_timezone(&tz)
        .date_naive()
        .format("%Y-%m-%d")
        .to_string()
}

/// Converts a local wall-clock time in `tz` to UTC. Skipped wall times (a DST
/// spring-forward gap, e.g. midnight in America/Santiago) resolve to the
/// first instant after the gap.
fn local_to_utc(tz: Tz, naive: NaiveDateTime) -> DateTime<Utc> {
    let direct = tz
        .from_local_datetime(&naive)
        .earliest()
        .map(|value| value.with_timezone(&Utc));
    if let Some(value) = direct {
        return value;
    }
    // The wall time does not exist: the instant one hour later always does.
    let fallback = tz
        .from_local_datetime(&(naive + chrono::Duration::hours(1)))
        .earliest()
        .map(|value| value.with_timezone(&Utc));
    // Only unreachable for pathological timezone data; UTC keeps the ledger
    // moving instead of panicking on a request path.
    fallback.unwrap_or_else(|| Utc.from_utc_datetime(&naive))
}

/// Start (inclusive, UTC) of the `period` bucket containing `at`.
/// `None` for `Total`, which has no boundaries.
pub fn period_start(tz: Tz, period: Period, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let local = at.with_timezone(&tz).date_naive();
    let start_naive = match period {
        Period::Day => local.and_hms_opt(0, 0, 0),
        Period::Week => {
            let back = local.weekday().num_days_from_monday() as u64;
            (local - chrono::Duration::days(back as i64)).and_hms_opt(0, 0, 0)
        }
        Period::Month => local.with_day(1)?.and_hms_opt(0, 0, 0),
        Period::Total => return None,
    }?;
    Some(local_to_utc(tz, start_naive))
}

/// End (exclusive, UTC) of the `period` bucket containing `at`; the instant
/// the bucket resets. `None` for `Total`.
pub fn period_end(tz: Tz, period: Period, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let local = at.with_timezone(&tz).date_naive();
    let end_naive = match period {
        Period::Day => (local + chrono::Duration::days(1)).and_hms_opt(0, 0, 0),
        Period::Week => {
            let forward = 7 - local.weekday().num_days_from_monday() as i64;
            (local + chrono::Duration::days(forward)).and_hms_opt(0, 0, 0)
        }
        Period::Month => month_start_date(local.year(), local.month() + 1).and_hms_opt(0, 0, 0),
        Period::Total => return None,
    }?;
    Some(local_to_utc(tz, end_naive))
}

/// The first day of `month` of `year`, normalizing month 13 to January of the
/// next year.
fn month_start_date(year: i32, month: u32) -> chrono::NaiveDate {
    let (year, month) = if month == 13 {
        (year + 1, 1)
    } else {
        (year, month)
    };
    chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(chrono::NaiveDate::MIN)
}

/// The ledger key of the `period` bucket containing `at`: the UTC start of
/// the bucket (the [`TOTAL_BUCKET`] sentinel for `total`). This is what
/// `budgets.period_start` / `token_caps.period_start` hold.
fn bucket_key(tz: Tz, period: Period, at: DateTime<Utc>) -> String {
    period_start(tz, period, at)
        .map(ts)
        .unwrap_or_else(|| TOTAL_BUCKET.to_string())
}

/// The end of `at`'s `period` bucket (the instant the row's counter would
/// reset — shown as `reset_at`); `None` for `total`.
fn bucket_end(tz: Tz, period: Period, at: DateTime<Utc>) -> Option<String> {
    period_end(tz, period, at).map(ts)
}

// ---------------------------------------------------------------------------
// Cost (decision D10, subset-aware per D22)
// ---------------------------------------------------------------------------

/// The outcome of pricing one request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost {
    /// USD cost; `None` when it cannot be computed (unpriced or unknown
    /// usage) — never a silent 0.
    pub cost_usd: Option<f64>,
    /// `true` when no used part was left unpriced: every used token has an
    /// applicable price (a subset may have inherited its superset's price,
    /// D22). `false` whenever `cost_usd` is `None`, and also when a used
    /// part had no applicable price and contributed 0 (the cost understates
    /// it).
    pub priced: bool,
}

/// Prices one settled usage against the offering's price row.
///
/// The five usage components are billed as the non-overlapping parts of
/// OpenAI-style usage (D22): `cache_read` is subtracted from `input` and
/// `reasoning` from `output`, every term clamped at zero. A part whose own
/// price is missing falls back to its superset's price (`cache_read` →
/// `input_per_m`, `reasoning` → `output_per_m`); a used part with no
/// applicable price contributes 0 and flags `priced = false`. Usage counts
/// are clamped to be non-negative here as well as at parse time (review M2),
/// and the result is clamped at zero, so no hostile input can credit a
/// budget.
///
/// Failure modes (all `cost_usd = None`, `priced = false`): no price row, a
/// price row with no known component at all, or a usage whose token counts
/// are entirely unknown. A request that consumed nothing (`usage` is `None`)
/// costs exactly 0 and is fully priced.
pub fn compute_cost(price: Option<&PriceRow>, usage: Option<&Usage>) -> Cost {
    let Some(usage) = usage else {
        return Cost {
            cost_usd: Some(0.0),
            priced: true,
        };
    };
    let Some(price) = price else {
        return Cost {
            cost_usd: None,
            priced: false,
        };
    };

    // Non-negative counts; a negative upstream count is garbage and clamps
    // to 0 (review M2).
    let count = |value: Option<i64>| value.unwrap_or(0).max(0);
    let input = count(usage.input_tokens);
    let output = count(usage.output_tokens);
    let cache_read = count(usage.cache_read_tokens);
    let cache_write = count(usage.cache_write_tokens);
    let reasoning = count(usage.reasoning_tokens);

    let any_usage_known = [
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_write_tokens,
        usage.reasoning_tokens,
    ]
    .iter()
    .any(Option::is_some);
    let any_price_known = [
        price.input_per_m,
        price.output_per_m,
        price.cache_read_per_m,
        price.cache_write_per_m,
        price.reasoning_per_m,
    ]
    .iter()
    .any(Option::is_some)
        || price.request_fee.is_some();
    if !any_usage_known || !any_price_known {
        return Cost {
            cost_usd: None,
            priced: false,
        };
    }

    // Non-overlapping parts with their applicable prices (D22).
    let parts: [(i64, Option<f64>); 5] = [
        (input.saturating_sub(cache_read).max(0), price.input_per_m),
        (cache_read, price.cache_read_per_m.or(price.input_per_m)),
        (cache_write, price.cache_write_per_m),
        (output.saturating_sub(reasoning).max(0), price.output_per_m),
        (reasoning, price.reasoning_per_m.or(price.output_per_m)),
    ];

    let mut per_m = 0.0;
    let mut priced = true;
    for (tokens, part_price) in parts {
        match part_price {
            Some(part_price) => per_m += tokens as f64 * part_price,
            // No applicable price on a used part contributes 0 and marks the
            // cost incomplete (the spec's `priced = 0` flag).
            None if tokens > 0 => priced = false,
            None => {}
        }
    }
    // A missing `request_fee` means "no per-request fee": sources that charge
    // one publish it (OpenRouter emits a literal 0), so it never degrades
    // `priced` and only contributes when known. The result is clamped at
    // zero: a price row with a negative entry can only waive, never credit.
    let cost_usd = (per_m / 1_000_000.0 + price.request_fee.unwrap_or(0.0)).max(0.0);
    Cost {
        cost_usd: Some(cost_usd),
        priced,
    }
}

/// Total tokens of a settled usage (all five components, unknown = 0).
/// Non-negative and saturating: hostile counts can never make it negative or
/// wrap (review M2).
pub fn total_tokens(usage: &Usage) -> i64 {
    [
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_write_tokens,
        usage.reasoning_tokens,
    ]
    .iter()
    .fold(0i64, |sum, count| {
        sum.saturating_add(count.unwrap_or(0).max(0))
    })
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum MoneyError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit write failed: {0}")]
    Audit(#[from] catalog::CatalogError),
    #[error("{0}")]
    Invalid(String),
    #[error("{0} not found")]
    NotFound(&'static str),
}

impl MoneyError {
    /// Maps the domain error onto the uniform API error shape.
    pub fn into_api_error(self, rid: Option<String>) -> crate::error::ApiError {
        use crate::error::ApiError;
        match self {
            MoneyError::Invalid(message) => ApiError::bad_request(message).with_request_id(rid),
            MoneyError::NotFound(what) => ApiError::not_found(what).with_request_id(rid),
            other => ApiError::internal("money", other).with_request_id(rid),
        }
    }
}

// ---------------------------------------------------------------------------
// Budgets and token caps (decision D9)
// ---------------------------------------------------------------------------

/// One budget as the API returns it: the **current-period view** of a
/// bucket-scoped row set (see [`list_budgets`]).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct BudgetView {
    pub user_id: String,
    pub period: String,
    pub limit_usd: f64,
    /// Spend booked into the *current* bucket; 0 before it has any.
    pub used_usd: f64,
    /// When the current period resets; `null` for `total`.
    pub reset_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One token cap as the API returns it (see [`list_token_caps`]).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct TokenCapView {
    pub user_id: String,
    pub model_id: String,
    pub period: String,
    pub limit_tokens: i64,
    /// Tokens booked into the *current* bucket; 0 before it has any.
    pub used_tokens: i64,
    /// When the current period resets; `null` for `total`.
    pub reset_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One `budgets` bucket row (internal).
struct BudgetRow {
    id: String,
    period: Period,
    /// Bucket key ([`bucket_key`]); rows are bucket-scoped (review M1).
    period_start: String,
    limit_usd: f64,
    used_usd: f64,
    created_at: String,
    updated_at: String,
}

/// One `token_caps` bucket row (internal).
struct CapRow {
    id: String,
    model_id: String,
    period: Period,
    period_start: String,
    limit_tokens: i64,
    used_tokens: i64,
    created_at: String,
    updated_at: String,
}

/// The flat decode shape of a `token_caps` row (see [`load_cap_rows`]).
type CapRowTuple = (String, String, String, String, i64, i64, String, String);

/// Every period in a stable order; the rows a user has configured decide
/// which of them gate spend at all (missing = unlimited).
const PERIODS: [Period; 4] = [Period::Day, Period::Week, Period::Month, Period::Total];

async fn load_budget_rows(
    tx: &mut SqliteConnection,
    user_id: &str,
) -> Result<Vec<BudgetRow>, sqlx::Error> {
    let rows: Vec<(String, String, String, f64, f64, String, String)> = sqlx::query_as(
        "SELECT id, period, period_start, limit_usd, used_usd, created_at, updated_at \
         FROM budgets WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(
            |(id, period, period_start, limit_usd, used_usd, created_at, updated_at)| {
                let period = Period::parse(&period)?;
                Some(BudgetRow {
                    id,
                    period,
                    period_start,
                    limit_usd,
                    used_usd,
                    created_at,
                    updated_at,
                })
            },
        )
        .collect())
}

/// Loads the user's `token_caps` bucket rows, optionally narrowed to one
/// model.
async fn load_cap_rows(
    tx: &mut SqliteConnection,
    user_id: &str,
    model_id: Option<&str>,
) -> Result<Vec<CapRow>, sqlx::Error> {
    let rows: Vec<CapRowTuple> = sqlx::query_as(
        "SELECT id, model_id, period, period_start, limit_tokens, used_tokens, created_at, \
                updated_at \
         FROM token_caps WHERE user_id = ? AND (? IS NULL OR model_id = ?)",
    )
    .bind(user_id)
    .bind(model_id)
    .bind(model_id)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(
            |(
                id,
                model_id,
                period,
                period_start,
                limit_tokens,
                used_tokens,
                created_at,
                updated_at,
            )| {
                let period = Period::parse(&period)?;
                Some(CapRow {
                    id,
                    model_id,
                    period,
                    period_start,
                    limit_tokens,
                    used_tokens,
                    created_at,
                    updated_at,
                })
            },
        )
        .collect())
}

/// The newest bucket row of `period` — the one whose limit is carried into
/// the next bucket (a reset creates a new row, review M1). `None` when the
/// period has no cap rows at all (no cap).
fn newest_budget_row(rows: &[BudgetRow], period: Period) -> Option<&BudgetRow> {
    rows.iter()
        .filter(|row| row.period == period)
        .max_by(|a, b| a.period_start.cmp(&b.period_start))
}

/// Lists a user's budgets as one **current-period view** per configured
/// period (missing = unlimited): `used_usd` is the current bucket's spend (0
/// before the bucket has any), `limit_usd` the limit carried across bucket
/// resets, `reset_at` the end of the current period. Failure mode:
/// [`MoneyError::Db`].
pub async fn list_budgets(
    pool: &SqlitePool,
    tz: Tz,
    user_id: &str,
) -> Result<Vec<BudgetView>, MoneyError> {
    let now = Utc::now();
    let rows = {
        let mut conn = pool.acquire().await?;
        load_budget_rows(&mut conn, user_id).await?
    };
    let mut views = Vec::new();
    for period in PERIODS {
        let Some(newest) = newest_budget_row(&rows, period) else {
            continue;
        };
        let target = bucket_key(tz, period, now);
        let current = rows
            .iter()
            .find(|row| row.period == period && row.period_start == target);
        views.push(BudgetView {
            user_id: user_id.to_string(),
            period: period.as_str().to_string(),
            limit_usd: newest.limit_usd,
            used_usd: current.map(|row| row.used_usd).unwrap_or(0.0),
            reset_at: bucket_end(tz, period, now),
            created_at: newest.created_at.clone(),
            updated_at: newest.updated_at.clone(),
        });
    }
    views.sort_by(|a, b| a.period.cmp(&b.period));
    Ok(views)
}

/// Sets (or replaces) the `limit_usd` of one budget period for the current
/// bucket, audited as `budget.set`. `used_usd` is kept: spend already booked
/// still counts. The transaction is **write-first** (review m3): its first
/// statement takes SQLite's write lock and captures the previous limit in one
/// shot, so concurrent settlements can never fail with spurious
/// `SQLITE_BUSY_SNAPSHOT` upgrades.
///
/// Failure mode: [`MoneyError::Invalid`] for a non-finite/negative limit,
/// [`MoneyError::NotFound`] when the user does not exist.
pub async fn set_budget(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    tz: Tz,
    user_id: &str,
    period: Period,
    limit_usd: f64,
) -> Result<BudgetView, MoneyError> {
    if !limit_usd.is_finite() || limit_usd < 0.0 {
        return Err(MoneyError::Invalid(
            "limit_usd must be a non-negative number".to_string(),
        ));
    }
    let now = Utc::now();
    let target = bucket_key(tz, period, now);
    let mut tx = pool.begin().await?;
    // Write-first anchor: grabs the write lock and returns this bucket's
    // previous limit (the audit detail) in one statement (review m3).
    let previous: Option<f64> = sqlx::query_scalar(
        "UPDATE budgets SET updated_at = updated_at \
         WHERE user_id = ? AND period = ? AND period_start = ? \
         RETURNING limit_usd",
    )
    .bind(user_id)
    .bind(period.as_str())
    .bind(&target)
    .fetch_optional(&mut *tx)
    .await?;
    user_exists(&mut tx, user_id).await?;
    sqlx::query(
        "INSERT INTO budgets \
           (id, user_id, period, period_start, limit_usd, used_usd, reset_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?) \
         ON CONFLICT (user_id, period, period_start) DO UPDATE SET \
           limit_usd = excluded.limit_usd, updated_at = excluded.updated_at",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(period.as_str())
    .bind(&target)
    .bind(limit_usd)
    .bind(bucket_end(tz, period, now))
    .bind(ts(now))
    .bind(ts(now))
    .execute(&mut *tx)
    .await?;
    let detail = serde_json::json!({
        "period": period.as_str(),
        "limit_usd": limit_usd,
        "previous_limit_usd": previous,
    });
    catalog::audit(&mut tx, actor, "budget.set", "user", user_id, &detail).await?;
    tx.commit().await?;

    let row = sqlx::query_as::<_, BudgetView>(
        "SELECT user_id, period, limit_usd, used_usd, reset_at, created_at, updated_at \
         FROM budgets WHERE user_id = ? AND period = ? AND period_start = ?",
    )
    .bind(user_id)
    .bind(period.as_str())
    .bind(&target)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Removes one budget period's cap (unlimited again), audited as
/// `budget.clear`. Every bucket row of the period goes, so the limit is not
/// carried into later buckets either. Returns whether a row existed. The
/// `DELETE` is the transaction's first statement (write-first, review m3).
pub async fn clear_budget(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    user_id: &str,
    period: Period,
) -> Result<bool, MoneyError> {
    let mut tx = pool.begin().await?;
    let deleted = sqlx::query("DELETE FROM budgets WHERE user_id = ? AND period = ?")
        .bind(user_id)
        .bind(period.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted > 0 {
        let detail = serde_json::json!({ "period": period.as_str() });
        catalog::audit(&mut tx, actor, "budget.clear", "user", user_id, &detail).await?;
    }
    tx.commit().await?;
    Ok(deleted > 0)
}

/// Lists a user's per-model token caps as current-period views (see
/// [`list_budgets`]): one entry per configured (model, period). Failure
/// mode: [`MoneyError::Db`].
pub async fn list_token_caps(
    pool: &SqlitePool,
    tz: Tz,
    user_id: &str,
) -> Result<Vec<TokenCapView>, MoneyError> {
    let now = Utc::now();
    let rows = {
        let mut conn = pool.acquire().await?;
        load_cap_rows(&mut conn, user_id, None).await?
    };
    // Group by (model, period); the map keeps the old `ORDER BY model_id,
    // period` listing order.
    let mut groups: BTreeMap<(&str, &str), Vec<&CapRow>> = BTreeMap::new();
    for row in &rows {
        groups
            .entry((row.model_id.as_str(), row.period.as_str()))
            .or_default()
            .push(row);
    }
    let mut views = Vec::new();
    for ((model_id, _), group) in &groups {
        let Some(newest) = group
            .iter()
            .max_by(|a, b| a.period_start.cmp(&b.period_start))
            .copied()
        else {
            continue;
        };
        let target = bucket_key(tz, newest.period, now);
        let current = group.iter().find(|row| row.period_start == target).copied();
        views.push(TokenCapView {
            user_id: user_id.to_string(),
            model_id: (*model_id).to_string(),
            period: newest.period.as_str().to_string(),
            limit_tokens: newest.limit_tokens,
            used_tokens: current.map(|row| row.used_tokens).unwrap_or(0),
            reset_at: bucket_end(tz, newest.period, now),
            created_at: newest.created_at.clone(),
            updated_at: newest.updated_at.clone(),
        });
    }
    Ok(views)
}

/// Sets (or replaces) one per-model token cap for the current bucket,
/// audited as `token_cap.set`. Write-first like [`set_budget`] (review m3).
///
/// Failure mode: [`MoneyError::Invalid`] for a negative limit,
/// [`MoneyError::NotFound`] when the user or model does not exist.
pub async fn set_token_cap(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    tz: Tz,
    user_id: &str,
    model_id: &str,
    period: Period,
    limit_tokens: i64,
) -> Result<TokenCapView, MoneyError> {
    if limit_tokens < 0 {
        return Err(MoneyError::Invalid(
            "limit_tokens must not be negative".to_string(),
        ));
    }
    let now = Utc::now();
    let target = bucket_key(tz, period, now);
    let mut tx = pool.begin().await?;
    // Write-first anchor: grabs the write lock and returns this bucket's
    // previous limit in one statement (review m3).
    let previous: Option<i64> = sqlx::query_scalar(
        "UPDATE token_caps SET updated_at = updated_at \
         WHERE user_id = ? AND model_id = ? AND period = ? AND period_start = ? \
         RETURNING limit_tokens",
    )
    .bind(user_id)
    .bind(model_id)
    .bind(period.as_str())
    .bind(&target)
    .fetch_optional(&mut *tx)
    .await?;
    user_exists(&mut tx, user_id).await?;
    let model: Option<i64> = sqlx::query_scalar("SELECT 1 FROM models WHERE id = ?")
        .bind(model_id)
        .fetch_optional(&mut *tx)
        .await?;
    if model.is_none() {
        return Err(MoneyError::NotFound("model"));
    }
    sqlx::query(
        "INSERT INTO token_caps \
           (id, user_id, model_id, period, period_start, limit_tokens, used_tokens, reset_at, \
            created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?, ?) \
         ON CONFLICT (user_id, model_id, period, period_start) DO UPDATE SET \
           limit_tokens = excluded.limit_tokens, updated_at = excluded.updated_at",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(model_id)
    .bind(period.as_str())
    .bind(&target)
    .bind(limit_tokens)
    .bind(bucket_end(tz, period, now))
    .bind(ts(now))
    .bind(ts(now))
    .execute(&mut *tx)
    .await?;
    let detail = serde_json::json!({
        "model_id": model_id,
        "period": period.as_str(),
        "limit_tokens": limit_tokens,
        "previous_limit_tokens": previous,
    });
    catalog::audit(&mut tx, actor, "token_cap.set", "user", user_id, &detail).await?;
    tx.commit().await?;

    let row = sqlx::query_as::<_, TokenCapView>(
        "SELECT user_id, model_id, period, limit_tokens, used_tokens, reset_at, created_at, \
                updated_at \
         FROM token_caps WHERE user_id = ? AND model_id = ? AND period = ? AND period_start = ?",
    )
    .bind(user_id)
    .bind(model_id)
    .bind(period.as_str())
    .bind(&target)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Removes one token cap (every bucket of the period), audited as
/// `token_cap.clear`. Returns whether a row existed. Write-first (review m3).
pub async fn clear_token_cap(
    pool: &SqlitePool,
    actor: &Actor<'_>,
    user_id: &str,
    model_id: &str,
    period: Period,
) -> Result<bool, MoneyError> {
    let mut tx = pool.begin().await?;
    let deleted =
        sqlx::query("DELETE FROM token_caps WHERE user_id = ? AND model_id = ? AND period = ?")
            .bind(user_id)
            .bind(model_id)
            .bind(period.as_str())
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if deleted > 0 {
        let detail = serde_json::json!({ "model_id": model_id, "period": period.as_str() });
        catalog::audit(&mut tx, actor, "token_cap.clear", "user", user_id, &detail).await?;
    }
    tx.commit().await?;
    Ok(deleted > 0)
}

async fn user_exists(tx: &mut SqliteConnection, user_id: &str) -> Result<(), MoneyError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    if found.is_none() {
        return Err(MoneyError::NotFound("user"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reservations (decision D7)
// ---------------------------------------------------------------------------

/// Why a reservation was refused; maps to `402 budget_exceeded` or
/// `429 token_cap_exceeded`.
#[derive(Debug, thiserror::Error)]
pub enum ReservationDenied {
    #[error("the {period} spend limit is exhausted")]
    Budget { period: Period },
    #[error("the {period} token limit for model {model_id} is exhausted")]
    TokenCap { period: Period, model_id: String },
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// What to reserve for one in-flight request.
#[derive(Debug, Clone)]
pub struct ReservationInput {
    pub request_id: String,
    pub user_id: String,
    pub api_key_id: Option<String>,
    /// First attempt's offering (the reservation's ledger row and the
    /// token-cap join; the USD estimate is priced on the first *priced*
    /// candidate, review m4).
    pub offering_id: String,
    /// First candidate's model (the token cap checked up front).
    pub model_id: String,
    pub estimated_tokens: i64,
    pub estimated_usd: f64,
}

/// Opens the reservation for one request, or refuses it.
///
/// The insert is the transaction's **first statement**, so the transaction
/// holds SQLite's write lock across the cap checks: two concurrent requests
/// can never both pass the same budget (the loser either waits and then sees
/// the winner's row, or gets a busy error). The check is `used + open > limit`
/// where the open sum includes the row just inserted — the spec's `used +
/// open + estimate > limit` without a double count.
///
/// Failure mode: [`ReservationDenied`] (`Budget`/`TokenCap` on refusal,
/// `Db` on storage failure); the transaction is rolled back on refusal so no
/// reservation row survives a denial.
pub async fn open_reservation(
    pool: &SqlitePool,
    tz: Tz,
    input: &ReservationInput,
) -> Result<(), ReservationDenied> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;

    // Write-first: from here on the transaction serializes on the write lock.
    sqlx::query(
        "INSERT INTO reservations \
           (id, request_id, user_id, api_key_id, offering_id, estimated_tokens, estimated_usd, \
            created_at, status) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'open')",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&input.request_id)
    .bind(&input.user_id)
    .bind(&input.api_key_id)
    .bind(&input.offering_id)
    .bind(input.estimated_tokens)
    .bind(input.estimated_usd)
    .bind(ts(now))
    .execute(&mut *tx)
    .await?;

    let denied = check_caps(&mut tx, tz, input, now).await;
    match denied {
        Some(reason) => {
            tx.rollback().await?;
            Err(reason)
        }
        None => {
            tx.commit().await?;
            Ok(())
        }
    }
}

/// The cap checks run inside the reservation transaction (after the insert,
/// which already holds SQLite's write lock). Each configured period is gated
/// on the row of the *current* bucket: a bucket that does not exist yet is
/// created carrying the newest limit forward (the reset creates a new row —
/// review M1), so a period rollover can never drop a configured cap.
async fn check_caps(
    tx: &mut SqliteConnection,
    tz: Tz,
    input: &ReservationInput,
    now: DateTime<Utc>,
) -> Option<ReservationDenied> {
    // USD budgets: every configured period must have headroom in its current
    // bucket. The open-reservation sum already includes the row just
    // inserted, so the check is the spec's `used + open + estimate > limit`
    // without a double count.
    let budgets = match load_budget_rows(tx, &input.user_id).await {
        Ok(rows) => rows,
        Err(e) => return Some(ReservationDenied::Db(e)),
    };
    let open_usd: f64 = match sqlx::query_scalar(
        "SELECT COALESCE(SUM(estimated_usd), 0.0) FROM reservations \
         WHERE user_id = ? AND status = 'open'",
    )
    .bind(&input.user_id)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(sum) => sum,
        Err(e) => return Some(ReservationDenied::Db(e)),
    };
    for period in PERIODS {
        let Some(newest) = newest_budget_row(&budgets, period) else {
            continue; // no cap configured for this period
        };
        let target = bucket_key(tz, period, now);
        let used = match budgets
            .iter()
            .find(|row| row.period == period && row.period_start == target)
        {
            Some(row) => row.used_usd,
            None => {
                // The reset: a fresh counter row for the new bucket, its
                // limit carried forward from the newest row.
                if let Err(e) = insert_budget_bucket(
                    tx,
                    &input.user_id,
                    period,
                    &target,
                    newest.limit_usd,
                    0.0,
                    bucket_end(tz, period, now),
                    now,
                )
                .await
                {
                    return Some(ReservationDenied::Db(e));
                }
                0.0
            }
        };
        if used + open_usd > newest.limit_usd {
            return Some(ReservationDenied::Budget { period });
        }
    }

    // Per-model token caps: the reservation's own model must have headroom.
    let caps = match load_cap_rows(tx, &input.user_id, Some(&input.model_id)).await {
        Ok(rows) => rows,
        Err(e) => return Some(ReservationDenied::Db(e)),
    };
    if !caps.is_empty() {
        let open_tokens: i64 = match sqlx::query_scalar(
            "SELECT COALESCE(SUM(r.estimated_tokens), 0) FROM reservations r \
             JOIN offerings o ON o.id = r.offering_id \
             WHERE r.user_id = ? AND r.status = 'open' AND o.model_id = ?",
        )
        .bind(&input.user_id)
        .bind(&input.model_id)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(sum) => sum,
            Err(e) => return Some(ReservationDenied::Db(e)),
        };
        for period in PERIODS {
            let Some(newest) = newest_cap_row(&caps, period) else {
                continue;
            };
            let target = bucket_key(tz, period, now);
            let used = match caps
                .iter()
                .find(|row| row.period == period && row.period_start == target)
            {
                Some(row) => row.used_tokens,
                None => {
                    if let Err(e) = insert_cap_bucket(
                        tx,
                        &input.user_id,
                        &input.model_id,
                        period,
                        &target,
                        newest.limit_tokens,
                        0,
                        bucket_end(tz, period, now),
                        now,
                    )
                    .await
                    {
                        return Some(ReservationDenied::Db(e));
                    }
                    0
                }
            };
            if used + open_tokens > newest.limit_tokens {
                return Some(ReservationDenied::TokenCap {
                    period,
                    model_id: input.model_id.clone(),
                });
            }
        }
    }
    None
}

/// Cap twin of [`newest_budget_row`] (the model filter is applied at load).
fn newest_cap_row(rows: &[CapRow], period: Period) -> Option<&CapRow> {
    rows.iter()
        .filter(|row| row.period == period)
        .max_by(|a, b| a.period_start.cmp(&b.period_start))
}

/// Inserts a fresh `budgets` bucket row (the reset or the first row of a
/// straddling request's bucket). Failure mode: [`sqlx::Error`].
#[allow(clippy::too_many_arguments)]
async fn insert_budget_bucket(
    tx: &mut SqliteConnection,
    user_id: &str,
    period: Period,
    period_start: &str,
    limit_usd: f64,
    used_usd: f64,
    reset_at: Option<String>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO budgets \
           (id, user_id, period, period_start, limit_usd, used_usd, reset_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(period.as_str())
    .bind(period_start)
    .bind(limit_usd)
    .bind(used_usd)
    .bind(reset_at.as_deref())
    .bind(ts(now))
    .bind(ts(now))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Inserts a fresh `token_caps` bucket row. Failure mode: [`sqlx::Error`].
#[allow(clippy::too_many_arguments)]
async fn insert_cap_bucket(
    tx: &mut SqliteConnection,
    user_id: &str,
    model_id: &str,
    period: Period,
    period_start: &str,
    limit_tokens: i64,
    used_tokens: i64,
    reset_at: Option<String>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO token_caps \
           (id, user_id, model_id, period, period_start, limit_tokens, used_tokens, reset_at, \
            created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(model_id)
    .bind(period.as_str())
    .bind(period_start)
    .bind(limit_tokens)
    .bind(used_tokens)
    .bind(reset_at.as_deref())
    .bind(ts(now))
    .bind(ts(now))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// What settlement did with the request's reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleOutcome {
    /// Nothing to book: rejected requests never reserve, and a request whose
    /// reservation is already `settled` is never booked twice (the settle
    /// marker — review B1).
    NoReservation,
    /// The request consumed nothing; the estimate was released.
    Released,
    /// The request consumed usage; actuals were booked exactly once and the
    /// row settled.
    Settled,
}

/// What to book at settlement.
#[derive(Debug, Clone)]
pub struct SettleInput {
    pub request_id: String,
    pub user_id: String,
    /// The model actually used (its token caps are charged).
    pub model_id: Option<String>,
    /// The wall-clock start of the request: the bucket every period books
    /// into, even when that bucket already reset (review M1).
    pub started_at: DateTime<Utc>,
    /// The request consumed usage (it was sent upstream).
    pub billable: bool,
    /// Actual cost, `None` when unpriced (books 0 — never a silent 0 on the
    /// ledger itself, which keeps `cost_usd` NULL).
    pub cost_usd: Option<f64>,
    /// Actual total tokens consumed.
    pub tokens: i64,
}

/// Settles the request's reservation inside the caller's transaction: moves
/// it to `settled` (billable) or `released` (nothing consumed) and books the
/// actuals into the bucket derived from `started_at` of every configured
/// period.
///
/// **Exactly-once** (review B1): the `UPDATE ... WHERE status <> 'settled'
/// RETURNING id` is this transaction's first statement when the caller has
/// not written yet, so concurrent settlements serialize on SQLite's write
/// lock and only the first can claim the marker — a second settlement (or a
/// crash-and-retry) matches nothing and books nothing. Booking is keyed on
/// the marker, never on `status = 'open'`: a reservation the reconciler
/// released is still booked.
///
/// Failure mode: [`sqlx::Error`] — the caller rolls the whole settlement back,
/// so the reservation row and the ledger row commit together or not at all.
pub async fn settle_reservation(
    tx: &mut SqliteConnection,
    tz: Tz,
    input: &SettleInput,
) -> Result<SettleOutcome, sqlx::Error> {
    let now = Utc::now();
    let marker: Option<(String,)> = match input.billable {
        true => sqlx::query_as(
            "UPDATE reservations SET status = 'settled', settled_at = ? \
                 WHERE request_id = ? AND status <> 'settled' \
                 RETURNING id",
        ),
        false => sqlx::query_as(
            "UPDATE reservations SET status = 'released', released_at = ? \
                 WHERE request_id = ? AND status <> 'settled' \
                 RETURNING id",
        ),
    }
    .bind(ts(now))
    .bind(&input.request_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(_marker) = marker else {
        return Ok(SettleOutcome::NoReservation);
    };
    if !input.billable {
        return Ok(SettleOutcome::Released);
    }

    // Book the actuals into the bucket derived from `started_at` of every
    // configured period — including a bucket that already reset: its old row
    // still receives the spend (review M1). A non-negative cost and
    // saturating token math keep hostile actuals from crediting the ledger
    // (review M2).
    let cost = input.cost_usd.unwrap_or(0.0).max(0.0);
    let tokens = input.tokens.max(0);
    let budgets = load_budget_rows(&mut *tx, &input.user_id).await?;
    for period in PERIODS {
        let Some(newest) = newest_budget_row(&budgets, period) else {
            continue; // no cap configured for this period
        };
        let target = bucket_key(tz, period, input.started_at);
        match budgets
            .iter()
            .find(|row| row.period == period && row.period_start == target)
        {
            Some(row) => {
                let used_usd = row.used_usd + cost;
                sqlx::query("UPDATE budgets SET used_usd = ?, updated_at = ? WHERE id = ?")
                    .bind(used_usd)
                    .bind(ts(now))
                    .bind(&row.id)
                    .execute(&mut *tx)
                    .await?;
            }
            None => {
                // The request's bucket predates the current row (it started
                // just before a reset) — give it its own row, limit carried
                // forward like at any reset.
                insert_budget_bucket(
                    &mut *tx,
                    &input.user_id,
                    period,
                    &target,
                    newest.limit_usd,
                    cost,
                    bucket_end(tz, period, input.started_at),
                    now,
                )
                .await?;
            }
        }
    }

    if let Some(model_id) = &input.model_id {
        let caps = load_cap_rows(&mut *tx, &input.user_id, Some(model_id)).await?;
        for period in PERIODS {
            let Some(newest) = newest_cap_row(&caps, period) else {
                continue;
            };
            let target = bucket_key(tz, period, input.started_at);
            match caps
                .iter()
                .find(|row| row.period == period && row.period_start == target)
            {
                Some(row) => {
                    let used_tokens = row.used_tokens.saturating_add(tokens);
                    sqlx::query(
                        "UPDATE token_caps SET used_tokens = ?, updated_at = ? WHERE id = ?",
                    )
                    .bind(used_tokens)
                    .bind(ts(now))
                    .bind(&row.id)
                    .execute(&mut *tx)
                    .await?;
                }
                None => {
                    insert_cap_bucket(
                        &mut *tx,
                        &input.user_id,
                        model_id,
                        period,
                        &target,
                        newest.limit_tokens,
                        tokens,
                        bucket_end(tz, period, input.started_at),
                        now,
                    )
                    .await?;
                }
            }
        }
    }

    Ok(SettleOutcome::Settled)
}

/// Releases `open` reservations older than `cutoff`; the reconciler's core.
/// Releases stamp `released_at`, never `settled_at` (review nit) — and
/// because settlement books actuals for any non-`settled` reservation, a
/// release can never hide spend (review B1), it only frees headroom.
/// Returns how many rows were released. Failure mode: [`sqlx::Error`].
pub async fn release_stale(pool: &SqlitePool, cutoff: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE reservations SET status = 'released', released_at = ? \
         WHERE status = 'open' AND created_at < ?",
    )
    .bind(ts(Utc::now()))
    .bind(ts(cutoff))
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// The background reconciler: every [`RECONCILE_INTERVAL`] it releases
/// reservations that stayed open longer than [`reservation_ttl`] of the
/// configured `timeout_total` (crashed settlements, killed processes) so
/// their headroom cannot lock a budget forever. Errors are logged and
/// retried on the next tick.
pub fn spawn_reconciler(pool: SqlitePool, timeout_total: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let ttl = reservation_ttl(timeout_total);
        let mut ticker = tokio::time::interval(RECONCILE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let cutoff = Utc::now() - chrono::Duration::from_std(ttl).unwrap_or_default();
            match release_stale(&pool, cutoff).await {
                Ok(0) => {}
                Ok(released) => {
                    tracing::info!(released, "released stale reservations");
                }
                Err(e) => tracing::error!(error = %e, "reservation reconciler failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        offering_id: &str,
        input: Option<f64>,
        output: Option<f64>,
        cache_read: Option<f64>,
        cache_write: Option<f64>,
        reasoning: Option<f64>,
        request_fee: Option<f64>,
    ) -> PriceRow {
        PriceRow {
            offering_id: offering_id.to_string(),
            input_per_m: input,
            output_per_m: output,
            cache_read_per_m: cache_read,
            cache_write_per_m: cache_write,
            reasoning_per_m: reasoning,
            request_fee,
            variable_price: 0,
            currency: "USD".to_string(),
            source: "manual".to_string(),
            source_url: None,
            fetched_at: None,
            updated_by: None,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        }
    }

    #[test]
    fn cost_math_covers_every_price_component() {
        let price = row(
            "off",
            Some(2.5),
            Some(10.0),
            Some(0.5),
            Some(2.0),
            Some(5.0),
            Some(0.0004),
        );
        // Non-overlapping parts (D22): 1M uncached input, 1M cached input,
        // 1M cache-write, 1M plain output, 1M reasoning.
        let usage = Usage {
            input_tokens: Some(2_000_000),
            output_tokens: Some(2_000_000),
            cache_read_tokens: Some(1_000_000),
            cache_write_tokens: Some(1_000_000),
            reasoning_tokens: Some(1_000_000),
        };
        // (2.5 + 0.5 + 2 + 10 + 5) + 0.0004
        let cost = compute_cost(Some(&price), Some(&usage));
        assert_eq!(cost.cost_usd, Some(20.0004));
        assert!(cost.priced);

        // Fully overlapping counts are billed once, never twice (review M3):
        // every input token is cached and every output token is reasoning.
        let overlapping = Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            cache_read_tokens: Some(1_000_000),
            cache_write_tokens: Some(0),
            reasoning_tokens: Some(1_000_000),
        };
        // 0*2.5 + 1M*0.5 + 0*2 + 0*10 + 1M*5 + 0.0004
        let cost = compute_cost(Some(&price), Some(&overlapping));
        assert_eq!(cost.cost_usd, Some(5.5004));
        assert!(cost.priced, "every part has an applicable price");
    }

    #[test]
    fn subset_prices_fall_back_to_their_superset_price() {
        // reasoning priced NULL + reasoning used → the row is still fully
        // priced when output_per_m exists (D22): the reasoning part falls
        // back to the output price. Same for cached input vs the input price.
        let price = row("off", Some(2.5), Some(10.0), None, None, None, None);
        let usage = Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            cache_read_tokens: Some(1_000_000),
            cache_write_tokens: None,
            reasoning_tokens: Some(1_000_000),
        };
        // 0*2.5 (all input cached) + 1M*2.5 (fallback) + 0*10 + 1M*10
        // (fallback)
        let cost = compute_cost(Some(&price), Some(&usage));
        assert_eq!(cost.cost_usd, Some(12.5));
        assert!(
            cost.priced,
            "missing subset prices inherit the superset price; no false priced=0"
        );

        // Without any applicable price the used part contributes 0 and flags
        // the row incomplete.
        let blank = row("off", None, None, None, None, None, None);
        let cost = compute_cost(Some(&blank), Some(&usage));
        assert_eq!(cost.cost_usd, None, "no known price at all is unpriced");
        assert!(!cost.priced);
    }

    #[test]
    fn hostile_usage_counts_never_credit_or_overflow() {
        // Negative and absurd counts (review M2): the cost can only debit and
        // the token sum is saturating — never negative, never a panic.
        let price = row("off", Some(2.5), Some(10.0), None, None, None, None);
        let hostile = Usage {
            input_tokens: Some(-5_000_000),
            output_tokens: Some(i64::MAX),
            cache_read_tokens: Some(i64::MIN),
            cache_write_tokens: None,
            reasoning_tokens: Some(i64::MAX),
        };
        let cost = compute_cost(Some(&price), Some(&hostile))
            .cost_usd
            .expect("priced");
        assert!(cost.is_finite(), "no overflow: {cost}");
        assert!(cost >= 0.0, "hostile counts can only debit: {cost}");

        let total = total_tokens(&hostile);
        assert!(total >= 0, "saturating sum stays non-negative: {total}");
        assert_eq!(total, i64::MAX, "the sum saturates instead of wrapping");

        // The extreme edge: every count at i64::MAX still prices finitely.
        let extreme = Usage {
            input_tokens: Some(i64::MAX),
            output_tokens: Some(i64::MAX),
            cache_read_tokens: Some(i64::MAX),
            cache_write_tokens: Some(i64::MAX),
            reasoning_tokens: Some(i64::MAX),
        };
        let cost = compute_cost(Some(&price), Some(&extreme))
            .cost_usd
            .expect("priced");
        assert!(cost.is_finite() && cost >= 0.0, "{cost}");
    }

    #[test]
    fn missing_components_contribute_zero_and_flag_the_row_incomplete() {
        // output priced, everything else unknown; only input+output used.
        let price = row("off", None, Some(10.0), None, None, None, None);
        let usage = Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            ..Usage::default()
        };
        let cost = compute_cost(Some(&price), Some(&usage));
        assert_eq!(
            cost.cost_usd,
            Some(10.0),
            "unknown input price contributes 0"
        );
        assert!(!cost.priced, "a used component without price is priced=0");

        // The same row with no input usage is complete: the missing price is
        // never used.
        let usage = Usage {
            input_tokens: Some(0),
            output_tokens: Some(1_000_000),
            ..Usage::default()
        };
        let cost = compute_cost(Some(&price), Some(&usage));
        assert_eq!(cost.cost_usd, Some(10.0));
        assert!(cost.priced);
    }

    #[test]
    fn unpriced_rows_stay_null() {
        let usage = Usage {
            input_tokens: Some(10),
            output_tokens: Some(10),
            ..Usage::default()
        };
        // No price row at all.
        let cost = compute_cost(None, Some(&usage));
        assert_eq!(cost.cost_usd, None);
        assert!(!cost.priced);

        // A price row where nothing is known is still unpriced (unknown is
        // never 0).
        let blank = row("off", None, None, None, None, None, None);
        let cost = compute_cost(Some(&blank), Some(&usage));
        assert_eq!(cost.cost_usd, None);
        assert!(!cost.priced);
    }

    #[test]
    fn unknown_usage_cannot_be_priced() {
        let price = row("off", Some(1.0), Some(2.0), None, None, None, Some(0.0));
        let cost = compute_cost(Some(&price), Some(&Usage::default()));
        assert_eq!(cost.cost_usd, None);
        assert!(!cost.priced);
    }

    #[test]
    fn no_consumption_costs_exactly_zero() {
        let cost = compute_cost(None, None);
        assert_eq!(cost.cost_usd, Some(0.0));
        assert!(cost.priced);
    }

    #[test]
    fn request_fee_is_known_when_set_and_absent_means_no_fee() {
        let usage = Usage {
            input_tokens: Some(0),
            output_tokens: Some(0),
            ..Usage::default()
        };
        let with_fee = row("off", Some(1.0), Some(1.0), None, None, None, Some(0.002));
        let cost = compute_cost(Some(&with_fee), Some(&usage));
        assert_eq!(cost.cost_usd, Some(0.002));

        let no_fee = row("off", Some(1.0), Some(1.0), None, None, None, None);
        let cost = compute_cost(Some(&no_fee), Some(&usage));
        assert_eq!(cost.cost_usd, Some(0.0));
        assert!(
            cost.priced,
            "an absent request fee is 'no fee', not unknown"
        );
    }

    #[test]
    fn week_starts_on_monday() {
        let tz = Tz::UTC;
        // 2026-11-16 is a Monday; 2026-11-15 a Sunday.
        let sunday = Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap();
        let monday = Utc.with_ymd_and_hms(2026, 11, 16, 0, 0, 0).unwrap();
        assert_eq!(
            period_start(tz, Period::Week, sunday).unwrap().timestamp(),
            Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 0)
                .unwrap()
                .timestamp()
        );
        assert_eq!(
            period_end(tz, Period::Week, sunday).unwrap().timestamp(),
            monday.timestamp()
        );
        assert_eq!(
            period_start(tz, Period::Week, monday).unwrap().timestamp(),
            monday.timestamp()
        );
    }

    #[test]
    fn period_boundaries_follow_the_configured_timezone() {
        // Tehran is UTC+3:30 with no DST — local midnight is 20:30 UTC the
        // day before. The helpers return UTC instants.
        let tz: Tz = "Asia/Tehran".parse().expect("tz");
        let at = Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap();
        let start = period_start(tz, Period::Day, at).unwrap();
        let end = period_end(tz, Period::Day, at).unwrap();
        assert_eq!(
            start.format("%Y-%m-%d %H:%M").to_string(),
            "2026-11-15 20:30",
            "local 2026-11-16 00:00"
        );
        assert_eq!(
            end.format("%Y-%m-%d %H:%M").to_string(),
            "2026-11-16 20:30",
            "local 2026-11-17 00:00"
        );
        assert_eq!(day_key(tz, at), "2026-11-16");
        assert_eq!(day_key(tz, end), "2026-11-17");

        // Month boundaries are local too: local 2026-12-01 00:00.
        let month_end = period_end(tz, Period::Month, at).unwrap();
        assert_eq!(
            month_end.format("%Y-%m-%d %H:%M").to_string(),
            "2026-11-30 20:30"
        );
    }

    #[test]
    fn total_never_resets() {
        assert_eq!(period_start(Tz::UTC, Period::Total, Utc::now()), None);
        assert_eq!(period_end(Tz::UTC, Period::Total, Utc::now()), None);
    }

    #[test]
    fn bucket_keys_are_start_derived_and_total_uses_the_sentinel() {
        let tz = Tz::UTC;
        let started = Utc.with_ymd_and_hms(2026, 11, 16, 23, 59, 0).unwrap();
        assert_eq!(
            bucket_key(tz, Period::Day, started),
            "2026-11-16T00:00:00.000Z"
        );
        assert_eq!(bucket_key(tz, Period::Total, started), TOTAL_BUCKET);
        assert_eq!(
            bucket_end(tz, Period::Day, started).as_deref(),
            Some("2026-11-17T00:00:00.000Z")
        );
        assert_eq!(bucket_end(tz, Period::Total, started), None);

        // A request that starts at 23:59 keys to the 23:59 bucket even when
        // it settles after midnight: the bucket derives from the start, not
        // from the settlement clock (review M1).
        let settled_after = Utc.with_ymd_and_hms(2026, 11, 17, 0, 0, 5).unwrap();
        assert_eq!(
            bucket_key(tz, Period::Day, settled_after),
            "2026-11-17T00:00:00.000Z"
        );
    }

    #[test]
    fn reservation_ttl_always_exceeds_the_total_timeout() {
        // The reconciler can never race a live request (review B1).
        for timeout_total in [
            Duration::ZERO,
            Duration::from_secs(60),
            Duration::from_secs(600),
            Duration::from_secs(3_600),
        ] {
            let ttl = reservation_ttl(timeout_total);
            assert!(ttl > timeout_total, "{timeout_total:?} -> {ttl:?}");
            assert!(ttl >= MIN_RESERVATION_TTL, "{ttl:?}");
        }
    }

    async fn pool() -> (std::path::PathBuf, SqlitePool) {
        let dir = std::env::temp_dir().join(format!("gateway-money-{}", uuid::Uuid::new_v4()));
        let pool = crate::db::open(&dir.join("gateway.db"))
            .await
            .expect("open database");
        (dir, pool)
    }

    async fn seed_user(pool: &SqlitePool) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, role, is_active) \
             VALUES (?, ?, ?, 'x', 'user', 1)",
        )
        .bind(&id)
        .bind(format!("{id}@example.com"))
        .bind("Test User")
        .execute(pool)
        .await
        .expect("insert user");
        id
    }

    async fn seed_model(pool: &SqlitePool) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO models (id, canonical_name) VALUES (?, ?)")
            .bind(&id)
            .bind(format!("model-{id}"))
            .execute(pool)
            .await
            .expect("insert model");
        id
    }

    async fn seed_offering(pool: &SqlitePool, model_id: &str) -> String {
        let provider_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO providers (id, name, base_url) VALUES (?, ?, 'https://x.example/v1')",
        )
        .bind(&provider_id)
        .bind(format!("provider-{provider_id}"))
        .execute(pool)
        .await
        .expect("insert provider");
        let offering_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO offerings (id, provider_id, model_id, upstream_model_id) \
             VALUES (?, ?, ?, 'up-1')",
        )
        .bind(&offering_id)
        .bind(&provider_id)
        .bind(model_id)
        .execute(pool)
        .await
        .expect("insert offering");
        offering_id
    }

    fn reservation(
        request_id: &str,
        user_id: &str,
        offering_id: &str,
        model_id: &str,
        tokens: i64,
        usd: f64,
    ) -> ReservationInput {
        ReservationInput {
            request_id: request_id.to_string(),
            user_id: user_id.to_string(),
            api_key_id: None,
            offering_id: offering_id.to_string(),
            model_id: model_id.to_string(),
            estimated_tokens: tokens,
            estimated_usd: usd,
        }
    }

    #[tokio::test]
    async fn budget_reservation_rejects_when_exhausted() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let offering = seed_offering(&pool, &model).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };
        set_budget(&pool, &actor, Tz::UTC, &user, Period::Total, 1.0)
            .await
            .expect("budget");

        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("req-1", &user, &offering, &model, 100, 0.5),
        )
        .await
        .expect("first reservation fits");
        let denied = open_reservation(
            &pool,
            Tz::UTC,
            &reservation("req-2", &user, &offering, &model, 100, 0.6),
        )
        .await
        .expect_err("second must not fit");
        assert!(
            matches!(
                denied,
                ReservationDenied::Budget {
                    period: Period::Total
                }
            ),
            "{denied:?}"
        );

        // The denial rolled back its row: no leaked reservation.
        let open: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM reservations WHERE status = 'open'")
                .fetch_one(&pool)
                .await
                .expect("count");
        assert_eq!(open, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn two_concurrent_reservations_cannot_exceed_one_budget() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let offering = seed_offering(&pool, &model).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };
        // The limit admits exactly one of the two estimates.
        set_budget(&pool, &actor, Tz::UTC, &user, Period::Total, 1.0)
            .await
            .expect("budget");

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for i in 0..2 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            let input = reservation(&format!("race-{i}"), &user, &offering, &model, 100, 0.75);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                open_reservation(&pool, Tz::UTC, &input).await
            }));
        }
        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(handle.await.expect("join"));
        }
        let ok = outcomes.iter().filter(|result| result.is_ok()).count();
        let denied = outcomes
            .iter()
            .filter(|result| matches!(result, Err(ReservationDenied::Budget { .. })))
            .count();
        assert_eq!(ok, 1, "exactly one request may reserve: {outcomes:?}");
        assert_eq!(denied, 1, "the other is refused: {outcomes:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn token_cap_reservation_rejects_when_exhausted() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let offering = seed_offering(&pool, &model).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };
        set_token_cap(&pool, &actor, Tz::UTC, &user, &model, Period::Day, 1000)
            .await
            .expect("cap");

        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("cap-1", &user, &offering, &model, 600, 0.0),
        )
        .await
        .expect("first fits");
        let denied = open_reservation(
            &pool,
            Tz::UTC,
            &reservation("cap-2", &user, &offering, &model, 500, 0.0),
        )
        .await
        .expect_err("second must not fit");
        assert!(
            matches!(
                denied,
                ReservationDenied::TokenCap {
                    period: Period::Day,
                    ..
                }
            ),
            "{denied:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn settling_books_actuals_and_frees_the_reservation() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let offering = seed_offering(&pool, &model).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };
        set_budget(&pool, &actor, Tz::UTC, &user, Period::Total, 10.0)
            .await
            .expect("budget");
        set_token_cap(&pool, &actor, Tz::UTC, &user, &model, Period::Total, 10_000)
            .await
            .expect("cap");

        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("s-1", &user, &offering, &model, 900, 1.0),
        )
        .await
        .expect("reserve");

        let mut tx = pool.begin().await.expect("tx");
        let outcome = settle_reservation(
            &mut tx,
            Tz::UTC,
            &SettleInput {
                request_id: "s-1".to_string(),
                user_id: user.clone(),
                model_id: Some(model.clone()),
                started_at: Utc::now(),
                billable: true,
                cost_usd: Some(2.5),
                tokens: 800,
            },
        )
        .await
        .expect("settle");
        tx.commit().await.expect("commit");
        assert_eq!(outcome, SettleOutcome::Settled);

        let (used, budget_reset): (f64, Option<String>) =
            sqlx::query_as("SELECT used_usd, reset_at FROM budgets WHERE user_id = ?")
                .bind(&user)
                .fetch_one(&pool)
                .await
                .expect("budget row");
        assert_eq!(used, 2.5);
        assert_eq!(budget_reset, None, "total never resets");
        let (tokens_used,): (i64,) =
            sqlx::query_as("SELECT used_tokens FROM token_caps WHERE user_id = ?")
                .bind(&user)
                .fetch_one(&pool)
                .await
                .expect("cap row");
        assert_eq!(tokens_used, 800);

        let (status, settled_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, settled_at FROM reservations WHERE request_id = 's-1'")
                .fetch_one(&pool)
                .await
                .expect("reservation row");
        assert_eq!(status, "settled");
        assert!(settled_at.is_some());

        // Settling again must not double-book anything.
        let mut tx = pool.begin().await.expect("tx");
        let outcome = settle_reservation(
            &mut tx,
            Tz::UTC,
            &SettleInput {
                request_id: "s-1".to_string(),
                user_id: user.clone(),
                model_id: Some(model.clone()),
                started_at: Utc::now(),
                billable: true,
                cost_usd: Some(2.5),
                tokens: 800,
            },
        )
        .await
        .expect("second settle");
        tx.commit().await.expect("commit");
        assert_eq!(outcome, SettleOutcome::NoReservation);
        let used: f64 = sqlx::query_scalar("SELECT used_usd FROM budgets WHERE user_id = ?")
            .bind(&user)
            .fetch_one(&pool)
            .await
            .expect("budget row");
        assert_eq!(used, 2.5, "no double-settle");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn releasing_frees_headroom_without_booking() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let offering = seed_offering(&pool, &model).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };
        set_budget(&pool, &actor, Tz::UTC, &user, Period::Total, 1.0)
            .await
            .expect("budget");

        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("r-1", &user, &offering, &model, 100, 0.6),
        )
        .await
        .expect("reserve");
        let mut tx = pool.begin().await.expect("tx");
        let outcome = settle_reservation(
            &mut tx,
            Tz::UTC,
            &SettleInput {
                request_id: "r-1".to_string(),
                user_id: user.clone(),
                model_id: Some(model.clone()),
                started_at: Utc::now(),
                billable: false,
                cost_usd: None,
                tokens: 0,
            },
        )
        .await
        .expect("release");
        tx.commit().await.expect("commit");
        assert_eq!(outcome, SettleOutcome::Released);

        // The released estimate frees the headroom and books nothing.
        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("r-2", &user, &offering, &model, 100, 0.9),
        )
        .await
        .expect("headroom is free again");
        let used: f64 = sqlx::query_scalar("SELECT used_usd FROM budgets WHERE user_id = ?")
            .bind(&user)
            .fetch_one(&pool)
            .await
            .expect("budget row");
        assert_eq!(used, 0.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn reconciler_releases_stale_reservations_and_prevents_lockout() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let offering = seed_offering(&pool, &model).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };
        set_budget(&pool, &actor, Tz::UTC, &user, Period::Total, 1.0)
            .await
            .expect("budget");

        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("stale", &user, &offering, &model, 100, 0.9),
        )
        .await
        .expect("reserve");
        // The next request is blocked while the stale row is open.
        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("next", &user, &offering, &model, 100, 0.2),
        )
        .await
        .expect_err("locked out by the open reservation");

        // Age the row past the TTL and reconcile.
        sqlx::query("UPDATE reservations SET created_at = '2020-01-01T00:00:00.000Z' WHERE request_id = 'stale'")
            .execute(&pool)
            .await
            .expect("age the row");
        let ttl = reservation_ttl(Duration::from_secs(600));
        let cutoff = Utc::now() - chrono::Duration::from_std(ttl).unwrap_or_default();
        let released = release_stale(&pool, cutoff).await.expect("reconcile");
        assert_eq!(released, 1);

        // No lockout: the headroom is available again and nothing was booked.
        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("next", &user, &offering, &model, 100, 0.9),
        )
        .await
        .expect("reconciler prevents lockout");
        let used: f64 = sqlx::query_scalar("SELECT used_usd FROM budgets WHERE user_id = ?")
            .bind(&user)
            .fetch_one(&pool)
            .await
            .expect("budget row");
        assert_eq!(used, 0.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn budget_and_cap_mutations_are_audited_and_validated() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };

        let budget = set_budget(&pool, &actor, Tz::UTC, &user, Period::Day, 3.5)
            .await
            .expect("set budget");
        assert_eq!(budget.limit_usd, 3.5);
        assert_eq!(budget.used_usd, 0.0);
        assert!(budget.reset_at.is_some(), "day budgets reset");

        let updated = set_budget(&pool, &actor, Tz::UTC, &user, Period::Day, 9.0)
            .await
            .expect("update budget");
        assert_eq!(updated.limit_usd, 9.0);

        assert!(
            set_budget(&pool, &actor, Tz::UTC, &user, Period::Day, -1.0)
                .await
                .is_err(),
            "negative limits are rejected"
        );
        assert!(
            set_budget(&pool, &actor, Tz::UTC, "missing-user", Period::Day, 1.0)
                .await
                .is_err(),
            "unknown users are rejected"
        );

        let cap = set_token_cap(&pool, &actor, Tz::UTC, &user, &model, Period::Total, 500)
            .await
            .expect("set cap");
        assert_eq!(cap.used_tokens, 0);
        assert_eq!(cap.reset_at, None);

        assert!(
            clear_budget(&pool, &actor, &user, Period::Day)
                .await
                .expect("clear")
        );
        assert!(
            !clear_budget(&pool, &actor, &user, Period::Day)
                .await
                .expect("second clear")
        );

        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("audit rows");
        assert_eq!(
            actions,
            ["budget.set", "budget.set", "token_cap.set", "budget.clear"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn settle_books_actuals_once_even_after_a_reconciler_release() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let offering = seed_offering(&pool, &model).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };
        set_budget(&pool, &actor, Tz::UTC, &user, Period::Total, 10.0)
            .await
            .expect("budget");
        set_token_cap(&pool, &actor, Tz::UTC, &user, &model, Period::Total, 10_000)
            .await
            .expect("cap");
        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("mid-flight", &user, &offering, &model, 900, 1.0),
        )
        .await
        .expect("reserve");

        // The reconciler releases the row while the request is still running
        // (simulated by aging it past the TTL).
        sqlx::query("UPDATE reservations SET created_at = '2020-01-01T00:00:00.000Z'")
            .execute(&pool)
            .await
            .expect("age the row");
        let ttl = reservation_ttl(Duration::from_secs(600));
        let cutoff = Utc::now() - chrono::Duration::from_std(ttl).unwrap_or_default();
        assert_eq!(release_stale(&pool, cutoff).await.expect("reconcile"), 1);

        // Booking is keyed on the settle marker, not on `status = 'open'`:
        // actuals land exactly once even though the row is `released`
        // (review B1).
        let settle = |cost: f64, tokens: i64| SettleInput {
            request_id: "mid-flight".to_string(),
            user_id: user.clone(),
            model_id: Some(model.clone()),
            started_at: Utc::now(),
            billable: true,
            cost_usd: Some(cost),
            tokens,
        };
        let mut tx = pool.begin().await.expect("tx");
        let outcome = settle_reservation(&mut tx, Tz::UTC, &settle(2.5, 800))
            .await
            .expect("settle");
        tx.commit().await.expect("commit");
        assert_eq!(outcome, SettleOutcome::Settled);

        // A second settle books nothing.
        let mut tx = pool.begin().await.expect("tx");
        let outcome = settle_reservation(&mut tx, Tz::UTC, &settle(2.5, 800))
            .await
            .expect("second settle");
        tx.commit().await.expect("commit");
        assert_eq!(outcome, SettleOutcome::NoReservation);

        let used: f64 = sqlx::query_scalar("SELECT used_usd FROM budgets WHERE user_id = ?")
            .bind(&user)
            .fetch_one(&pool)
            .await
            .expect("budget row");
        assert_eq!(used, 2.5, "booked exactly once");
        let tokens_used: i64 =
            sqlx::query_scalar("SELECT used_tokens FROM token_caps WHERE user_id = ?")
                .bind(&user)
                .fetch_one(&pool)
                .await
                .expect("cap row");
        assert_eq!(tokens_used, 800, "booked exactly once");
        let (status, settled_at): (String, Option<String>) = sqlx::query_as(
            "SELECT status, settled_at FROM reservations WHERE request_id = 'mid-flight'",
        )
        .fetch_one(&pool)
        .await
        .expect("reservation row");
        assert_eq!(status, "settled");
        assert!(settled_at.is_some(), "settled rows stamp settled_at");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn concurrent_settlements_book_the_actuals_exactly_once() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let offering = seed_offering(&pool, &model).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };
        set_budget(&pool, &actor, Tz::UTC, &user, Period::Total, 10.0)
            .await
            .expect("budget");
        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("race", &user, &offering, &model, 100, 1.0),
        )
        .await
        .expect("reserve");

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            let user = user.clone();
            let model = model.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let mut tx = pool.begin().await.expect("tx");
                let outcome = settle_reservation(
                    &mut tx,
                    Tz::UTC,
                    &SettleInput {
                        request_id: "race".to_string(),
                        user_id: user,
                        model_id: Some(model),
                        started_at: Utc::now(),
                        billable: true,
                        cost_usd: Some(3.0),
                        tokens: 500,
                    },
                )
                .await
                .expect("settle");
                tx.commit().await.expect("commit");
                outcome
            }));
        }
        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(handle.await.expect("join"));
        }
        let settled = outcomes
            .iter()
            .filter(|outcome| **outcome == SettleOutcome::Settled)
            .count();
        let skipped = outcomes
            .iter()
            .filter(|outcome| **outcome == SettleOutcome::NoReservation)
            .count();
        assert_eq!(settled, 1, "exactly one settlement books: {outcomes:?}");
        assert_eq!(skipped, 1, "the other finds the marker gone: {outcomes:?}");

        let used: f64 = sqlx::query_scalar("SELECT used_usd FROM budgets WHERE user_id = ?")
            .bind(&user)
            .fetch_one(&pool)
            .await
            .expect("budget row");
        assert_eq!(used, 3.0, "no double booking");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn period_straddling_spend_books_into_its_start_bucket() {
        let (dir, pool) = pool().await;
        let user = seed_user(&pool).await;
        let model = seed_model(&pool).await;
        let offering = seed_offering(&pool, &model).await;
        let actor = Actor {
            user_id: &user,
            ip: "127.0.0.1",
        };
        set_budget(&pool, &actor, Tz::UTC, &user, Period::Day, 5.0)
            .await
            .expect("budget");

        // A slow request starting at 23:59 and a tiny one one second after
        // midnight, both reserved while their buckets were current.
        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("slow", &user, &offering, &model, 900, 1.0),
        )
        .await
        .expect("reserve slow");
        open_reservation(
            &pool,
            Tz::UTC,
            &reservation("tiny", &user, &offering, &model, 10, 0.1),
        )
        .await
        .expect("reserve tiny");

        let started = |day: u32, hour: u32, minute: u32, second: u32| {
            Utc.with_ymd_and_hms(2020, 1, day, hour, minute, second)
                .unwrap()
        };
        let settle =
            |request_id: &str, started_at: DateTime<Utc>, cost: f64, tokens: i64| SettleInput {
                request_id: request_id.to_string(),
                user_id: user.clone(),
                model_id: Some(model.clone()),
                started_at,
                billable: true,
                cost_usd: Some(cost),
                tokens,
            };

        // The tiny request settles first (at 00:00:02): its bucket opens.
        let mut tx = pool.begin().await.expect("tx");
        settle_reservation(
            &mut tx,
            Tz::UTC,
            &settle("tiny", started(6, 0, 0, 1), 0.1, 5),
        )
        .await
        .expect("settle tiny");
        tx.commit().await.expect("commit");

        // The slow request settles after midnight: its big cost is charged
        // to the day bucket of 23:59 and today's bucket is unaffected by it
        // (review M1 — the old code skipped it entirely).
        let mut tx = pool.begin().await.expect("tx");
        settle_reservation(
            &mut tx,
            Tz::UTC,
            &settle("slow", started(5, 23, 59, 0), 4.0, 800),
        )
        .await
        .expect("settle slow");
        tx.commit().await.expect("commit");

        let buckets: Vec<(String, f64, f64)> = sqlx::query_as(
            "SELECT period_start, limit_usd, used_usd FROM budgets \
             WHERE user_id = ? AND period = 'day' ORDER BY period_start",
        )
        .bind(&user)
        .fetch_all(&pool)
        .await
        .expect("bucket rows");
        let find = |bucket: &str| {
            buckets
                .iter()
                .find(|(start, _, _)| start == bucket)
                .map(|(_, limit, used)| (*limit, *used))
        };
        assert_eq!(
            find("2020-01-05T00:00:00.000Z"),
            Some((5.0, 4.0)),
            "the big cost lands in the 23:59 bucket"
        );
        assert_eq!(
            find("2020-01-06T00:00:00.000Z"),
            Some((5.0, 0.1)),
            "the 00:00 bucket keeps only the tiny request"
        );
        // The bucket `set_budget` created for the real current period is
        // untouched and the limit carried into every bucket (no reset can
        // drop the cap).
        let others: Vec<&(String, f64, f64)> = buckets
            .iter()
            .filter(|(start, _, _)| !start.starts_with("2020-01"))
            .collect();
        assert_eq!(others.len(), 1);
        assert_eq!(others[0].2, 0.0, "the current bucket is unaffected");
        assert_eq!(others[0].1, 5.0, "the limit is carried forward");

        // And the API view shows the current period, not the straddled spend.
        let views = list_budgets(&pool, Tz::UTC, &user).await.expect("views");
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].limit_usd, 5.0);
        assert_eq!(views[0].used_usd, 0.0, "current bucket only");
        assert!(views[0].reset_at.is_some(), "day budgets reset");
        std::fs::remove_dir_all(&dir).ok();
    }
}
