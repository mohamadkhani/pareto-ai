import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import { AuthService } from '../auth/auth.service';
import { BalanceCache } from './balance-cache';
import { CatalogService } from './catalog.service';
import { Connectors } from './connectors';
import type { ConnectorKind, PriceDisplay } from './connectors.types';
import { Credentials } from './credentials';
import { describeError, isForbidden } from './errors';
import { formatAge } from './format';
import type {
  Credential,
  DiscoverySummary,
  Provider,
  ProviderInput,
  ProviderKind,
  ProviderPatch,
} from './catalog.types';
import { UiBadge } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiDialog } from '../ui/dialog';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';
import { UiSelect } from '../ui/select';
import { UiSwitch } from '../ui/switch';
import { UiTable } from '../ui/table';
import type { BadgeTone } from '../ui/badge';

/** One provider-row connector badge. */
interface ConnectorBadge {
  kind: ConnectorKind;
  label: string;
  tone: BadgeTone;
  title: string;
}

/** The aggregate balance rendered in the provider row. */
interface ProviderBalance {
  text: string;
  available: boolean | null;
  low: boolean;
  days: string | null;
  lowCredential: boolean;
}

/** The provider form's editable fields. */
interface ProviderDraft {
  name: string;
  base_url: string;
  kind: ProviderKind;
  enabled: boolean;
}

/** One run of the Load models dialog. */
interface DiscoveryState {
  provider: Provider;
  credentials: Credential[];
  credentialId: string;
  /** Result of the last dry run; Apply stays disabled until it exists. */
  preview: DiscoverySummary | null;
  /** Result of the applied run, when Apply has succeeded. */
  applied: DiscoverySummary | null;
  error: string | null;
  running: 'preview' | 'apply' | null;
}

type FormState = { mode: 'create' } | { mode: 'edit'; provider: Provider };

function emptyDraft(): ProviderDraft {
  return { name: '', base_url: '', kind: 'openai_compatible', enabled: true };
}

/**
 * Providers: register upstreams, manage their credentials inline, and load
 * each provider's models through a preview-then-apply dialog (D31). The list
 * shows counts and the last applied discovery so the catalogue state is
 * visible without opening a provider.
 */
@Component({
  selector: 'app-providers',
  imports: [
    RouterLink,
    Connectors,
    Credentials,
    UiBadge,
    UiBanner,
    UiButton,
    UiDialog,
    UiEmpty,
    UiField,
    UiInput,
    UiSelect,
    UiSwitch,
    UiTable,
  ],
  templateUrl: './providers.html',
  styleUrl: './providers.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Providers {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);
  private readonly balanceCache = inject(BalanceCache);

  protected readonly providers = signal<Provider[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly notice = signal<string | null>(null);

  protected readonly selectedId = signal<string | null>(null);
  protected readonly togglingId = signal<string | null>(null);

  protected readonly formState = signal<FormState | null>(null);
  protected readonly draft = signal<ProviderDraft>(emptyDraft());
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly formError = signal<string | null>(null);

  protected readonly deleting = signal<Provider | null>(null);
  protected readonly deletePending = signal(false);

  protected readonly discovery = signal<DiscoveryState | null>(null);
  protected readonly connectorProvider = signal<Provider | null>(null);

  private readonly displays = signal<ReadonlyMap<string, PriceDisplay>>(new Map());

  private readonly credentialsByProvider = signal<ReadonlyMap<string, Credential[]>>(new Map());
  private readonly offeringCounts = signal<ReadonlyMap<string, number>>(new Map());

  protected readonly errors = computed<Partial<Record<'name' | 'base_url', string>>>(() =>
    validateDraft(this.draft(), this.attempted()),
  );

  protected readonly formTitle = computed(() => {
    const state = this.formState();
    return state?.mode === 'edit' ? `Edit ${state.provider.name}` : 'New provider';
  });

  protected readonly formatAge = formatAge;

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
      const providers = await this.catalog.listProviders();
      this.providers.set(providers);
      // Counts come from the existing lists: one credentials read per provider
      // and the offerings list. A count failure must not blank the page.
      const [credentialLists, offerings] = await Promise.all([
        Promise.all(
          providers.map((provider) =>
            this.catalog.listCredentials(provider.id).catch((): Credential[] => []),
          ),
        ),
        this.catalog.listOfferings().catch(() => []),
      ]);
      const credentials = new Map<string, Credential[]>();
      providers.forEach((provider, index) => {
        credentials.set(provider.id, credentialLists[index] ?? []);
      });
      this.credentialsByProvider.set(credentials);

      const counts = new Map<string, number>();
      for (const offering of offerings) {
        counts.set(offering.provider_id, (counts.get(offering.provider_id) ?? 0) + 1);
      }
      this.offeringCounts.set(counts);

      // Displays come from each provider's catalog connector so the balance
      // column uses the operator's symbol and decimals. Providers without a
      // catalog connector are skipped, so a plain provider list stays cheap.
      void this.loadDisplays(providers);

      // Keep the expanded panel pointed at the fresh copy of its provider.
      const selectedId = this.selectedId();
      if (selectedId !== null && !providers.some((provider) => provider.id === selectedId)) {
        this.selectedId.set(null);
      }
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected credentialCount(providerId: string): number {
    return this.credentialsByProvider().get(providerId)?.length ?? 0;
  }

  protected offeringCount(providerId: string): number {
    return this.offeringCounts().get(providerId) ?? 0;
  }

  private async loadDisplays(providers: readonly Provider[]): Promise<void> {
    const withCatalog = providers.filter((provider) =>
      provider.connectors.some((connector) => connector.kind === 'catalog'),
    );
    if (withCatalog.length === 0) {
      this.displays.set(new Map());
      return;
    }
    const views = await Promise.all(
      withCatalog.map((provider) =>
        this.catalog.getConnector(provider.id, 'catalog').catch(() => null),
      ),
    );
    const displays = new Map<string, PriceDisplay>();
    withCatalog.forEach((provider, index) => {
      const display = views[index]?.config?.catalog?.unit.display;
      if (display !== undefined) {
        displays.set(provider.id, display);
      }
    });
    this.displays.set(displays);
  }

  /** One badge per connector kind; absent kinds stay visible but muted. */
  protected connectorBadges(provider: Provider): ConnectorBadge[] {
    const configured = new Map(provider.connectors.map((connector) => [connector.kind, connector]));
    return (['catalog', 'balance', 'health'] as const).map((kind) => {
      const connector = configured.get(kind);
      const label = kind === 'catalog' ? 'prices' : kind;
      if (connector === undefined) {
        return {
          kind,
          label,
          tone: 'neutral',
          title: `No ${label} connector configured.`,
        };
      }
      return {
        kind,
        label,
        tone: connector.enabled ? 'success' : 'warning',
        title: `${label} connector · ${connector.enabled ? 'enabled' : 'disabled'} · updated ${formatAge(
          connector.updated_at,
        )}`,
      };
    });
  }

  protected connectorTone(badge: ConnectorBadge): BadgeTone {
    return badge.tone;
  }

  protected openConnectors(provider: Provider): void {
    this.connectorProvider.set(provider);
  }

  protected closeConnectors(): void {
    this.connectorProvider.set(null);
  }

  protected async onConnectorsChanged(): Promise<void> {
    await this.reload();
  }

  /**
   * Sums the credentials' known balances. An unavailable provider balance is
   * never zero: with no usable number the row says "unavailable".
   */
  protected providerBalance(provider: Provider): ProviderBalance {
    const credentials = this.credentialsByProvider().get(provider.id) ?? [];
    if (credentials.length === 0) {
      return { text: '—', available: null, low: false, days: null, lowCredential: false };
    }
    const known = credentials.filter(
      (credential) => credential.balance_available === true && credential.balance_usd !== null,
    );
    if (known.length === 0) {
      return {
        text: 'unavailable',
        available: false,
        low: false,
        days: null,
        lowCredential: false,
      };
    }
    const total = known.reduce((sum, credential) => sum + (credential.balance_usd ?? 0), 0);
    const display = this.displays().get(provider.id) ?? { symbol: '$', decimals: 2 };
    const lowCredential = known.some(
      (credential) =>
        credential.balance_low_threshold !== null &&
        (credential.balance_usd ?? Number.POSITIVE_INFINITY) <= credential.balance_low_threshold,
    );
    let days: string | null = null;
    for (const credential of known) {
      const result = this.balanceCache.get(credential.id);
      if (result?.days_remaining !== undefined && result?.days_remaining !== null) {
        days = `runs out in ${formatDays(result.days_remaining)}`;
        break;
      }
    }
    return {
      text: `${display.symbol}${total.toFixed(display.decimals)}`,
      available: true,
      low: total <= 0 || lowCredential,
      days,
      lowCredential,
    };
  }

  /** "Last load" for the list: persisted on the provider, so it survives a reload. */
  protected discoveryLabel(provider: Provider): string {
    const at = provider.last_discovery_at;
    if (at === null) {
      return 'never';
    }
    const created = provider.last_discovery_created ?? 0;
    const unchanged = provider.last_discovery_unchanged ?? 0;
    return `+${created} models · ${unchanged} unchanged · ${formatAge(at)}`;
  }

  protected toggleSelected(provider: Provider): void {
    this.selectedId.set(this.selectedId() === provider.id ? null : provider.id);
  }

  // --- provider form -------------------------------------------------------

  protected openCreate(): void {
    this.draft.set(emptyDraft());
    this.attempted.set(false);
    this.formError.set(null);
    this.formState.set({ mode: 'create' });
  }

  protected openEdit(provider: Provider): void {
    this.draft.set({
      name: provider.name,
      base_url: provider.base_url,
      kind: provider.kind,
      enabled: provider.enabled,
    });
    this.attempted.set(false);
    this.formError.set(null);
    this.formState.set({ mode: 'edit', provider });
  }

  protected closeForm(): void {
    this.formState.set(null);
  }

  protected setName(value: string): void {
    this.draft.update((draft) => ({ ...draft, name: value }));
  }

  protected setBaseUrl(value: string): void {
    this.draft.update((draft) => ({ ...draft, base_url: value }));
  }

  protected setKind(value: string): void {
    if (value === 'openai_compatible') {
      this.draft.update((draft) => ({ ...draft, kind: value }));
    }
  }

  protected setEnabled(value: boolean): void {
    this.draft.update((draft) => ({ ...draft, enabled: value }));
  }

  protected async save(): Promise<void> {
    this.attempted.set(true);
    this.formError.set(null);
    const state = this.formState();
    if (state === null || Object.keys(validateDraft(this.draft(), true)).length > 0) {
      return;
    }
    const draft = this.draft();
    this.saving.set(true);
    try {
      if (state.mode === 'create') {
        const input: ProviderInput = {
          name: draft.name.trim(),
          base_url: draft.base_url.trim(),
          kind: draft.kind,
        };
        await this.catalog.createProvider(input);
        this.notice.set('Provider created.');
      } else {
        const patch = buildPatch(state.provider, draft);
        if (Object.keys(patch).length > 0) {
          await this.catalog.updateProvider(state.provider.id, patch);
          this.notice.set('Provider saved.');
        }
      }
      this.closeForm();
      await this.reload();
    } catch (err) {
      this.formError.set(describeError(err, 'Saving the provider failed.'));
    } finally {
      this.saving.set(false);
    }
  }

  protected async toggleEnabled(provider: Provider): Promise<void> {
    this.togglingId.set(provider.id);
    this.actionError.set(null);
    try {
      await this.catalog.updateProvider(provider.id, { enabled: !provider.enabled });
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Changing the provider failed.'));
    } finally {
      this.togglingId.set(null);
    }
  }

  protected askDelete(provider: Provider): void {
    this.deleting.set(provider);
  }

  protected closeDelete(): void {
    this.deleting.set(null);
  }

  protected async confirmDelete(): Promise<void> {
    const provider = this.deleting();
    if (provider === null) {
      return;
    }
    this.deletePending.set(true);
    this.actionError.set(null);
    try {
      await this.catalog.deleteProvider(provider.id);
      if (this.selectedId() === provider.id) {
        this.selectedId.set(null);
      }
      this.deleting.set(null);
      this.notice.set('Provider deleted.');
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Deleting the provider failed.'));
    } finally {
      this.deletePending.set(false);
    }
  }

  // --- Load models (discovery) --------------------------------------------

  protected openDiscovery(provider: Provider): void {
    const credentials = this.credentialsByProvider().get(provider.id) ?? [];
    this.discovery.set({
      provider,
      credentials,
      credentialId: defaultCredentialId(credentials),
      preview: null,
      applied: null,
      error: null,
      running: null,
    });
  }

  protected closeDiscovery(): void {
    this.discovery.set(null);
  }

  protected setDiscoveryCredential(credentialId: string): void {
    this.discovery.update((state) =>
      state === null
        ? state
        : { ...state, credentialId, preview: null, applied: null, error: null },
    );
  }

  protected async runDiscovery(dryRun: boolean): Promise<void> {
    const state = this.discovery();
    if (state === null || state.running !== null) {
      return;
    }
    if (state.credentialId === '') {
      this.discovery.set({
        ...state,
        error: 'This provider has no credentials yet. Add one before loading models.',
      });
      return;
    }
    this.discovery.set({
      ...state,
      running: dryRun ? 'preview' : 'apply',
      error: null,
      preview: dryRun ? null : state.preview,
      applied: null,
    });
    try {
      const summary = await this.catalog.discoverModels(state.provider.id, {
        credential_id: state.credentialId,
        dry_run: dryRun,
      });
      const current = this.discovery();
      if (current === null) {
        return;
      }
      if (dryRun) {
        this.discovery.set({ ...current, running: null, preview: summary });
      } else {
        this.discovery.set({ ...current, running: null, applied: summary });
        // Refreshes counts and the provider copy (including the persisted
        // last-discovery fields the list renders) without closing the dialog.
        await this.reload();
      }
    } catch (err) {
      const current = this.discovery();
      if (current !== null) {
        // The server's message is secret-free by design; show it verbatim.
        this.discovery.set({
          ...current,
          running: null,
          error: describeError(err, 'Loading models failed.'),
        });
      }
    }
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading providers failed.'));
  }
}

/** The first active credential: ok wins, then anything not failing. */
function defaultCredentialId(credentials: readonly Credential[]): string {
  const active =
    credentials.find((credential) => credential.status === 'ok') ??
    credentials.find(
      (credential) => credential.status !== 'error' && credential.status !== 'cooldown',
    ) ??
    credentials[0];
  return active?.id ?? '';
}

function validateDraft(
  draft: ProviderDraft,
  attempted: boolean,
): Partial<Record<'name' | 'base_url', string>> {
  const errors: Partial<Record<'name' | 'base_url', string>> = {};
  if (!attempted) {
    return errors;
  }
  if (draft.name.trim() === '') {
    errors.name = 'Name is required.';
  }
  const baseUrl = draft.base_url.trim();
  if (baseUrl === '') {
    errors.base_url = 'Base URL is required.';
  } else if (!/^https?:\/\/[^\s]+$/.test(baseUrl)) {
    errors.base_url = 'Enter an http(s) URL, for example https://api.example.com/v1.';
  }
  return errors;
}

/** "runs out in N days": whole days above one, hours below. */
function formatDays(days: number): string {
  if (days >= 1) {
    const whole = Math.floor(days);
    return `${whole} ${whole === 1 ? 'day' : 'days'}`;
  }
  return `${Math.max(0, Math.round(days * 24))} h`;
}

/** Only the fields the admin changed; `kind` cannot change over the wire. */
function buildPatch(provider: Provider, draft: ProviderDraft): ProviderPatch {
  const patch: ProviderPatch = {};
  const name = draft.name.trim();
  const baseUrl = draft.base_url.trim();
  if (name !== provider.name) {
    patch.name = name;
  }
  if (baseUrl !== provider.base_url) {
    patch.base_url = baseUrl;
  }
  if (draft.enabled !== provider.enabled) {
    patch.enabled = draft.enabled;
  }
  return patch;
}
