import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { AuthService } from '../auth/auth.service';
import { BalanceCache } from './balance-cache';
import { BreakerPanel } from './breaker-panel';
import { CatalogService } from './catalog.service';
import { Connectors } from './connectors';
import type { ConnectorKind, PriceDisplay } from './connectors.types';
import { Credentials, type CredentialChange } from './credentials';
import { describeError, isForbidden } from './errors';
import { formatAge } from './format';
import type {
  ApplyPresetResult,
  Credential,
  DiscoverySummary,
  Offering,
  Provider,
  ProviderInput,
  ProviderKind,
  ProviderPatch,
  ProviderPreset,
  ProviderPresetId,
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
import type { BadgeTone } from '../ui/badge';

/** A connector tab: the Prices tab is the `catalog` connector. */
type SetupTab = 'catalog' | 'balance' | 'health';

/** One provider-row connector badge; clicking it opens that setup tab. */
interface ConnectorBadge {
  kind: ConnectorKind;
  tab: SetupTab;
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
  /** Empty means no preset (the pre-D52 behaviour). */
  preset: ProviderPresetId | '';
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
  return {
    name: '',
    base_url: '',
    kind: 'openai_compatible',
    enabled: true,
    preset: '',
  };
}

/** The URL tab names accepted by `/providers?setup=<id>&tab=...`. */
function setupTabFromParam(raw: string | null): SetupTab {
  if (raw === 'balance' || raw === 'health') {
    return raw;
  }
  return 'catalog';
}

/** The query-string name for a setup tab (`prices` is the user-facing one). */
function setupTabParam(tab: SetupTab): string {
  return tab === 'catalog' ? 'prices' : tab;
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
    BreakerPanel,
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
  ],
  templateUrl: './providers.html',
  styleUrl: './providers.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Providers {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);
  private readonly balanceCache = inject(BalanceCache);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);

  protected readonly providers = signal<Provider[]>([]);
  /** Full catalog lists shared with the breaker panel (D50). */
  protected readonly offerings = signal<Offering[]>([]);
  protected readonly credentials = signal<Credential[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly notice = signal<string | null>(null);

  protected readonly credentialsProvider = signal<Provider | null>(null);
  protected readonly togglingId = signal<string | null>(null);
  /** The button that opened the credentials drawer, for focus restoration. */
  private credentialsTrigger: HTMLElement | null = null;

  protected readonly formState = signal<FormState | null>(null);
  protected readonly draft = signal<ProviderDraft>(emptyDraft());
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly formError = signal<string | null>(null);

  protected readonly deleting = signal<Provider | null>(null);
  protected readonly deletePending = signal(false);

  protected readonly discovery = signal<DiscoveryState | null>(null);
  /** The open Provider setup dialog and the tab it should show. */
  protected readonly connectorProvider = signal<Provider | null>(null);
  protected readonly connectorTab = signal<SetupTab>('catalog');
  /** The provider id from `?setup=` waiting for the list to load. */
  private pendingSetupId: string | null = null;

  protected readonly presets = signal<ProviderPreset[]>([]);
  protected readonly applyingPreset = signal<Provider | null>(null);
  protected readonly presetForce = signal(false);
  protected readonly presetPending = signal(false);
  protected readonly presetResult = signal<ApplyPresetResult | null>(null);

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
      this.route.queryParamMap.subscribe((params) => {
        this.syncSetupFromQuery(params.get('setup'), params.get('tab'));
      });
    } else {
      this.forbidden.set(true);
    }
  }

  private async loadPresets(): Promise<void> {
    try {
      this.presets.set(await this.catalog.providerPresets());
    } catch {
      // The create dialog still works without presets (free-form base URL).
    }
  }

  /**
   * Opens or closes the setup dialog from `?setup=`/`?tab=` so a link and the
   * browser back/forward buttons both work. An unknown provider id is ignored.
   */
  private syncSetupFromQuery(setupId: string | null, tab: string | null): void {
    if (setupId === null || setupId === '') {
      this.connectorProvider.set(null);
      this.pendingSetupId = null;
      return;
    }
    const provider = this.providers().find((candidate) => candidate.id === setupId);
    if (provider === undefined) {
      // The list may still be loading; reload() applies it once known.
      this.pendingSetupId = setupId;
      return;
    }
    this.pendingSetupId = null;
    this.connectorTab.set(setupTabFromParam(tab));
    this.connectorProvider.set(provider);
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
      this.credentials.set(credentialLists.flat());
      this.offerings.set(offerings);

      const counts = new Map<string, number>();
      for (const offering of offerings) {
        counts.set(offering.provider_id, (counts.get(offering.provider_id) ?? 0) + 1);
      }
      this.offeringCounts.set(counts);

      // Displays come from each provider's catalog connector so the balance
      // column uses the operator's symbol and decimals. Providers without a
      // catalog connector are skipped, so a plain provider list stays cheap.
      void this.loadDisplays(providers);

      // Keep an open credentials drawer pointed at the fresh copy of its
      // provider, and close it when that provider no longer exists.
      const openId = this.credentialsProvider()?.id;
      if (openId !== undefined) {
        this.credentialsProvider.set(providers.find((provider) => provider.id === openId) ?? null);
      }
      if (this.pendingSetupId !== null) {
        const pending = this.pendingSetupId;
        this.pendingSetupId = null;
        this.syncSetupFromQuery(pending, this.route.snapshot.queryParamMap.get('tab'));
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

  /**
   * The balance column's symbol and decimals: the balance unit's display when
   * a balance connector exists (D52), otherwise the catalog unit's. Providers
   * with neither connector are skipped, so the list stays cheap.
   */
  private async loadDisplays(providers: readonly Provider[]): Promise<void> {
    const relevant = providers.filter((provider) =>
      provider.connectors.some(
        (connector) => connector.kind === 'balance' || connector.kind === 'catalog',
      ),
    );
    if (relevant.length === 0) {
      this.displays.set(new Map());
      return;
    }
    const views = await Promise.all(
      relevant.map(async (provider) => {
        const kinds = provider.connectors.map((connector) => connector.kind);
        if (kinds.includes('balance')) {
          const view = await this.catalog.getConnector(provider.id, 'balance').catch(() => null);
          if (view?.config?.balance?.unit.display !== undefined) {
            return view.config.balance.unit.display;
          }
        }
        if (kinds.includes('catalog')) {
          const view = await this.catalog.getConnector(provider.id, 'catalog').catch(() => null);
          return view?.config?.catalog?.unit.display;
        }
        return undefined;
      }),
    );
    const displays = new Map<string, PriceDisplay>();
    relevant.forEach((provider, index) => {
      const display = views[index];
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
          tab: kind,
          label,
          tone: 'neutral',
          title: `No ${label} settings configured. Click to open the ${tabLabel(kind)} tab.`,
        };
      }
      return {
        kind,
        tab: kind,
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

  protected openConnectors(provider: Provider, tab: SetupTab = 'catalog'): void {
    this.connectorTab.set(tab);
    this.connectorProvider.set(provider);
    void this.router.navigate(['/providers'], {
      queryParams: { setup: provider.id, tab: setupTabParam(tab) },
    });
  }

  protected closeConnectors(): void {
    this.connectorProvider.set(null);
    void this.router.navigate(['/providers'], {});
  }

  /** The setup tab from a badge click. */
  protected openSetupTab(provider: Provider, badge: ConnectorBadge): void {
    this.openConnectors(provider, badge.tab);
  }

  /** True when the provider has a balance connector that needs attention. */
  protected needsBalanceSetup(provider: Provider): boolean {
    const hasConnector = provider.connectors.some(
      (connector) => connector.kind === 'balance' && connector.enabled,
    );
    if (!hasConnector) {
      return false;
    }
    const credentials = this.credentialsByProvider().get(provider.id) ?? [];
    return credentials.some(
      (credential) => credential.balance_error !== null || credential.balance_available === false,
    );
  }

  protected async onConnectorsChanged(): Promise<void> {
    await this.reload();
  }

  /**
   * The credentials panel reports its fresh list, so the provider row's count
   * and an open Load models picker update without a second read.
   */
  protected onCredentialsChanged(change: CredentialChange): void {
    this.credentialsByProvider.update((map) =>
      new Map(map).set(change.providerId, [...change.credentials]),
    );
    const state = this.discovery();
    if (state === null || state.provider.id !== change.providerId) {
      return;
    }
    const selectedStillExists = change.credentials.some(
      (credential) => credential.id === state.credentialId,
    );
    this.discovery.set({
      ...state,
      credentials: [...change.credentials],
      credentialId: selectedStillExists
        ? state.credentialId
        : defaultCredentialId(change.credentials),
    });
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

  protected openCredentials(provider: Provider, event: MouseEvent): void {
    this.credentialsTrigger =
      event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
    this.credentialsProvider.set(provider);
  }

  protected closeCredentials(): void {
    const trigger = this.credentialsTrigger;
    this.credentialsTrigger = null;
    this.credentialsProvider.set(null);
    queueMicrotask(() => trigger?.focus());
  }

  // --- provider form -------------------------------------------------------

  protected openCreate(): void {
    if (this.presets().length === 0) {
      void this.loadPresets();
    }
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
      preset: provider.preset ?? '',
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

  protected setEnabled(value: boolean): void {
    this.draft.update((draft) => ({ ...draft, enabled: value }));
  }

  /** Picking a preset prefills its base URL; everything stays editable. */
  protected setPreset(id: string): void {
    const preset = this.presets().find((candidate) => candidate.id === id);
    this.draft.update((draft) =>
      preset === undefined
        ? { ...draft, preset: '' }
        : { ...draft, preset: preset.id, base_url: preset.base_url },
    );
  }

  protected presetDescription(): string | null {
    const id = this.draft().preset;
    if (id === '') {
      return null;
    }
    const preset = this.presets().find((candidate) => candidate.id === id);
    return preset?.description ?? null;
  }

  /** The preset's user-facing label, or null for a hand-configured provider. */
  protected presetLabel(provider: Provider): string | null {
    if (provider.preset === null) {
      return null;
    }
    return this.presets().find((preset) => preset.id === provider.preset)?.label ?? provider.preset;
  }

  /** The automatic-configuration note shown under the create form. */
  protected presetSeedsConnectors(): boolean {
    const preset = this.presets().find((candidate) => candidate.id === this.draft().preset);
    return preset?.configs !== undefined;
  }

  protected askApplyPreset(provider: Provider): void {
    this.presetForce.set(false);
    this.presetResult.set(null);
    this.applyingPreset.set(provider);
  }

  protected closeApplyPreset(): void {
    this.applyingPreset.set(null);
  }

  protected setPresetForce(value: boolean): void {
    this.presetForce.set(value);
  }

  protected async confirmApplyPreset(): Promise<void> {
    const provider = this.applyingPreset();
    if (provider === null) {
      return;
    }
    this.presetPending.set(true);
    this.actionError.set(null);
    try {
      const result = await this.catalog.applyPreset(provider.id, {
        force: this.presetForce(),
      });
      this.presetResult.set(result);
      this.applyingPreset.set(null);
      this.notice.set(describePresetResult(result));
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Applying the preset failed.'));
    } finally {
      this.presetPending.set(false);
    }
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
        const baseUrl = draft.base_url.trim();
        const input: ProviderInput = {
          name: draft.name.trim(),
          kind: draft.kind,
          ...(baseUrl === '' ? {} : { base_url: baseUrl }),
          ...(draft.preset === '' ? {} : { preset: draft.preset }),
        };
        await this.catalog.createProvider(input);
        this.notice.set(
          draft.preset === ''
            ? 'Provider created.'
            : 'Provider created with its preset; prices and balance are ready to test.',
        );
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
      if (this.credentialsProvider()?.id === provider.id) {
        this.credentialsProvider.set(null);
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

  protected async openDiscovery(provider: Provider): Promise<void> {
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
    // The cached list is stale after a credential was just created or edited:
    // re-read it before the operator can run a preview with it.
    await this.refreshDiscoveryCredentials(provider.id);
  }

  /**
   * Re-reads a provider's credentials while its Load models dialog is open.
   * A failure keeps the cached list so the dialog stays usable.
   */
  private async refreshDiscoveryCredentials(providerId: string): Promise<void> {
    let credentials: Credential[];
    try {
      credentials = await this.catalog.listCredentials(providerId);
    } catch {
      return;
    }
    this.credentialsByProvider.update((map) => new Map(map).set(providerId, credentials));
    const state = this.discovery();
    if (state === null || state.provider.id !== providerId) {
      return;
    }
    const selectedStillExists = credentials.some(
      (credential) => credential.id === state.credentialId,
    );
    this.discovery.set({
      ...state,
      credentials,
      credentialId: selectedStillExists ? state.credentialId : defaultCredentialId(credentials),
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

/** The user-facing name of a setup tab. */
function tabLabel(tab: ConnectorKind): string {
  return tab === 'catalog' ? 'Prices' : tab === 'balance' ? 'Balance' : 'Health';
}

/** "Preset applied: catalog created, balance unchanged." */
function describePresetResult(result: ApplyPresetResult): string {
  const parts = result.changes.map((change) => `${change.kind} ${change.action}`);
  return parts.length === 0
    ? 'Preset applied; it defines no settings.'
    : `Preset applied: ${parts.join(', ')}.`;
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
