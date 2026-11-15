import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { ApiError } from '../api';
import { AuthService } from '../auth/auth.service';
import { AuditCursor, AuditEntry, AuditPage, AuditService } from './audit.service';

const PAGE_SIZE = 50;
const DETAIL_PREVIEW_CHARS = 600;

function isForbidden(err: unknown): boolean {
  return err instanceof ApiError && (err.code === 'forbidden' || err.status === 403);
}

function describeError(err: unknown): string {
  return err instanceof ApiError ? err.message : 'Loading the audit log failed. Please try again.';
}

/** The audit log: every admin action, newest first. Admin only. */
@Component({
  selector: 'app-audit',
  templateUrl: './audit.html',
  styleUrl: './audit.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Audit {
  private readonly auth = inject(AuthService);
  private readonly auditService = inject(AuditService);

  protected readonly entries = signal<AuditEntry[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadingMore = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  /** The server's `next_cursor`; null once paging has ended. */
  private readonly nextCursor = signal<AuditCursor | null>(null);
  protected readonly hasMore = computed(() => this.nextCursor() !== null);
  private readonly expandedIds = signal<ReadonlySet<number>>(new Set());

  constructor() {
    // Non-admins never call the API: it would answer 403 and the read is audited.
    if (this.auth.isAdmin()) {
      void this.reload();
    } else {
      this.forbidden.set(true);
    }
  }

  protected async reload(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      const page = await this.auditService.list(PAGE_SIZE);
      this.entries.set(page.entries);
      this.nextCursor.set(this.cursorOf(page));
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected async loadMore(): Promise<void> {
    const cursor = this.nextCursor();
    if (cursor === null || this.loadingMore()) {
      return;
    }
    this.loadingMore.set(true);
    this.loadError.set(null);
    try {
      // The composite cursor keeps entries that share one millisecond in order.
      const page = await this.auditService.list(PAGE_SIZE, cursor);
      this.entries.set([...this.entries(), ...page.entries]);
      this.nextCursor.set(this.cursorOf(page));
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loadingMore.set(false);
    }
  }

  /** Paging ends on a short page or when the server omits `next_cursor`. */
  private cursorOf(page: AuditPage): AuditCursor | null {
    return page.entries.length < PAGE_SIZE ? null : page.next_cursor;
  }

  protected isExpanded(id: number): boolean {
    return this.expandedIds().has(id);
  }

  protected toggleDetails(id: number): void {
    const next = new Set(this.expandedIds());
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    this.expandedIds.set(next);
  }

  /** Pretty-printed `detail_json`, truncated for the expandable row. */
  protected detailPreview(entry: AuditEntry): string {
    if (entry.detail_json === null) {
      return 'No detail recorded for this action.';
    }
    let text = entry.detail_json;
    try {
      text = JSON.stringify(JSON.parse(text), null, 2);
    } catch {
      // Not valid JSON; show the raw payload instead.
    }
    return text.length > DETAIL_PREVIEW_CHARS
      ? `${text.slice(0, DETAIL_PREVIEW_CHARS)}… (truncated)`
      : text;
  }

  protected target(entry: AuditEntry): string {
    if (entry.target_type === null) {
      return '—';
    }
    return entry.target_id === null ? entry.target_type : `${entry.target_type}:${entry.target_id}`;
  }

  protected formatTime(iso: string): string {
    const time = new Date(iso);
    return Number.isNaN(time.getTime()) ? iso : time.toLocaleString();
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err));
  }
}
