import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  computed,
  inject,
  signal,
  viewChild,
} from '@angular/core';
import { Location } from '@angular/common';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { ApiError } from '../api';
import { AuthService } from '../auth/auth.service';
import { RoutingService } from '../routing/routing.service';
import type { BenchmarkSyncRun, BenchmarkSyncStatus } from '../routing/routing.types';
import type {
  Model,
  ModelInput,
  OfferingInput,
  OfferingPatch,
  PriceInput,
  Provider,
  SyncRun,
  SyncStatus,
} from './catalog.types';
import { runError } from './catalog.types';
import { CatalogService } from './catalog.service';
import { describeError, isForbidden } from './errors';
import { formatAge, formatCount, formatPricePerM, formatTime, parseTags } from './format';
import { type HeaderRow, HeadersEditor, headerMapOf } from './headers-editor';
import { MixProfiles } from './mix-profiles';
import { MixService } from './mix.service';
import {
  MIX_COMPONENT_LABELS,
  type MixBreakdown,
  type MixComponentPrices,
  type MixPriceView,
  type MixProfile,
  type PricingSettings,
  mixBreakdown,
  profileSummary,
} from './mix.types';
import {
  type AggregatePrice,
  type ModelsOverview,
  type OverviewPrice,
  type OverviewProvider,
  type OverviewQuality,
  type OverviewRow,
  isProviderRow,
  overviewRowKey,
} from './overview.types';
import { UiBadge, type BadgeTone } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiChip } from '../ui/chip';
import { type ComboboxOption, UiCombobox } from '../ui/combobox';
import { UiDialog } from '../ui/dialog';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';
import { UiSelect } from '../ui/select';
import { UiSwitch } from '../ui/switch';
import { UiTable } from '../ui/table';

/** A table column that can reorder rows. */
type SortKey = 'name' | 'context' | 'mix' | 'input' | 'output' | 'cache' | 'quality';

/** The persisted price-basis choice (`localStorage`); the URL wins on load. */
const PRICE_BASIS_STORAGE_KEY = 'pareto.models.priceBasis';

/** Whether the sync details were expanded last time (`localStorage`). */
const SYNC_DETAILS_STORAGE_KEY = 'pareto.models.syncDetails';

/** How long typing must pause before the search hits the API. */
const SEARCH_DEBOUNCE_MS = 250;

/** Provider chips a row shows before the rest collapse into "+N". */
const MAX_PROVIDER_CHIPS = 3;

/** One number in a benchmark sync row, e.g. `112/161` matched. */
interface SyncMetric {
  value: string;
  label: string;
}

/** `components`, `default` (resolved server-side), or a profile id/name. */
const COMPONENTS_BASIS = 'components';
const DEFAULT_BASIS = 'default';

/** The editable fields of the existing model form. */
type ModelField =
  | 'canonical_name'
  | 'family'
  | 'modalities_input'
  | 'modalities_output'
  | 'context_length'
  | 'max_output_tokens'
  | 'capabilities';

type ModelDraft = Record<ModelField, string>;

/** The manual price editor; blank means "keep the stored value" (D20). */
type PriceField =
  | 'input_per_m'
  | 'output_per_m'
  | 'cache_read_per_m'
  | 'cache_write_per_m'
  | 'reasoning_per_m'
  | 'request_fee'
  | 'currency';

type PriceDraft = Record<PriceField, string>;

/** The Add offering form's text fields; numbers stay strings until saved. */
interface OfferingDraft {
  model_id: string;
  upstream_model_id: string;
  context_override: string;
  max_output_override: string;
  enabled: boolean;
}

function emptyOfferingDraft(): OfferingDraft {
  return {
    model_id: '',
    upstream_model_id: '',
    context_override: '',
    max_output_override: '',
    enabled: true,
  };
}

const PRICE_KEYS = [
  'input_per_m',
  'output_per_m',
  'cache_read_per_m',
  'cache_write_per_m',
  'reasoning_per_m',
  'request_fee',
] as const;

const PRICE_FIELDS: ReadonlyArray<{ key: Exclude<PriceField, 'currency'>; label: string }> = [
  { key: 'input_per_m', label: 'Input per 1M (USD)' },
  { key: 'output_per_m', label: 'Output per 1M (USD)' },
  { key: 'cache_read_per_m', label: 'Cache read per 1M (USD)' },
  { key: 'cache_write_per_m', label: 'Cache write per 1M (USD)' },
  { key: 'reasoning_per_m', label: 'Reasoning per 1M (USD)' },
  { key: 'request_fee', label: 'Request fee (USD)' },
];

function emptyModelDraft(): ModelDraft {
  return {
    canonical_name: '',
    family: '',
    modalities_input: '',
    modalities_output: '',
    context_length: '',
    max_output_tokens: '',
    capabilities: '',
  };
}

function emptyPriceDraft(): PriceDraft {
  return {
    input_per_m: '',
    output_per_m: '',
    cache_read_per_m: '',
    cache_write_per_m: '',
    reasoning_per_m: '',
    request_fee: '',
    currency: 'USD',
  };
}

/**
 * Models — the single catalog page per D31. The provider filter switches
 * between the cross-provider view (one row per model with average/minimum
 * price and model-level quality) and a per-offering view for one provider.
 * The table keeps only the fields an operator scans; everything else
 * (all price components, modalities, effective limits, upstream id, quality
 * provenance, manual price editor, override editor) opens in one dialog.
 *
 * Price sync and benchmark sync are toolbar actions with their last-run
 * status; the Offerings, Benchmarks, and Price sync pages are retired.
 */
@Component({
  selector: 'app-models',
  imports: [
    RouterLink,
    HeadersEditor,
    MixProfiles,
    UiBadge,
    UiBanner,
    UiButton,
    UiChip,
    UiCombobox,
    UiDialog,
    UiEmpty,
    UiField,
    UiInput,
    UiSelect,
    UiSwitch,
    UiTable,
  ],
  templateUrl: './models.html',
  styleUrl: './models.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '(document:keydown)': 'onDocumentKeydown($event)' },
})
export class Models {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);
  private readonly mix = inject(MixService);
  private readonly routing = inject(RoutingService);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly location = inject(Location);
  private readonly destroyRef = inject(DestroyRef);
  private readonly searchControl = viewChild(UiInput, { read: ElementRef });

  protected readonly providers = signal<Provider[]>([]);
  protected readonly overview = signal<ModelsOverview | null>(null);
  protected readonly providerId = signal<string>('all');
  /** The text in the search box; the API sees the debounced `searchTerm`. */
  protected readonly searchInput = signal('');
  /** The debounced, trimmed term the overview was last filtered with. */
  protected readonly searchTerm = signal('');
  /** The maximum price the table shows, on the column named by `maxPriceLabel`. */
  protected readonly maxPriceInput = signal('');
  /** The minimum quality score a row must reach. */
  protected readonly minQualityInput = signal('');
  /** Rows before the search filter, for the "N of M models" count. */
  protected readonly totalModels = signal<number | null>(null);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly notice = signal<string | null>(null);

  // Price basis (D34): a mix profile, or the component prices.
  protected readonly profiles = signal<MixProfile[]>([]);
  protected readonly pricing = signal<PricingSettings | null>(null);
  protected readonly priceBasis = signal<string>(DEFAULT_BASIS);
  protected readonly profilesOpen = signal(false);
  /** True once the URL, storage, or the operator picked a concrete basis. */
  private readonly basisExplicit = signal(false);

  protected readonly sortKey = signal<SortKey>('name');
  protected readonly sortDirection = signal<'asc' | 'desc'>('asc');
  /** Rows whose provider chips are expanded past the "+N" collapse. */
  protected readonly expandedProviderRows = signal<ReadonlySet<string>>(new Set());

  protected readonly detailKey = signal<string | null>(null);
  protected readonly detailOpen = signal(false);
  protected readonly modelFormOpen = signal(false);
  protected readonly savingModel = signal(false);
  protected readonly modelAttempted = signal(false);
  protected readonly modelDraft = signal<ModelDraft>(emptyModelDraft());
  protected readonly priceDraft = signal<PriceDraft>(emptyPriceDraft());
  protected readonly priceError = signal<string | null>(null);
  protected readonly savingPrice = signal(false);
  protected readonly togglingEnabled = signal(false);
  /**
   * Row whose enable switch is in flight (`offering:<id>` or `model:<id>`).
   * One shared signal keeps a second click from racing the first.
   */
  protected readonly togglingModelKey = signal<string | null>(null);
  protected readonly overrideDraft = signal('');
  protected readonly overrideError = signal<string | null>(null);
  protected readonly savingOverride = signal(false);
  protected readonly removingOverride = signal(false);

  // Offering management inside the provider-scoped detail dialog.
  protected readonly offeringOpen = signal(false);
  protected readonly offeringModels = signal<Model[]>([]);
  /** Model options for the add-offering picker, searchable by name/family. */
  protected readonly offeringModelOptions = computed<ComboboxOption[]>(() =>
    this.offeringModels().map((model) => ({
      value: model.id,
      label: model.canonical_name,
      searchText: model.family ?? '',
    })),
  );
  protected readonly offeringDraft = signal<OfferingDraft>(emptyOfferingDraft());
  protected readonly offeringAttempted = signal(false);
  protected readonly offeringHeaderRows = signal<HeaderRow[]>([]);
  protected readonly offeringHeadersTouched = signal(false);
  protected readonly savingOffering = signal(false);
  protected readonly offeringError = signal<string | null>(null);
  protected readonly removeOfferingOpen = signal(false);
  protected readonly removingOffering = signal(false);

  protected readonly syncStatus = signal<SyncStatus | null>(null);
  protected readonly benchmarkStatus = signal<BenchmarkSyncStatus | null>(null);
  /** The sync detail rows; the summary line is always visible. */
  protected readonly syncDetailsOpen = signal(false);
  protected readonly benchmarkSyncAvailable = signal(true);
  protected readonly syncingPrices = signal(false);
  protected readonly syncingBenchmarks = signal(false);

  protected readonly priceFields = PRICE_FIELDS;
  protected readonly formatPrice = formatPricePerM;
  protected readonly formatCount = formatCount;
  protected readonly formatAge = formatAge;
  protected readonly formatTime = formatTime;
  protected readonly runError = runError;

  private readonly providerById = computed(
    () => new Map(this.providers().map((provider) => [provider.id, provider])),
  );

  /** The selected provider has no catalog connector, so prices cannot sync. */
  protected readonly missingCatalogConnector = computed(() => {
    const id = this.providerId();
    if (id === 'all') {
      return false;
    }
    const provider = this.providerById().get(id);
    return (
      provider !== undefined &&
      !provider.connectors.some((connector) => connector.kind === 'catalog')
    );
  });

  protected readonly scope = computed(() => this.overview()?.scope ?? 'all');
  /** Every row the overview returned, before the client-side filters. */
  private readonly overviewRows = computed<OverviewRow[]>(() => this.overview()?.rows ?? []);
  protected readonly maxPrice = computed(() => parseOptionalNumber(this.maxPriceInput()));
  protected readonly minQuality = computed(() => parseOptionalNumber(this.minQualityInput()));
  protected readonly rows = computed<OverviewRow[]>(() =>
    this.overviewRows()
      .filter((row) => this.matchesRefinements(row))
      .sort((a, b) => this.compareRows(a, b)),
  );
  protected readonly detailRow = computed<OverviewRow | null>(() => {
    const key = this.detailKey();
    if (key === null) {
      return null;
    }
    return this.rows().find((row) => overviewRowKey(row) === key) ?? null;
  });
  /**
   * Opens the provider's setup dialog on the Prices tab. Uses the deep link so
   * the behaviour matches a hand-typed URL and back/forward.
   */
  protected openPriceSetup(): void {
    const id = this.providerId();
    void this.router.navigate(['/providers'], {
      queryParams: { setup: id === 'all' ? null : id, tab: 'prices' },
    });
  }

  /** True while the table shows mix prices instead of the three components. */
  protected readonly mixSelected = computed(() => this.priceBasis() !== COMPONENTS_BASIS);
  protected readonly activeProfile = computed<MixProfile | null>(() => {
    const id = this.priceBasis();
    return this.profiles().find((profile) => profile.id === id) ?? null;
  });
  /** The profile the overview's `mix_price` values were computed with. */
  protected readonly overviewProfile = computed<MixProfile | null>(() => {
    const id = this.overview()?.mix?.profile_id;
    return this.profiles().find((profile) => profile.id === id) ?? this.activeProfile();
  });
  protected readonly basisLabel = computed(
    () =>
      this.overviewProfile()?.name ??
      this.overview()?.mix?.profile_name ??
      this.activeProfile()?.name ??
      'default',
  );
  protected readonly effectiveProfileId = computed(
    () => this.pricing()?.effective_profile.profile_id ?? null,
  );
  protected readonly detailBreakdown = computed<MixBreakdown | null>(() => {
    const row = this.detailRow();
    const profile = this.overviewProfile();
    if (row === null || profile === null) {
      return null;
    }
    return mixBreakdown(profile, componentPricesOf(row));
  });
  protected readonly detailMix = computed<MixPriceView | null>(
    () => this.detailRow()?.mix_price ?? null,
  );
  protected readonly modelErrors = computed<Partial<Record<ModelField, string>>>(() =>
    validateModel(this.modelDraft(), this.modelAttempted()),
  );
  protected readonly offeringErrors = computed<
    Partial<
      Record<'model_id' | 'upstream_model_id' | 'context_override' | 'max_output_override', string>
    >
  >(() => validateOffering(this.offeringDraft(), this.offeringAttempted()));
  protected readonly anyPriced = computed(() =>
    this.rows().some((row) => (isProviderRow(row) ? row.price !== null : row.price_avg !== null)),
  );
  protected readonly allUnpriced = computed(() => this.rows().length > 0 && !this.anyPriced());
  /** True while a non-empty search term filters the list. */
  protected readonly filterActive = computed(() => this.searchTerm() !== '');
  /** True while a price or quality cap narrows the rows client-side. */
  protected readonly refinementActive = computed(
    () => this.maxPrice() !== null || this.minQuality() !== null,
  );
  protected readonly anyFilterActive = computed(
    () => this.filterActive() || this.refinementActive(),
  );
  /** The price column the maximum applies to. */
  protected readonly maxPriceLabel = computed(() =>
    this.mixSelected() ? 'Max mix price ($/M)' : 'Max input price ($/M)',
  );
  /** "N of M models" while filtering, otherwise "N models". */
  protected readonly resultSummary = computed(() => {
    const shown = this.rows().length;
    const total = this.totalModels() ?? this.overviewRows().length;
    if (this.anyFilterActive()) {
      return `${shown} of ${total} model${total === 1 ? '' : 's'}`;
    }
    return `${shown} model${shown === 1 ? '' : 's'}`;
  });

  private searchTimer: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    // A link from Providers ("View models for …") arrives as `?provider=<id>`;
    // the filter starts on that provider so the page lands scoped.
    const filter = this.route.snapshot.queryParamMap.get('provider');
    if (filter !== null && filter !== '') {
      this.providerId.set(filter);
    }
    // The price basis: an explicit `?mix=` wins, then the stored choice, then
    // the resolved default profile (D34).
    this.priceBasis.set(this.initialBasis());
    this.basisExplicit.set(
      this.route.snapshot.queryParamMap.get('mix') !== null || this.storedBasis() !== null,
    ); // Non-admins never call the API: it would answer 403 and the read is audited.
    // A `?q=` deep link (or reload/back) restores the applied search as-is.
    const term = this.route.snapshot.queryParamMap.get('q')?.trim() ?? '';
    this.searchInput.set(term);
    this.searchTerm.set(term);
    this.maxPriceInput.set(this.route.snapshot.queryParamMap.get('max_price')?.trim() ?? '');
    this.minQualityInput.set(this.route.snapshot.queryParamMap.get('min_quality')?.trim() ?? '');
    this.syncDetailsOpen.set(this.storedSyncDetails());
    this.destroyRef.onDestroy(() => {
      if (this.searchTimer !== null) {
        clearTimeout(this.searchTimer);
      }
    });
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
      const [providers, profiles, pricing] = await Promise.all([
        this.catalog.listProviders(),
        this.mix.listProfiles(),
        this.mix.pricingSettings(),
      ]);
      this.providers.set(providers);
      this.profiles.set(profiles);
      this.pricing.set(pricing);
      this.canonicaliseBasis(profiles);
      await this.loadOverviewData();
      await this.loadSyncStatuses();
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected async loadOverview(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      await this.loadOverviewData();
      this.detailOpen.set(false);
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  /**
   * Fetches the overview with the active search and provider filter. While a
   * search is active it also fetches the unfiltered rows once, so the header
   * can say "N of M models" without the response gaining a count field.
   */
  private async loadOverviewData(): Promise<void> {
    const provider = this.selectedProvider();
    const term = this.searchTerm();
    const mix = this.mixQuery();
    const [overview, unfiltered] = await Promise.all([
      this.catalog.modelsOverview(provider, mix, term),
      term === '' ? Promise.resolve(null) : this.catalog.modelsOverview(provider, mix, null),
    ]);
    this.adoptOverviewProfile(overview);
    this.overview.set(overview);
    this.totalModels.set((unfiltered ?? overview).rows.length);
  }

  /** Changing the filter switches the payload's scope (all ↔ one provider). */
  protected onProviderFilter(value: string): void {
    if (value === this.providerId()) {
      return;
    }
    this.providerId.set(value);
    this.writeUrlParam('provider', value === 'all' ? null : value);
    this.notice.set(null);
    this.actionError.set(null);
    void this.loadOverview();
  }

  // --- search --------------------------------------------------------------

  /** Debounces typing so one pause produces at most one API call. */
  protected onSearchInput(value: string): void {
    this.searchInput.set(value);
    if (this.searchTimer !== null) {
      clearTimeout(this.searchTimer);
    }
    this.searchTimer = setTimeout(() => {
      this.searchTimer = null;
      void this.applySearch(this.searchInput());
    }, SEARCH_DEBOUNCE_MS);
  }

  /** Applies the trimmed term immediately and mirrors it into the URL. */
  private async applySearch(raw: string): Promise<void> {
    const term = raw.trim();
    if (term === this.searchTerm()) {
      return;
    }
    this.searchTerm.set(term);
    this.writeUrlParam('q', term === '' ? null : term);
    await this.loadOverview();
  }

  /** Clears the search box; the provider and the limits stay. */
  protected clearSearch(): void {
    if (this.searchInput() === '' && this.searchTerm() === '') {
      return;
    }
    this.resetSearch();
    void this.loadOverview();
  }

  // --- price and quality refinements --------------------------------------

  /** Narrows by the shown price column; the API payload stays whole. */
  protected onMaxPriceInput(value: string): void {
    this.maxPriceInput.set(value);
    this.writeUrlParam('max_price', value.trim() === '' ? null : value.trim());
  }

  protected onMinQualityInput(value: string): void {
    this.minQualityInput.set(value);
    this.writeUrlParam('min_quality', value.trim() === '' ? null : value.trim());
  }

  /** Clears the price and quality caps without touching the search. */
  protected clearRefinements(): void {
    this.maxPriceInput.set('');
    this.minQualityInput.set('');
    this.writeUrlParam('max_price', null);
    this.writeUrlParam('min_quality', null);
  }

  /** Clears the provider, the search, and the limits, then reloads once. */
  protected clearAllFilters(): void {
    this.providerId.set('all');
    this.writeUrlParam('provider', null);
    this.clearRefinements();
    this.resetSearch();
    void this.loadOverview();
  }

  /** Empties the search box and its term without touching the list. */
  private resetSearch(): void {
    if (this.searchTimer !== null) {
      clearTimeout(this.searchTimer);
      this.searchTimer = null;
    }
    this.searchInput.set('');
    this.searchTerm.set('');
    this.writeUrlParam('q', null);
  }

  /**
   * True when a row satisfies both caps. An unknown value never passes a cap:
   * an unpriced or unranked row is not "cheap" or "low quality", it is unknown.
   */
  private matchesRefinements(row: OverviewRow): boolean {
    const maxPrice = this.maxPrice();
    if (maxPrice !== null) {
      const price = this.shownPrice(row);
      if (price === null || price > maxPrice) {
        return false;
      }
    }
    const minQuality = this.minQuality();
    if (minQuality !== null) {
      const score = row.quality?.score ?? null;
      if (score === null || score < minQuality) {
        return false;
      }
    }
    return true;
  }

  /** The number the table shows in its price column: mix or input. */
  private shownPrice(row: OverviewRow): number | null {
    return this.mixSelected() ? (row.mix_price?.per_m ?? null) : this.inputPrice(row);
  }

  /** `/` focuses the search box unless a field or a dialog already has focus. */
  protected onDocumentKeydown(event: KeyboardEvent): void {
    if (event.key !== '/' || event.ctrlKey || event.metaKey || event.altKey) {
      return;
    }
    const target = event.target;
    if (target instanceof HTMLElement) {
      const tag = target.tagName.toLowerCase();
      if (tag === 'input' || tag === 'textarea' || tag === 'select' || target.isContentEditable) {
        return;
      }
    }
    const input = this.searchControl()?.nativeElement.querySelector('input');
    if (!(input instanceof HTMLInputElement)) {
      return;
    }
    event.preventDefault();
    input.focus();
  }

  // --- price basis (D34) ---------------------------------------------------

  /** Switches between the component columns and one profile's mix price. */
  protected onBasisChange(value: string): void {
    if (value === '' || value === this.priceBasis()) {
      return;
    }
    this.priceBasis.set(value);
    this.basisExplicit.set(true);
    // The old sort key may name a column that is no longer visible.
    if (value === COMPONENTS_BASIS) {
      if (this.sortKey() === 'mix') {
        this.sortKey.set('name');
      }
    } else if (this.sortKey() !== 'mix') {
      this.sortKey.set('mix');
    }
    this.persistBasis(value);
    this.syncUrl(value);
    this.notice.set(null);
    this.actionError.set(null);
    void this.loadOverview();
  }

  protected profileOptionLabel(profile: MixProfile): string {
    const suffix = profile.id === this.effectiveProfileId() ? ' · default' : '';
    return `${profile.name} · ${profileSummary(profile)}%${suffix}`;
  }

  protected openProfiles(): void {
    this.actionError.set(null);
    this.notice.set(null);
    this.profilesOpen.set(true);
  }

  protected closeProfiles(): void {
    this.profilesOpen.set(false);
  }

  /** A profile or the default changed: reload the list and the overview. */
  protected async onProfilesChanged(): Promise<void> {
    try {
      const [profiles, pricing] = await Promise.all([
        this.mix.listProfiles(),
        this.mix.pricingSettings(),
      ]);
      this.profiles.set(profiles);
      this.pricing.set(pricing);
      // Nobody chose a concrete basis: follow the (possibly new) default.
      if (!this.basisExplicit()) {
        this.priceBasis.set(DEFAULT_BASIS);
      }
      this.canonicaliseBasis(profiles);
      await this.loadOverview();
    } catch (err) {
      this.actionError.set(describeError(err, 'Reloading the mix profiles failed.'));
    }
  }

  private initialBasis(): string {
    const fromUrl = this.route.snapshot.queryParamMap.get('mix');
    if (fromUrl !== null && fromUrl !== '') {
      return fromUrl;
    }
    return this.storedBasis() ?? DEFAULT_BASIS;
  }

  private storedBasis(): string | null {
    try {
      return window.localStorage.getItem(PRICE_BASIS_STORAGE_KEY);
    } catch {
      return null;
    }
  }

  private persistBasis(value: string): void {
    try {
      window.localStorage.setItem(PRICE_BASIS_STORAGE_KEY, value);
    } catch {
      // A blocked localStorage only costs persistence, never the page.
    }
  }

  /** Writes `?mix=` while keeping the provider filter, search, and path. */
  private syncUrl(value: string): void {
    this.writeUrlParam('mix', value);
  }

  /**
   * Sets, replaces, or removes one query parameter and keeps the rest. Used
   * for `q`, `provider`, and `mix` so a reload or Back restores page state.
   */
  private writeUrlParam(key: string, value: string | null): void {
    const [pathname, query = ''] = this.location.path().split('?');
    const params = new URLSearchParams(query);
    if (value === null || value === '') {
      params.delete(key);
    } else {
      params.set(key, value);
    }
    this.location.replaceState(pathname === '' ? '/' : pathname, params.toString());
  }

  /**
   * A URL/stored basis may name a deleted profile or use a name instead of an
   * id; canonicalise it to an id before the overview is fetched.
   */
  private canonicaliseBasis(profiles: readonly MixProfile[]): void {
    const basis = this.priceBasis();
    if (basis === COMPONENTS_BASIS || basis === DEFAULT_BASIS) {
      return;
    }
    const match = profiles.find((profile) => profile.id === basis || profile.name === basis);
    if (match !== undefined) {
      if (match.id !== basis) {
        this.priceBasis.set(match.id);
      }
      return;
    }
    this.priceBasis.set(DEFAULT_BASIS);
    this.basisExplicit.set(false);
    this.persistBasis(DEFAULT_BASIS);
  }

  /** The server resolves the default; only an explicit profile is sent. */
  private mixQuery(): string | null {
    const basis = this.priceBasis();
    return basis === COMPONENTS_BASIS || basis === DEFAULT_BASIS ? null : basis;
  }

  /** The default is echoed as a canonical id by the overview response. */
  private adoptOverviewProfile(overview: ModelsOverview): void {
    if (this.priceBasis() !== DEFAULT_BASIS) {
      return;
    }
    const id = overview.mix?.profile_id;
    if (id !== undefined && id !== '') {
      this.priceBasis.set(id);
    }
  }

  // --- table sorting -------------------------------------------------------

  protected setSort(key: SortKey): void {
    if (this.sortKey() === key) {
      this.sortDirection.update((direction) => (direction === 'asc' ? 'desc' : 'asc'));
      return;
    }
    this.sortKey.set(key);
    this.sortDirection.set('asc');
  }

  protected sortAria(key: SortKey): 'ascending' | 'descending' | 'none' {
    if (this.sortKey() !== key) {
      return 'none';
    }
    return this.sortDirection() === 'asc' ? 'ascending' : 'descending';
  }

  protected sortIndicator(key: SortKey): string {
    if (this.sortKey() !== key) {
      return '';
    }
    return this.sortDirection() === 'asc' ? '▲' : '▼';
  }

  private compareRows(a: OverviewRow, b: OverviewRow): number {
    const primary = this.comparePrimary(a, b);
    if (primary !== 0) {
      return primary;
    }
    // Deterministic tie-break independent of the sort direction.
    const name = a.canonical_name.localeCompare(b.canonical_name);
    return name !== 0 ? name : overviewRowKey(a).localeCompare(overviewRowKey(b));
  }

  /**
   * Compares the active column. Numeric columns sort direction-aware, but a
   * missing value (unpriced, variable price, unknown context) always sorts
   * last: it is "unknown", not a very small number, so it must never look
   * cheapest in the ascending cheap end (or most expensive on the flip).
   */
  private comparePrimary(a: OverviewRow, b: OverviewRow): number {
    const key = this.sortKey();
    if (key === 'name') {
      return a.canonical_name.localeCompare(b.canonical_name);
    }
    const aValue = sortableValue(a, key);
    const bValue = sortableValue(b, key);
    if (aValue === null || bValue === null) {
      if (aValue === null && bValue === null) {
        return 0;
      }
      return aValue === null ? 1 : -1;
    }
    const direction = this.sortDirection() === 'asc' ? 1 : -1;
    return (aValue - bValue) * direction;
  }

  // --- row display ---------------------------------------------------------

  /** Every provider behind a row; the provider scope has exactly one. */
  private rowProviders(row: OverviewRow): OverviewProvider[] {
    if (!isProviderRow(row)) {
      // A gateway older than this build sends no provider list; render the
      // row without chips instead of failing the page during an upgrade.
      return row.providers ?? [];
    }
    const provider = this.providerById().get(this.overview()?.provider_id ?? '');
    return provider === undefined ? [] : [{ id: provider.id, name: provider.name }];
  }

  /** Chips to render: all providers, or the first few plus the "+N" chip. */
  protected visibleProviders(row: OverviewRow): OverviewProvider[] {
    const providers = this.rowProviders(row);
    if (providers.length <= MAX_PROVIDER_CHIPS || this.providerRowExpanded(row)) {
      return providers;
    }
    return providers.slice(0, MAX_PROVIDER_CHIPS);
  }

  /** Providers hidden behind the "+N" chip. */
  protected hiddenProviders(row: OverviewRow): OverviewProvider[] {
    const visible = this.visibleProviders(row).length;
    return this.rowProviders(row).slice(visible);
  }

  /** Tooltip naming the providers the "+N" chip hides. */
  protected hiddenChipTitle(row: OverviewRow): string {
    const hidden = this.hiddenProviders(row);
    return `Show ${hidden.length} more provider${hidden.length === 1 ? '' : 's'}: ${hidden
      .map((provider) => provider.name)
      .join(', ')}`;
  }

  protected providerRowExpanded(row: OverviewRow): boolean {
    return this.expandedProviderRows().has(this.rowKey(row));
  }

  /** Expands or collapses a row's provider chips without touching the filter. */
  protected toggleProviderChips(row: OverviewRow): void {
    const key = this.rowKey(row);
    this.expandedProviderRows.update((keys) => {
      const next = new Set(keys);
      if (!next.delete(key)) {
        next.add(key);
      }
      return next;
    });
  }

  protected statusTone(row: OverviewRow): BadgeTone {
    if (isProviderRow(row)) {
      return row.enabled && row.model_enabled ? 'success' : 'neutral';
    }
    return row.model_enabled && row.offerings_count > 0 ? 'success' : 'neutral';
  }

  protected statusLabel(row: OverviewRow): string {
    if (!row.model_enabled) {
      return 'disabled (all)';
    }
    if (isProviderRow(row)) {
      return row.enabled ? 'enabled' : 'disabled';
    }
    return `${row.offerings_count} offering${row.offerings_count === 1 ? '' : 's'}`;
  }

  /** Tooltip for a status badge whose model is disabled for every provider. */
  protected statusTitle(row: OverviewRow): string | null {
    return row.model_enabled
      ? null
      : 'This model is disabled for all providers. Enable it with All providers selected.';
  }

  protected rowUnpriced(row: OverviewRow): boolean {
    return isProviderRow(row) ? row.price === null : row.price_avg === null;
  }

  protected rowUnpricedCount(row: OverviewRow): number {
    if (isProviderRow(row)) {
      return row.price === null ? 1 : 0;
    }
    // A variable price makes the offering unpriced too; the "variable" badge
    // already says why, so do not print "unpriced" for the same offering.
    return Math.max(0, row.unpriced_offerings - row.variable_offerings);
  }

  /**
   * True when the row has no fixed price but its source publishes a variable
   * one (the OpenRouter `-1` sentinel). Such a row is unpriced for routing
   * and sorting, but "unpriced" would be misleading: it is priced, just not
   * knowable in advance.
   */
  protected rowVariable(row: OverviewRow): boolean {
    return isProviderRow(row)
      ? (row.price?.variable_price ?? false)
      : row.price_avg === null && row.variable_offerings > 0;
  }

  /** Offerings of the row whose source publishes a variable price. */
  protected rowVariableCount(row: OverviewRow): number {
    return isProviderRow(row)
      ? row.price?.variable_price === true
        ? 1
        : 0
      : row.variable_offerings;
  }

  /** Tooltip for the "variable" badge. */
  protected readonly variableTooltip =
    'The provider publishes a variable price (OpenRouter sends -1): no fixed number to rank on. ' +
    'Routing imputes the neutral price score; a combo price cap excludes it.';

  protected rowKey(row: OverviewRow): string {
    return overviewRowKey(row);
  }

  protected upstreamId(row: OverviewRow): string {
    return isProviderRow(row) ? row.upstream_model_id : '';
  }

  protected offeringEnabled(row: OverviewRow): boolean {
    // Effective state: a model disabled for all providers overrides the
    // offering's own flag, which is kept for when the model is enabled again.
    return isProviderRow(row) && row.enabled && row.model_enabled;
  }

  /** The per-provider switch is blocked while the model is disabled everywhere. */
  protected offeringToggleBlocked(row: OverviewRow): boolean {
    return isProviderRow(row) && !row.model_enabled;
  }

  /** Action-button label: the target is the offering or the whole model. */
  protected toggleModelLabel(row: OverviewRow): string {
    if (!isProviderRow(row)) {
      return row.model_enabled ? 'Disable all' : 'Enable all';
    }
    return row.enabled ? 'Disable' : 'Enable';
  }

  /** Tooltip explaining a blocked per-provider switch. */
  protected toggleModelTitle(row: OverviewRow): string | null {
    if (!isProviderRow(row)) {
      return row.model_enabled
        ? 'Disable this model for every provider, including providers added later'
        : 'Enable this model again for every provider that offers it';
    }
    return row.model_enabled
      ? `${row.enabled ? 'Disable' : 'Enable'} this provider's offering`
      : 'This model is disabled for all providers. Select All providers to enable it first.';
  }

  protected detailPrice(row: OverviewRow): OverviewPrice | null {
    return isProviderRow(row) ? row.price : null;
  }

  protected averagePrice(row: OverviewRow): AggregatePrice | null {
    return isProviderRow(row) ? null : row.price_avg;
  }

  protected minimumPrice(row: OverviewRow): AggregatePrice | null {
    return isProviderRow(row) ? null : row.price_min;
  }

  protected qualityLabel(quality: OverviewQuality | null): string {
    if (quality === null) {
      return 'unknown';
    }
    if (quality.overridden || quality.source === 'override') {
      return 'override';
    }
    if (quality.source.startsWith('benchmark')) {
      return 'benchmark';
    }
    return 'default';
  }

  protected qualityTone(quality: OverviewQuality | null): BadgeTone {
    switch (this.qualityLabel(quality)) {
      case 'override':
        return 'warning';
      case 'benchmark':
        return 'info';
      default:
        return 'neutral';
    }
  }

  protected qualityScore(quality: OverviewQuality | null): string {
    return quality === null ? '—' : quality.score.toFixed(1);
  }

  protected modalityText(row: OverviewRow): string {
    const input = (row.modalities_input ?? []).join(', ') || '—';
    const output = (row.modalities_output ?? []).join(', ') || '—';
    return `${input} → ${output}`;
  }

  /** Input price value for the table/detail: per-offering or the average. */
  protected inputPrice(row: OverviewRow): number | null {
    return priceValue(row, 'input_per_m');
  }

  protected outputPrice(row: OverviewRow): number | null {
    return priceValue(row, 'output_per_m');
  }

  protected cachePrice(row: OverviewRow): number | null {
    return priceValue(row, 'cache_read_per_m');
  }

  /** The row's mix price (per-offering or the model average) in $/M. */
  protected mixPrice(row: OverviewRow): MixPriceView | null {
    return row.mix_price;
  }

  /** The minimum mix price across a model's offerings; provider scope has none. */
  protected mixPriceMin(row: OverviewRow): MixPriceView | null {
    return isProviderRow(row) ? null : row.mix_price_min;
  }

  protected componentLabel(component: keyof typeof MIX_COMPONENT_LABELS): string {
    return MIX_COMPONENT_LABELS[component];
  }

  protected basisTone(mix: MixPriceView): BadgeTone {
    return mix.basis === 'measured' ? 'success' : 'neutral';
  }

  protected shareLabel(share: number): string {
    return `${(share * 100).toFixed(1)}%`;
  }

  protected inheritedLabel(from: 'input' | 'output' | null): string {
    return from === null ? 'own price' : `${from} price`;
  }

  protected minPrice(
    row: OverviewRow,
    component: 'input_per_m' | 'output_per_m' | 'cache_read_per_m',
  ): number | null {
    return isProviderRow(row) ? null : (row.price_min?.[component] ?? null);
  }

  /**
   * True when a minimum differs from the shown average. A model with one
   * priced offering would otherwise repeat the same number twice in one cell.
   */
  protected minimumAddsInformation(shown: number | null, minimum: number | null): boolean {
    return shown !== null && minimum !== null && Math.abs(shown - minimum) > 1e-9;
  }

  protected detailProviderName(): string {
    const id = this.overview()?.provider_id;
    return id === null || id === undefined ? '' : (this.providerById().get(id)?.name ?? id);
  }

  /** The last price-sync run, or null before the first one. */
  protected priceRun(): SyncRun | null {
    return this.syncStatus()?.last_run ?? null;
  }

  private storedSyncDetails(): boolean {
    try {
      return window.localStorage.getItem(SYNC_DETAILS_STORAGE_KEY) === 'open';
    } catch {
      return false;
    }
  }

  /** Expands or collapses the sync rows and remembers the choice. */
  protected toggleSyncDetails(): void {
    const open = !this.syncDetailsOpen();
    this.syncDetailsOpen.set(open);
    try {
      window.localStorage.setItem(SYNC_DETAILS_STORAGE_KEY, open ? 'open' : 'closed');
    } catch {
      // A blocked localStorage only costs persistence, never the page.
    }
  }

  /**
   * The worst state across the benchmark sources: an error outranks a stale
   * source, which outranks a source that never ran.
   */
  protected benchmarkSummaryLabel(): string {
    const runs = this.benchmarkRuns();
    const errors = runs.filter((run) => run.status === 'error').length;
    if (errors > 0) {
      return `${errors} error${errors === 1 ? '' : 's'}`;
    }
    if (runs.some((run) => run.fresh === false && run.status !== 'never')) {
      return 'stale';
    }
    if (runs.every((run) => run.status === 'never')) {
      return 'never';
    }
    return 'success';
  }

  protected benchmarkSummaryTone(): BadgeTone {
    switch (this.benchmarkSummaryLabel()) {
      case 'stale':
        return 'warning';
      case 'never':
        return 'neutral';
      case 'success':
        return 'success';
      default:
        return 'danger';
    }
  }

  /**
   * The summary line beside the benchmark badge: the failing or stale sources
   * by name, the source count, and the newest run's age. Naming the problem
   * sources keeps a partial failure visible while the details are collapsed.
   */
  protected benchmarkSummaryMeta(): string {
    const runs = this.benchmarkRuns();
    const parts: string[] = [];
    const problems = runs
      .filter((run) => run.status === 'error' || (run.fresh === false && run.status !== 'never'))
      .map((run) => run.source);
    if (problems.length > 0) {
      parts.push(problems.join(', '));
    }
    parts.push(`${runs.length} source${runs.length === 1 ? '' : 's'}`);
    const finished = runs
      .map((run) => run.finished_at)
      .filter((value): value is string => value !== null);
    if (finished.length > 0) {
      const latest = finished.reduce((newest, value) => (value > newest ? value : newest));
      parts.push(`latest ${formatAge(latest)}`);
    }
    return parts.join(' · ');
  }

  protected benchmarkRuns(): readonly BenchmarkSyncRun[] {
    return this.benchmarkStatus()?.sources ?? [];
  }

  protected syncTone(status: string): BadgeTone {
    if (status === 'success') {
      return 'success';
    }
    if (status === 'error') {
      return 'danger';
    }
    return status === 'never' ? 'neutral' : 'warning';
  }

  /**
   * The numbers a benchmark run's sync row shows: the matched ratio, then only
   * the non-zero changes and skips, so a healthy run stays short.
   */
  protected benchmarkMetrics(run: BenchmarkSyncRun): SyncMetric[] {
    const counts = run.counts ?? {};
    const metrics: SyncMetric[] = [];
    const matched = counts['matched'];
    const published = counts['published'];
    if (matched !== undefined) {
      metrics.push({
        value: published === undefined ? String(matched) : `${matched}/${published}`,
        label: 'matched',
      });
    } else if (published !== undefined) {
      metrics.push({ value: String(published), label: 'published' });
    }
    const optional: ReadonlyArray<[string, string]> = [
      ['inserted', 'new'],
      ['updated', 'updated'],
      ['skipped_unknown', 'unknown'],
      ['ambiguous', 'ambiguous'],
      ['failed_pages', 'failed pages'],
    ];
    for (const [key, label] of optional) {
      const value = counts[key];
      if (value !== undefined && value !== 0) {
        metrics.push({ value: String(value), label });
      }
    }
    return metrics;
  }

  // --- detail dialog -------------------------------------------------------

  protected openDetail(row: OverviewRow): void {
    this.detailKey.set(overviewRowKey(row));
    this.detailOpen.set(true);
    this.actionError.set(null);
    this.notice.set(null);
    this.priceError.set(null);
    this.overrideError.set(null);
    this.offeringError.set(null);
    this.removeOfferingOpen.set(false);
    this.overrideDraft.set(row.quality?.score.toString() ?? '');
    this.priceDraft.set(priceDraftOf(isProviderRow(row) ? row.price : null));
  }

  protected closeDetail(): void {
    this.detailOpen.set(false);
    this.detailKey.set(null);
    this.removeOfferingOpen.set(false);
  }

  protected setModelValue(field: ModelField, value: string): void {
    this.modelDraft.update((draft) => ({ ...draft, [field]: value }));
  }

  protected openModelForm(): void {
    this.modelDraft.set(emptyModelDraft());
    this.modelAttempted.set(false);
    this.actionError.set(null);
    this.notice.set(null);
    this.modelFormOpen.set(true);
  }

  protected closeModelForm(): void {
    this.modelFormOpen.set(false);
  }

  protected async saveModel(): Promise<void> {
    this.modelAttempted.set(true);
    this.actionError.set(null);
    if (Object.keys(validateModel(this.modelDraft(), true)).length > 0) {
      return;
    }
    this.savingModel.set(true);
    try {
      await this.catalog.createModel(this.modelInput());
      this.modelFormOpen.set(false);
      this.notice.set('Model created.');
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the model failed.'));
    } finally {
      this.savingModel.set(false);
    }
  }

  protected setPriceValue(field: PriceField, value: string): void {
    this.priceDraft.update((draft) => ({ ...draft, [field]: value }));
  }

  protected async savePrice(): Promise<void> {
    const row = this.detailRow();
    if (row === null || !isProviderRow(row)) {
      return;
    }
    this.priceError.set(null);
    this.actionError.set(null);
    const result = buildPriceInput(this.priceDraft(), row.price);
    if ('error' in result) {
      this.priceError.set(result.error);
      return;
    }
    this.savingPrice.set(true);
    try {
      await this.catalog.setPrice(row.offering_id, result.input);
      this.notice.set('Price saved as manual. Sync will no longer overwrite it.');
      await this.reload();
    } catch (err) {
      // The server's 400 message names the rejected field; show it verbatim.
      this.priceError.set(describeError(err, 'Saving the price failed.'));
    } finally {
      this.savingPrice.set(false);
    }
  }

  protected async toggleEnabled(): Promise<void> {
    const row = this.detailRow();
    if (row === null || !isProviderRow(row) || !row.model_enabled || this.togglingEnabled()) {
      return;
    }
    this.togglingEnabled.set(true);
    this.actionError.set(null);
    try {
      const patch: OfferingPatch = { enabled: !row.enabled };
      await this.catalog.updateOffering(row.offering_id, patch);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Changing the offering failed.'));
    } finally {
      this.togglingEnabled.set(false);
    }
  }

  /**
   * Enables or disables a row from the list. With a provider selected the
   * switch targets that provider's offering; with All providers selected it
   * targets the model itself, so providers added later stay disabled too
   * (D61). A success banner states the scope of the change.
   */
  protected async toggleModelEnabled(row: OverviewRow): Promise<void> {
    if (this.togglingModelKey() !== null) {
      return;
    }
    if (isProviderRow(row)) {
      if (!row.model_enabled) {
        this.notice.set(
          `${row.canonical_name} is disabled for all providers. Select All providers and enable it there first.`,
        );
        return;
      }
      this.togglingModelKey.set(this.rowKey(row));
      this.actionError.set(null);
      this.notice.set(null);
      try {
        await this.catalog.updateOffering(row.offering_id, { enabled: !row.enabled });
        const provider = this.providerById().get(row.provider_id)?.name ?? 'this provider';
        this.notice.set(
          row.enabled
            ? `${row.canonical_name} disabled for ${provider}.`
            : `${row.canonical_name} enabled for ${provider}.`,
        );
        await this.reload();
      } catch (err) {
        this.actionError.set(describeError(err, 'Changing the offering failed.'));
      } finally {
        this.togglingModelKey.set(null);
      }
      return;
    }

    const enable = !row.model_enabled;
    this.togglingModelKey.set(this.rowKey(row));
    this.actionError.set(null);
    this.notice.set(null);
    try {
      await this.catalog.updateModel(row.model_id, { enabled: enable });
      const providers = row.providers_count;
      this.notice.set(
        enable
          ? `${row.canonical_name} enabled again for every provider; each provider's own offering setting applies.`
          : `${row.canonical_name} disabled for all ${providers} provider${
              providers === 1 ? '' : 's'
            }. New offerings of this model stay disabled until it is enabled again.`,
      );
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Changing the model failed.'));
    } finally {
      this.togglingModelKey.set(null);
    }
  }

  protected async saveOverride(): Promise<void> {
    const row = this.detailRow();
    const score = Number(this.overrideDraft());
    if (row === null || this.overrideDraft().trim() === '' || !Number.isFinite(score)) {
      this.overrideError.set('Enter a numeric score first.');
      return;
    }
    this.savingOverride.set(true);
    this.overrideError.set(null);
    try {
      await this.routing.setQualityOverride(row.model_id, score);
      this.notice.set('Quality override saved.');
      await this.reload();
    } catch (err) {
      // The scale check names the accepted range; show that message as-is.
      this.overrideError.set(describeError(err, 'Saving the override failed.'));
    } finally {
      this.savingOverride.set(false);
    }
  }

  protected async removeOverride(): Promise<void> {
    const row = this.detailRow();
    if (row === null) {
      return;
    }
    this.removingOverride.set(true);
    this.overrideError.set(null);
    try {
      await this.routing.clearQualityOverride(row.model_id);
      this.notice.set('Quality override removed.');
      await this.reload();
    } catch (err) {
      this.overrideError.set(describeError(err, 'Removing the override failed.'));
    } finally {
      this.removingOverride.set(false);
    }
  }

  // --- offering management (provider scope) --------------------------------

  /** Opens Add offering for the provider scope and loads the model picker. */
  protected async openAddOffering(): Promise<void> {
    this.offeringDraft.set(emptyOfferingDraft());
    this.offeringHeaderRows.set([]);
    this.offeringHeadersTouched.set(false);
    this.offeringAttempted.set(false);
    this.offeringError.set(null);
    this.notice.set(null);
    this.offeringOpen.set(true);
    try {
      const models = await this.catalog.listModels();
      this.offeringModels.set(models);
      if (this.offeringDraft().model_id === '' && models.length > 0) {
        this.offeringDraft.update((draft) => ({ ...draft, model_id: models[0].id }));
      }
    } catch (err) {
      this.offeringError.set(describeError(err, 'Loading models failed.'));
    }
  }

  protected closeAddOffering(): void {
    this.offeringOpen.set(false);
  }

  protected setOfferingDraft(
    field: 'model_id' | 'upstream_model_id' | 'context_override' | 'max_output_override',
    value: string,
  ): void {
    this.offeringDraft.update((draft) => ({ ...draft, [field]: value }));
  }

  protected setOfferingEnabled(value: boolean): void {
    this.offeringDraft.update((draft) => ({ ...draft, enabled: value }));
  }

  protected onOfferingHeaderRows(rows: HeaderRow[]): void {
    this.offeringHeaderRows.set(rows);
    this.offeringHeadersTouched.set(true);
  }

  protected async saveOffering(): Promise<void> {
    this.offeringAttempted.set(true);
    this.offeringError.set(null);
    const providerId = this.overview()?.provider_id;
    if (
      providerId === null ||
      providerId === undefined ||
      Object.keys(validateOffering(this.offeringDraft(), true)).length > 0
    ) {
      return;
    }
    const draft = this.offeringDraft();
    const input: OfferingInput = {
      provider_id: providerId,
      model_id: draft.model_id,
      upstream_model_id: draft.upstream_model_id.trim(),
      context_override: parsePositiveInteger(draft.context_override),
      max_output_override: parsePositiveInteger(draft.max_output_override),
      enabled: draft.enabled,
      headers: this.offeringHeadersTouched() ? headerMapOf(this.offeringHeaderRows()) : null,
    };
    this.savingOffering.set(true);
    try {
      await this.catalog.createOffering(input);
      this.offeringOpen.set(false);
      this.notice.set('Offering created.');
      await this.reload();
    } catch (err) {
      this.offeringError.set(describeError(err, 'Saving the offering failed.'));
    } finally {
      this.savingOffering.set(false);
    }
  }

  protected askRemoveOffering(): void {
    this.removeOfferingOpen.set(true);
  }

  protected closeRemoveOffering(): void {
    this.removeOfferingOpen.set(false);
  }

  protected async confirmRemoveOffering(): Promise<void> {
    const row = this.detailRow();
    if (row === null || !isProviderRow(row)) {
      return;
    }
    this.removingOffering.set(true);
    this.actionError.set(null);
    try {
      await this.catalog.deleteOffering(row.offering_id);
      this.removeOfferingOpen.set(false);
      this.closeDetail();
      this.notice.set('Offering removed.');
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Removing the offering failed.'));
    } finally {
      this.removingOffering.set(false);
    }
  }

  // --- toolbar sync actions ------------------------------------------------

  protected async syncPrices(): Promise<void> {
    if (this.syncingPrices()) {
      return;
    }
    this.syncingPrices.set(true);
    this.actionError.set(null);
    this.notice.set(null);
    try {
      const outcome = await this.catalog.syncOpenRouter();
      if (outcome.status === 'error') {
        this.actionError.set(outcome.error ?? 'The price sync failed and no snapshot was kept.');
      } else if (outcome.status === 'stale-if-error') {
        this.actionError.set(
          `The price sync failed; the previous snapshot is being served.${
            outcome.error === null ? '' : ` ${outcome.error}`
          }`,
        );
      } else {
        this.notice.set(`Price sync finished: ${outcome.model_count} models imported.`);
      }
      await this.loadSyncStatuses();
      await this.loadOverviewQuietly();
    } catch (err) {
      this.actionError.set(describeError(err, 'The price sync failed to start.'));
    } finally {
      this.syncingPrices.set(false);
    }
  }

  protected async syncBenchmarks(): Promise<void> {
    if (this.syncingBenchmarks() || !this.benchmarkSyncAvailable()) {
      return;
    }
    this.syncingBenchmarks.set(true);
    this.actionError.set(null);
    this.notice.set(null);
    try {
      const response = await this.routing.syncBenchmarks({});
      const failed = response.results.filter((result) => result.status === 'error');
      const succeeded = response.results.filter((result) => result.status !== 'error');
      if (failed.length > 0) {
        const detail = failed
          .map((result) => `${result.source}: ${result.error ?? 'the sync failed'}`)
          .join('; ');
        this.actionError.set(
          succeeded.length > 0
            ? `Benchmark sync finished with errors. ${detail}`
            : `The benchmark sync failed. ${detail}`,
        );
      } else {
        this.notice.set(
          `Benchmark sync finished: ${succeeded.length} source${
            succeeded.length === 1 ? '' : 's'
          } updated.`,
        );
      }
      await this.loadSyncStatuses();
      await this.loadOverviewQuietly();
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        this.benchmarkSyncAvailable.set(false);
        this.actionError.set('Benchmark sync is not available in this build.');
      } else {
        this.actionError.set(describeError(err, 'The benchmark sync failed to start.'));
      }
    } finally {
      this.syncingBenchmarks.set(false);
    }
  }

  private async loadOverviewQuietly(): Promise<void> {
    try {
      await this.loadOverviewData();
    } catch {
      // A stale table is better than blanking the page after a successful sync.
    }
  }

  private async loadSyncStatuses(): Promise<void> {
    const [price, benchmarks] = await Promise.all([
      this.catalog.syncStatus().catch(() => null),
      this.routing.benchmarkSyncStatus().catch(() => null),
    ]);
    this.syncStatus.set(price);
    this.benchmarkStatus.set(benchmarks);
    this.benchmarkSyncAvailable.set(benchmarks !== null);
  }

  private selectedProvider(): string | null {
    return this.providerId() === 'all' ? null : this.providerId();
  }

  private modelInput(): ModelInput {
    const draft = this.modelDraft();
    return {
      canonical_name: draft.canonical_name.trim(),
      family: draft.family.trim() === '' ? null : draft.family.trim(),
      modalities_input: parseTags(draft.modalities_input),
      modalities_output: parseTags(draft.modalities_output),
      context_length: parsePositiveInteger(draft.context_length),
      max_output_tokens: parsePositiveInteger(draft.max_output_tokens),
      capabilities: parseTags(draft.capabilities),
    };
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading models failed.'));
  }
}

/** An empty or non-numeric filter box means "no cap". */
function parseOptionalNumber(raw: string): number | null {
  const trimmed = raw.trim();
  if (trimmed === '') {
    return null;
  }
  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}

function priceValue(
  row: OverviewRow,
  component: 'input_per_m' | 'output_per_m' | 'cache_read_per_m',
): number | null {
  return isProviderRow(row)
    ? (row.price?.[component] ?? null)
    : (row.price_avg?.[component] ?? null);
}

/**
 * The component prices the mix block blends: one offering's full row, or the
 * model's averages in the all-providers scope (which carry no cache-write or
 * reasoning component, so a profile that uses those shares cannot blend).
 */
function componentPricesOf(row: OverviewRow): MixComponentPrices {
  if (isProviderRow(row)) {
    return {
      input_per_m: row.price?.input_per_m ?? null,
      output_per_m: row.price?.output_per_m ?? null,
      cache_read_per_m: row.price?.cache_read_per_m ?? null,
      cache_write_per_m: row.price?.cache_write_per_m ?? null,
      reasoning_per_m: row.price?.reasoning_per_m ?? null,
    };
  }
  return {
    input_per_m: row.price_avg?.input_per_m ?? null,
    output_per_m: row.price_avg?.output_per_m ?? null,
    cache_read_per_m: row.price_avg?.cache_read_per_m ?? null,
    cache_write_per_m: null,
    reasoning_per_m: null,
  };
}

/** The numeric value behind one sortable column; null means unknown. */
function sortableValue(row: OverviewRow, key: Exclude<SortKey, 'name'>): number | null {
  switch (key) {
    case 'context':
      return row.context_length;
    case 'mix':
      return row.mix_price?.per_m ?? null;
    case 'input':
      return priceValue(row, 'input_per_m');
    case 'output':
      return priceValue(row, 'output_per_m');
    case 'cache':
      return priceValue(row, 'cache_read_per_m');
    case 'quality':
      return row.quality?.score ?? null;
  }
}

function validateModel(draft: ModelDraft, attempted: boolean): Partial<Record<ModelField, string>> {
  const errors: Partial<Record<ModelField, string>> = {};
  if (!attempted) {
    return errors;
  }
  if (draft.canonical_name.trim() === '') {
    errors.canonical_name = 'Canonical name is required.';
  }
  for (const field of ['context_length', 'max_output_tokens'] as const) {
    const value = draft[field].trim();
    if (value !== '' && (!/^\d+$/.test(value) || Number(value) < 1)) {
      errors[field] = 'Use a whole number of 1 or more.';
    }
  }
  return errors;
}

/** Validates the Add offering form; overrides stay optional. */
function validateOffering(
  draft: OfferingDraft,
  attempted: boolean,
): Partial<
  Record<'model_id' | 'upstream_model_id' | 'context_override' | 'max_output_override', string>
> {
  const errors: Partial<
    Record<'model_id' | 'upstream_model_id' | 'context_override' | 'max_output_override', string>
  > = {};
  if (!attempted) {
    return errors;
  }
  if (draft.model_id === '') {
    errors.model_id = 'Pick a model.';
  }
  if (draft.upstream_model_id.trim() === '') {
    errors.upstream_model_id = 'Upstream model id is required.';
  }
  for (const field of ['context_override', 'max_output_override'] as const) {
    const value = draft[field].trim();
    if (value !== '' && (!/^\d+$/.test(value) || Number(value) < 1)) {
      errors[field] = 'Use a whole number of 1 or more.';
    }
  }
  return errors;
}

function parsePositiveInteger(value: string): number | null {
  const trimmed = value.trim();
  return trimmed === '' ? null : Number(trimmed);
}

function priceDraftOf(price: { [K in PriceField]?: string | number | null } | null): PriceDraft {
  const draft = emptyPriceDraft();
  if (price === null) {
    return draft;
  }
  for (const key of PRICE_KEYS) {
    const value = price[key];
    draft[key] = value === null || value === undefined ? '' : String(value);
  }
  draft.currency =
    typeof price.currency === 'string' && price.currency !== '' ? price.currency : 'USD';
  return draft;
}

/**
 * Builds the `PUT price` body. Blank fields keep the stored value (D20), and
 * every emitted amount is a concrete number, so an all-blank first save asks
 * for input locally instead of sending the server's all-null 400.
 */
function buildPriceInput(
  draft: PriceDraft,
  current: { [K in PriceField]?: string | number | null } | null,
): { input: PriceInput } | { error: string } {
  const input: PriceInput = {
    input_per_m: null,
    output_per_m: null,
    cache_read_per_m: null,
    cache_write_per_m: null,
    reasoning_per_m: null,
    request_fee: null,
  };
  for (const field of PRICE_FIELDS) {
    const raw = draft[field.key].trim();
    if (raw === '') {
      const stored = current?.[field.key];
      input[field.key] = typeof stored === 'number' ? stored : null;
      continue;
    }
    const value = Number(raw);
    if (!Number.isFinite(value) || value < 0) {
      return { error: `${field.label} must be a non-negative number.` };
    }
    input[field.key] = value;
  }
  if (PRICE_KEYS.every((key) => input[key] === null)) {
    return { error: 'Enter at least one price component; unpriced offerings stay unpriced.' };
  }
  const currency = draft.currency.trim();
  const storedCurrency = current?.currency;
  if (currency !== '') {
    input.currency = currency;
  } else if (typeof storedCurrency === 'string' && storedCurrency !== '') {
    input.currency = storedCurrency;
  }
  return { input };
}
