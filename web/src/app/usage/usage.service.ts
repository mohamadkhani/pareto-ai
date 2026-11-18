import { Injectable } from '@angular/core';
import { requestJson } from '../api';
import type {
  ErrorSummaryRow,
  LogAttempt,
  LogCursor,
  LogPage,
  MyUsageReport,
  RequestLogEntry,
  UsageGroupBy,
  UsageReport,
} from './usage.types';

interface LogListResponse {
  entries: RequestLogEntry[];
  next_cursor?: LogCursor;
}

/** One report's inclusive date range, as `YYYY-MM-DD` day keys. */
export interface UsageRange {
  /** Inclusive start day; omitted means "from the beginning". */
  from?: string;
  /** Inclusive end day; omitted means "up to today". */
  to?: string;
}

/** The admin report's narrowing options. */
export interface AdminUsageQuery extends UsageRange {
  /** Narrow to one user id; omitted aggregates every account. */
  user?: string;
  /** Defaults to `day` server-side. */
  groupBy?: UsageGroupBy;
}

/** Filters for one page of request logs. */
export interface LogQuery {
  /** Maximum entries to return (1–200); the server defaults to 50. */
  limit: number;
  /** The previous page's `next_cursor`. */
  cursor?: LogCursor;
  /** Narrow to one user id. */
  user?: string;
  /** Exact settled status, e.g. `502`. */
  status?: number;
  /** Taxonomy code, e.g. `upstream_auth`. */
  errorClass?: string;
  /** Narrow to one provider id. */
  provider?: string;
  /** Narrow to one offering id. */
  offering?: string;
}

/** Read access to the usage reports and the request logs. */
@Injectable({ providedIn: 'root' })
export class UsageService {
  /** The caller's own report (`GET /api/usage/me`): totals plus daily rows. */
  myUsage(range: UsageRange = {}): Promise<MyUsageReport> {
    return requestJson<MyUsageReport>(`/api/usage/me${queryString(range)}`);
  }

  /** The admin aggregate (`GET /api/usage`), grouped by day, user, or offering. */
  adminUsage(query: AdminUsageQuery = {}): Promise<UsageReport> {
    const params = new URLSearchParams();
    if (query.user !== undefined) {
      params.set('user', query.user);
    }
    if (query.from !== undefined) {
      params.set('from', query.from);
    }
    if (query.to !== undefined) {
      params.set('to', query.to);
    }
    if (query.groupBy !== undefined) {
      params.set('group_by', query.groupBy);
    }
    const search = params.toString();
    return requestJson<UsageReport>(search === '' ? '/api/usage' : `/api/usage?${search}`);
  }

  /**
   * Loads one page of request logs, newest first. `cursor` is the previous
   * page's `next_cursor`, a composite `before`/`before_request_id` position
   * that does not skip entries sharing one millisecond. The filters are the
   * server's `user`, `status`, `error_class`, `provider`, and `offering`
   * (D49). Admin only.
   */
  async listLogs(query: LogQuery): Promise<LogPage> {
    const params = new URLSearchParams({ limit: String(query.limit) });
    if (query.cursor !== undefined) {
      params.set('before', query.cursor.before);
      params.set('before_request_id', query.cursor.before_request_id);
    }
    if (query.user !== undefined) {
      params.set('user', query.user);
    }
    if (query.status !== undefined) {
      params.set('status', String(query.status));
    }
    if (query.errorClass !== undefined) {
      params.set('error_class', query.errorClass);
    }
    if (query.provider !== undefined) {
      params.set('provider', query.provider);
    }
    if (query.offering !== undefined) {
      params.set('offering', query.offering);
    }
    const response = await requestJson<LogListResponse>(`/api/logs?${params.toString()}`);
    return { entries: response.entries, next_cursor: response.next_cursor ?? null };
  }

  /** Every attempt of one request, oldest first (D49). Admin only. */
  async listAttempts(requestId: string): Promise<LogAttempt[]> {
    return requestJson<LogAttempt[]>(`/api/logs/${encodeURIComponent(requestId)}/attempts`);
  }

  /** Failed attempts per provider in the last `hours` (D49). Admin only. */
  async errorSummary(hours = 24): Promise<ErrorSummaryRow[]> {
    return requestJson<ErrorSummaryRow[]>(`/api/errors/summary?hours=${hours}`);
  }
}

/** Appends `?from=&to=` when the range carries any bound. */
function queryString(range: UsageRange): string {
  const params = new URLSearchParams();
  if (range.from !== undefined) {
    params.set('from', range.from);
  }
  if (range.to !== undefined) {
    params.set('to', range.to);
  }
  const search = params.toString();
  return search === '' ? '' : `?${search}`;
}
