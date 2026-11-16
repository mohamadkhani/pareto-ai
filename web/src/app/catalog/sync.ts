import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from './catalog.service';
import { describeError, isForbidden } from './errors';
import { type SyncRun, runError } from './catalog.types';
import { formatAge, formatTime } from './format';

/**
 * OpenRouter price sync: trigger a run and inspect the last recorded one.
 * A failed run that kept the previous snapshot shows as `stale-if-error`; a
 * failure with no snapshot to serve shows as `error`.
 */
@Component({
  selector: 'app-sync',
  templateUrl: './sync.html',
  styleUrl: './sync.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Sync {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);

  protected readonly run = signal<SyncRun | null>(null);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly syncing = signal(false);
  protected readonly actionError = signal<string | null>(null);

  protected readonly formatTime = formatTime;
  protected readonly formatAge = formatAge;
  protected readonly runError = runError;

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
      this.run.set((await this.catalog.syncStatus()).last_run);
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected async runSync(): Promise<void> {
    if (this.syncing()) {
      return;
    }
    this.syncing.set(true);
    this.actionError.set(null);
    try {
      // The trigger always answers 200 with the recorded outcome — including
      // `error` and `stale-if-error` when the upstream fetch failed. Show it
      // as the current run instead of treating it as a request failure.
      const outcome = await this.catalog.syncOpenRouter();
      this.run.set({
        id: outcome.run_id,
        source: 'openrouter',
        started_at: outcome.started_at,
        finished_at: outcome.finished_at,
        status: outcome.status,
        model_count: outcome.model_count,
        detail: outcome.error === null ? null : { error: outcome.error },
      });
    } catch (err) {
      this.actionError.set(describeError(err, 'The sync failed to start.'));
    } finally {
      this.syncing.set(false);
    }
  }

  /** `stale-if-error` kept the previous snapshot: prices may be out of date. */
  protected isStale(run: SyncRun): boolean {
    return run.status === 'stale-if-error';
  }

  protected statusClass(run: SyncRun): string {
    switch (run.status) {
      case 'success':
        return 'ok';
      case 'stale-if-error':
        return 'warn';
      default:
        return 'error';
    }
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading the sync status failed.'));
  }
}
