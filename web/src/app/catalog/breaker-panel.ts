import { ChangeDetectionStrategy, Component, computed, inject, input, signal } from '@angular/core';
import { AuthService } from '../auth/auth.service';
import { describeError } from '../catalog/errors';
import { formatTime } from '../catalog/format';
import type { BreakerEntry } from '../catalog/breaker.types';
import type { Credential, Offering, Provider } from '../catalog/catalog.types';
import { CatalogService } from '../catalog/catalog.service';
import { UiBadge, type BadgeTone } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiEmpty } from '../ui/empty';
import { UiSpinner } from '../ui/spinner';
import { UiSwitch } from '../ui/switch';

/** One pair with its catalog labels resolved. */
interface BreakerRow {
  entry: BreakerEntry;
  offering: string;
  provider: string;
}

/** One credential with its offerings (D50: grouped credential -> offering). */
interface BreakerGroup {
  credentialId: string;
  credential: string;
  provider: string;
  open: boolean;
  rows: BreakerRow[];
}

/**
 * The breaker section of the providers page (D50). Reads live state from
 * `/api/breaker`, resolves offering and credential labels from the catalog,
 * and offers a manual reset per pair. A restart clears the live state; the
 * trip history stays in the database.
 */
@Component({
  selector: 'app-breaker-panel',
  imports: [UiBadge, UiBanner, UiButton, UiEmpty, UiSpinner, UiSwitch],
  templateUrl: './breaker-panel.html',
  styleUrl: './breaker-panel.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class BreakerPanel {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);

  /** Catalog data resolved by the providers page, so only one call is made here. */
  readonly providers = input<readonly Provider[]>([]);
  readonly offerings = input<readonly Offering[]>([]);
  readonly credentials = input<readonly Credential[]>([]);

  protected readonly entries = signal<BreakerEntry[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly actionError = signal<string | null>(null);
  protected readonly notice = signal<string | null>(null);
  protected readonly resetting = signal<string | null>(null);
  protected readonly openOnly = signal(false);

  protected readonly formatTime = formatTime;

  protected readonly groups = computed<BreakerGroup[]>(() => {
    const providers = new Map(this.providers().map((provider) => [provider.id, provider.name]));
    const offerings = new Map(this.offerings().map((offering) => [offering.id, offering]));
    const credentials = new Map(
      this.credentials().map((credential) => [credential.id, credential]),
    );
    const byCredential = new Map<string, BreakerGroup>();
    for (const entry of this.entries()) {
      const offering = offerings.get(entry.offering_id);
      const credential = credentials.get(entry.credential_id);
      const providerId = offering?.provider_id ?? credential?.provider_id ?? '';
      const provider = providers.get(providerId) ?? providerId.slice(0, 8);
      const groupKey = entry.credential_id;
      const group = byCredential.get(groupKey) ?? {
        credentialId: entry.credential_id,
        credential: credential?.label ?? entry.credential_id.slice(0, 8),
        provider,
        open: false,
        rows: [],
      };
      group.open = group.open || entry.status === 'open';
      group.rows.push({
        entry,
        offering: offering?.upstream_model_id ?? entry.offering_id.slice(0, 8),
        provider,
      });
      byCredential.set(groupKey, group);
    }
    return [...byCredential.values()]
      .filter((group) => !this.openOnly() || group.open)
      .map((group) => ({
        ...group,
        rows: [...group.rows].sort((a, b) => a.offering.localeCompare(b.offering)),
      }))
      .sort(
        (a, b) =>
          Number(b.open) - Number(a.open) ||
          a.provider.localeCompare(b.provider) ||
          a.credential.localeCompare(b.credential),
      );
  });

  constructor() {
    if (this.auth.isAdmin()) {
      void this.reload();
    }
  }

  protected async reload(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      this.entries.set(await this.catalog.breakerSnapshot());
    } catch (err) {
      this.loadError.set(describeError(err, 'Loading breaker state failed.'));
    } finally {
      this.loading.set(false);
    }
  }

  protected reset(row: BreakerRow): void {
    const key = `${row.entry.credential_id}:${row.entry.offering_id}`;
    if (this.resetting() !== null) {
      return;
    }
    this.resetting.set(key);
    this.actionError.set(null);
    this.notice.set(null);
    void this.catalog
      .resetBreaker(row.entry.credential_id, row.entry.offering_id)
      .then(() => {
        this.notice.set('Breaker reset. The pair is closed and its streak cleared.');
        return this.reload();
      })
      .catch((err: unknown) =>
        this.actionError.set(describeError(err, 'Resetting the breaker failed.')),
      )
      .finally(() => this.resetting.set(null));
  }

  protected statusTone(status: BreakerEntry['status']): BadgeTone {
    return status === 'open' ? 'danger' : status === 'half_open' ? 'warning' : 'neutral';
  }

  protected statusLabel(status: BreakerEntry['status']): string {
    return status === 'half_open' ? 'half-open' : status;
  }

  protected openFor(entry: BreakerEntry): string {
    if (entry.status !== 'open' || entry.open_for_ms === null) {
      return '—';
    }
    const seconds = Math.ceil(entry.open_for_ms / 1000);
    return `${seconds}s`;
  }

  protected rowKey(row: BreakerRow): string {
    return `${row.entry.credential_id}:${row.entry.offering_id}`;
  }
}
