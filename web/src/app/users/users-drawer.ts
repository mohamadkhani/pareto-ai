import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  input,
  output,
  signal,
} from '@angular/core';
import { CatalogService } from '../catalog/catalog.service';
import type { Credential, Model, Offering, Provider } from '../catalog/catalog.types';
import { describeError } from '../catalog/errors';
import { formatTime, formatUsd } from '../catalog/format';
import { GrantsService } from '../grants/grants.service';
import type { GrantMode, GrantResourceType, GrantView } from '../grants/grants.types';
import { KeysService } from '../keys/keys.service';
import type { ApiKeyView, CreatedKey } from '../keys/keys.types';
import { MoneyService } from '../money/money.service';
import {
  MONEY_PERIODS,
  type BudgetView,
  type MoneyPeriod,
  type TokenCapView,
} from '../money/money.types';
import { UiBadge } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiCheckbox } from '../ui/checkbox';
import { UiDialog } from '../ui/dialog';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';
import { UiMeter } from '../ui/meter';
import { UiSelect } from '../ui/select';
import { UiSpinner } from '../ui/spinner';
import { UiTable } from '../ui/table';
import { type TabItem, UiTabs } from '../ui/tabs';
import { UsersService } from './users.service';
import { MIN_PASSWORD_LEN, type UserView } from './users.types';

/** The drawer's four tabs, in the order the task defines them. */
type DrawerTab = 'limits' | 'grants' | 'keys' | 'overview';

/** One selectable resource in the advanced grant form. */
interface ResourceOption {
  id: string;
  label: string;
}

/** The plaintext key shown once after creation. */
interface ShownKey {
  name: string;
  key: string;
}

/** How full a limit is, 0–100; a zero limit is fully locked. */
function percent(used: number, limit: number): number {
  if (limit <= 0) {
    return used > 0 ? 100 : 0;
  }
  return Math.min(100, Math.round((used / limit) * 100));
}

/** Tone for a percent-of-limit meter. */
function meterTone(value: number): 'accent' | 'warning' | 'danger' {
  return value >= 100 ? 'danger' : value >= 80 ? 'warning' : 'accent';
}

/** `model|period` key for one cap row. */
function capKey(modelId: string, period: MoneyPeriod): string {
  return `${modelId}|${period}`;
}

/**
 * The focused per-user path: limits, grants, keys, and account state in a
 * right-edge drawer. Tabs load lazily, each tab keeps its own error, and the
 * grants tab defaults to a model allow-list whose changes map to `allow`
 * grants only (deny grants stay untouched and are edited under Advanced).
 */
@Component({
  selector: 'app-user-drawer',
  imports: [
    UiBadge,
    UiBanner,
    UiButton,
    UiCheckbox,
    UiDialog,
    UiEmpty,
    UiField,
    UiInput,
    UiMeter,
    UiSelect,
    UiSpinner,
    UiTable,
    UiTabs,
  ],
  templateUrl: './users-drawer.html',
  styleUrl: './users-drawer.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UserDrawer implements OnInit {
  private readonly money = inject(MoneyService);
  private readonly grantsService = inject(GrantsService);
  private readonly keysService = inject(KeysService);
  private readonly usersService = inject(UsersService);
  private readonly catalog = inject(CatalogService);

  readonly user = input.required<UserView>();
  readonly closed = output<void>();
  readonly userChanged = output<UserView>();

  protected readonly tabs: readonly TabItem[] = [
    { id: 'limits', label: 'Limits' },
    { id: 'grants', label: 'Grants' },
    { id: 'keys', label: 'Keys' },
    { id: 'overview', label: 'Overview' },
  ];

  protected readonly tab = signal<DrawerTab>('limits');
  protected readonly periods = MONEY_PERIODS;
  protected readonly minPasswordLength = MIN_PASSWORD_LEN;
  protected readonly formatTime = formatTime;
  protected readonly formatUsd = formatUsd;
  protected readonly percent = percent;

  protected readonly notice = signal<string | null>(null);
  protected readonly actionError = signal<string | null>(null);

  private readonly loaded = new Set<DrawerTab>();

  // --- Limits -------------------------------------------------------------

  protected readonly budgets = signal<BudgetView[]>([]);
  protected readonly caps = signal<TokenCapView[]>([]);
  protected readonly models = signal<Model[]>([]);
  protected readonly limitsLoading = signal(false);
  protected readonly limitsError = signal<string | null>(null);
  protected readonly budgetDraft = signal<Partial<Record<MoneyPeriod, number | null>>>({});
  protected readonly capDraft = signal<Readonly<Record<string, number | null>>>({});
  protected readonly savingBudget = signal<MoneyPeriod | null>(null);
  protected readonly savingCap = signal<string | null>(null);
  protected readonly removingCap = signal<string | null>(null);
  protected readonly newCap = signal<{
    modelId: string;
    period: MoneyPeriod;
    limit: number | null;
  }>({
    modelId: '',
    period: 'day',
    limit: null,
  });
  protected readonly addingCap = signal(false);

  // --- Grants -------------------------------------------------------------

  protected readonly grants = signal<GrantView[]>([]);
  protected readonly grantModels = signal<Model[]>([]);
  protected readonly grantsLoading = signal(false);
  protected readonly grantsError = signal<string | null>(null);
  protected readonly checked = signal<ReadonlySet<string>>(new Set());
  protected readonly search = signal('');
  protected readonly savingGrants = signal(false);
  protected readonly advanced = signal(false);
  protected readonly advancedLoaded = signal(false);
  protected readonly offerings = signal<Offering[]>([]);
  protected readonly providers = signal<Provider[]>([]);
  protected readonly credentials = signal<Credential[]>([]);
  protected readonly resourceType = signal<GrantResourceType>('model');
  protected readonly resourceId = signal('');
  protected readonly grantMode = signal<GrantMode>('allow');
  protected readonly addingGrant = signal(false);

  protected readonly deniedIds = computed(
    () =>
      new Set(
        this.grants()
          .filter((grant) => grant.resource_type === 'model' && grant.mode === 'deny')
          .map((grant) => grant.resource_id),
      ),
  );

  protected readonly filteredModels = computed(() => {
    const term = this.search().trim().toLowerCase();
    const models = this.grantModels();
    if (term === '') {
      return models;
    }
    return models.filter(
      (model) =>
        model.canonical_name.toLowerCase().includes(term) ||
        (model.family ?? '').toLowerCase().includes(term),
    );
  });

  protected readonly resourceOptions = computed<ResourceOption[]>(() => {
    switch (this.resourceType()) {
      case 'model':
        return this.grantModels().map((model) => ({
          id: model.id,
          label: model.canonical_name,
        }));
      case 'offering':
        return this.offerings().map((offering) => ({
          id: offering.id,
          label: `${this.providerName(offering.provider_id)} → ${this.modelName(
            offering.model_id,
          )} (${offering.upstream_model_id})`,
        }));
      case 'credential':
        return this.credentials().map((credential) => ({
          id: credential.id,
          label: `${this.providerName(credential.provider_id)} · ${credential.label}`,
        }));
    }
  });

  // --- Keys ---------------------------------------------------------------

  protected readonly keys = signal<ApiKeyView[]>([]);
  protected readonly keysLoading = signal(false);
  protected readonly keysError = signal<string | null>(null);
  protected readonly keyName = signal('');
  protected readonly creatingKey = signal(false);
  protected readonly createdKey = signal<ShownKey | null>(null);
  protected readonly copied = signal(false);
  protected readonly revokeTarget = signal<ApiKeyView | null>(null);
  protected readonly revoking = signal(false);

  // --- Overview -----------------------------------------------------------

  protected readonly resetOpen = signal(false);
  protected readonly newPassword = signal('');
  protected readonly resetting = signal(false);
  protected readonly deactivateArmed = signal(false);
  protected readonly savingActive = signal(false);

  ngOnInit(): void {
    this.ensureLoaded(this.tab());
  }

  protected onTab(tab: string): void {
    this.tab.set(tab as DrawerTab);
    this.notice.set(null);
    this.actionError.set(null);
    this.ensureLoaded(this.tab());
  }

  protected close(): void {
    this.closed.emit();
  }

  protected onClosed(): void {
    this.closed.emit();
  }

  private ensureLoaded(tab: DrawerTab): void {
    if (this.loaded.has(tab)) {
      return;
    }
    this.loaded.add(tab);
    switch (tab) {
      case 'limits':
        void this.loadLimits();
        break;
      case 'grants':
        void this.loadGrants();
        break;
      case 'keys':
        void this.loadKeys();
        break;
      case 'overview':
        break;
    }
  }

  // --- Limits methods -----------------------------------------------------

  protected async loadLimits(): Promise<void> {
    this.limitsLoading.set(true);
    this.limitsError.set(null);
    try {
      const userId = this.user().id;
      const [budgets, caps, models] = await Promise.all([
        this.money.listBudgets(userId),
        this.money.listTokenCaps(userId),
        this.catalog.listModels().catch(() => [] as Model[]),
      ]);
      this.budgets.set(budgets);
      this.caps.set(caps);
      this.models.set(models);
      const draft: Partial<Record<MoneyPeriod, number | null>> = {};
      const capsDraft: Record<string, number | null> = {};
      for (const period of MONEY_PERIODS) {
        draft[period] = budgets.find((budget) => budget.period === period)?.limit_usd ?? null;
      }
      for (const cap of caps) {
        capsDraft[capKey(cap.model_id, cap.period)] = cap.limit_tokens;
      }
      this.budgetDraft.set(draft);
      this.capDraft.set(capsDraft);
    } catch (err) {
      this.limitsError.set(describeError(err, 'Loading limits failed.'));
    } finally {
      this.limitsLoading.set(false);
    }
  }

  protected budgetOf(period: MoneyPeriod): BudgetView | undefined {
    return this.budgets().find((budget) => budget.period === period);
  }

  protected budgetValue(period: MoneyPeriod): number | null {
    return this.budgetDraft()[period] ?? null;
  }

  protected onBudgetInput(period: MoneyPeriod, value: string | number | null): void {
    this.budgetDraft.update((draft) => ({
      ...draft,
      [period]: typeof value === 'number' ? value : null,
    }));
  }

  /** Saves one budget; `null` keeps the stored limit (D20). */
  protected async saveBudget(period: MoneyPeriod): Promise<void> {
    const value = this.budgetValue(period);
    if (value === null || !Number.isFinite(value) || value < 0) {
      return;
    }
    if (this.budgetOf(period)?.limit_usd === value) {
      return;
    }
    this.savingBudget.set(period);
    this.actionError.set(null);
    try {
      await this.money.setBudget(this.user().id, period, { limit_usd: value });
      this.notice.set(`${period} budget saved.`);
      await this.loadLimits();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the budget failed.'));
    } finally {
      this.savingBudget.set(null);
    }
  }

  protected async clearBudget(period: MoneyPeriod): Promise<void> {
    this.savingBudget.set(period);
    this.actionError.set(null);
    try {
      await this.money.clearBudget(this.user().id, period);
      this.notice.set(`${period} budget removed (unlimited).`);
      await this.loadLimits();
    } catch (err) {
      this.actionError.set(describeError(err, 'Removing the budget failed.'));
    } finally {
      this.savingBudget.set(null);
    }
  }

  protected capValue(cap: TokenCapView): number | null {
    return this.capDraft()[capKey(cap.model_id, cap.period)] ?? null;
  }

  protected onCapInput(cap: TokenCapView, value: string | number | null): void {
    const key = capKey(cap.model_id, cap.period);
    this.capDraft.update((draft) => ({
      ...draft,
      [key]: typeof value === 'number' ? value : null,
    }));
  }

  protected async saveCap(cap: TokenCapView): Promise<void> {
    const value = this.capValue(cap);
    if (value === null || !Number.isFinite(value) || value < 0) {
      return;
    }
    if (cap.limit_tokens === value) {
      return;
    }
    const key = capKey(cap.model_id, cap.period);
    this.savingCap.set(key);
    this.actionError.set(null);
    try {
      await this.money.setTokenCap(this.user().id, cap.model_id, cap.period, {
        limit_tokens: value,
      });
      this.notice.set('Token cap saved.');
      await this.loadLimits();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the token cap failed.'));
    } finally {
      this.savingCap.set(null);
    }
  }

  protected async removeCap(cap: TokenCapView): Promise<void> {
    const key = capKey(cap.model_id, cap.period);
    this.removingCap.set(key);
    this.actionError.set(null);
    try {
      await this.money.clearTokenCap(this.user().id, cap.model_id, cap.period);
      this.notice.set('Token cap removed (unlimited).');
      await this.loadLimits();
    } catch (err) {
      this.actionError.set(describeError(err, 'Removing the token cap failed.'));
    } finally {
      this.removingCap.set(null);
    }
  }

  protected setNewCapModel(value: string): void {
    this.newCap.update((draft) => ({ ...draft, modelId: value }));
  }

  protected setNewCapPeriod(value: string): void {
    this.newCap.update((draft) => ({ ...draft, period: value as MoneyPeriod }));
  }

  protected setNewCapLimit(value: string | number | null): void {
    this.newCap.update((draft) => ({
      ...draft,
      limit: typeof value === 'number' ? value : null,
    }));
  }

  protected async addCap(): Promise<void> {
    const draft = this.newCap();
    if (
      draft.modelId === '' ||
      draft.limit === null ||
      !Number.isFinite(draft.limit) ||
      draft.limit < 0
    ) {
      return;
    }
    this.addingCap.set(true);
    this.actionError.set(null);
    try {
      await this.money.setTokenCap(this.user().id, draft.modelId, draft.period, {
        limit_tokens: draft.limit,
      });
      this.notice.set('Token cap added.');
      this.newCap.set({ modelId: '', period: 'day', limit: null });
      await this.loadLimits();
    } catch (err) {
      this.actionError.set(describeError(err, 'Adding the token cap failed.'));
    } finally {
      this.addingCap.set(false);
    }
  }

  protected capPercent(cap: TokenCapView): number {
    return percent(cap.used_tokens, cap.limit_tokens);
  }

  protected capTone(cap: TokenCapView): 'accent' | 'warning' | 'danger' {
    return meterTone(this.capPercent(cap));
  }

  protected budgetPercent(budget: BudgetView): number {
    return percent(budget.used_usd, budget.limit_usd);
  }

  protected budgetTone(budget: BudgetView): 'accent' | 'warning' | 'danger' {
    return meterTone(this.budgetPercent(budget));
  }

  protected formatTokens(value: number): string {
    return value.toLocaleString();
  }

  protected modelName(modelId: string): string {
    return this.models().find((model) => model.id === modelId)?.canonical_name ?? modelId;
  }

  // --- Grants methods -----------------------------------------------------

  protected async loadGrants(): Promise<void> {
    this.grantsLoading.set(true);
    this.grantsError.set(null);
    try {
      const userId = this.user().id;
      const [grants, models] = await Promise.all([
        this.grantsService.list({ subject_type: 'user', subject_id: userId }),
        this.catalog.listModels().catch(() => [] as Model[]),
      ]);
      this.grants.set(grants.filter((grant) => grant.subject_type === 'user'));
      this.grantModels.set(models);
      this.checked.set(
        new Set(
          grants
            .filter((grant) => grant.resource_type === 'model' && grant.mode === 'allow')
            .map((grant) => grant.resource_id),
        ),
      );
    } catch (err) {
      this.grantsError.set(describeError(err, 'Loading grants failed.'));
    } finally {
      this.grantsLoading.set(false);
    }
  }

  protected onSearch(value: string | number | null): void {
    this.search.set(typeof value === 'string' ? value : '');
  }

  protected isChecked(modelId: string): boolean {
    return this.checked().has(modelId);
  }

  protected toggleModel(modelId: string, value: boolean): void {
    this.checked.update((current) => {
      const next = new Set(current);
      if (value) {
        next.add(modelId);
      } else {
        next.delete(modelId);
      }
      return next;
    });
  }

  protected allowAll(): void {
    this.checked.set(new Set(this.grantModels().map((model) => model.id)));
  }

  protected clearAll(): void {
    this.checked.set(new Set());
  }

  /** Maps the checkbox set to allow grants; deny grants are never touched. */
  protected async saveAllowList(): Promise<void> {
    const userId = this.user().id;
    const existingAllow = this.grants().filter(
      (grant) =>
        grant.subject_type === 'user' &&
        grant.resource_type === 'model' &&
        grant.mode === 'allow' &&
        grant.subject_id === userId,
    );
    const existingIds = new Set(existingAllow.map((grant) => grant.resource_id));
    const checked = this.checked();
    const toAdd = [...checked].filter((id) => !existingIds.has(id));
    const toRemove = existingAllow.filter((grant) => !checked.has(grant.resource_id));
    if (toAdd.length === 0 && toRemove.length === 0) {
      this.notice.set('No grant changes to save.');
      return;
    }
    this.savingGrants.set(true);
    this.actionError.set(null);
    try {
      await Promise.all([
        ...toAdd.map((id) =>
          this.grantsService.create({
            subject_type: 'user',
            subject_id: userId,
            resource_type: 'model',
            resource_id: id,
            mode: 'allow',
          }),
        ),
        ...toRemove.map((grant) => this.grantsService.remove(grant.id)),
      ]);
      this.notice.set(`Allow-list saved: ${toAdd.length} added, ${toRemove.length} removed.`);
      await this.loadGrants();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the allow-list failed.'));
    } finally {
      this.savingGrants.set(false);
    }
  }

  protected async toggleAdvanced(): Promise<void> {
    const next = !this.advanced();
    this.advanced.set(next);
    if (next) {
      await this.loadAdvanced();
    }
  }

  private async loadAdvanced(): Promise<void> {
    if (this.advancedLoaded()) {
      return;
    }
    try {
      const [offerings, providers] = await Promise.all([
        this.catalog.listOfferings().catch(() => [] as Offering[]),
        this.catalog.listProviders().catch(() => [] as Provider[]),
      ]);
      const credentials = (
        await Promise.all(
          providers.map((provider) =>
            this.catalog.listCredentials(provider.id).catch(() => [] as Credential[]),
          ),
        )
      ).flat();
      this.offerings.set(offerings);
      this.providers.set(providers);
      this.credentials.set(credentials);
      this.advancedLoaded.set(true);
    } catch (err) {
      this.actionError.set(describeError(err, 'Loading the grant matrix failed.'));
    }
  }

  protected providerName(providerId: string): string {
    return this.providers().find((provider) => provider.id === providerId)?.name ?? providerId;
  }

  protected setResourceType(value: string): void {
    this.resourceType.set(value as GrantResourceType);
    this.resourceId.set('');
  }

  protected setResourceId(value: string): void {
    this.resourceId.set(value);
  }

  protected setGrantMode(value: string): void {
    this.grantMode.set(value as GrantMode);
  }

  protected async addGrant(): Promise<void> {
    if (this.resourceId() === '') {
      return;
    }
    this.addingGrant.set(true);
    this.actionError.set(null);
    try {
      await this.grantsService.create({
        subject_type: 'user',
        subject_id: this.user().id,
        resource_type: this.resourceType(),
        resource_id: this.resourceId(),
        mode: this.grantMode(),
      });
      this.notice.set('Grant added.');
      this.resourceId.set('');
      await this.loadGrants();
    } catch (err) {
      this.actionError.set(describeError(err, 'Adding the grant failed.'));
    } finally {
      this.addingGrant.set(false);
    }
  }

  protected async removeGrant(grant: GrantView): Promise<void> {
    this.actionError.set(null);
    try {
      await this.grantsService.remove(grant.id);
      this.notice.set('Grant removed.');
      await this.loadGrants();
    } catch (err) {
      this.actionError.set(describeError(err, 'Removing the grant failed.'));
    }
  }

  protected resourceLabel(grant: GrantView): string {
    switch (grant.resource_type) {
      case 'model':
        return (
          this.grantModels().find((model) => model.id === grant.resource_id)?.canonical_name ??
          grant.resource_id
        );
      case 'offering':
        return grant.resource_id;
      case 'credential':
        return (
          this.credentials().find((credential) => credential.id === grant.resource_id)?.label ??
          grant.resource_id
        );
    }
  }

  // --- Keys methods -------------------------------------------------------

  protected async loadKeys(): Promise<void> {
    this.keysLoading.set(true);
    this.keysError.set(null);
    try {
      this.keys.set(await this.keysService.list(this.user().id));
    } catch (err) {
      this.keysError.set(describeError(err, 'Loading API keys failed.'));
    } finally {
      this.keysLoading.set(false);
    }
  }

  protected setKeyName(value: string | number | null): void {
    this.keyName.set(typeof value === 'string' ? value : '');
  }

  protected async createKey(): Promise<void> {
    const name = this.keyName().trim();
    if (name === '') {
      return;
    }
    this.creatingKey.set(true);
    this.actionError.set(null);
    try {
      const created: CreatedKey = await this.keysService.create(this.user().id, { name });
      this.createdKey.set({ name: created.api_key.name, key: created.key });
      this.copied.set(false);
      this.keyName.set('');
      await this.loadKeys();
    } catch (err) {
      this.actionError.set(describeError(err, 'Creating the key failed.'));
    } finally {
      this.creatingKey.set(false);
    }
  }

  protected async copyKey(): Promise<void> {
    const created = this.createdKey();
    if (created === null) {
      return;
    }
    try {
      await navigator.clipboard.writeText(created.key);
      this.copied.set(true);
    } catch {
      this.copied.set(false);
    }
  }

  protected dismissKey(): void {
    this.createdKey.set(null);
    this.copied.set(false);
  }

  protected async revokeKey(): Promise<void> {
    const key = this.revokeTarget();
    if (key === null) {
      return;
    }
    this.revoking.set(true);
    this.actionError.set(null);
    try {
      await this.keysService.revoke(key.id);
      this.revokeTarget.set(null);
      this.notice.set('Key revoked.');
      await this.loadKeys();
    } catch (err) {
      this.actionError.set(describeError(err, 'Revoking the key failed.'));
    } finally {
      this.revoking.set(false);
    }
  }

  protected isExpired(key: ApiKeyView): boolean {
    return key.expires_at !== null && Date.parse(key.expires_at) <= Date.now();
  }

  // --- Overview methods ---------------------------------------------------

  protected setNewPassword(value: string | number | null): void {
    this.newPassword.set(typeof value === 'string' ? value : '');
  }

  protected async resetPassword(): Promise<void> {
    const password = this.newPassword();
    if (password.length < MIN_PASSWORD_LEN) {
      this.actionError.set(`Use at least ${MIN_PASSWORD_LEN} characters.`);
      return;
    }
    this.resetting.set(true);
    this.actionError.set(null);
    try {
      const updated = await this.usersService.update(this.user().id, { password });
      this.newPassword.set('');
      this.resetOpen.set(false);
      this.notice.set('Password reset. Existing keys and sessions were revoked.');
      this.userChanged.emit(updated);
      if (this.loaded.has('keys')) {
        await this.loadKeys();
      }
    } catch (err) {
      this.actionError.set(describeError(err, 'Resetting the password failed.'));
    } finally {
      this.resetting.set(false);
    }
  }

  /** Activates or deactivates the account; deactivation needs two clicks. */
  protected async setActive(active: boolean): Promise<void> {
    if (!active && !this.deactivateArmed()) {
      this.deactivateArmed.set(true);
      return;
    }
    this.savingActive.set(true);
    this.actionError.set(null);
    try {
      const updated = await this.usersService.update(this.user().id, { is_active: active });
      this.deactivateArmed.set(false);
      this.notice.set(active ? 'Account activated.' : 'Account deactivated.');
      this.userChanged.emit(updated);
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the account failed.'));
    } finally {
      this.savingActive.set(false);
    }
  }
}
