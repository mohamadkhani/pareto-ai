import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { AuthService } from '../auth/auth.service';
import { describeError, isForbidden } from '../catalog/errors';
import { formatUsd } from '../catalog/format';
import { UsersService } from '../users/users.service';
import type { UserView } from '../users/users.types';
import { UsageService } from './usage.service';
import type {
  UsageCounters,
  UsageDay,
  UsageGroup,
  UsageGroupBy,
  UsageReport,
  MyUsageReport,
} from './usage.types';

/** One rendered row: its group key plus the flattened counters. */
interface RowView {
  key: string;
  totals: UsageCounters;
}

/** Which report is loaded: the caller's own, or an admin aggregate. */
type Scope = 'me' | 'all' | string;

/** Turns a self-report's day rows into display rows. */
function dayRows(report: MyUsageReport): RowView[] {
  return report.days.map((day: UsageDay) => ({ key: day.day, totals: day }));
}

/** Turns an aggregate's group rows into display rows for the active grouping. */
function groupRows(report: UsageReport, groupBy: UsageGroupBy): RowView[] {
  return report.groups.map((group: UsageGroup) => ({
    key: groupKey(group, groupBy),
    totals: group,
  }));
}

function groupKey(group: UsageGroup, groupBy: UsageGroupBy): string {
  // `''` (not null) is what the rollup stores for a deleted user or offering;
  // treat it like null so the row never renders an empty label.
  switch (groupBy) {
    case 'day':
      return group.day || '—';
    case 'user':
      return group.email || group.user_id || '—';
    case 'offering':
      return group.offering_id || '—';
  }
}

/** Usage reports: what was spent, by day, user, or offering. */
@Component({
  selector: 'app-usage',
  templateUrl: './usage.html',
  styleUrl: './usage.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Usage {
  private readonly auth = inject(AuthService);
  private readonly usageService = inject(UsageService);
  private readonly usersService = inject(UsersService);

  protected readonly isAdmin = this.auth.isAdmin;
  protected readonly users = signal<UserView[]>([]);
  protected readonly scope = signal<Scope>('me');
  protected readonly groupBy = signal<UsageGroupBy>('day');
  protected readonly from = signal('');
  protected readonly to = signal('');
  protected readonly totals = signal<UsageCounters | null>(null);
  protected readonly rows = signal<RowView[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);

  protected readonly formatUsd = formatUsd;
  protected readonly groupByOptions: readonly UsageGroupBy[] = ['day', 'user', 'offering'];

  /** The row-label column header follows the active report. */
  protected readonly keyHeader = computed(() =>
    this.scope() === 'me'
      ? 'Day'
      : this.groupBy() === 'user'
        ? 'User'
        : this.groupBy() === 'offering'
          ? 'Offering'
          : 'Day',
  );

  constructor() {
    if (this.auth.isAdmin()) {
      void this.usersService
        .list()
        .then((users) => this.users.set(users))
        .catch((err: unknown) => {
          this.actionError.set(describeError(err, 'Loading accounts failed.'));
        });
    }
    void this.reload();
  }

  protected async reload(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      const range = { from: this.from() || undefined, to: this.to() || undefined };
      const scope = this.scope();
      if (scope === 'me') {
        const report = await this.usageService.myUsage(range);
        this.totals.set(report.totals);
        this.rows.set(dayRows(report));
      } else {
        const report = await this.usageService.adminUsage({
          ...range,
          user: scope === 'all' ? undefined : scope,
          groupBy: this.groupBy(),
        });
        this.totals.set(report.totals);
        this.rows.set(groupRows(report, this.groupBy()));
      }
    } catch (err) {
      if (isForbidden(err)) {
        this.forbidden.set(true);
      } else {
        this.loadError.set(describeError(err, 'Loading the usage report failed.'));
      }
    } finally {
      this.loading.set(false);
    }
  }

  protected onScopeChange(event: Event): void {
    const select = event.target;
    if (!(select instanceof HTMLSelectElement)) {
      return;
    }
    this.scope.set(select.value);
    void this.reload();
  }

  protected onGroupByChange(event: Event): void {
    const select = event.target;
    if (!(select instanceof HTMLSelectElement)) {
      return;
    }
    this.groupBy.set(select.value as UsageGroupBy);
    void this.reload();
  }

  protected onFromChange(event: Event): void {
    const input = event.target;
    if (!(input instanceof HTMLInputElement)) {
      return;
    }
    this.from.set(input.value);
  }

  protected onToChange(event: Event): void {
    const input = event.target;
    if (!(input instanceof HTMLInputElement)) {
      return;
    }
    this.to.set(input.value);
  }

  /** Token counts with grouping separators. */
  protected formatTokens(value: number): string {
    return value.toLocaleString();
  }
}
