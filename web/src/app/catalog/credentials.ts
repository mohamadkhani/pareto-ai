import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
} from '@angular/core';
import { CatalogService } from './catalog.service';
import { BalanceCache } from './balance-cache';
import type {
  BalanceGate,
  Credential,
  CredentialInput,
  CredentialPatch,
  Provider,
} from './catalog.types';
import { describeError } from './errors';
import { formatAge, formatTime } from './format';
import { type HeaderRow, HeadersEditor, headerMapOf } from './headers-editor';
import { UiBadge, type BadgeTone } from '../ui/badge';
import { UiBanner, type BannerTone } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiDialog } from '../ui/dialog';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';
import { UiSelect } from '../ui/select';
import { UiTable } from '../ui/table';

/** The credential form's text fields; numbers stay strings until saved. */
interface CredentialDraft {
  label: string;
  secret: string;
  max_concurrency: string;
  balance_usd: string;
}

/** One test-probe outcome to surface above the list. */
interface TestBanner {
  tone: BannerTone;
  text: string;
}

function emptyDraft(): CredentialDraft {
  return { label: '', secret: '', max_concurrency: '', balance_usd: '' };
}

/**
 * Credentials for one provider: list, create/edit (secret write-only), delete,
 * and a health probe. Loaded whenever the `provider` input's id changes.
 */
@Component({
  selector: 'app-credentials',
  imports: [
    HeadersEditor,
    UiBadge,
    UiBanner,
    UiButton,
    UiDialog,
    UiEmpty,
    UiField,
    UiInput,
    UiSelect,
    UiTable,
  ],
  templateUrl: './credentials.html',
  styleUrl: './credentials.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Credentials {
  private readonly catalog = inject(CatalogService);
  private readonly balanceCache = inject(BalanceCache);

  readonly provider = input.required<Provider>();
  /** Raised when a balance refresh changes data the provider row renders. */
  readonly changed = output<void>();

  protected readonly credentials = signal<Credential[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly actionError = signal<string | null>(null);
  protected readonly notice = signal<string | null>(null);
  protected readonly testBanner = signal<TestBanner | null>(null);

  protected readonly formOpen = signal(false);
  protected readonly editing = signal<Credential | null>(null);
  protected readonly draft = signal<CredentialDraft>(emptyDraft());
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly formError = signal<string | null>(null);
  protected readonly headerRows = signal<HeaderRow[]>([]);
  protected readonly headersTouched = signal(false);

  protected readonly deleting = signal<Credential | null>(null);
  protected readonly deletePending = signal(false);
  protected readonly testingId = signal<string | null>(null);
  protected readonly refreshingBalanceId = signal<string | null>(null);
  protected readonly gateSavingId = signal<string | null>(null);

  protected readonly formatTime = formatTime;
  protected readonly formatAge = formatAge;

  protected readonly errors = computed<Partial<Record<keyof CredentialDraft, string>>>(() =>
    validateDraft(this.draft(), this.editing() === null, this.attempted()),
  );

  protected readonly formTitle = computed(() =>
    this.editing() === null ? 'New credential' : `Edit ${this.editing()?.label ?? ''}`,
  );

  private readonly providerId = computed(() => this.provider().id);

  constructor() {
    effect(() => {
      const providerId = this.providerId();
      this.formOpen.set(false);
      this.editing.set(null);
      void this.load(providerId);
    });
  }

  protected async refresh(): Promise<void> {
    await this.load(this.providerId());
  }

  protected openCreate(): void {
    this.editing.set(null);
    this.draft.set(emptyDraft());
    this.headerRows.set([]);
    this.headersTouched.set(false);
    this.attempted.set(false);
    this.formError.set(null);
    this.formOpen.set(true);
  }

  protected openEdit(credential: Credential): void {
    this.editing.set(credential);
    this.draft.set({
      label: credential.label,
      secret: '',
      max_concurrency:
        credential.max_concurrency === null ? '' : String(credential.max_concurrency),
      balance_usd: credential.balance_usd === null ? '' : String(credential.balance_usd),
    });
    // Values are write-only, so rows are seeded with names and empty values.
    this.headerRows.set(credential.header_names.map((name) => ({ name, value: '' })));
    this.headersTouched.set(false);
    this.attempted.set(false);
    this.formError.set(null);
    this.formOpen.set(true);
  }

  protected closeForm(): void {
    this.formOpen.set(false);
    this.editing.set(null);
  }

  protected setDraft(field: keyof CredentialDraft, value: string): void {
    this.draft.update((draft) => ({ ...draft, [field]: value }));
  }

  protected onHeaderRows(rows: HeaderRow[]): void {
    this.headerRows.set(rows);
    this.headersTouched.set(true);
  }

  protected async save(): Promise<void> {
    this.attempted.set(true);
    this.formError.set(null);
    if (Object.keys(validateDraft(this.draft(), this.editing() === null, true)).length > 0) {
      return;
    }
    const editing = this.editing();
    this.saving.set(true);
    try {
      if (editing === null) {
        await this.catalog.createCredential(this.providerId(), this.buildInput());
        this.notice.set('Credential created.');
      } else {
        const patch = this.buildPatch(editing);
        if (Object.keys(patch).length > 0) {
          await this.catalog.updateCredential(editing.id, patch);
          this.notice.set('Credential saved.');
        }
      }
      this.closeForm();
      await this.load(this.providerId());
    } catch (err) {
      this.formError.set(describeError(err, 'Saving the credential failed.'));
    } finally {
      this.saving.set(false);
    }
  }

  protected async runTest(credential: Credential): Promise<void> {
    this.testingId.set(credential.id);
    this.testBanner.set(null);
    this.actionError.set(null);
    try {
      const result = await this.catalog.testCredential(credential.id);
      const tested = formatTime(result.tested_at);
      this.testBanner.set(
        result.ok
          ? {
              tone: 'success',
              text: `Probe passed for ${credential.label} (last tested ${tested}).`,
            }
          : {
              tone: 'danger',
              text: `Probe failed for ${credential.label} (last tested ${tested}): ${
                result.error ?? 'no detail recorded.'
              }`,
            },
      );
      await this.load(this.providerId());
    } catch (err) {
      this.actionError.set(describeError(err, 'The credential probe failed.'));
    } finally {
      this.testingId.set(null);
    }
  }

  /** Refreshes the provider-reported balance for one credential (D42). */
  protected async refreshBalance(credential: Credential): Promise<void> {
    this.refreshingBalanceId.set(credential.id);
    this.actionError.set(null);
    this.testBanner.set(null);
    try {
      const result = await this.catalog.refreshCredentialBalance(this.providerId(), credential.id);
      this.balanceCache.set(result);
      this.testBanner.set(
        result.available
          ? {
              tone: 'success',
              text: `${credential.label}: ${balanceText(result.balance_usd, result.currency)} available.`,
            }
          : {
              tone: 'warning',
              text: `${credential.label}: the provider reports no available balance.${
                result.error === null ? '' : ` ${result.error}`
              }`,
            },
      );
      await this.load(this.providerId());
      this.changed.emit();
    } catch (err) {
      this.actionError.set(describeError(err, 'Refreshing the balance failed.'));
    } finally {
      this.refreshingBalanceId.set(null);
    }
  }

  /** Saves the warn/block policy for one credential. */
  protected setGate(credential: Credential, gate: string): void {
    if (gate === 'warn' || gate === 'block') {
      void this.saveGate(credential, gate);
    }
  }

  private async saveGate(credential: Credential, gate: BalanceGate): Promise<void> {
    this.gateSavingId.set(credential.id);
    this.actionError.set(null);
    try {
      const updated = await this.catalog.updateCredential(credential.id, { balance_gate: gate });
      this.credentials.update((credentials) =>
        credentials.map((candidate) => (candidate.id === updated.id ? updated : candidate)),
      );
      this.notice.set(`Balance policy for ${credential.label} set to ${gate}.`);
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the balance policy failed.'));
    } finally {
      this.gateSavingId.set(null);
    }
  }

  /** The balance shown in the row: unavailable is never zero. */
  protected balanceText(credential: Credential): string {
    const result = this.balanceCache.get(credential.id);
    const available = result?.available ?? credential.balance_available;
    const value = result?.balance_usd ?? credential.balance_usd;
    if (available === false) {
      return 'unavailable';
    }
    if (value === null) {
      return '—';
    }
    return balanceText(value, result?.currency ?? credential.balance_currency);
  }

  protected balanceDetail(credential: Credential): string | null {
    const result = this.balanceCache.get(credential.id);
    if (result?.days_remaining !== undefined && result?.days_remaining !== null) {
      return `runs out in ${formatDays(result.days_remaining)}`;
    }
    return credential.balance_error;
  }

  protected isLow(credential: Credential): boolean {
    const result = this.balanceCache.get(credential.id);
    const value = result?.balance_usd ?? credential.balance_usd;
    return (
      value !== null &&
      credential.balance_low_threshold !== null &&
      value <= credential.balance_low_threshold
    );
  }

  protected askDelete(credential: Credential): void {
    this.deleting.set(credential);
  }

  protected closeDelete(): void {
    this.deleting.set(null);
  }

  protected async confirmDelete(): Promise<void> {
    const credential = this.deleting();
    if (credential === null) {
      return;
    }
    this.deletePending.set(true);
    this.actionError.set(null);
    try {
      await this.catalog.deleteCredential(credential.id);
      this.deleting.set(null);
      this.notice.set('Credential deleted.');
      await this.load(this.providerId());
    } catch (err) {
      this.actionError.set(describeError(err, 'Deleting the credential failed.'));
    } finally {
      this.deletePending.set(false);
    }
  }

  protected statusTone(status: Credential['status']): BadgeTone {
    switch (status) {
      case 'ok':
        return 'success';
      case 'error':
        return 'danger';
      case 'cooldown':
        return 'warning';
      default:
        return 'neutral';
    }
  }

  protected sourceTone(source: Credential['balance_source']): BadgeTone {
    return source === 'manual' ? 'warning' : source === 'provider_api' ? 'info' : 'neutral';
  }

  private async load(providerId: string): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      const credentials = await this.catalog.listCredentials(providerId);
      if (this.providerId() === providerId) {
        this.credentials.set(credentials);
      }
    } catch (err) {
      if (this.providerId() === providerId) {
        this.loadError.set(describeError(err, 'Loading credentials failed.'));
      }
    } finally {
      this.loading.set(false);
    }
  }

  private buildInput(): CredentialInput {
    const draft = this.draft();
    return {
      label: draft.label.trim(),
      secret: draft.secret,
      headers: headerMapOf(this.headerRows()),
      max_concurrency: parseWhole(draft.max_concurrency),
      balance_usd: parseAmount(draft.balance_usd),
      balance_low_threshold: null,
    };
  }

  /** Only the fields the admin changed; the secret only when retyped. */
  private buildPatch(credential: Credential): CredentialPatch {
    const draft = this.draft();
    const patch: CredentialPatch = {};
    const label = draft.label.trim();
    if (label !== credential.label) {
      patch.label = label;
    }
    if (draft.secret !== '') {
      patch.secret = draft.secret;
    }
    if (this.headersTouched()) {
      patch.headers = headerMapOf(this.headerRows());
    }
    // Nullable columns cannot be cleared over the wire (null means "keep"), so
    // only concrete values go out.
    const maxConcurrency = parseWhole(draft.max_concurrency);
    if (maxConcurrency !== null && maxConcurrency !== credential.max_concurrency) {
      patch.max_concurrency = maxConcurrency;
    }
    const balance = parseAmount(draft.balance_usd);
    if (balance !== null && balance !== credential.balance_usd) {
      patch.balance_usd = balance;
    }
    return patch;
  }
}

/** Validates the form fields; `creating` makes the secret mandatory. */
function validateDraft(
  draft: CredentialDraft,
  creating: boolean,
  attempted: boolean,
): Partial<Record<keyof CredentialDraft, string>> {
  const errors: Partial<Record<keyof CredentialDraft, string>> = {};
  if (!attempted) {
    return errors;
  }
  if (draft.label.trim() === '') {
    errors.label = 'Label is required.';
  }
  if (creating && draft.secret === '') {
    errors.secret = 'Secret is required.';
  }
  const concurrency = draft.max_concurrency.trim();
  if (concurrency !== '' && (!/^\d+$/.test(concurrency) || Number(concurrency) < 1)) {
    errors.max_concurrency = 'Use a whole number of 1 or more.';
  }
  const balance = draft.balance_usd.trim();
  if (balance !== '' && (!Number.isFinite(Number(balance)) || Number(balance) < 0)) {
    errors.balance_usd = 'Cannot be negative.';
  }
  return errors;
}

function parseWhole(value: string): number | null {
  return value.trim() === '' ? null : Number(value);
}

function parseAmount(value: string): number | null {
  return value.trim() === '' ? null : Number(value);
}

function balanceText(value: number | null, currency: string | null): string {
  if (value === null) {
    return '—';
  }
  const code = (currency ?? 'USD').toUpperCase();
  const symbol = code === 'USD' ? '$' : code === 'EUR' ? '€' : code === 'GBP' ? '£' : `${code} `;
  return `${symbol}${value.toFixed(2)}`;
}

function formatDays(days: number): string {
  if (days >= 1) {
    const whole = Math.floor(days);
    return `${whole} ${whole === 1 ? 'day' : 'days'}`;
  }
  return `${Math.max(0, Math.round(days * 24))} h`;
}
