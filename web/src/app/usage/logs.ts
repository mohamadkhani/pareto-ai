import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { ActivatedRoute } from '@angular/router';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from '../catalog/catalog.service';
import type { Provider } from '../catalog/catalog.types';
import { describeError, isForbidden } from '../catalog/errors';
import { formatTime, formatUsd } from '../catalog/format';
import { DecisionPanel } from '../routing/decision-panel';
import { parseDecision } from '../routing/routing.types';
import { UiBadge } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiSelect } from '../ui/select';
import { UiTable } from '../ui/table';
import { UsersService } from '../users/users.service';
import type { UserView } from '../users/users.types';
import { UsageService } from './usage.service';
import type { LogAttempt, LogCursor, LogPage, RequestLogEntry } from './usage.types';

const PAGE_SIZE = 50;

/** Taxonomy codes an operator can filter on (D49). */
const ERROR_CLASSES = [
  'invalid_request',
  'upstream_auth',
  'upstream_quota',
  'upstream_rate_limited',
  'upstream_timeout',
  'upstream_stream_failed',
  'upstream_error',
  'internal',
] as const;

/** Settled statuses worth filtering on, including the common failures. */
const STATUSES = [200, 400, 401, 402, 403, 404, 422, 429, 500, 502, 503, 504] as const;

/** The request log: metadata and settled usage, never bodies. Admin only. */
@Component({
  selector: 'app-logs',
  imports: [DecisionPanel, UiBadge, UiBanner, UiButton, UiEmpty, UiField, UiSelect, UiTable],
  templateUrl: './logs.html',
  styleUrl: './logs.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Logs {
  private readonly auth = inject(AuthService);
  private readonly usageService = inject(UsageService);
  private readonly usersService = inject(UsersService);
  private readonly catalog = inject(CatalogService);
  private readonly route = inject(ActivatedRoute);

  protected readonly users = signal<UserView[]>([]);
  protected readonly providers = signal<Provider[]>([]);
  protected readonly entries = signal<RequestLogEntry[]>([]);
  protected readonly userFilter = signal<string>('');
  protected readonly statusFilter = signal<string>('');
  protected readonly classFilter = signal<string>('');
  protected readonly providerFilter = signal<string>('');
  protected readonly loading = signal(false);
  protected readonly loadingMore = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  /** The server's `next_cursor`; null once paging has ended. */
  private readonly nextCursor = signal<LogCursor | null>(null);
  protected readonly hasMore = computed(() => this.nextCursor() !== null);
  protected readonly errorClasses = ERROR_CLASSES;
  protected readonly statuses = STATUSES;

  /** The entry whose details are open. */
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

  /** Attempts per request id, loaded the first time a row is opened (D49). */
  private readonly attemptsByRequest = signal<ReadonlyMap<string, LogAttempt[]>>(new Map());
  protected readonly attemptsLoading = signal<string | null>(null);
  protected readonly expandedAttempts = computed(
    () => this.attemptsByRequest().get(this.expandedId() ?? '') ?? [],
  );

  protected readonly formatTime = formatTime;
  protected readonly formatUsd = formatUsd;

  constructor() {
    // Non-admins never call the API: it would answer 403 and the read is audited.
    if (this.auth.isAdmin()) {
      // The dashboard's error card links here with `?provider=<id>`.
      this.providerFilter.set(this.route.snapshot.queryParamMap.get('provider') ?? '');
      void this.reload();
      void this.usersService
        .list()
        .then((users) => this.users.set(users))
        .catch((err: unknown) => {
          this.actionError.set(describeError(err, 'Loading accounts failed.'));
        });
      void this.catalog
        .listProviders()
        .then((providers) => this.providers.set(providers))
        .catch((err: unknown) => {
          this.actionError.set(describeError(err, 'Loading providers failed.'));
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
      this.expandedId.set(null);
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

  protected onUserFilterChange(value: string): void {
    this.userFilter.set(value);
    void this.reload();
  }

  protected onStatusFilterChange(value: string): void {
    this.statusFilter.set(value);
    void this.reload();
  }

  protected onClassFilterChange(value: string): void {
    this.classFilter.set(value);
    void this.reload();
  }

  protected onProviderFilterChange(value: string): void {
    this.providerFilter.set(value);
    void this.reload();
  }

  /** Paging ends on a short page or when the server omits `next_cursor`. */
  private cursorOf(page: LogPage): LogCursor | null {
    return page.entries.length < PAGE_SIZE ? null : page.next_cursor;
  }

  private query(): {
    limit: number;
    user?: string;
    status?: number;
    errorClass?: string;
    provider?: string;
  } {
    return {
      limit: PAGE_SIZE,
      ...(this.userFilter() === '' ? {} : { user: this.userFilter() }),
      ...(this.statusFilter() === '' ? {} : { status: Number(this.statusFilter()) }),
      ...(this.classFilter() === '' ? {} : { errorClass: this.classFilter() }),
      ...(this.providerFilter() === '' ? {} : { provider: this.providerFilter() }),
    };
  }

  /**
   * 402 budget, 429 token cap, and 403 access refusals are money events: the
   * request was refused before anything was sent upstream (see `usage.types.ts`).
   */
  protected isMoneyEvent(entry: RequestLogEntry): boolean {
    return entry.status === 402 || entry.status === 403 || entry.status === 429;
  }

  /** Opens/closes the details of one entry and loads its attempts on demand. */
  protected async toggleDetails(entry: RequestLogEntry): Promise<void> {
    const open = this.expandedId() === entry.request_id;
    this.expandedId.set(open ? null : entry.request_id);
    if (!open) {
      await this.ensureAttempts(entry.request_id);
    }
  }

  private async ensureAttempts(requestId: string): Promise<void> {
    if (this.attemptsByRequest().has(requestId)) {
      return;
    }
    this.attemptsLoading.set(requestId);
    try {
      const attempts = await this.usageService.listAttempts(requestId);
      const next = new Map(this.attemptsByRequest());
      next.set(requestId, attempts);
      this.attemptsByRequest.set(next);
    } catch (err) {
      this.actionError.set(describeError(err, 'Loading the request attempts failed.'));
    } finally {
      this.attemptsLoading.set(null);
    }
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

  protected providerName(id: string): string {
    return this.providers().find((provider) => provider.id === id)?.name ?? id.slice(0, 8);
  }

  /** `open_for_ms` is relative; show it as seconds still to wait, not a time. */
  protected attemptStatus(attempt: LogAttempt): string {
    return attempt.upstream_status === null ? '—' : String(attempt.upstream_status);
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading the request log failed.'));
  }
}
