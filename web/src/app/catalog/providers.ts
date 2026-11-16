import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from './catalog.service';
import { Credentials } from './credentials';
import { DeleteConfirm } from './delete-confirm';
import { describeError, isForbidden } from './errors';
import type { Provider, ProviderKind, ProviderPatch } from './catalog.types';
import { httpUrl } from './validators';

type FormState = { mode: 'create' } | { mode: 'edit'; provider: Provider };

/** Providers list plus the credentials nested under the selected provider. */
@Component({
  selector: 'app-providers',
  imports: [ReactiveFormsModule, Credentials, DeleteConfirm],
  templateUrl: './providers.html',
  styleUrl: './providers.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Providers {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);

  protected readonly providers = signal<Provider[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly formState = signal<FormState | null>(null);
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly togglingId = signal<string | null>(null);
  protected readonly deletingId = signal<string | null>(null);
  protected readonly selected = signal<Provider | null>(null);

  protected readonly form = new FormGroup({
    name: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    base_url: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required, httpUrl],
    }),
    kind: new FormControl<ProviderKind>('openai_compatible', {
      nonNullable: true,
      validators: [Validators.required],
    }),
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
      const providers = await this.catalog.listProviders();
      this.providers.set(providers);
      // Keep the credentials panel pointed at the fresh copy of its provider.
      const selectedId = this.selected()?.id ?? null;
      this.selected.set(
        selectedId === null ? null : (providers.find((p) => p.id === selectedId) ?? null),
      );
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected openCreate(): void {
    this.form.reset({ name: '', base_url: '', kind: 'openai_compatible' });
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'create' });
  }

  protected openEdit(provider: Provider): void {
    this.form.reset({ name: provider.name, base_url: provider.base_url, kind: provider.kind });
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'edit', provider });
  }

  protected closeForm(): void {
    this.formState.set(null);
  }

  protected select(provider: Provider): void {
    this.selected.set(this.selected()?.id === provider.id ? null : provider);
  }

  protected clearSelection(): void {
    this.selected.set(null);
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
      const { name, base_url, kind } = this.form.getRawValue();
      if (state.mode === 'create') {
        await this.catalog.createProvider({ name: name.trim(), base_url: base_url.trim(), kind });
      } else {
        const patch = buildPatch(state.provider, name.trim(), base_url.trim());
        if (Object.keys(patch).length > 0) {
          await this.catalog.updateProvider(state.provider.id, patch);
        }
      }
      this.formState.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the provider failed.'));
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

  protected async deleteProvider(provider: Provider): Promise<void> {
    this.deletingId.set(provider.id);
    this.actionError.set(null);
    try {
      await this.catalog.deleteProvider(provider.id);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Deleting the provider failed.'));
    } finally {
      this.deletingId.set(null);
    }
  }

  protected showError(field: 'name' | 'base_url' | 'kind'): boolean {
    const control = this.form.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected fieldError(field: 'name' | 'base_url' | 'kind'): string {
    const control = this.form.controls[field];
    if (control.hasError('required')) {
      return field === 'name' ? 'Name is required.' : 'Base URL is required.';
    }
    if (control.hasError('httpUrl')) {
      return 'Enter an http(s) URL, for example https://api.example.com/v1.';
    }
    return '';
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading providers failed.'));
  }
}

/** Only the fields the admin changed. */
function buildPatch(provider: Provider, name: string, baseUrl: string): ProviderPatch {
  const patch: ProviderPatch = {};
  if (name !== provider.name) {
    patch.name = name;
  }
  if (baseUrl !== provider.base_url) {
    patch.base_url = baseUrl;
  }
  // `kind` cannot change over the wire; the form only offers the single kind.
  return patch;
}
