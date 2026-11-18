import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { Router, RouterLink } from '@angular/router';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from '../catalog/catalog.service';
import type { Credential, Offering, Provider, SyncStatus } from '../catalog/catalog.types';
import { describeError, isForbidden } from '../catalog/errors';
import { formatAge, formatUsd } from '../catalog/format';
import type { ModelsOverview } from '../catalog/overview.types';
import type { BenchmarkSyncRun, BenchmarkSyncStatus } from '../routing/routing.types';
import { RoutingService } from '../routing/routing.service';
import { UiBadge, type BadgeTone } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiCard } from '../ui/card';
import { UiEmpty } from '../ui/empty';
import { UiIconButton } from '../ui/icon-button';
import { UiSparkline } from '../ui/sparkline';
import { DashboardActivity, type ActivityRow } from './activity';
import { DashboardAttention, type Attention } from './attention';
import { UsageService } from '../usage/usage.service';
import type {
  ErrorSummaryRow,
  RequestLogEntry,
  UsageGroup,
  UsageReport,
} from '../usage/usage.types';
import { UsersService } from '../users/users.service';
import type { UserView } from '../users/users.types';

/** Spend for the three dashboard windows, in USD. */
interface SpendView {
  today: number;
  week: number;
  month: number;
}

/** Catalog counts and the two sync statuses. */
interface CatalogView {
  providers: number;
  offerings: number;
  pricedModels: number;
  unpricedModels: number;
  priceRun: string | null;
  priceStatus: string;
  priceStale: boolean;
  benchmarkSources: readonly BenchmarkSyncRun[];
  benchmarkStale: boolean;
}

/** One actionable health note. */
interface HealthNote {
  id: string;
  provider: string;
  credential: string;
  text: string;
  tone: BadgeTone;
}

/** One slice of the request outcome bar. */
interface RequestSegment {
  label: string;
  count: number;
  tone: 'success' | 'warning' | 'info';
}

const LOG_SAMPLE = 100;
const ACTIVITY_ROWS = 8;

function localDayKey(date: Date): string {
  const pad = (value: number): string => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

function dayAt(now: Date, offset: number): Date {
  return new Date(now.getFullYear(), now.getMonth(), now.getDate() + offset, 12);
}

function sumCost(groups: readonly UsageGroup[], keys: ReadonlySet<string>): number {
  return groups.reduce(
    (total, group) => (group.day !== null && keys.has(group.day) ? total + group.cost_usd : total),
    0,
  );
}

/** Error percentage over the sampled request page, or null when it is empty. */
function errorPercent(entries: readonly RequestLogEntry[]): number | null {
  if (entries.length === 0) {
    return null;
  }
  const errors = entries.filter((entry) => entry.status !== null && entry.status >= 400).length;
  return Math.round((errors / entries.length) * 100);
}

/**
 * Admin landing page. Every section loads independently so one failing
 * endpoint never blanks the rest: spend/requests share `/api/usage`, recent
 * activity and the error sample share `/api/logs`, and the catalog/health
 * cards compose the existing list and sync-status endpoints client-side.
 */
@Component({
  selector: 'app-dashboard',
  imports: [
    RouterLink,
    UiBadge,
    UiBanner,
    UiButton,
    UiCard,
    UiEmpty,
    UiIconButton,
    UiSparkline,
    DashboardActivity,
    DashboardAttention,
  ],
  templateUrl: './dashboard.html',
  styleUrl: './dashboard.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Dashboard {
  private readonly auth = inject(AuthService);
  private readonly router = inject(Router);
  private readonly usageService = inject(UsageService);
  private readonly usersService = inject(UsersService);
  private readonly catalog = inject(CatalogService);
  private readonly routing = inject(RoutingService);

  protected readonly forbidden = signal(!this.auth.isAdmin());

  protected readonly usage = signal<UsageReport | null>(null);
  protected readonly usageLoading = signal(false);
  protected readonly usageError = signal<string | null>(null);

  protected readonly logs = signal<RequestLogEntry[]>([]);
  protected readonly logsLoading = signal(false);
  protected readonly logsError = signal<string | null>(null);

  /** Failed attempts per provider in the last 24h (D49). */
  protected readonly errorSummary = signal<ErrorSummaryRow[]>([]);
  protected readonly errorSummaryLoading = signal(false);
  protected readonly errorSummaryError = signal<string | null>(null);

  protected readonly providers = signal<Provider[]>([]);
  protected readonly overview = signal<ModelsOverview | null>(null);
  protected readonly offerings = signal<Offering[]>([]);
  protected readonly priceStatus = signal<SyncStatus | null>(null);
  protected readonly benchmarkStatus = signal<BenchmarkSyncStatus | null>(null);
  protected readonly catalogLoading = signal(false);
  protected readonly catalogError = signal<string | null>(null);

  protected readonly credentials = signal<Credential[]>([]);
  protected readonly healthLoading = signal(false);
  protected readonly healthError = signal<string | null>(null);
  protected readonly healthPartial = signal(false);

  protected readonly users = signal<UserView[]>([]);

  protected readonly syncingPrices = signal(false);
  protected readonly syncingBenchmarks = signal(false);
  protected readonly actionNotice = signal<string | null>(null);
  protected readonly actionError = signal<string | null>(null);

  protected readonly formatUsd = formatUsd;

  protected readonly spend = computed<SpendView | null>(() => {
    const report = this.usage();
    if (report === null) {
      return null;
    }
    const now = new Date();
    const weekKeys = new Set<string>();
    for (let offset = 0; offset < 7; offset += 1) {
      weekKeys.add(localDayKey(dayAt(now, -offset)));
    }
    const monthKey = localDayKey(now).slice(0, 7);
    const monthKeys = new Set(
      report.groups
        .map((group) => group.day)
        .filter((day): day is string => day !== null && day.startsWith(monthKey)),
    );
    return {
      today: sumCost(report.groups, new Set([localDayKey(now)])),
      week: sumCost(report.groups, weekKeys),
      month: sumCost(report.groups, monthKeys),
    };
  });

  protected readonly totals = computed(() => this.usage()?.totals ?? null);

  protected readonly errorPercent = computed(() => errorPercent(this.logs()));

  protected errorPercentLabel(): string {
    const value = this.errorPercent();
    return value === null ? '—' : `${value}%`;
  }

  protected readonly catalogView = computed<CatalogView | null>(() => {
    const overview = this.overview();
    if (overview === null && this.providers().length === 0) {
      return null;
    }
    const rows = overview?.rows ?? [];
    const pricedModels = rows.filter((row) =>
      'price_avg' in row ? row.price_avg !== null : row.price !== null,
    ).length;
    const offerings =
      this.offerings().length > 0
        ? this.offerings().length
        : rows.reduce(
            (total, row) => total + ('offerings_count' in row ? row.offerings_count : 1),
            0,
          );
    const status = this.priceStatus();
    const sources = this.benchmarkStatus()?.sources ?? [];
    return {
      providers: this.providers().length,
      offerings,
      pricedModels,
      unpricedModels: Math.max(0, rows.length - pricedModels),
      priceRun: status?.last_run?.finished_at ?? null,
      priceStatus: status?.last_run?.status ?? 'never',
      priceStale: status?.snapshot_stale ?? status?.last_run === null,
      benchmarkSources: sources,
      benchmarkStale: sources.length === 0 || sources.some((source) => source.fresh === false),
    };
  });

  /** Credentials that are failing, cooling down, or low on balance. */
  protected readonly healthNotes = computed<readonly HealthNote[]>(() => {
    const names = new Map(this.providers().map((provider) => [provider.id, provider.name]));
    const now = Date.now();
    const notes: HealthNote[] = [];
    for (const credential of this.credentials()) {
      const provider = names.get(credential.provider_id) ?? credential.provider_id;
      if (credential.status === 'error') {
        notes.push({
          id: `${credential.id}-error`,
          provider,
          credential: credential.label,
          text: 'last probe failed',
          tone: 'danger',
        });
      }
      const cooling =
        credential.status === 'cooldown' ||
        (credential.cooldown_until !== null && Date.parse(credential.cooldown_until) > now);
      if (cooling) {
        notes.push({
          id: `${credential.id}-cooldown`,
          provider,
          credential: credential.label,
          text: 'cooling down',
          tone: 'warning',
        });
      }
      const exhausted =
        credential.balance_available === false ||
        (credential.balance_usd !== null && credential.balance_usd <= 0);
      if (exhausted) {
        notes.push({
          id: `${credential.id}-balance`,
          provider,
          credential: credential.label,
          text:
            credential.balance_gate === 'block' ? 'balance blocks routing' : 'balance exhausted',
          tone: credential.balance_gate === 'block' ? 'danger' : 'warning',
        });
      } else if (
        credential.balance_usd !== null &&
        credential.balance_low_threshold !== null &&
        credential.balance_usd <= credential.balance_low_threshold
      ) {
        notes.push({
          id: `${credential.id}-low`,
          provider,
          credential: credential.label,
          text: 'balance below threshold',
          tone: 'warning',
        });
      }
    }
    return notes;
  });

  protected readonly health = computed(() => ({
    providers: this.providers().length,
    credentials: this.credentials().length,
    errors: this.healthNotes().filter((note) => note.tone === 'danger').length,
    warnings: this.healthNotes().filter((note) => note.tone === 'warning').length,
  }));

  /** Priced spend for the last seven local days, oldest first. */
  protected readonly spendSeries = computed<readonly number[]>(() => {
    const report = this.usage();
    if (report === null) {
      return [];
    }
    const byDay = new Map<string, number>();
    for (const group of report.groups) {
      if (group.day !== null) {
        byDay.set(group.day, (byDay.get(group.day) ?? 0) + group.cost_usd);
      }
    }
    const now = new Date();
    const series: number[] = [];
    for (let offset = 6; offset >= 0; offset -= 1) {
      series.push(byDay.get(localDayKey(dayAt(now, -offset))) ?? 0);
    }
    return series;
  });

  protected readonly spendSeriesLabel = computed(() => {
    const total = this.spendSeries().reduce((sum, value) => sum + value, 0);
    return `Priced spend over the last 7 days: ${this.formatUsd(total)}`;
  });

  protected requestBarLabel(): string {
    const totals = this.totals();
    if (totals === null || totals.requests === 0) {
      return '';
    }
    const parts = this.requestSegments().map((segment) => `${segment.count} ${segment.label}`);
    return `${totals.requests} requests: ${parts.join(', ')}`;
  }

  protected readonly requestSegments = computed<readonly RequestSegment[]>(() => {
    const totals = this.totals();
    if (totals === null) {
      return [];
    }
    return [
      { label: 'priced', count: totals.priced_requests, tone: 'success' },
      { label: 'partial', count: totals.partial_requests, tone: 'warning' },
      { label: 'unpriced', count: totals.unpriced_requests, tone: 'warning' },
      { label: 'refused', count: totals.refused_requests, tone: 'info' },
    ];
  });

  protected readonly attention = computed<Attention | null>(() => {
    if (this.providers().length === 0) {
      return null;
    }
    const health = this.health();
    const catalog = this.catalogView();
    const parts: string[] = [];
    const links: { label: string; path: string }[] = [];
    if (health.errors > 0) {
      parts.push(`${health.errors} credential error${health.errors === 1 ? '' : 's'}`);
    }
    if (health.warnings > 0) {
      parts.push(`${health.warnings} credential warning${health.warnings === 1 ? '' : 's'}`);
    }
    if (catalog?.priceStale === true) {
      parts.push('prices are stale');
    }
    if (catalog?.benchmarkStale === true) {
      parts.push('benchmarks are stale');
    }
    if (parts.length === 0) {
      return null;
    }
    if (health.errors > 0 || health.warnings > 0) {
      links.push({ label: 'Review health', path: '/providers' });
    }
    if (catalog?.priceStale === true || catalog?.benchmarkStale === true) {
      links.push({ label: 'Review sync', path: '/models' });
    }
    return {
      tone: health.errors > 0 ? 'danger' : 'warning',
      parts,
      links,
    };
  });

  protected readonly recent = computed<ActivityRow[]>(() => {
    const providers = new Map(this.providers().map((provider) => [provider.id, provider.name]));
    const offerings = new Map(
      this.offerings().map((offering) => [offering.id, offering.provider_id]),
    );
    const users = new Map(this.users().map((user) => [user.id, user.email]));
    return this.logs()
      .slice(0, ACTIVITY_ROWS)
      .map((entry) => {
        const providerId =
          entry.resolved_offering_id === null
            ? null
            : (offerings.get(entry.resolved_offering_id) ?? null);
        return {
          entry,
          user:
            entry.user_id === null ? '—' : (users.get(entry.user_id) ?? entry.user_id.slice(0, 8)),
          provider:
            providerId === null ? '—' : (providers.get(providerId) ?? providerId.slice(0, 8)),
        };
      });
  });

  constructor() {
    if (!this.auth.isAdmin()) {
      return;
    }
    this.loadUsage();
    this.loadLogs();
    this.loadErrorSummary();
    this.loadProviders();
    this.loadOverview();
    this.loadPriceStatus();
    this.loadBenchmarkStatus();
    this.loadSecondary();
  }

  protected loadUsage(): void {
    this.usageLoading.set(true);
    this.usageError.set(null);
    const now = new Date();
    const monthStart = new Date(now.getFullYear(), now.getMonth(), 1, 12);
    const weekStart = dayAt(now, -6);
    const from = localDayKey(monthStart <= weekStart ? monthStart : weekStart);
    void this.usageService
      .adminUsage({ from, to: localDayKey(now), groupBy: 'day' })
      .then((report) => this.usage.set(report))
      .catch((err: unknown) => this.usageError.set(mapError(err, 'Loading spend failed.')))
      .finally(() => this.usageLoading.set(false));
  }

  protected loadLogs(): void {
    this.logsLoading.set(true);
    this.logsError.set(null);
    void this.usageService
      .listLogs({ limit: LOG_SAMPLE })
      .then((page) => this.logs.set(page.entries))
      .catch((err: unknown) => this.logsError.set(mapError(err, 'Loading recent activity failed.')))
      .finally(() => this.logsLoading.set(false));
  }

  protected loadErrorSummary(): void {
    this.errorSummaryLoading.set(true);
    this.errorSummaryError.set(null);
    void this.usageService
      .errorSummary(24)
      .then((rows) => this.errorSummary.set(rows))
      .catch((err: unknown) =>
        this.errorSummaryError.set(mapError(err, 'Loading upstream errors failed.')),
      )
      .finally(() => this.errorSummaryLoading.set(false));
  }

  protected loadProviders(): void {
    this.healthLoading.set(true);
    this.healthError.set(null);
    void this.catalog
      .listProviders()
      .then(async (providers) => {
        this.providers.set(providers);
        const lists = await Promise.all(
          providers.map(async (provider) => {
            try {
              return await this.catalog.listCredentials(provider.id);
            } catch {
              this.healthPartial.set(true);
              return [] as Credential[];
            }
          }),
        );
        this.credentials.set(lists.flat());
      })
      .catch((err: unknown) =>
        this.healthError.set(mapError(err, 'Loading provider health failed.')),
      )
      .finally(() => this.healthLoading.set(false));
  }

  protected loadOverview(): void {
    this.catalogLoading.set(true);
    this.catalogError.set(null);
    void this.catalog
      .modelsOverview(null)
      .then((overview) => this.overview.set(overview))
      .catch((err: unknown) => this.catalogError.set(mapError(err, 'Loading the catalog failed.')))
      .finally(() => this.catalogLoading.set(false));
  }

  protected loadPriceStatus(): void {
    void this.catalog
      .syncStatus()
      .then((status) => this.priceStatus.set(status))
      .catch((err: unknown) => this.catalogError.set(mapError(err, 'Loading sync status failed.')));
  }

  protected loadBenchmarkStatus(): void {
    void this.routing
      .benchmarkSyncStatus()
      .then((status) => this.benchmarkStatus.set(status))
      .catch((err: unknown) =>
        this.catalogError.set(mapError(err, 'Loading benchmark status failed.')),
      );
  }

  /** Users name recent rows; offerings map them to providers. Both are secondary. */
  private loadSecondary(): void {
    void this.usersService
      .list()
      .then((users) => this.users.set(users))
      .catch(() => undefined);
    void this.catalog
      .listOfferings()
      .then((offerings) => this.offerings.set(offerings))
      .catch(() => undefined);
  }

  protected refreshAll(): void {
    this.actionError.set(null);
    this.actionNotice.set(null);
    this.loadUsage();
    this.loadLogs();
    this.loadErrorSummary();
    this.loadProviders();
    this.loadOverview();
    this.loadPriceStatus();
    this.loadBenchmarkStatus();
    this.loadSecondary();
  }

  protected retrySpend(): void {
    this.loadUsage();
  }

  protected retryLogs(): void {
    this.loadLogs();
  }

  protected retryCatalog(): void {
    this.loadOverview();
    this.loadProviders();
    this.loadPriceStatus();
    this.loadBenchmarkStatus();
  }

  protected retryHealth(): void {
    this.loadProviders();
  }

  protected go(path: string): void {
    void this.router.navigateByUrl(path);
  }

  protected providerName(providerId: string): string {
    return this.providers().find((provider) => provider.id === providerId)?.name ?? providerId;
  }

  protected priceRunText(catalog: CatalogView): string {
    return catalog.priceRun === null
      ? 'never synced'
      : `${catalog.priceStatus} · ${formatAge(catalog.priceRun)}`;
  }

  protected benchmarkSummary(sources: readonly BenchmarkSyncRun[]): string {
    if (sources.length === 0) {
      return 'never synced';
    }
    const failed = sources.filter((source) => source.status !== 'success').length;
    const newest = sources
      .map((source) => source.finished_at)
      .filter((value): value is string => value !== null)
      .sort()
      .at(-1);
    const when = newest === undefined ? 'never' : formatAge(newest);
    return failed === 0
      ? `${sources.length} sources · ${when}`
      : `${failed} of ${sources.length} failed`;
  }

  /** Status pill tone for a request-log HTTP status. */

  protected async syncPrices(): Promise<void> {
    if (this.syncingPrices()) {
      return;
    }
    this.syncingPrices.set(true);
    this.actionError.set(null);
    this.actionNotice.set(null);
    try {
      const outcome = await this.catalog.syncOpenRouter();
      this.actionNotice.set(
        outcome.status === 'success'
          ? `Price sync finished: ${outcome.model_count} models imported.`
          : `Price sync returned ${outcome.status}${
              outcome.error === null ? '.' : `: ${outcome.error}`
            }`,
      );
      this.loadPriceStatus();
      this.loadOverview();
    } catch (err) {
      this.actionError.set(mapError(err, 'The price sync failed to start.'));
    } finally {
      this.syncingPrices.set(false);
    }
  }

  protected async syncBenchmarks(): Promise<void> {
    if (this.syncingBenchmarks()) {
      return;
    }
    this.syncingBenchmarks.set(true);
    this.actionError.set(null);
    this.actionNotice.set(null);
    try {
      const response = await this.routing.syncBenchmarks({});
      const failed = response.results.filter((result) => result.status !== 'success');
      this.actionNotice.set(
        failed.length === 0
          ? `Benchmark sync finished: ${response.results.length} sources updated.`
          : `Benchmark sync finished with ${failed.length} source error(s).`,
      );
      this.loadBenchmarkStatus();
    } catch (err) {
      this.actionError.set(mapError(err, 'The benchmark sync failed to start.'));
    } finally {
      this.syncingBenchmarks.set(false);
    }
  }
}

function mapError(err: unknown, fallback: string): string {
  return isForbidden(err)
    ? 'This page is restricted to administrators.'
    : describeError(err, fallback);
}
