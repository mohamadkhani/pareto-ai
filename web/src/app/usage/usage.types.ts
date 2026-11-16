/**
 * Wire types for the M4 usage reports (`/api/usage/me`, `/api/usage`) and the
 * admin request-log viewer (`/api/logs`).
 *
 * The report counters are shared by totals and rows: the server flattens them
 * into each `days`/`groups` row (serde `flatten`), so a row is its key fields
 * plus the counters — not a nested `totals` object. Requests refused before
 * anything was sent upstream (402 budget, 429 token cap, 403 access) are
 * counted in `refused_requests` and never priced. Rows with a NULL cost are
 * "unpriced" and are never added as 0; "partial" rows are priced but
 * understated (a used component had no price). Log entries carry request
 * metadata and settled usage only — never bodies (decision D11).
 */

/** The counters of one report group; identical shape for totals and rows. */
export interface UsageCounters {
  requests: number;
  input_tokens: number;
  output_tokens: number;
  /** Sum of the priced spend; unpriced requests are excluded. */
  cost_usd: number;
  /** Requests whose cost is complete (every used component priced). */
  priced_requests: number;
  /** Requests that could not be priced at all. */
  unpriced_requests: number;
  /** Requests priced but understated (a used component had no price). */
  partial_requests: number;
  /**
   * Requests refused before anything was sent upstream (402 budget, 429
   * token cap, 403 access, ...). They consumed nothing and are excluded from
   * `priced_requests`; refused + priced + unpriced + partial == requests.
   */
  refused_requests: number;
}

/** One `GET /api/usage/me` day row: local day key plus flattened counters. */
export interface UsageDay extends UsageCounters {
  day: string;
}

/** How `GET /api/usage` groups its rows. */
export type UsageGroupBy = 'day' | 'user' | 'offering';

/**
 * One `GET /api/usage` group row: which key fields are set depends on
 * `group_by` (`day`/`user`/`offering`), plus flattened counters.
 */
export interface UsageGroup extends UsageCounters {
  day: string | null;
  user_id: string | null;
  /** Email of the user, when grouping by user and the account still exists. */
  email: string | null;
  offering_id: string | null;
}

/** The self-service usage report. */
export interface MyUsageReport {
  totals: UsageCounters;
  /** One entry per local day with usage, ascending. */
  days: UsageDay[];
}

/** The admin aggregate report. */
export interface UsageReport {
  totals: UsageCounters;
  groups: UsageGroup[];
}

/**
 * One request-log entry: request metadata plus the settled usage counters
 * when a ledger row exists. Never carries request or response bodies.
 */
export interface RequestLogEntry {
  request_id: string;
  client_request_id: string | null;
  occurred_at: string;
  method: string | null;
  path: string | null;
  requested_model: string | null;
  resolved_offering_id: string | null;
  /** JSON object with the routing decision, when one was recorded. */
  decision_json: string | null;
  status: number | null;
  error_class: string | null;
  bytes_in: number | null;
  bytes_out: number | null;
  user_id: string | null;
  api_key_id: string | null;
  input_tokens: number | null;
  output_tokens: number | null;
  cost_usd: number | null;
  tokens_estimated: number | null;
  latency_ms: number | null;
}

/**
 * The composite paging cursor: only entries strictly older than this
 * position. `before_request_id` orders entries that share one millisecond.
 */
export interface LogCursor {
  /** Entries must be strictly older than this `occurred_at` instant. */
  before: string;
  /** At the same instant, only entries with a lower id are returned. */
  before_request_id: string;
}

/** One page of request logs, newest first. */
export interface LogPage {
  entries: RequestLogEntry[];
  /** Position of the last entry of a full page; null when there is no next. */
  next_cursor: LogCursor | null;
}
