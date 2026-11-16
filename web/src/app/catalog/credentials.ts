import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  signal,
} from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { CatalogService } from './catalog.service';
import type { Credential, CredentialInput, CredentialPatch, Provider } from './catalog.types';
import { DeleteConfirm } from './delete-confirm';
import { describeError } from './errors';
import { formatAge, formatTime, formatUsd } from './format';
import { type HeaderRow, HeadersEditor, headerMapOf } from './headers-editor';
import { nonNegative, positiveInteger } from './validators';

type FormState = { mode: 'create' } | { mode: 'edit'; credential: Credential };

/** One test-probe outcome to surface above the list. */
interface TestBanner {
  ok: boolean;
  text: string;
}

/**
 * Credentials for one provider: list, create/edit (secret write-only), delete,
 * and a health probe. Loaded whenever the `provider` input's id changes.
 */
@Component({
  selector: 'app-credentials',
  imports: [ReactiveFormsModule, DeleteConfirm, HeadersEditor],
  templateUrl: './credentials.html',
  styleUrl: './credentials.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Credentials {
  private readonly catalog = inject(CatalogService);

  readonly provider = input.required<Provider>();

  protected readonly credentials = signal<Credential[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly actionError = signal<string | null>(null);
  protected readonly formState = signal<FormState | null>(null);
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly deletingId = signal<string | null>(null);
  protected readonly testingId = signal<string | null>(null);
  protected readonly testBanner = signal<TestBanner | null>(null);
  protected readonly headerRows = signal<HeaderRow[]>([]);
  protected readonly headersTouched = signal(false);

  protected readonly formatTime = formatTime;
  protected readonly formatAge = formatAge;
  protected readonly formatUsd = formatUsd;

  protected readonly form = new FormGroup({
    label: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    secret: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    max_concurrency: new FormControl<number | null>(null, { validators: [positiveInteger] }),
    balance_usd: new FormControl<number | null>(null, { validators: [nonNegative] }),
  });

  private readonly providerId = computed(() => this.provider().id);

  constructor() {
    effect(() => {
      const providerId = this.providerId();
      this.formState.set(null);
      void this.load(providerId);
    });
  }

  protected async refresh(): Promise<void> {
    await this.load(this.providerId());
  }

  protected openCreate(): void {
    this.form.reset({ label: '', secret: '', max_concurrency: null, balance_usd: null });
    this.form.controls.secret.setValidators(Validators.required);
    this.form.controls.secret.updateValueAndValidity();
    this.headerRows.set([]);
    this.headersTouched.set(false);
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'create' });
  }

  protected openEdit(credential: Credential): void {
    this.form.reset({
      label: credential.label,
      secret: '',
      max_concurrency: credential.max_concurrency,
      balance_usd: credential.balance_usd,
    });
    // "Leave blank to keep": the secret is optional while editing.
    this.form.controls.secret.clearValidators();
    this.form.controls.secret.updateValueAndValidity();
    // Values are write-only, so rows are seeded with names and empty values.
    this.headerRows.set(credential.header_names.map((name) => ({ name, value: '' })));
    this.headersTouched.set(false);
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'edit', credential });
  }

  protected closeForm(): void {
    this.formState.set(null);
  }

  protected onHeaderRows(rows: HeaderRow[]): void {
    this.headerRows.set(rows);
    this.headersTouched.set(true);
  }

  protected async save(): Promise<void> {
    this.attempted.set(true);
    this.actionError.set(null);
    const state = this.formState();
    if (state === null || this.form.invalid) {
      return;
    }
    this.saving.set(true);
    try {
      if (state.mode === 'create') {
        await this.catalog.createCredential(this.providerId(), this.buildInput());
      } else {
        const patch = this.buildPatch(state.credential);
        if (Object.keys(patch).length > 0) {
          await this.catalog.updateCredential(state.credential.id, patch);
        }
      }
      this.formState.set(null);
      await this.load(this.providerId());
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the credential failed.'));
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
      this.testBanner.set({
        ok: result.ok,
        text: result.ok
          ? `Probe passed for ${credential.label} (last tested ${tested}).`
          : `Probe failed for ${credential.label} (last tested ${tested}): ${result.error ?? 'no detail recorded.'}`,
      });
      await this.load(this.providerId());
    } catch (err) {
      this.actionError.set(describeError(err, 'The credential probe failed.'));
    } finally {
      this.testingId.set(null);
    }
  }

  protected async deleteCredential(credential: Credential): Promise<void> {
    this.deletingId.set(credential.id);
    this.actionError.set(null);
    try {
      await this.catalog.deleteCredential(credential.id);
      await this.load(this.providerId());
    } catch (err) {
      this.actionError.set(describeError(err, 'Deleting the credential failed.'));
    } finally {
      this.deletingId.set(null);
    }
  }

  protected showError(field: 'label' | 'secret' | 'max_concurrency' | 'balance_usd'): boolean {
    const control = this.form.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected fieldError(field: 'label' | 'secret' | 'max_concurrency' | 'balance_usd'): string {
    const control = this.form.controls[field];
    if (control.hasError('required')) {
      return field === 'label' ? 'Label is required.' : 'Secret is required.';
    }
    if (control.hasError('positiveInteger')) {
      return 'Use a whole number of 1 or more.';
    }
    if (control.hasError('min')) {
      return 'Cannot be negative.';
    }
    return '';
  }

  protected statusClass(status: Credential['status']): string {
    switch (status) {
      case 'ok':
        return 'ok';
      case 'error':
        return 'error';
      case 'cooldown':
        return 'warn';
      default:
        return '';
    }
  }

  protected sourceClass(source: Credential['balance_source']): string {
    return source === 'manual' ? 'warn' : source === 'provider_api' ? 'synced' : '';
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
    const { label, secret, max_concurrency, balance_usd } = this.form.getRawValue();
    return {
      label: label.trim(),
      secret,
      headers: headerMapOf(this.headerRows()),
      max_concurrency,
      balance_usd,
      balance_low_threshold: null,
    };
  }

  /** Only the fields the admin changed; the secret only when retyped. */
  private buildPatch(credential: Credential): CredentialPatch {
    const { label, secret, max_concurrency, balance_usd } = this.form.getRawValue();
    const patch: CredentialPatch = {};
    const trimmed = label.trim();
    if (trimmed !== credential.label) {
      patch.label = trimmed;
    }
    if (secret !== '') {
      patch.secret = secret;
    }
    if (this.headersTouched()) {
      patch.headers = headerMapOf(this.headerRows());
    }
    // Nullable columns cannot be cleared over the wire (null means "keep"), so
    // only concrete values go out.
    if (max_concurrency !== null && max_concurrency !== credential.max_concurrency) {
      patch.max_concurrency = max_concurrency;
    }
    if (balance_usd !== null && balance_usd !== credential.balance_usd) {
      patch.balance_usd = balance_usd;
    }
    return patch;
  }
}
