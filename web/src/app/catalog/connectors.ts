import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
} from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { BalanceCache } from './balance-cache';
import { CatalogService } from './catalog.service';
import type { BalanceGate, Credential, Provider } from './catalog.types';
import {
  type BalancePointerField,
  type CatalogPointerField,
  type PointerAssignment,
  type TierInfo,
  PRICE_POINTER_FIELDS,
  BALANCE_EXAMPLE_RAW,
  balanceExample,
  convertPrice,
  detectTemplateId,
  detectTier,
  entryRelativePointer,
  formatConverted,
  resolvePointer,
  rewriteTier,
  sampleValueLabel,
  unitExample,
} from './connector-form';
import {
  type BalanceConfig,
  type BalanceUnit,
  type CatalogConfig,
  type ConnectorConfig,
  type ConnectorKind,
  type ConnectorTemplate,
  type ConnectorView,
  type ConnectorTestView,
  type HealthConfig,
  type PriceRefreshView,
  type PriceUnit,
  type SampleView,
  type UnitSource,
  emptyConnectorConfig,
} from './connectors.types';
import { describeError } from './errors';
import { formatAge, formatTime } from './format';
import { type HeaderRow, headerMapOf } from './headers-editor';
import { JsonTree } from './json-tree';
import { UiBadge } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiDialog } from '../ui/dialog';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';
import { UiSelect } from '../ui/select';
import { UiSwitch } from '../ui/switch';
import { UiTable } from '../ui/table';
import { UiTabs, type TabItem } from '../ui/tabs';

type Busy = 'sample' | 'test' | 'save' | 'refresh';

interface CatalogForm {
  entries: string;
  model_id: string;
  input: string;
  output: string;
  cache_read: string;
  cache_write: string;
  reasoning: string;
  modalities_input: string;
  modalities_output: string;
  context_length: string;
  max_output: string;
  apply_on_discovery: boolean;
  refresh_seconds: string;
  unit_source: UnitSource;
  unit_multiplier: string;
  unit_currency: string;
  unit_fx: string;
  unit_symbol: string;
  unit_decimals: string;
  unit_note: string;
  active: CatalogPointerField | null;
}

interface BalanceForm {
  balance: string;
  subtract: string;
  available: string;
  currency: string;
  period_spend: string;
  burn_rate: string;
  days_remaining: string;
  refresh_seconds: string;
  unit_multiplier: string;
  unit_currency: string;
  unit_fx: string;
  unit_symbol: string;
  unit_decimals: string;
  unit_note: string;
  active: BalancePointerField | null;
}

interface HealthForm {
  ok_pointer: string;
  active: 'ok_pointer' | null;
}

interface KindForm {
  configured: boolean;
  enabled: boolean;
  url: string;
  headers: HeaderRow[];
  allowPrivate: boolean;
  updatedAt: string | null;
  updatedBy: string | null;
  templateId: string;
  catalog: CatalogForm;
  balance: BalanceForm;
  health: HealthForm;
}

interface Forms {
  catalog: KindForm;
  balance: KindForm;
  health: KindForm;
}

interface SampleState {
  url: string;
  status: number;
  bytes: number;
  truncated: boolean;
  fetchedAt: string;
  document: unknown;
  error: string | null;
}

interface TestState {
  result: ConnectorTestView | null;
  error: string | null;
}

interface PriceRefreshState {
  result: PriceRefreshView | null;
  error: string | null;
}

const TABS: TabItem[] = [
  { id: 'catalog', label: 'Prices' },
  { id: 'balance', label: 'Balance' },
  { id: 'health', label: 'Health' },
];

function emptyCatalogForm(): CatalogForm {
  return {
    entries: '/data',
    model_id: '/id',
    input: '',
    output: '',
    cache_read: '',
    cache_write: '',
    reasoning: '',
    modalities_input: '',
    modalities_output: '',
    context_length: '',
    max_output: '',
    apply_on_discovery: false,
    refresh_seconds: '',
    unit_source: 'per_million_tokens',
    unit_multiplier: '1',
    unit_currency: 'USD',
    unit_fx: '',
    unit_symbol: '$',
    unit_decimals: '4',
    unit_note: '',
    active: null,
  };
}

function emptyBalanceForm(): BalanceForm {
  return {
    balance: '/balance',
    subtract: '',
    available: '',
    currency: 'USD',
    period_spend: '',
    burn_rate: '',
    days_remaining: '',
    refresh_seconds: '900',
    unit_multiplier: '1',
    unit_currency: 'USD',
    unit_fx: '',
    unit_symbol: '$',
    unit_decimals: '2',
    unit_note: '',
    active: null,
  };
}

function emptyHealthForm(): HealthForm {
  return { ok_pointer: '', active: null };
}

function emptyKindForm(): KindForm {
  return {
    configured: false,
    enabled: true,
    url: '',
    headers: [{ name: 'authorization', value: '' }],
    allowPrivate: false,
    updatedAt: null,
    updatedBy: null,
    templateId: '',
    catalog: emptyCatalogForm(),
    balance: emptyBalanceForm(),
    health: emptyHealthForm(),
  };
}

function emptyForms(): Forms {
  return {
    catalog: emptyKindForm(),
    balance: emptyKindForm(),
    health: emptyKindForm(),
  };
}

function kindOfTab(tab: string): ConnectorKind {
  return tab === 'balance' ? 'balance' : tab === 'health' ? 'health' : 'catalog';
}

/** Accepts both the URL spelling (`prices`) and the tab id (`catalog`). */
function normalizeTab(tab: string): string {
  return tab === 'balance' || tab === 'health' ? tab : 'catalog';
}

/**
 * Provider connectors dialog (D42): prices, balance, and health documents,
 * each configured by pointing at the provider's own payload. Fetching a sample
 * and clicking a value fills the active pointer; a preview shows the converted
 * prices before anything is saved. The dialog writes only on Save/Refresh.
 */
@Component({
  selector: 'app-connectors',
  imports: [
    JsonTree,
    NgTemplateOutlet,
    UiBadge,
    UiBanner,
    UiButton,
    UiDialog,
    UiField,
    UiInput,
    UiSelect,
    UiSwitch,
    UiTable,
    UiTabs,
  ],
  templateUrl: './connectors.html',
  styleUrl: './connectors.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Connectors implements OnInit {
  private readonly catalog = inject(CatalogService);
  private readonly balanceCache = inject(BalanceCache);

  readonly provider = input.required<Provider>();
  /**
   * The tab the deep link asked for (`catalog` for Prices). Following it is
   * only on open; a later user tab click stays put until the URL changes.
   */
  readonly initialTab = input<string>('catalog');
  readonly closed = output<void>();
  /** Raised after a save or refresh so the provider list can reload. */
  readonly changed = output<void>();

  protected readonly tabs = TABS;
  protected readonly catalogFields: readonly { key: CatalogPointerField; label: string }[] = [
    { key: 'entries', label: 'Entries (array)' },
    { key: 'model_id', label: 'Model id' },
    { key: 'input', label: 'Input price' },
    { key: 'output', label: 'Output price' },
    { key: 'cache_read', label: 'Cache read' },
    { key: 'cache_write', label: 'Cache write' },
    { key: 'reasoning', label: 'Reasoning' },
    { key: 'modalities_input', label: 'Input modalities' },
    { key: 'modalities_output', label: 'Output modalities' },
    { key: 'context_length', label: 'Context length' },
    { key: 'max_output', label: 'Max output' },
  ];
  protected readonly balanceFields: readonly { key: BalancePointerField; label: string }[] = [
    { key: 'balance', label: 'Balance (required)' },
    { key: 'subtract', label: 'Subtract (optional)' },
    { key: 'available', label: 'Available flag' },
    { key: 'period_spend', label: 'Period spend' },
    { key: 'burn_rate', label: 'Burn rate' },
    { key: 'days_remaining', label: 'Days remaining' },
  ];
  protected readonly tab = signal<string>(normalizeTab(this.initialTab()));
  protected readonly kind = computed<ConnectorKind>(() => kindOfTab(this.tab()));

  protected readonly forms = signal<Forms>(emptyForms());
  protected readonly templates = signal<ConnectorTemplate[]>([]);
  protected readonly credentials = signal<Credential[]>([]);
  protected readonly loading = signal(true);
  protected readonly loadError = signal<string | null>(null);

  protected readonly samples = signal<Record<ConnectorKind, SampleState | null>>({
    catalog: null,
    balance: null,
    health: null,
  });
  protected readonly tests = signal<Record<ConnectorKind, TestState | null>>({
    catalog: null,
    balance: null,
    health: null,
  });
  protected readonly priceRefresh = signal<PriceRefreshState | null>(null);

  protected readonly busy = signal<Busy | null>(null);
  protected readonly notice = signal<string | null>(null);
  protected readonly error = signal<string | null>(null);
  protected readonly gateSaving = signal<string | null>(null);
  protected readonly balanceRefreshing = signal<string | null>(null);

  protected readonly formatAge = formatAge;
  protected readonly formatTime = formatTime;

  protected readonly currentForm = computed<KindForm>(() => this.forms()[this.kind()]);
  protected readonly currentSample = computed<SampleState | null>(
    () => this.samples()[this.kind()],
  );
  protected readonly currentTest = computed<TestState | null>(() => this.tests()[this.kind()]);

  protected readonly activePointer = computed<string | null>(() => {
    const form = this.currentForm();
    switch (this.kind()) {
      case 'catalog':
        return form.catalog.active === null ? null : form.catalog[form.catalog.active];
      case 'balance':
        return form.balance.active === null ? null : form.balance[form.balance.active];
      case 'health':
        return form.health.ok_pointer === '' ? null : form.health.ok_pointer;
    }
  });

  protected readonly unit = computed<PriceUnit>(() => {
    const form = this.forms().catalog.catalog;
    return {
      source: form.unit_source,
      multiplier: numberOr(form.unit_multiplier, 1),
      currency: form.unit_currency.trim().toUpperCase() || 'USD',
      fx_to_usd: form.unit_fx.trim() === '' ? null : numberOr(form.unit_fx, 0),
      display: {
        symbol: form.unit_symbol || '$',
        decimals: clampDecimals(numberOr(form.unit_decimals, 4)),
      },
      note: form.unit_note.trim() === '' ? null : form.unit_note,
    };
  });

  protected readonly example = computed(() => unitExample(this.unit()));

  protected readonly unitFactorLabel = computed(() => {
    const unit = this.unit();
    if (unit.source === 'per_token') {
      return 'Published numbers are multiplied by 1,000,000.';
    }
    if (unit.source === 'custom') {
      return `Published numbers are multiplied by ${unit.multiplier}.`;
    }
    return 'Published numbers are already USD per 1M tokens.';
  });

  protected readonly balanceUnit = computed<BalanceUnit>(() => {
    const form = this.forms().balance.balance;
    return {
      multiplier: numberOr(form.unit_multiplier, 1),
      currency: form.unit_currency.trim().toUpperCase() || 'USD',
      fx_to_usd: form.unit_fx.trim() === '' ? null : numberOr(form.unit_fx, 0),
      display: {
        symbol: form.unit_symbol || '$',
        decimals: clampDecimals(numberOr(form.unit_decimals, 2)),
      },
      note: form.unit_note.trim() === '' ? null : form.unit_note,
    };
  });

  /**
   * The live balance example. When a sample is loaded and the balance pointer
   * resolves to a number, that raw value is used; otherwise the
   * SailResearch-shaped 136275 cents.
   */
  protected readonly balanceExampleText = computed(() => {
    const form = this.forms().balance.balance;
    const sample = this.samples().balance?.document;
    const raw = sample === undefined ? undefined : resolvePointer(sample, form.balance.trim());
    return balanceExample(
      this.balanceUnit(),
      typeof raw === 'number' && Number.isFinite(raw) ? raw : BALANCE_EXAMPLE_RAW,
    );
  });

  protected readonly balanceUnitWarnings = computed<readonly string[]>(() => {
    const unit = this.balanceUnit();
    const warnings: string[] = [];
    if (!Number.isFinite(unit.multiplier) || unit.multiplier <= 0) {
      warnings.push(
        'A multiplier of zero or less makes the balance unavailable; prices and spend are not stored.',
      );
    }
    if (unit.currency !== 'USD' && unit.fx_to_usd === null) {
      warnings.push(
        `${unit.currency} balances are stored in ${unit.currency} and ignored by routing and the balance gate until a static FX rate is set.`,
      );
    }
    if (unit.fx_to_usd !== null && unit.fx_to_usd <= 0) {
      warnings.push('An FX rate of zero or less is invalid; the balance stays non-routable.');
    }
    return warnings;
  });

  protected readonly tier = computed<TierInfo | null>(() => {
    const form = this.forms().catalog.catalog;
    const pointers = priceAssignments(form)
      .map((assignment) => assignment.pointer)
      .filter((pointer) => pointer !== '');
    return detectTier(this.samples().catalog?.document, pointers);
  });

  protected readonly allWarnings = computed<readonly string[]>(() => {
    const test = this.currentTest()?.result;
    if (test === null || test === undefined) {
      return [];
    }
    const warnings = new Set(test.warnings);
    for (const sample of test.samples) {
      for (const warning of sample.warnings) {
        warnings.add(warning);
      }
    }
    for (const error of test.errors) {
      warnings.add(error);
    }
    return [...warnings];
  });

  protected readonly summary = computed(() => {
    const test = this.currentTest()?.result;
    if (test === null || test === undefined) {
      return null;
    }
    if (test.kind === 'catalog') {
      const errors = test.samples.filter((sample) => sample.error !== null).length;
      return `${test.matched ?? 0} priced · ${test.skipped ?? 0} skipped · ${errors} errors · ${
        test.entries_seen ?? 0
      } entries`;
    }
    if (test.kind === 'balance') {
      return test.balance === null
        ? 'No balance extracted'
        : test.balance.available
          ? 'Balance available'
          : 'Provider says the balance is unavailable';
    }
    return test.health?.ok === true ? 'Healthy' : 'Unhealthy';
  });

  constructor() {
    // A back/forward navigation can change the requested tab while the dialog
    // stays open; mirror it.
    effect(() => {
      this.tab.set(normalizeTab(this.initialTab()));
    });
  }

  ngOnInit(): void {
    void this.load();
  }

  protected formFor(kind: string): KindForm {
    return this.forms()[kindOfTab(kind)];
  }

  protected close(): void {
    this.closed.emit();
  }

  protected setTab(tab: string): void {
    this.tab.set(tab);
    this.notice.set(null);
    this.error.set(null);
  }

  // --- loading -------------------------------------------------------------

  private async load(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    const providerId = this.provider().id;
    const results = await Promise.allSettled([
      this.catalog.connectorTemplates(providerId),
      this.catalog.getConnector(providerId, 'catalog'),
      this.catalog.getConnector(providerId, 'balance'),
      this.catalog.getConnector(providerId, 'health'),
      this.catalog.listCredentials(providerId),
    ]);
    const [templates, catalogView, balanceView, healthView, credentials] = results;
    if (templates.status === 'fulfilled') {
      this.templates.set(templates.value);
    }
    const forms = emptyForms();
    if (catalogView.status === 'fulfilled') {
      forms.catalog = kindFormFromView(catalogView.value);
    }
    if (balanceView.status === 'fulfilled') {
      forms.balance = kindFormFromView(balanceView.value);
    }
    if (healthView.status === 'fulfilled') {
      forms.health = kindFormFromView(healthView.value);
    }
    this.forms.set(forms);
    if (credentials.status === 'fulfilled') {
      this.credentials.set(credentials.value);
    }
    const failed = results.find((result) => result.status === 'rejected');
    if (failed !== undefined && failed.status === 'rejected') {
      this.loadError.set(describeError(failed.reason, 'Loading connectors failed.'));
    }
    this.loading.set(false);
  }

  // --- form setters --------------------------------------------------------

  protected setUrl(kind: ConnectorKind, value: string): void {
    this.patchKind(kind, { url: value });
  }

  protected setAllowPrivate(kind: ConnectorKind, value: boolean): void {
    this.patchKind(kind, { allowPrivate: value });
  }

  protected setEnabled(kind: ConnectorKind, value: boolean): void {
    this.patchKind(kind, { enabled: value });
  }

  protected setHeaders(kind: ConnectorKind, rows: HeaderRow[]): void {
    this.patchKind(kind, { headers: rows });
  }

  protected setHeader(
    kind: ConnectorKind,
    index: number,
    field: 'name' | 'value',
    value: string,
  ): void {
    const form = this.forms()[kind];
    const rows = form.headers.map((row, i) => (i === index ? { ...row, [field]: value } : row));
    this.patchKind(kind, { headers: rows });
  }

  protected addHeader(kind: ConnectorKind): void {
    const form = this.forms()[kind];
    this.patchKind(kind, { headers: [...form.headers, { name: '', value: '' }] });
  }

  protected removeHeader(kind: ConnectorKind, index: number): void {
    const form = this.forms()[kind];
    this.patchKind(kind, { headers: form.headers.filter((_, i) => i !== index) });
  }

  protected setCatalog(field: keyof CatalogForm, value: string): void {
    const forms = this.forms();
    const form = forms.catalog;
    this.forms.set({
      ...forms,
      catalog: { ...form, catalog: { ...form.catalog, [field]: value } },
    });
  }

  protected setCatalogFlag(field: 'apply_on_discovery', value: boolean): void {
    const forms = this.forms();
    const form = forms.catalog;
    this.forms.set({
      ...forms,
      catalog: { ...form, catalog: { ...form.catalog, [field]: value } },
    });
  }

  protected setUnitSource(value: string): void {
    if (value === 'per_million_tokens' || value === 'per_token' || value === 'custom') {
      this.setCatalog('unit_source', value);
    }
  }

  protected setBalance(field: keyof BalanceForm, value: string): void {
    const forms = this.forms();
    const form = forms.balance;
    this.forms.set({
      ...forms,
      balance: { ...form, balance: { ...form.balance, [field]: value } },
    });
  }

  protected setHealth(field: 'ok_pointer', value: string): void {
    const forms = this.forms();
    const form = forms.health;
    this.forms.set({
      ...forms,
      health: { ...form, health: { ...form.health, [field]: value } },
    });
  }

  /** Selects which pointer field the next tree click fills. */
  protected focusCatalogField(field: CatalogPointerField): void {
    const forms = this.forms();
    const form = forms.catalog;
    this.forms.set({
      ...forms,
      catalog: {
        ...form,
        catalog: { ...form.catalog, active: form.catalog.active === field ? null : field },
      },
    });
  }

  protected focusBalanceField(field: BalancePointerField): void {
    const forms = this.forms();
    const form = forms.balance;
    this.forms.set({
      ...forms,
      balance: {
        ...form,
        balance: { ...form.balance, active: form.balance.active === field ? null : field },
      },
    });
  }

  protected focusHealthField(): void {
    const forms = this.forms();
    const form = forms.health;
    this.forms.set({
      ...forms,
      health: {
        ...form,
        health: {
          ...form.health,
          active: form.health.active === 'ok_pointer' ? null : 'ok_pointer',
        },
      },
    });
  }

  protected pick(pointer: string): void {
    const kind = this.kind();
    if (kind === 'catalog') {
      const field = this.forms().catalog.catalog.active;
      if (field !== null) {
        const entries = this.forms().catalog.catalog.entries;
        const value = field === 'entries' ? pointer : entryRelativePointer(entries, pointer);
        this.setCatalog(field, value);
      }
      return;
    }
    if (kind === 'balance') {
      const field = this.forms().balance.balance.active;
      if (field !== null) {
        this.setBalance(field, pointer);
      }
      return;
    }
    this.setHealth('ok_pointer', pointer);
  }

  protected setGate(credential: Credential, gate: string): void {
    if (gate !== 'warn' && gate !== 'block') {
      return;
    }
    void this.saveGate(credential, gate);
  }

  private async saveGate(credential: Credential, gate: BalanceGate): Promise<void> {
    this.gateSaving.set(credential.id);
    this.error.set(null);
    try {
      const updated = await this.catalog.updateCredential(credential.id, { balance_gate: gate });
      this.credentials.update((credentials) =>
        credentials.map((candidate) => (candidate.id === updated.id ? updated : candidate)),
      );
      this.notice.set(`Balance policy for ${credential.label} set to ${gate}.`);
      this.changed.emit();
    } catch (err) {
      this.error.set(describeError(err, 'Saving the balance policy failed.'));
    } finally {
      this.gateSaving.set(null);
    }
  }

  // --- templates -----------------------------------------------------------

  protected templatesFor(kind: ConnectorKind): readonly ConnectorTemplate[] {
    return this.templates().filter((template) => template.configs[kind] !== undefined);
  }

  protected templateDescription(kind: ConnectorKind): string {
    const id = this.forms()[kind].templateId;
    return this.templates().find((template) => template.id === id)?.description ?? '';
  }

  protected readonly multiplierWarning = computed<string | null>(() => {
    const form = this.forms().catalog.catalog;
    const multiplier = numberOr(form.unit_multiplier, 1);
    return form.unit_source !== 'custom' && multiplier !== 1
      ? `A multiplier of ${multiplier} is ignored because the source is not “custom”.`
      : null;
  });

  protected readonly nonUsdHint = computed<string | null>(() => {
    const unit = this.unit();
    return unit.currency !== 'USD' && unit.fx_to_usd === null
      ? `${unit.currency} prices stay ${unit.currency} and do not feed routing until a static FX rate is set.`
      : null;
  });

  protected selectTemplate(kind: ConnectorKind, templateId: string): void {
    const templates = this.forms();
    this.forms.set({
      ...templates,
      [kind]: { ...templates[kind], templateId },
    });
    if (templateId !== '') {
      this.applyTemplate(kind, templateId);
    }
  }

  private applyTemplate(kind: ConnectorKind, templateId: string): void {
    const template = this.templates().find((candidate) => candidate.id === templateId);
    const config = template?.configs[kind];
    if (config === undefined) {
      return;
    }
    const forms = this.forms();
    const form = forms[kind];
    const next = kindFormFromConfig(config, form);
    this.forms.set({ ...forms, [kind]: { ...next, templateId } });
    this.notice.set(`Template “${template?.label ?? templateId}” applied; fields stay editable.`);
  }

  protected detectTemplate(): void {
    const sample = this.currentSample();
    if (sample === null || sample.error !== null) {
      this.error.set('Fetch a sample first, then detect again.');
      return;
    }
    const kind = this.kind();
    const id = detectTemplateId(this.templates(), kind, sample.document);
    if (id === null) {
      this.notice.set('No shipped template matches this sample; configure the fields by hand.');
      return;
    }
    this.selectTemplate(kind, id);
    this.notice.set(`Detected ${id} from the sample.`);
  }

  // --- sample, preview, save, refresh -------------------------------------

  protected async fetchSample(): Promise<void> {
    if (this.busy() !== null) {
      return;
    }
    this.busy.set('sample');
    this.error.set(null);
    this.notice.set(null);
    const kind = this.kind();
    try {
      const view = await this.sample(kind);
      this.samples.update((samples) => ({ ...samples, [kind]: toSampleState(view) }));
      this.notice.set(`Sample fetched: ${view.bytes} bytes from ${view.url}.`);
    } catch (err) {
      this.samples.update((samples) => ({ ...samples, [kind]: sampleError(err) }));
      this.error.set(describeError(err, 'Fetching the sample failed.'));
    } finally {
      this.busy.set(null);
    }
  }

  protected async runTest(): Promise<void> {
    if (this.busy() !== null) {
      return;
    }
    this.busy.set('test');
    this.error.set(null);
    this.notice.set(null);
    const kind = this.kind();
    try {
      const result = await this.catalog.testConnector(this.provider().id, kind, this.draft(kind));
      this.tests.update((tests) => ({ ...tests, [kind]: { result, error: null } }));
      this.notice.set(`Preview finished: ${summarizeTest(result)}.`);
    } catch (err) {
      this.tests.update((tests) => ({
        ...tests,
        [kind]: { result: null, error: describeError(err, 'The preview failed.') },
      }));
      this.error.set(describeError(err, 'The preview failed.'));
    } finally {
      this.busy.set(null);
    }
  }

  protected async save(): Promise<void> {
    if (this.busy() !== null) {
      return;
    }
    const kind = this.kind();
    const validation = this.validate(kind);
    if (validation !== null) {
      this.error.set(validation);
      return;
    }
    this.busy.set('save');
    this.error.set(null);
    this.notice.set(null);
    try {
      const form = this.forms()[kind];
      const view = await this.catalog.putConnector(
        this.provider().id,
        kind,
        this.buildConfig(kind),
        form.enabled,
      );
      this.forms.update((forms) => ({
        ...forms,
        [kind]: {
          ...forms[kind],
          configured: true,
          updatedAt: view.updated_at,
          updatedBy: view.updated_by,
        },
      }));
      this.notice.set(`${kindLabel(kind)} connector saved.`);
      this.changed.emit();
    } catch (err) {
      this.error.set(describeError(err, `Saving the ${kind} connector failed.`));
    } finally {
      this.busy.set(null);
    }
  }

  protected async refreshPrices(): Promise<void> {
    if (this.busy() !== null) {
      return;
    }
    this.busy.set('refresh');
    this.error.set(null);
    this.notice.set(null);
    this.priceRefresh.set(null);
    try {
      const result = await this.catalog.refreshPrices(
        this.provider().id,
        this.credentials()[0]?.id,
      );
      this.priceRefresh.set({ result, error: null });
      this.notice.set(refreshSummary(result));
      this.changed.emit();
    } catch (err) {
      const message = describeError(err, 'Refreshing prices failed.');
      this.priceRefresh.set({ result: null, error: message });
      this.error.set(message);
    } finally {
      this.busy.set(null);
    }
  }

  protected async refreshBalances(): Promise<void> {
    if (this.balanceRefreshing() !== null) {
      return;
    }
    this.balanceRefreshing.set('all');
    this.error.set(null);
    try {
      const providerBalances = await this.catalog.refreshProviderBalances(this.provider().id);
      this.balanceCache.setAll(providerBalances.results);
      this.credentials.set(await this.catalog.listCredentials(this.provider().id));
      this.notice.set(
        `Balances refreshed: ${providerBalances.refreshed} ok, ${providerBalances.failed} failed.`,
      );
      this.changed.emit();
    } catch (err) {
      this.error.set(describeError(err, 'Refreshing balances failed.'));
    } finally {
      this.balanceRefreshing.set(null);
    }
  }

  protected async refreshOneBalance(credential: Credential): Promise<void> {
    if (this.balanceRefreshing() !== null) {
      return;
    }
    this.balanceRefreshing.set(credential.id);
    this.error.set(null);
    try {
      const result = await this.catalog.refreshCredentialBalance(this.provider().id, credential.id);
      this.balanceCache.set(result);
      this.credentials.set(await this.catalog.listCredentials(this.provider().id));
      this.notice.set(
        result.available
          ? `${credential.label}: ${currencyAmount(result.balance_usd, result.currency)}.`
          : `${credential.label}: the provider reports no available balance.`,
      );
      this.changed.emit();
    } catch (err) {
      this.error.set(describeError(err, 'Refreshing the balance failed.'));
    } finally {
      this.balanceRefreshing.set(null);
    }
  }

  protected balanceLabel(credential: Credential): string {
    const result = this.balanceCache.get(credential.id);
    const available = result?.available ?? credential.balance_available;
    const value = result?.balance_usd ?? credential.balance_usd;
    if (available === false) {
      return 'unavailable';
    }
    if (value === null) {
      return '—';
    }
    return currencyAmount(value, result?.currency ?? credential.balance_currency);
  }

  protected balanceDetail(credential: Credential): string | null {
    const result = this.balanceCache.get(credential.id);
    if (result?.days_remaining !== undefined && result?.days_remaining !== null) {
      return `runs out in ${formatDays(result.days_remaining)}`;
    }
    return credential.balance_error;
  }

  // --- validation and wire documents --------------------------------------

  private validate(kind: ConnectorKind): string | null {
    const form = this.forms()[kind];
    if (form.url.trim() === '') {
      return 'A URL is required. Use an absolute URL or a path beginning with /.';
    }
    if (kind === 'catalog') {
      const catalog = form.catalog;
      if (catalog.entries.trim() === '' || catalog.model_id.trim() === '') {
        return 'The entries and model id pointers are required.';
      }
      const prices = priceAssignments(catalog).filter((assignment) => assignment.pointer !== '');
      if (prices.length === 0) {
        return 'Pick at least one price pointer (input or output).';
      }
      if (catalog.unit_source === 'custom' && numberOr(catalog.unit_multiplier, 0) <= 0) {
        return 'A custom multiplier must be greater than zero.';
      }
    }
    if (kind === 'balance' && form.balance.balance.trim() === '') {
      return 'The balance pointer is required.';
    }
    return null;
  }

  private buildConfig(kind: ConnectorKind): ConnectorConfig {
    const form = this.forms()[kind];
    const config: ConnectorConfig = {
      url: form.url.trim(),
      headers: headerMapOf(form.headers.filter((row) => row.value !== '')) ?? {},
      allow_private: form.allowPrivate,
    };
    if (kind === 'catalog') {
      config.catalog = catalogConfigFromForm(form.catalog);
    } else if (kind === 'balance') {
      config.balance = balanceConfigFromForm(form.balance);
    } else {
      config.health = { ok_pointer: pointerOrNull(form.health.ok_pointer) };
    }
    return config;
  }

  private draft(kind: ConnectorKind): { config: ConnectorConfig; credential_id?: string } {
    const credentialId = this.credentials()[0]?.id;
    return {
      config: this.buildConfig(kind),
      ...(credentialId === undefined ? {} : { credential_id: credentialId }),
    };
  }

  private async sample(kind: ConnectorKind): Promise<SampleView> {
    return this.catalog.sampleConnector(this.provider().id, kind, this.draft(kind));
  }

  // --- helpers used by the template ---------------------------------------

  protected rewriteTier(tier: string): void {
    const info = this.tier();
    if (info === null) {
      return;
    }
    const form = this.forms().catalog.catalog;
    const assignments = rewriteTier(priceAssignments(form), info, tier);
    const byField = new Map(
      assignments.map((assignment) => [assignment.field, assignment.pointer]),
    );
    const next = { ...form };
    for (const field of PRICE_POINTER_FIELDS) {
      const pointer = byField.get(field);
      if (pointer !== undefined) {
        next[field] = pointer;
      }
    }
    const forms = this.forms();
    this.forms.set({
      ...forms,
      catalog: { ...forms.catalog, catalog: next },
    });
  }

  protected sampleValue(pointer: string | null): string | null {
    return sampleValueLabel(this.samples().catalog?.document, pointer);
  }

  protected unitExampleFor(raw: number): string {
    return formatConverted(convertPrice(raw, this.unit()), this.unit().display);
  }

  /** A converted per-million number with the connector's display settings. */
  protected formattedSample(value: number | null): string {
    return value === null ? '—' : formatConverted(value, this.unit().display);
  }

  protected formattedBalance(value: number | null, currency: string | null): string {
    return currencyAmount(value, currency);
  }

  protected formattedNumber(value: number | null): string {
    return value === null ? '—' : value.toFixed(1);
  }

  protected isBusy(state: Busy): boolean {
    return this.busy() === state;
  }

  protected activeLabel(kind: 'catalog' | 'balance' | 'health'): string {
    if (kind === 'catalog') {
      return this.forms().catalog.catalog.active ?? '';
    }
    if (kind === 'balance') {
      return this.forms().balance.balance.active ?? '';
    }
    return this.forms().health.health.active ?? '';
  }

  protected credentialLabel(credential: Credential): string {
    return `${credential.label} (${credential.status})`;
  }

  protected priceRefreshBanner(state: PriceRefreshState): string {
    if (state.error !== null) {
      return state.error;
    }
    return state.result === null ? '' : refreshSummary(state.result);
  }

  protected testError(kind: ConnectorKind): string | null {
    return this.tests()[kind]?.error ?? null;
  }

  protected resultOf(kind: ConnectorKind): ConnectorTestView | null {
    return this.tests()[kind]?.result ?? null;
  }

  protected templateLabel(template: ConnectorTemplate): string {
    return template.label;
  }

  private patchKind(kind: ConnectorKind, patch: Partial<KindForm>): void {
    const forms = this.forms();
    this.forms.set({ ...forms, [kind]: { ...forms[kind], ...patch } });
  }
}

// ---------------------------------------------------------------------------
// Form <-> wire conversion
// ---------------------------------------------------------------------------

function kindFormFromView(view: ConnectorView): KindForm {
  if (view.config === null) {
    return {
      ...emptyKindForm(),
      configured: false,
      enabled: true,
    };
  }
  return kindFormFromConfig(view.config, {
    ...emptyKindForm(),
    configured: view.configured,
    enabled: view.enabled,
    updatedAt: view.updated_at,
    updatedBy: view.updated_by,
  });
}

function kindFormFromConfig(config: ConnectorConfig, base: KindForm): KindForm {
  const next: KindForm = {
    ...base,
    configured: true,
    url: config.url,
    headers: Object.entries(config.headers).map(([name, value]) => ({ name, value })),
    allowPrivate: config.allow_private,
  };
  if (config.catalog !== undefined && config.catalog !== null) {
    next.catalog = catalogFormFromConfig(config.catalog);
  }
  if (config.balance !== undefined && config.balance !== null) {
    next.balance = balanceFormFromConfig(config.balance);
  }
  if (config.health !== undefined && config.health !== null) {
    next.health = { ok_pointer: config.health.ok_pointer ?? '', active: null };
  }
  return next;
}

function catalogFormFromConfig(config: CatalogConfig): CatalogForm {
  return {
    entries: config.entries,
    model_id: config.model_id,
    input: config.input ?? '',
    output: config.output ?? '',
    cache_read: config.cache_read ?? '',
    cache_write: config.cache_write ?? '',
    reasoning: config.reasoning ?? '',
    modalities_input: config.modalities_input ?? '',
    modalities_output: config.modalities_output ?? '',
    context_length: config.context_length ?? '',
    max_output: config.max_output ?? '',
    apply_on_discovery: config.apply_on_discovery,
    refresh_seconds: config.refresh_seconds === null ? '' : String(config.refresh_seconds),
    unit_source: config.unit.source,
    unit_multiplier: String(config.unit.multiplier),
    unit_currency: config.unit.currency,
    unit_fx: config.unit.fx_to_usd === null ? '' : String(config.unit.fx_to_usd),
    unit_symbol: config.unit.display.symbol,
    unit_decimals: String(config.unit.display.decimals),
    unit_note: config.unit.note ?? '',
    active: null,
  };
}

function balanceFormFromConfig(config: BalanceConfig): BalanceForm {
  return {
    balance: config.balance,
    subtract: config.subtract ?? '',
    available: config.available ?? '',
    currency: config.currency ?? '',
    period_spend: config.period_spend ?? '',
    burn_rate: config.burn_rate ?? '',
    days_remaining: config.days_remaining ?? '',
    refresh_seconds: config.refresh_seconds === null ? '' : String(config.refresh_seconds),
    unit_multiplier: String(config.unit.multiplier),
    unit_currency: config.unit.currency,
    unit_fx: config.unit.fx_to_usd === null ? '' : String(config.unit.fx_to_usd),
    unit_symbol: config.unit.display.symbol,
    unit_decimals: String(config.unit.display.decimals),
    unit_note: config.unit.note ?? '',
    active: null,
  };
}

function catalogConfigFromForm(form: CatalogForm): CatalogConfig {
  return {
    entries: form.entries.trim(),
    model_id: form.model_id.trim(),
    input: pointerOrNull(form.input),
    output: pointerOrNull(form.output),
    cache_read: pointerOrNull(form.cache_read),
    cache_write: pointerOrNull(form.cache_write),
    reasoning: pointerOrNull(form.reasoning),
    unit: {
      source: form.unit_source,
      multiplier: numberOr(form.unit_multiplier, 1),
      currency: form.unit_currency.trim().toUpperCase() || 'USD',
      fx_to_usd: form.unit_fx.trim() === '' ? null : numberOr(form.unit_fx, 0),
      display: {
        symbol: form.unit_symbol || '$',
        decimals: clampDecimals(numberOr(form.unit_decimals, 4)),
      },
      note: form.unit_note.trim() === '' ? null : form.unit_note,
    },
    modalities_input: pointerOrNull(form.modalities_input),
    modalities_output: pointerOrNull(form.modalities_output),
    context_length: pointerOrNull(form.context_length),
    max_output: pointerOrNull(form.max_output),
    apply_on_discovery: form.apply_on_discovery,
    refresh_seconds:
      form.refresh_seconds.trim() === '' ? null : Math.trunc(numberOr(form.refresh_seconds, 0)),
  };
}

function balanceConfigFromForm(form: BalanceForm): BalanceConfig {
  return {
    balance: form.balance.trim(),
    subtract: pointerOrNull(form.subtract),
    available: pointerOrNull(form.available),
    currency: form.currency.trim() === '' ? null : form.currency.trim(),
    period_spend: pointerOrNull(form.period_spend),
    burn_rate: pointerOrNull(form.burn_rate),
    days_remaining: pointerOrNull(form.days_remaining),
    unit: {
      multiplier: numberOr(form.unit_multiplier, 1),
      currency: form.unit_currency.trim().toUpperCase() || 'USD',
      fx_to_usd: form.unit_fx.trim() === '' ? null : numberOr(form.unit_fx, 0),
      display: {
        symbol: form.unit_symbol || '$',
        decimals: clampDecimals(numberOr(form.unit_decimals, 2)),
      },
      note: form.unit_note.trim() === '' ? null : form.unit_note,
    },
    refresh_seconds:
      form.refresh_seconds.trim() === '' ? null : Math.trunc(numberOr(form.refresh_seconds, 900)),
  };
}

function priceAssignments(form: CatalogForm): PointerAssignment[] {
  return PRICE_POINTER_FIELDS.map((field) => ({ field, pointer: form[field].trim() }));
}

function pointerOrNull(value: string): string | null {
  const trimmed = value.trim();
  return trimmed === '' ? null : trimmed;
}

function numberOr(value: string, fallback: number): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function clampDecimals(value: number): number {
  return Math.min(8, Math.max(0, Math.trunc(value)));
}

function toSampleState(view: SampleView): SampleState {
  try {
    return {
      url: view.url,
      status: view.status,
      bytes: view.bytes,
      truncated: view.truncated,
      fetchedAt: view.fetched_at,
      document: JSON.parse(view.body) as unknown,
      error: null,
    };
  } catch {
    return {
      url: view.url,
      status: view.status,
      bytes: view.bytes,
      truncated: view.truncated,
      fetchedAt: view.fetched_at,
      document: undefined,
      error: 'The response is not valid JSON.',
    };
  }
}

function sampleError(err: unknown): SampleState {
  return {
    url: '',
    status: 0,
    bytes: 0,
    truncated: false,
    fetchedAt: new Date().toISOString(),
    document: undefined,
    error: describeError(err, 'Fetching the sample failed.'),
  };
}

function kindLabel(kind: ConnectorKind): string {
  return kind === 'catalog' ? 'Prices' : kind === 'balance' ? 'Balance' : 'Health';
}

function summarizeTest(result: ConnectorTestView): string {
  if (result.kind === 'catalog') {
    return `${result.matched ?? 0} priced, ${result.skipped ?? 0} skipped, ${
      result.samples.filter((sample) => sample.error !== null).length
    } errors`;
  }
  if (result.kind === 'balance') {
    return result.balance?.available === true
      ? `${currencyAmount(result.balance.balance_usd, result.balance.currency)} available`
      : 'balance unavailable';
  }
  return result.health?.ok === true ? 'healthy' : 'unhealthy';
}

function refreshSummary(result: PriceRefreshView): string {
  if (!result.ok) {
    return result.error ?? 'The price refresh failed.';
  }
  return `Prices refreshed: ${result.created} created · ${result.updated} updated · ${result.unchanged} unchanged · ${result.skipped_manual} manual kept · ${result.skipped_unresolved} unresolved.`;
}

function currencyAmount(value: number | null, currency: string | null): string {
  if (value === null) {
    return '—';
  }
  const code = (currency ?? 'USD').toUpperCase();
  const symbol = code === 'USD' ? '$' : code === 'EUR' ? '€' : code === 'GBP' ? '£' : `${code} `;
  return `${symbol}${value.toFixed(2)}`;
}

function formatDays(days: number): string {
  if (days >= 1) {
    return `${Math.floor(days)} ${Math.floor(days) === 1 ? 'day' : 'days'}`;
  }
  return `${Math.max(0, Math.round(days * 24))} h`;
}
