import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';
import { RouterLink } from '@angular/router';
import { formatTime, formatUsd } from '../catalog/format';
import type { RequestLogEntry } from '../usage/usage.types';
import { UiBadge, type BadgeTone } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiCard } from '../ui/card';
import { UiTable } from '../ui/table';

/** One recent-activity row, already resolved to display names. */
export interface ActivityRow {
  entry: RequestLogEntry;
  user: string;
  provider: string;
}

/**
 * Recent requests card. It owns the desktop table and the compact mobile
 * list so the dashboard page only has to provide the rows and load state.
 */
@Component({
  selector: 'app-dashboard-activity',
  imports: [RouterLink, UiBadge, UiBanner, UiCard, UiTable],
  templateUrl: './activity.html',
  styleUrl: './activity.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class DashboardActivity {
  readonly rows = input<readonly ActivityRow[]>([]);
  readonly loading = input(false);
  readonly error = input<string | null>(null);

  readonly retry = output<void>();

  protected readonly formatTime = formatTime;
  protected readonly formatUsd = formatUsd;

  protected statusTone(status: number | null): BadgeTone {
    if (status === null) {
      return 'neutral';
    }
    if (status < 300) {
      return 'success';
    }
    if (status === 402 || status === 403 || status === 429) {
      return 'warning';
    }
    return status >= 500 ? 'danger' : 'warning';
  }

  protected statusLabel(status: number | null): string {
    return status === null ? 'unknown' : String(status);
  }
}
