import { Injectable } from '@angular/core';
import { requestJson } from '../api';

/** One audit log entry as returned by `GET /api/audit`. */
export interface AuditEntry {
  id: number;
  actor_user_id: string | null;
  action: string;
  target_type: string | null;
  target_id: string | null;
  /** JSON object with action-specific detail, when the action recorded any. */
  detail_json: string | null;
  ip: string | null;
  occurred_at: string;
}

/**
 * The composite paging cursor: only entries strictly older than this position.
 * `before_id` orders entries that share one millisecond.
 */
export interface AuditCursor {
  /** Entries must be strictly older than this `occurred_at` instant. */
  before: string;
  /** At the same instant, only entries with a lower `id` are returned. */
  before_id: number;
}

/** One page of the audit log. */
export interface AuditPage {
  entries: AuditEntry[];
  /** Position of the last entry of a full page; null when there is no further page. */
  next_cursor: AuditCursor | null;
}

interface AuditListResponse {
  entries: AuditEntry[];
  next_cursor?: AuditCursor;
}

/** Admin read access to the audit log. */
@Injectable({ providedIn: 'root' })
export class AuditService {
  /**
   * Loads one page of the audit log, newest first. `cursor` is the previous
   * page's `next_cursor`, a composite `before`/`before_id` position that does
   * not skip entries sharing one millisecond. Throws `ApiError` with code
   * `forbidden` for non-admins and `unauthorized` once the session expires.
   */
  async list(limit: number, cursor?: AuditCursor): Promise<AuditPage> {
    const params = new URLSearchParams({ limit: String(limit) });
    if (cursor !== undefined) {
      params.set('before', cursor.before);
      params.set('before_id', String(cursor.before_id));
    }
    const response = await requestJson<AuditListResponse>(`/api/audit?${params.toString()}`);
    return { entries: response.entries, next_cursor: response.next_cursor ?? null };
  }
}
