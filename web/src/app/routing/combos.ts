import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from '../catalog/catalog.service';
import type { Model, Offering, Provider } from '../catalog/catalog.types';
import { describeError, isForbidden } from '../catalog/errors';
import { MixService } from '../catalog/mix.service';
import type { MixProfile } from '../catalog/mix.types';
import { UiBadge, type BadgeTone } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiCard } from '../ui/card';
import { type ComboboxOption, UiCombobox } from '../ui/combobox';
import { UiDialog } from '../ui/dialog';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';
import { UiSelect } from '../ui/select';
import { UiSwitch } from '../ui/switch';
import { UiTable } from '../ui/table';
import { type TabItem, UiTabs } from '../ui/tabs';
import { ComboPreviewPanel } from './combo-preview';
import { CombosService } from './combos.service';
import type {
  Combo,
  ComboInput,
  ComboPool,
  ComboPreview,
  ComboStrategy,
  ComboWeights,
  ComboWeightsInput,
  PoolSort,
  ProviderSelection,
  StickinessKeySource,
  WeightComponent,
  WeightPreset,
} from './combos.types';

type FormState = { mode: 'create' } | { mode: 'edit'; combo: Combo };

/** The weights editor mode: a named preset or the four custom components. */
type WeightsMode = WeightPreset | 'custom';

/** One editable member row (an offering plus its score multiplier). */
interface MemberRow {
  offering_id: string;
  weight: number;
}

/** Everything the dialog edits, kept as plain signals. */
interface ComboDraft {
  name: string;
  strategy: ComboStrategy;
  weightsMode: WeightsMode;
  weights: ComboWeights;
  enabled: boolean;
  /** The pool source (D35): explicit members or the constrained catalog. */
  pool: ComboPool;
  /** Constraint inputs stay strings until saved; blank means unconstrained. */
  minQuality: string;
  maxMixPrice: string;
  /** A mix profile id, or '' for the global default. */
  mixProfile: string;
  /** The `pool = all` ordering axis (D64). */
  poolSort: PoolSort;
  /** How the pool picks among the providers of one model (D64). */
  providerSelection: ProviderSelection;
  stickinessEnabled: boolean;
  stickinessTtl: number;
  stickinessKeySource: StickinessKeySource;
  /** Sticky price tolerance percent, kept as typed until save (D64). */
  stickinessTolerance: string;
}

/** The four custom weight inputs, in form order. */
const WEIGHT_FIELDS: readonly WeightComponent[] = ['price', 'speed', 'quality', 'reliability'];

const WEIGHT_LABELS: Record<WeightComponent, string> = {
  price: 'Price',
  speed: 'Speed',
  quality: 'Quality',
  reliability: 'Reliability',
};

const STRATEGY_LABELS: Record<ComboStrategy, string> = {
  ordered: 'ordered',
  weighted: 'weighted',
  round_robin: 'round_robin',
};

const WEIGHTS_MODES: readonly WeightsMode[] = ['balanced', 'cheapest', 'fastest', 'best', 'custom'];

const DEFAULT_WEIGHTS: ComboWeights = { price: 0.25, speed: 0.3, quality: 0.3, reliability: 0.15 };

/** The advertised stickiness TTLs: 5, 30, 60, and 240 minutes. */
const STICKINESS_TTLS: ReadonlyArray<{ seconds: number; label: string }> = [
  { seconds: 300, label: '5 minutes' },
  { seconds: 1800, label: '30 minutes (default)' },
  { seconds: 3600, label: '60 minutes' },
  { seconds: 14400, label: '240 minutes' },
];

/** The server's own words for a catalog pool with no constraint. */
const UNCONSTRAINED_POOL_MESSAGE =
  'pool = all requires at least one constraint (min_quality or max_mix_price_per_m); an ' +
  'unconstrained catalog pool is almost always a mistake';

/** The server's own words for a non-weighted catalog pool. */
const CATALOG_POOL_STRATEGY_MESSAGE =
  'pool = all only supports the weighted strategy: ordered and round_robin need an explicit ' +
  'member list';

/** What a preview was computed from, so Refresh can recompute it. */
type PreviewSource = { kind: 'draft'; input: ComboInput } | { kind: 'saved'; combo: Combo };

/**
 * Combos: named offering pools addressable as `combo:<name>`, with the
 * strategy/weights editor and the ordered member picker. Members are
 * offerings; the stored order is the fallback chain for `ordered` and the
 * rotation for `round_robin`.
 */
@Component({
  selector: 'app-combos',
  imports: [
    RouterLink,
    ComboPreviewPanel,
    UiBadge,
    UiBanner,
    UiButton,
    UiCard,
    UiCombobox,
    UiDialog,
    UiEmpty,
    UiField,
    UiInput,
    UiSelect,
    UiSwitch,
    UiTable,
    UiTabs,
  ],
  templateUrl: './combos.html',
  styleUrl: './combos.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Combos {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);
  private readonly mix = inject(MixService);
  private readonly combosService = inject(CombosService);

  protected readonly combos = signal<Combo[]>([]);
  protected readonly providers = signal<Provider[]>([]);
  protected readonly models = signal<Model[]>([]);
  protected readonly offerings = signal<Offering[]>([]);
  protected readonly mixProfiles = signal<MixProfile[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly notice = signal<string | null>(null);
  protected readonly formState = signal<FormState | null>(null);
  protected readonly draft = signal<ComboDraft>(emptyDraft());
  protected readonly members = signal<MemberRow[]>([]);
  protected readonly memberPicker = signal('');
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly formError = signal<string | null>(null);
  protected readonly togglingId = signal<string | null>(null);
  protected readonly deleteTarget = signal<Combo | null>(null);
  protected readonly deleting = signal(false);
  protected readonly deleteError = signal<string | null>(null);

  // Routing preview (D35): the draft or a saved combo, from the runtime engine.
  protected readonly previewOpen = signal(false);
  protected readonly preview = signal<ComboPreview | null>(null);
  protected readonly previewLoading = signal(false);
  protected readonly previewError = signal<string | null>(null);
  protected readonly previewSource = signal<PreviewSource | null>(null);
  protected readonly previewTitle = signal('Routing preview');
  protected readonly previewSessionKey = signal('');

  protected readonly weightFields = WEIGHT_FIELDS;
  protected readonly weightLabels = WEIGHT_LABELS;
  protected readonly weightsModes = WEIGHTS_MODES;
  protected readonly strategies: readonly ComboStrategy[] = ['ordered', 'weighted', 'round_robin'];
  protected readonly strategyLabels = STRATEGY_LABELS;

  protected readonly poolTabs: readonly TabItem[] = [
    { id: 'members', label: 'Members list' },
    { id: 'all', label: 'All models with constraints' },
  ];

  private readonly providerById = computed(
    () => new Map(this.providers().map((provider) => [provider.id, provider])),
  );
  private readonly modelById = computed(
    () => new Map(this.models().map((model) => [model.id, model])),
  );

  /** Offerings not yet in the combo, for the member picker. */
  protected readonly addableOfferings = computed(() =>
    this.offerings().filter(
      (offering) => !this.members().some((member) => member.offering_id === offering.id),
    ),
  );

  /** Searchable options for the member picker (provider / model / upstream id). */
  protected readonly addableOfferingOptions = computed<ComboboxOption[]>(() =>
    this.addableOfferings().map((offering) => ({
      value: offering.id,
      label: this.offeringLabel(offering.id),
      searchText: offering.upstream_model_id,
    })),
  );

  protected readonly nameError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    return this.draft().name.trim() === '' ? 'This field is required.' : '';
  });

  protected readonly membersError = computed(() => {
    if (!this.attempted() || this.draft().pool !== 'members') {
      return '';
    }
    return this.members().length === 0 ? 'Add at least one member offering.' : '';
  });

  /**
   * Catalog-pool constraints (D35): at least one is required, and the values
   * must be usable. The blank message is the server's, so the UI never
   * invents a different rule.
   */
  protected readonly constraintError = computed(() => {
    if (this.draft().pool !== 'all') {
      return '';
    }
    const quality = this.draft().minQuality.trim();
    const price = this.draft().maxMixPrice.trim();
    if (quality === '' && price === '') {
      return this.attempted() ? UNCONSTRAINED_POOL_MESSAGE : '';
    }
    if (quality !== '' && (!Number.isFinite(Number(quality)) || Number(quality) < 0)) {
      return 'min_quality must be a finite, non-negative number';
    }
    if (price !== '' && (!Number.isFinite(Number(price)) || Number(price) <= 0)) {
      return 'max_mix_price_per_m must be a finite number greater than zero';
    }
    return '';
  });

  protected readonly catalogPool = computed(() => this.draft().pool === 'all');

  /** The sticky price tolerance must be a number in 0–100 (D64). */
  protected readonly stickinessToleranceError = computed(() => {
    if (!this.draft().stickinessEnabled) {
      return '';
    }
    const value = this.draft().stickinessTolerance.trim();
    if (value === '') {
      return '';
    }
    const parsed = Number(value);
    return Number.isFinite(parsed) && parsed >= 0 && parsed <= 100
      ? ''
      : 'Price tolerance must be a number between 0 and 100.';
  });

  /** The advertised TTLs, plus a stored custom value when one is in use. */
  protected readonly stickinessOptions = computed(() => {
    const ttl = this.draft().stickinessTtl;
    if (STICKINESS_TTLS.some((option) => option.seconds === ttl)) {
      return STICKINESS_TTLS;
    }
    return [
      ...STICKINESS_TTLS,
      { seconds: ttl, label: `${Math.round(ttl / 60)} minutes (stored)` },
    ];
  });

  protected readonly strategyRestriction = computed(() =>
    this.catalogPool() ? CATALOG_POOL_STRATEGY_MESSAGE : '',
  );

  protected readonly memberWeightsError = computed(() => {
    if (!this.attempted() || this.members().length === 0 || this.catalogPool()) {
      return '';
    }
    return this.memberWeightsInvalid() ? 'Member weights must be positive numbers.' : '';
  });

  protected readonly weightsError = computed(() => {
    if (!this.attempted() || this.draft().weightsMode !== 'custom') {
      return '';
    }
    return this.customWeightsInvalid() ? 'At least one weight must be above zero.' : '';
  });

  protected readonly formTitle = computed(() =>
    this.formState()?.mode === 'edit' ? 'Edit combo' : 'New combo',
  );

  protected readonly deleteTitle = computed(() => {
    const combo = this.deleteTarget();
    return combo === null ? 'Delete combo' : `Delete combo: ${combo.name}`;
  });

  protected readonly strategyHint = computed(() => {
    if (this.catalogPool()) {
      return CATALOG_POOL_STRATEGY_MESSAGE;
    }
    switch (this.draft().strategy) {
      case 'ordered':
        return 'Members are tried top to bottom — the stored order is the fallback chain.';
      case 'round_robin':
        return 'Requests rotate over the members, starting at the top of the stored order.';
      case 'weighted':
        return 'The pool is scored; a member weight multiplies its score.';
    }
  });

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
      const [combos, offerings, providers, models, mixProfiles] = await Promise.all([
        this.combosService.list(),
        this.catalog.listOfferings(),
        this.catalog.listProviders(),
        this.catalog.listModels(),
        this.mix.listProfiles(),
      ]);
      this.combos.set(combos);
      this.offerings.set(offerings);
      this.providers.set(providers);
      this.models.set(models);
      this.mixProfiles.set(mixProfiles);
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected offeringLabel(offeringId: string): string {
    const offering = this.offerings().find((candidate) => candidate.id === offeringId);
    if (offering === undefined) {
      return offeringId;
    }
    const provider = this.providerById().get(offering.provider_id)?.name ?? offering.provider_id;
    const model = this.modelById().get(offering.model_id)?.canonical_name ?? offering.model_id;
    return `${provider} / ${model} · ${offering.upstream_model_id}`;
  }

  // --- table display -------------------------------------------------------

  protected strategyTone(strategy: ComboStrategy): BadgeTone {
    switch (strategy) {
      case 'weighted':
        return 'accent';
      case 'ordered':
        return 'info';
      case 'round_robin':
        return 'neutral';
    }
  }

  /** A preset name, or `custom` for hand-set components. */
  protected weightsLabel(combo: Combo): string {
    return combo.preset ?? 'custom';
  }

  /** Presets are pure orders (D24); only weighted combos actually use weights. */
  protected weightsSummary(combo: Combo): string {
    if (combo.strategy !== 'weighted') {
      return 'order only — no weights';
    }
    const weights = combo.weights;
    return WEIGHT_FIELDS.map((field) => `${field} ${weights[field].toFixed(2)}`).join(' · ');
  }

  /** The constraints visible on a catalog-pool row (D35). */
  protected constraintSummary(combo: Combo): string {
    const parts: string[] = [];
    if (combo.min_quality !== null) {
      parts.push(`quality ≥ ${combo.min_quality}`);
    }
    if (combo.max_mix_price_per_m !== null) {
      parts.push(`mix ≤ $${combo.max_mix_price_per_m}/M`);
    }
    return parts.join(' · ') || 'no constraint';
  }

  /** A profile name for a combo's stored mix profile id. */
  protected mixProfileName(combo: Combo): string {
    if (combo.mix_profile === null) {
      return 'global default';
    }
    const profile = this.mixProfiles().find((candidate) => candidate.id === combo.mix_profile);
    return profile?.name ?? combo.mix_profile;
  }

  protected stickinessLabel(combo: Combo): string {
    if (!combo.stickiness.enabled) {
      return 'off';
    }
    return `pinned ${Math.round(combo.stickiness.ttl_seconds / 60)}m`;
  }

  // --- dialog --------------------------------------------------------------

  protected openCreate(): void {
    this.draft.set(emptyDraft());
    this.members.set([]);
    this.memberPicker.set('');
    this.attempted.set(false);
    this.formError.set(null);
    this.actionError.set(null);
    this.notice.set(null);
    this.formState.set({ mode: 'create' });
  }

  protected openEdit(combo: Combo): void {
    this.draft.set({
      name: combo.name,
      strategy: combo.strategy,
      // A stored custom weighting comes back without a preset name.
      weightsMode: combo.preset ?? 'custom',
      weights: { ...combo.weights },
      enabled: combo.enabled,
      pool: combo.pool,
      minQuality: combo.min_quality === null ? '' : String(combo.min_quality),
      maxMixPrice: combo.max_mix_price_per_m === null ? '' : String(combo.max_mix_price_per_m),
      mixProfile: combo.mix_profile ?? '',
      poolSort: combo.sort,
      providerSelection: combo.provider_selection,
      stickinessEnabled: combo.stickiness.enabled,
      stickinessTtl: combo.stickiness.ttl_seconds,
      stickinessKeySource: combo.stickiness.key_source,
      stickinessTolerance: String(combo.stickiness.price_tolerance_pct),
    });
    this.members.set(combo.members.map((member) => ({ ...member })));
    this.memberPicker.set('');
    this.attempted.set(false);
    this.formError.set(null);
    this.actionError.set(null);
    this.notice.set(null);
    this.formState.set({ mode: 'edit', combo });
  }

  protected closeForm(): void {
    this.formState.set(null);
    this.formError.set(null);
  }

  protected setName(value: string): void {
    this.draft.update((draft) => ({ ...draft, name: value }));
  }

  protected setStrategy(value: string): void {
    this.draft.update((draft) => ({ ...draft, strategy: strategyFromValue(value) }));
  }

  protected setWeightsMode(value: string): void {
    this.draft.update((draft) => ({ ...draft, weightsMode: weightsModeFromValue(value) }));
  }

  protected setWeight(component: WeightComponent, value: string): void {
    const weight = Number(value);
    this.draft.update((draft) => ({
      ...draft,
      weights: { ...draft.weights, [component]: Number.isFinite(weight) ? weight : 0 },
    }));
  }

  protected setEnabled(enabled: boolean): void {
    this.draft.update((draft) => ({ ...draft, enabled }));
  }

  // --- pool mode (D35) -----------------------------------------------------

  /**
   * Switches the pool source. A catalog pool only supports `weighted`, so the
   * strategy is forced and the other options are disabled with the server's
   * explanation.
   */
  protected setPool(value: string): void {
    const pool: ComboPool = value === 'all' ? 'all' : 'members';
    this.draft.update((draft) => ({
      ...draft,
      pool,
      strategy: pool === 'all' ? 'weighted' : draft.strategy,
    }));
  }

  protected setMinQuality(value: string): void {
    this.draft.update((draft) => ({ ...draft, minQuality: value }));
  }

  protected setMaxMixPrice(value: string): void {
    this.draft.update((draft) => ({ ...draft, maxMixPrice: value }));
  }

  protected setPoolSort(value: string): void {
    this.draft.update((draft) => ({ ...draft, poolSort: poolSortFromValue(value) }));
  }

  protected setProviderSelection(value: string): void {
    this.draft.update((draft) => ({
      ...draft,
      providerSelection: providerSelectionFromValue(value),
    }));
  }

  protected setMixProfile(value: string): void {
    this.draft.update((draft) => ({ ...draft, mixProfile: value }));
  }

  protected setStickinessEnabled(enabled: boolean): void {
    this.draft.update((draft) => ({ ...draft, stickinessEnabled: enabled }));
  }

  protected setStickinessTtl(value: string): void {
    const seconds = Number(value);
    if (!Number.isFinite(seconds) || seconds <= 0) {
      return;
    }
    this.draft.update((draft) => ({ ...draft, stickinessTtl: seconds }));
  }

  protected setStickinessKeySource(value: string): void {
    const source: StickinessKeySource = value === 'header' ? 'header' : 'auto';
    this.draft.update((draft) => ({ ...draft, stickinessKeySource: source }));
  }

  protected setStickinessTolerance(value: string): void {
    this.draft.update((draft) => ({ ...draft, stickinessTolerance: value }));
  }

  /** `header` reads X-Session-Id; `auto` hashes identity + first message. */
  protected stickinessHint(): string {
    return this.draft().stickinessKeySource === 'header'
      ? 'Reads X-Session-Id; requests without it are not pinned.'
      : 'Derived from the user or API key and the first message; X-Session-Id wins.';
  }

  /** True for the weights editor: presets only matter to the weighted strategy. */
  protected showWeights(): boolean {
    return this.draft().strategy === 'weighted';
  }

  protected isEditing(): boolean {
    return this.formState()?.mode === 'edit';
  }

  // --- members -------------------------------------------------------------

  protected addMember(offeringId: string): void {
    if (offeringId === '') {
      return;
    }
    this.members.update((rows) => [...rows, { offering_id: offeringId, weight: 1 }]);
    // The picker offers the remaining offerings; reset it for the next pick.
    this.memberPicker.set('');
  }

  protected removeMember(index: number): void {
    this.members.update((rows) => rows.filter((_, i) => i !== index));
  }

  protected moveMember(index: number, delta: -1 | 1): void {
    const rows = [...this.members()];
    const target = index + delta;
    if (target < 0 || target >= rows.length) {
      return;
    }
    const moved = rows[index];
    rows[index] = rows[target];
    rows[target] = moved;
    this.members.set(rows);
  }

  protected setMemberWeight(index: number, value: string): void {
    const weight = Number(value);
    this.members.update((rows) =>
      rows.map((member, i) =>
        i === index ? { ...member, weight: Number.isFinite(weight) ? weight : 0 } : member,
      ),
    );
  }

  /** A member weight must be a positive finite number. */
  protected memberWeightsInvalid(): boolean {
    return this.members().some((member) => !Number.isFinite(member.weight) || member.weight <= 0);
  }

  /** Custom weights must not all be zero (the wire rejects an all-zero set). */
  protected customWeightsInvalid(): boolean {
    return WEIGHT_FIELDS.every((field) => this.draft().weights[field] <= 0);
  }

  // --- save, toggle, delete ------------------------------------------------

  /** Native submit (Enter, and the specs) goes through the same validation path. */
  protected onSubmit(event: Event): void {
    event.preventDefault();
    void this.save();
  }

  protected async save(): Promise<void> {
    this.attempted.set(true);
    this.formError.set(null);
    this.notice.set(null);
    const state = this.formState();
    if (state === null || this.formInvalid()) {
      return;
    }
    this.saving.set(true);
    try {
      const input = this.buildInput();
      if (state.mode === 'create') {
        await this.combosService.create(input);
      } else {
        await this.combosService.update(state.combo.id, input);
      }
      this.formState.set(null);
      this.notice.set('Combo saved.');
      await this.reload();
    } catch (err) {
      // Invalid input (unknown member offering, taken name, …) keeps the form
      // open with the server's message — it names the offending field.
      this.formError.set(describeError(err, 'Saving the combo failed.'));
    } finally {
      this.saving.set(false);
    }
  }

  protected async toggleEnabled(combo: Combo, enabled: boolean): Promise<void> {
    this.togglingId.set(combo.id);
    this.actionError.set(null);
    this.notice.set(null);
    try {
      await this.combosService.update(combo.id, comboInputOf(combo, enabled));
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Changing the combo failed.'));
    } finally {
      this.togglingId.set(null);
    }
  }

  protected askDelete(combo: Combo): void {
    this.deleteTarget.set(combo);
    this.deleteError.set(null);
    this.actionError.set(null);
  }

  protected cancelDelete(): void {
    this.deleteTarget.set(null);
    this.deleteError.set(null);
  }

  protected async confirmDelete(): Promise<void> {
    const combo = this.deleteTarget();
    if (combo === null) {
      return;
    }
    this.deleting.set(true);
    this.deleteError.set(null);
    try {
      await this.combosService.remove(combo.id);
      this.deleteTarget.set(null);
      this.notice.set('Combo deleted.');
      await this.reload();
    } catch (err) {
      this.deleteError.set(describeError(err, 'Deleting the combo failed.'));
    } finally {
      this.deleting.set(false);
    }
  }

  // --- routing preview (D35) -----------------------------------------------

  /** Previews the current draft through the runtime engine. */
  protected previewDraft(): void {
    this.attempted.set(true);
    this.formError.set(null);
    if (this.formInvalid()) {
      return;
    }
    const input = this.buildInput();
    this.previewSource.set({ kind: 'draft', input });
    this.previewTitle.set(`Routing preview: ${input.name}`);
    this.previewSessionKey.set('');
    this.preview.set(null);
    this.previewError.set(null);
    this.previewOpen.set(true);
    void this.refreshPreview();
  }

  /** Previews a saved combo, including its current pin per session. */
  protected previewSaved(combo: Combo): void {
    this.previewSource.set({ kind: 'saved', combo });
    this.previewTitle.set(`Routing preview: combo:${combo.name}`);
    this.previewSessionKey.set('');
    this.preview.set(null);
    this.previewError.set(null);
    this.previewOpen.set(true);
    void this.refreshPreview();
  }

  protected closePreview(): void {
    this.previewOpen.set(false);
    this.previewSource.set(null);
    this.preview.set(null);
    this.previewError.set(null);
  }

  protected setPreviewSessionKey(value: string): void {
    this.previewSessionKey.set(value);
  }

  /** Recomputes the open preview (also refreshes the pin for a saved combo). */
  protected async refreshPreview(): Promise<void> {
    const source = this.previewSource();
    if (source === null) {
      return;
    }
    this.previewLoading.set(true);
    this.previewError.set(null);
    try {
      this.preview.set(
        source.kind === 'draft'
          ? await this.combosService.preview(source.input)
          : await this.combosService.previewSaved(source.combo.id, this.previewSessionKey()),
      );
    } catch (err) {
      // A rejected draft (invalid constraint, unknown member) keeps the
      // server's message; it names the offending field.
      this.previewError.set(describeError(err, 'Loading the routing preview failed.'));
    } finally {
      this.previewLoading.set(false);
    }
  }

  protected previewIsSaved(): boolean {
    return this.previewSource()?.kind === 'saved';
  }

  private formInvalid(): boolean {
    const membersPool = this.draft().pool === 'members';
    return (
      this.nameError() !== '' ||
      this.membersError() !== '' ||
      this.constraintError() !== '' ||
      this.stickinessToleranceError() !== '' ||
      (membersPool && this.memberWeightsInvalid()) ||
      (this.draft().weightsMode === 'custom' && this.customWeightsInvalid())
    );
  }

  private buildInput(): ComboInput {
    const { name, strategy, weightsMode, weights, enabled, pool } = this.draft();
    return {
      name: name.trim(),
      strategy,
      weights: this.buildWeights(weightsMode, weights),
      // A catalog pool has no explicit members; ordered/round_robin require
      // the list, and the server rejects them for `pool = all` (D35).
      members:
        pool === 'members'
          ? this.members().map((member) => ({
              offering_id: member.offering_id,
              weight: member.weight,
            }))
          : [],
      enabled,
      pool,
      min_quality: pool === 'all' ? nullableNumber(this.draft().minQuality) : null,
      max_mix_price_per_m: pool === 'all' ? nullableNumber(this.draft().maxMixPrice) : null,
      mix_profile: this.draft().mixProfile === '' ? null : this.draft().mixProfile,
      stickiness: {
        enabled: this.draft().stickinessEnabled,
        ttl_seconds: this.draft().stickinessTtl,
        key_source: this.draft().stickinessKeySource,
        price_tolerance_pct: stickyToleranceFromInput(this.draft().stickinessTolerance),
      },
      // The ordering knobs only describe a catalog pool (D64); a member pool
      // leaves them off so the server keeps its defaults.
      sort: pool === 'all' ? this.draft().poolSort : undefined,
      provider_selection: pool === 'all' ? this.draft().providerSelection : undefined,
    };
  }

  /** A named preset goes out as `{"preset": …}`, custom as the four parts. */
  private buildWeights(mode: WeightsMode, weights: ComboWeights): ComboWeightsInput {
    if (mode !== 'custom') {
      return { preset: mode };
    }
    return { ...weights };
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading combos failed.'));
  }
}

function emptyDraft(): ComboDraft {
  return {
    name: '',
    strategy: 'weighted',
    weightsMode: 'balanced',
    weights: { ...DEFAULT_WEIGHTS },
    enabled: true,
    pool: 'members',
    minQuality: '',
    maxMixPrice: '',
    mixProfile: '',
    poolSort: 'price',
    providerSelection: 'mix_price',
    stickinessEnabled: false,
    stickinessTtl: 1800,
    stickinessKeySource: 'auto',
    stickinessTolerance: '5',
  };
}

/** A trimmed blank means unconstrained (null); otherwise the number. */
function nullableNumber(value: string): number | null {
  const trimmed = value.trim();
  return trimmed === '' ? null : Number(trimmed);
}

function strategyFromValue(value: string): ComboStrategy {
  switch (value) {
    case 'ordered':
    case 'round_robin':
    case 'weighted':
      return value;
    default:
      return 'weighted';
  }
}

function poolSortFromValue(value: string): PoolSort {
  return value === 'quality' ? 'quality' : 'price';
}

function providerSelectionFromValue(value: string): ProviderSelection {
  switch (value) {
    case 'speed':
    case 'reliability':
    case 'mix_price':
      return value;
    default:
      return 'mix_price';
  }
}

/** The typed tolerance, clamped to 0–100; blank falls back to the 5% default. */
function stickyToleranceFromInput(value: string): number {
  const trimmed = value.trim();
  if (trimmed === '') {
    return 5;
  }
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) && parsed >= 0 && parsed <= 100 ? parsed : 5;
}

function weightsModeFromValue(value: string): WeightsMode {
  switch (value) {
    case 'cheapest':
    case 'fastest':
    case 'best':
    case 'custom':
    case 'balanced':
      return value;
    default:
      return 'balanced';
  }
}

/** The wire body of an existing combo (used for the enable/disable toggle). */
function comboInputOf(combo: Combo, enabled: boolean): ComboInput {
  return {
    name: combo.name,
    strategy: combo.strategy,
    weights: combo.preset === null ? { ...combo.weights } : { preset: combo.preset },
    members: combo.members.map((member) => ({
      offering_id: member.offering_id,
      weight: member.weight,
    })),
    enabled,
    pool: combo.pool,
    min_quality: combo.min_quality,
    max_mix_price_per_m: combo.max_mix_price_per_m,
    mix_profile: combo.mix_profile,
    stickiness: { ...combo.stickiness },
    sort: combo.sort,
    provider_selection: combo.provider_selection,
  };
}
