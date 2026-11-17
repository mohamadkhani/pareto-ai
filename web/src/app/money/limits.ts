import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import {
  AbstractControl,
  FormControl,
  FormGroup,
  ReactiveFormsModule,
  ValidationErrors,
  Validators,
} from '@angular/forms';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from '../catalog/catalog.service';
import type { Model } from '../catalog/catalog.types';
import { describeError, isForbidden } from '../catalog/errors';
import { formatTime, formatUsd } from '../catalog/format';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiCard } from '../ui/card';
import { UiDialog } from '../ui/dialog';
import { UiField } from '../ui/field';
import { UiTable } from '../ui/table';
import { UsersService } from '../users/users.service';
import type { UserView } from '../users/users.types';
import { MoneyService } from './money.service';
import { MONEY_PERIODS, type BudgetView, type MoneyPeriod, type TokenCapView } from './money.types';

type CapFormState = { mode: 'create' } | { mode: 'edit'; cap: TokenCapView };

/** Requires a whole number of 0 or more (0 blocks all spend/use). */
function nonNegativeInteger(control: AbstractControl): ValidationErrors | null {
  const value: unknown = control.value;
  if (value === null || value === '') {
    return null;
  }
  const valid = typeof value === 'number' && Number.isInteger(value) && value >= 0;
  return valid ? null : { nonNegativeInteger: true };
}

/** How full a limit is, 0–100; a zero limit is fully locked. */
function progressPercent(used: number, limit: number): number {
  if (limit <= 0) {
    return used > 0 ? 100 : 0;
  }
  return Math.min(100, Math.round((used / limit) * 100));
}

/** Budgets and token caps per user: hard spend and usage gates. */
@Component({
  selector: 'app-limits',
  imports: [ReactiveFormsModule, UiBanner, UiButton, UiCard, UiDialog, UiField, UiTable],
  templateUrl: './limits.html',
  styleUrl: './limits.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Limits {
  private readonly auth = inject(AuthService);
  private readonly money = inject(MoneyService);
  private readonly usersService = inject(UsersService);
  private readonly catalog = inject(CatalogService);

  protected readonly users = signal<UserView[]>([]);
  protected readonly models = signal<Model[]>([]);
  protected readonly budgets = signal<BudgetView[]>([]);
  protected readonly caps = signal<TokenCapView[]>([]);
  protected readonly selectedUserId = signal('');
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly actionError = signal<string | null>(null);
  protected readonly savingKey = signal<string | null>(null);
  protected readonly deletingKey = signal<string | null>(null);
  protected readonly budgetRemoveTarget = signal<MoneyPeriod | null>(null);
  protected readonly capRemoveTarget = signal<TokenCapView | null>(null);

  protected readonly capFormState = signal<CapFormState | null>(null);
  protected readonly attempted = signal(false);
  protected readonly savingCap = signal(false);

  protected readonly periods = MONEY_PERIODS;
  protected readonly formatTime = formatTime;
  protected readonly formatUsd = formatUsd;
  protected readonly progressPercent = progressPercent;
  protected readonly isAdmin = this.auth.isAdmin;

  /** Token counts with grouping separators. */
  protected formatTokens(value: number): string {
    return value.toLocaleString();
  }

  /** "never" for `total` budgets and caps, the reset instant otherwise. */
  protected resetText(iso: string | null | undefined): string {
    return iso === null || iso === undefined ? 'never' : formatTime(iso);
  }

  /** Per-period USD draft; `null` means "keep the stored limit". */
  protected readonly budgetForm = new FormGroup({
    day: new FormControl<number | null>(null, { validators: [Validators.min(0)] }),
    week: new FormControl<number | null>(null, { validators: [Validators.min(0)] }),
    month: new FormControl<number | null>(null, { validators: [Validators.min(0)] }),
    total: new FormControl<number | null>(null, { validators: [Validators.min(0)] }),
  });

  protected readonly capForm = new FormGroup({
    model_id: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    period: new FormControl<MoneyPeriod>('day', { nonNullable: true }),
    limit_tokens: new FormControl<number | null>(null, {
      validators: [Validators.required, nonNegativeInteger],
    }),
  });

  constructor() {
    this.selectedUserId.set(this.auth.user()?.id ?? '');
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

  /** Admins may inspect another account; non-admins stay on their own. */
  protected onUserChange(event: Event): void {
    const select = event.target;
    if (!(select instanceof HTMLSelectElement)) {
      return;
    }
    this.selectedUserId.set(select.value);
    void this.reload();
  }

  protected async reload(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      const userId = this.selectedUserId();
      // Budgets and caps are readable for the caller's own account, so they
      // are the only calls that decide the page state. The model catalog is
      // admin-only and feeds the cap picker; a refusal there must not hide
      // the self-service budgets.
      const [budgets, caps] = await Promise.all([
        this.money.listBudgets(userId),
        this.money.listTokenCaps(userId),
      ]);
      this.budgets.set(budgets);
      this.caps.set(caps);
      // Prefill the drafts with the stored limits; null stays "keep".
      this.budgetForm.reset({
        day: this.budgetOf('day')?.limit_usd ?? null,
        week: this.budgetOf('week')?.limit_usd ?? null,
        month: this.budgetOf('month')?.limit_usd ?? null,
        total: this.budgetOf('total')?.limit_usd ?? null,
      });
    } catch (err) {
      if (isForbidden(err)) {
        this.loadError.set('You may only read budgets and token caps for your own account.');
      } else {
        this.loadError.set(describeError(err, 'Loading limits failed.'));
      }
    } finally {
      this.loading.set(false);
    }
    if (this.auth.isAdmin()) {
      await this.loadModels();
    }
  }

  /** Loads the catalog for the cap model picker; failures never blank limits. */
  private async loadModels(): Promise<void> {
    try {
      this.models.set(await this.catalog.listModels());
    } catch (err) {
      this.actionError.set(
        isForbidden(err)
          ? 'The model catalog is restricted to administrators.'
          : describeError(err, 'Loading models failed.'),
      );
    }
  }

  /** Saves one period's USD limit; an empty or unchanged input keeps it. */
  protected async saveBudget(period: MoneyPeriod): Promise<void> {
    const value = this.budgetForm.controls[period].value;
    const current = this.budgetOf(period);
    if (value === null || this.budgetForm.controls[period].invalid) {
      return;
    }
    if (current !== undefined && current.limit_usd === value) {
      return;
    }
    this.savingKey.set(period);
    this.actionError.set(null);
    try {
      await this.money.setBudget(this.selectedUserId(), period, { limit_usd: value });
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the budget failed.'));
    } finally {
      this.savingKey.set(null);
    }
  }

  /** Removes one period's budget (unlimited again). */
  protected async removeBudget(period: MoneyPeriod): Promise<void> {
    this.deletingKey.set(period);
    this.actionError.set(null);
    try {
      await this.money.clearBudget(this.selectedUserId(), period);
      this.budgetRemoveTarget.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Removing the budget failed.'));
    } finally {
      this.deletingKey.set(null);
    }
  }

  protected openBudgetRemove(period: MoneyPeriod): void {
    this.budgetRemoveTarget.set(period);
  }

  protected closeBudgetRemove(): void {
    this.budgetRemoveTarget.set(null);
  }

  protected confirmBudgetRemove(): void {
    const period = this.budgetRemoveTarget();
    if (period !== null) {
      void this.removeBudget(period);
    }
  }

  protected openCapCreate(): void {
    this.capForm.reset({ model_id: '', period: 'day', limit_tokens: null });
    this.attempted.set(false);
    this.actionError.set(null);
    this.capFormState.set({ mode: 'create' });
  }

  protected openCapEdit(cap: TokenCapView): void {
    this.capForm.reset({
      model_id: cap.model_id,
      period: cap.period,
      limit_tokens: cap.limit_tokens,
    });
    this.attempted.set(false);
    this.actionError.set(null);
    this.capFormState.set({ mode: 'edit', cap });
  }

  protected closeCapForm(): void {
    this.capFormState.set(null);
  }

  /** Saves one token cap; an unchanged input keeps the stored limit. */
  protected async saveCap(): Promise<void> {
    this.attempted.set(true);
    this.actionError.set(null);
    const state = this.capFormState();
    if (state === null || this.capForm.invalid) {
      return;
    }
    const input = this.capForm.getRawValue();
    if (
      state.mode === 'edit' &&
      input.limit_tokens !== null &&
      input.limit_tokens === state.cap.limit_tokens
    ) {
      this.capFormState.set(null);
      return;
    }
    this.savingCap.set(true);
    try {
      const modelId = state.mode === 'create' ? input.model_id : state.cap.model_id;
      const period = state.mode === 'create' ? input.period : state.cap.period;
      await this.money.setTokenCap(this.selectedUserId(), modelId, period, {
        limit_tokens: input.limit_tokens ?? 0,
      });
      this.capFormState.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the token cap failed.'));
    } finally {
      this.savingCap.set(false);
    }
  }

  /** Removes one token cap (unlimited again). */
  protected async removeCap(cap: TokenCapView): Promise<void> {
    this.deletingKey.set(`${cap.model_id}|${cap.period}`);
    this.actionError.set(null);
    try {
      await this.money.clearTokenCap(this.selectedUserId(), cap.model_id, cap.period);
      this.capRemoveTarget.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Removing the token cap failed.'));
    } finally {
      this.deletingKey.set(null);
    }
  }

  protected openCapRemove(cap: TokenCapView): void {
    this.capRemoveTarget.set(cap);
  }

  protected closeCapRemove(): void {
    this.capRemoveTarget.set(null);
  }

  protected confirmCapRemove(): void {
    const cap = this.capRemoveTarget();
    if (cap !== null) {
      void this.removeCap(cap);
    }
  }

  protected budgetOf(period: MoneyPeriod): BudgetView | undefined {
    return this.budgets().find((budget) => budget.period === period);
  }

  protected modelName(id: string): string {
    return this.models().find((model) => model.id === id)?.canonical_name ?? id;
  }

  protected showCapError(field: 'model_id' | 'limit_tokens'): boolean {
    const control = this.capForm.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected capFieldError(field: 'model_id' | 'limit_tokens'): string {
    const control = this.capForm.controls[field];
    if (control.hasError('required')) {
      return field === 'model_id' ? 'Choose a model.' : 'A limit is required.';
    }
    if (control.hasError('nonNegativeInteger')) {
      return 'Use a whole number of 0 or more.';
    }
    return '';
  }

  protected showBudgetError(period: MoneyPeriod): boolean {
    return this.budgetForm.controls[period].invalid;
  }
}
