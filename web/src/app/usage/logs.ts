import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { AuthService } from '../auth/auth.service';
import { describeError, isForbidden } from '../catalog/errors';
import { formatTime, formatUsd } from '../catalog/format';
import { DecisionPanel } from '../routing/decision-panel';
import { parseDecision } from '../routing/routing.types';
import { UsersService } from '../users/users.service';
import type { UserView } from '../users/users.types';
import { UsageService } from './usage.service';
import type { LogCursor, LogPage, RequestLogEntry } from './usage.types';

const PAGE_SIZE = 50;

/** The request log: metadata and settled usage, never bodies. Admin only. */
@Component({
  selector: 'app-logs',
  imports: [DecisionPanel],
  templateUrl: './logs.html',
  styleUrl: './logs.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Logs {
  private readonly auth = inject(AuthService);
  private readonly usageService = inject(UsageService);
  private readonly usersService = inject(UsersService);

  protected readonly users = signal<UserView[]>([]);
  protected readonly entries = signal<RequestLogEntry[]>([]);
  protected readonly userFilter = signal<string>('');
  protected readonly loading = signal(false);
  protected readonly loadingMore = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  /** The server's `next_cursor`; null once paging has ended. */
  private readonly nextCursor = signal<LogCursor | null>(null);
  protected readonly hasMore = computed(() => this.nextCursor() !== null);

  /** The entry whose "why this model" panel is open. */
  protected readonly expandedId = signal<string | null>(null);
  private readonly entryById = computed(
    () => new Map(this.entries().map((entry) => [entry.request_id, entry])),
  );
  /** The parsed `decision_json` of the expanded entry; null when unreadable. */
  protected readonly expandedDecision = computed(() => {
    const id = this.expandedId();
    if (id === null) {
      return null;
    }
    return parseDecision(this.entryById().get(id)?.decision_json ?? null);
  });

  protected readonly formatTime = formatTime;
  protected readonly formatUsd = formatUsd;

  constructor() {
    // Non-admins never call the API: it would answer 403 and the read is audited.
    if (this.auth.isAdmin()) {
      void this.reload();
      void this.usersService
        .list()
        .then((users) => this.users.set(users))
        .catch((err: unknown) => {
          this.actionError.set(describeError(err, 'Loading accounts failed.'));
        });
    } else {
      this.forbidden.set(true);
    }
  }

  protected async reload(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      const page = await this.usageService.listLogs(this.query());
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
      const page = await this.usageService.listLogs({ ...this.query(), cursor });
      this.entries.set([...this.entries(), ...page.entries]);
      this.nextCursor.set(this.cursorOf(page));
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loadingMore.set(false);
    }
  }

  protected onUserFilterChange(event: Event): void {
    const select = event.target;
    if (!(select instanceof HTMLSelectElement)) {
      return;
    }
    this.userFilter.set(select.value);
    void this.reload();
  }

  /** Paging ends on a short page or when the server omits `next_cursor`. */
  private cursorOf(page: LogPage): LogCursor | null {
    return page.entries.length < PAGE_SIZE ? null : page.next_cursor;
  }

  private query(): { limit: number; user?: string } {
    const user = this.userFilter();
    return user === '' ? { limit: PAGE_SIZE } : { limit: PAGE_SIZE, user };
  }

  /**
   * 402 budget, 429 token cap, and 403 access refusals are money events: the
   * request was refused before anything was sent upstream (see `usage.types.ts`).
   */
  protected isMoneyEvent(entry: RequestLogEntry): boolean {
    return entry.status === 402 || entry.status === 403 || entry.status === 429;
  }

  /** Opens/closes the "why this model" panel of one entry. */
  protected toggleDecision(entry: RequestLogEntry): void {
    this.expandedId.set(this.expandedId() === entry.request_id ? null : entry.request_id);
  }

  /** True when settled usage exists for the entry. */
  protected hasUsage(entry: RequestLogEntry): boolean {
    return entry.cost_usd !== null || entry.input_tokens !== null || entry.output_tokens !== null;
  }

  protected tokenText(entry: RequestLogEntry): string {
    if (entry.input_tokens === null && entry.output_tokens === null) {
      return '—';
    }
    return `${entry.input_tokens ?? 0} in / ${entry.output_tokens ?? 0} out`;
  }

  protected userName(id: string | null): string {
    if (id === null) {
      return '—';
    }
    return this.users().find((user) => user.id === id)?.email ?? id;
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading the request log failed.'));
  }
}
