import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from './catalog.service';
import { DeleteConfirm } from './delete-confirm';
import { describeError, isForbidden } from './errors';
import type {
  Offering,
  OfferingInput,
  OfferingPatch,
  Price,
  PriceInput,
  Provider,
  Model,
} from './catalog.types';
import { formatAge, formatTime, formatUsd } from './format';
import { type HeaderRow, HeadersEditor, headerMapOf } from './headers-editor';
import { nonNegative, positiveInteger } from './validators';

type FormState = { mode: 'create' } | { mode: 'edit'; offering: Offering };

/** Which price field a row of the price editor edits. */
type PriceField = keyof PriceInput;

/**
 * Offerings: which provider serves which model under which upstream id, with
 * the per-offering price editor. Prices carry their source badge; a manual
 * edit is visually distinct from synced values.
 */
@Component({
  selector: 'app-offerings',
  imports: [ReactiveFormsModule, DeleteConfirm, HeadersEditor],
  templateUrl: './offerings.html',
  styleUrl: './offerings.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Offerings {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);

  protected readonly providers = signal<Provider[]>([]);
  protected readonly models = signal<Model[]>([]);
  protected readonly offerings = signal<Offering[]>([]);
  /**
   * Prices keyed by offering id; the listing has no embedded price, so each
   * offering's price row loads through `GET /api/offerings/{id}/price`.
   */
  protected readonly prices = signal<Map<string, Price>>(new Map());
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly notice = signal<string | null>(null);
  protected readonly formState = signal<FormState | null>(null);
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly deletingId = signal<string | null>(null);
  protected readonly togglingId = signal<string | null>(null);
  protected readonly filterProviderId = signal('');
  protected readonly filterModelId = signal('');
  protected readonly headerRows = signal<HeaderRow[]>([]);
  protected readonly headersTouched = signal(false);

  protected readonly priceFor = signal<Offering | null>(null);
  protected readonly savingPrice = signal(false);
  protected readonly priceAttempted = signal(false);

  protected readonly formatTime = formatTime;
  protected readonly formatAge = formatAge;
  protected readonly formatUsd = formatUsd;

  protected readonly form = new FormGroup({
    provider_id: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    model_id: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    upstream_model_id: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required],
    }),
    context_override: new FormControl<number | null>(null, { validators: [positiveInteger] }),
    max_output_override: new FormControl<number | null>(null, { validators: [positiveInteger] }),
    enabled: new FormControl(true, { nonNullable: true }),
  });

  protected readonly priceForm = new FormGroup({
    input_per_m: new FormControl<number | null>(null, { validators: [nonNegative] }),
    output_per_m: new FormControl<number | null>(null, { validators: [nonNegative] }),
    cache_read_per_m: new FormControl<number | null>(null, { validators: [nonNegative] }),
    cache_write_per_m: new FormControl<number | null>(null, { validators: [nonNegative] }),
    reasoning_per_m: new FormControl<number | null>(null, { validators: [nonNegative] }),
    request_fee: new FormControl<number | null>(null, { validators: [nonNegative] }),
  });

  private readonly providerById = computed(
    () => new Map(this.providers().map((provider) => [provider.id, provider])),
  );
  private readonly modelById = computed(
    () => new Map(this.models().map((model) => [model.id, model])),
  );

  protected readonly filtered = computed(() =>
    this.offerings().filter(
      (offering) =>
        (this.filterProviderId() === '' || offering.provider_id === this.filterProviderId()) &&
        (this.filterModelId() === '' || offering.model_id === this.filterModelId()),
    ),
  );

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
      const [providers, models, offerings] = await Promise.all([
        this.catalog.listProviders(),
        this.catalog.listModels(),
        this.catalog.listOfferings(),
      ]);
      this.providers.set(providers);
      this.models.set(models);
      this.offerings.set(offerings);
      // Prices ride along per offering; an unpriced offering answers 404 and
      // simply has no entry in the map.
      const entries = await Promise.all(
        offerings.map(async (offering): Promise<[string, Price] | null> => {
          const price = await this.catalog.getPrice(offering.id);
          return price === null ? null : [offering.id, price];
        }),
      );
      this.prices.set(new Map(entries.filter((entry) => entry !== null)));
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected providerName(providerId: string): string {
    return this.providerById().get(providerId)?.name ?? providerId;
  }

  protected modelName(modelId: string): string {
    return this.modelById().get(modelId)?.canonical_name ?? modelId;
  }

  protected setFilterProvider(event: Event): void {
    this.filterProviderId.set(selectValue(event));
  }

  protected setFilterModel(event: Event): void {
    this.filterModelId.set(selectValue(event));
  }

  protected openCreate(): void {
    this.form.reset({
      provider_id: this.providers()[0]?.id ?? '',
      model_id: this.models()[0]?.id ?? '',
      upstream_model_id: '',
      context_override: null,
      max_output_override: null,
      enabled: true,
    });
    this.form.controls.provider_id.enable();
    this.form.controls.model_id.enable();
    this.headerRows.set([]);
    this.headersTouched.set(false);
    this.attempted.set(false);
    this.actionError.set(null);
    this.notice.set(null);
    this.formState.set({ mode: 'create' });
  }

  protected openEdit(offering: Offering): void {
    this.form.reset({
      provider_id: offering.provider_id,
      model_id: offering.model_id,
      upstream_model_id: offering.upstream_model_id,
      context_override: offering.context_override,
      max_output_override: offering.max_output_override,
      enabled: offering.enabled,
    });
    // An offering cannot move to another provider or model over the wire.
    this.form.controls.provider_id.disable();
    this.form.controls.model_id.disable();
    // Values are write-only, so rows are seeded with names and empty values.
    this.headerRows.set(offering.header_names.map((name) => ({ name, value: '' })));
    this.headersTouched.set(false);
    this.attempted.set(false);
    this.actionError.set(null);
    this.notice.set(null);
    this.formState.set({ mode: 'edit', offering });
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
    this.notice.set(null);
    const state = this.formState();
    if (state === null || this.form.invalid) {
      return;
    }
    this.saving.set(true);
    try {
      const input = this.buildInput();
      if (state.mode === 'create') {
        await this.catalog.createOffering(input);
      } else {
        const patch = buildPatch(state.offering, input, this.headersTouched());
        if (Object.keys(patch).length > 0) {
          await this.catalog.updateOffering(state.offering.id, patch);
        }
      }
      this.formState.set(null);
      this.notice.set('Offering saved.');
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the offering failed.'));
    } finally {
      this.saving.set(false);
    }
  }

  protected async toggleEnabled(offering: Offering): Promise<void> {
    this.togglingId.set(offering.id);
    this.actionError.set(null);
    this.notice.set(null);
    try {
      await this.catalog.updateOffering(offering.id, { enabled: !offering.enabled });
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Changing the offering failed.'));
    } finally {
      this.togglingId.set(null);
    }
  }

  protected async deleteOffering(offering: Offering): Promise<void> {
    this.deletingId.set(offering.id);
    this.actionError.set(null);
    this.notice.set(null);
    try {
      await this.catalog.deleteOffering(offering.id);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Deleting the offering failed.'));
    } finally {
      this.deletingId.set(null);
    }
  }

  // --- price editor --------------------------------------------------------

  /** The offering's loaded price row, or null when it has none. */
  protected priceOf(offering: Offering): Price | null {
    return this.prices().get(offering.id) ?? null;
  }

  protected openPrice(offering: Offering): void {
    const price = this.priceOf(offering);
    this.priceForm.reset({
      input_per_m: price?.input_per_m ?? null,
      output_per_m: price?.output_per_m ?? null,
      cache_read_per_m: price?.cache_read_per_m ?? null,
      cache_write_per_m: price?.cache_write_per_m ?? null,
      reasoning_per_m: price?.reasoning_per_m ?? null,
      request_fee: price?.request_fee ?? null,
    });
    this.priceAttempted.set(false);
    this.actionError.set(null);
    this.notice.set(null);
    this.priceFor.set(offering);
  }

  protected closePrice(): void {
    this.priceFor.set(null);
  }

  protected async savePrice(): Promise<void> {
    this.priceAttempted.set(true);
    this.actionError.set(null);
    this.notice.set(null);
    const offering = this.priceFor();
    if (offering === null || this.priceForm.invalid) {
      return;
    }
    this.savingPrice.set(true);
    try {
      await this.catalog.setPrice(offering.id, this.buildPriceInput());
      this.priceFor.set(null);
      this.notice.set('Price saved as manual. Sync will no longer overwrite it.');
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the price failed.'));
    } finally {
      this.savingPrice.set(false);
    }
  }

  protected priceError(field: PriceField): string {
    return this.priceForm.controls[field].hasError('min') ? 'Cannot be negative.' : '';
  }

  protected showPriceError(field: PriceField): boolean {
    const control = this.priceForm.controls[field];
    return control.invalid && (control.touched || this.priceAttempted());
  }

  protected priceSourceClass(offering: Offering): string {
    const price = this.priceOf(offering);
    return price === null ? '' : price.source === 'manual' ? 'manual' : 'synced';
  }

  protected priceSummary(offering: Offering): string {
    const price = this.priceOf(offering);
    if (price === null) {
      return '—';
    }
    const parts: string[] = [];
    if (price.input_per_m !== null) {
      parts.push(`in ${formatUsd(price.input_per_m)}/M`);
    }
    if (price.output_per_m !== null) {
      parts.push(`out ${formatUsd(price.output_per_m)}/M`);
    }
    if (price.request_fee !== null) {
      parts.push(`fee ${formatUsd(price.request_fee)}/req`);
    }
    return parts.length === 0 ? '—' : parts.join(' · ');
  }

  protected showError(
    field:
      'provider_id' | 'model_id' | 'upstream_model_id' | 'context_override' | 'max_output_override',
  ): boolean {
    const control = this.form.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected fieldError(
    field:
      'provider_id' | 'model_id' | 'upstream_model_id' | 'context_override' | 'max_output_override',
  ): string {
    const control = this.form.controls[field];
    if (control.hasError('required')) {
      return 'This field is required.';
    }
    if (control.hasError('positiveInteger')) {
      return 'Use a whole number of 1 or more.';
    }
    return '';
  }

  private buildInput(): OfferingInput {
    const {
      provider_id,
      model_id,
      upstream_model_id,
      context_override,
      max_output_override,
      enabled,
    } = this.form.getRawValue();
    return {
      provider_id,
      model_id,
      upstream_model_id: upstream_model_id.trim(),
      context_override,
      max_output_override,
      enabled,
      headers: headerMapOf(this.headerRows()),
    };
  }

  private buildPriceInput(): PriceInput {
    const {
      input_per_m,
      output_per_m,
      cache_read_per_m,
      cache_write_per_m,
      reasoning_per_m,
      request_fee,
    } = this.priceForm.getRawValue();
    // The server records `source='manual'` itself; the body is amounts only.
    return {
      input_per_m,
      output_per_m,
      cache_read_per_m,
      cache_write_per_m,
      reasoning_per_m,
      request_fee,
    };
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading offerings failed.'));
  }
}

/**
 * Only the fields the admin changed. The wire patch cannot move an offering
 * between providers/models and cannot clear nullable overrides (null means
 * "keep"), so those fields never go out from here.
 */
function buildPatch(
  offering: Offering,
  input: OfferingInput,
  headersTouched: boolean,
): OfferingPatch {
  const patch: OfferingPatch = {};
  if (input.upstream_model_id !== offering.upstream_model_id) {
    patch.upstream_model_id = input.upstream_model_id;
  }
  if (input.context_override !== null && input.context_override !== offering.context_override) {
    patch.context_override = input.context_override;
  }
  if (
    input.max_output_override !== null &&
    input.max_output_override !== offering.max_output_override
  ) {
    patch.max_output_override = input.max_output_override;
  }
  if (input.enabled !== offering.enabled) {
    patch.enabled = input.enabled;
  }
  // Header values are write-only: `headers` is a full replace, so it goes out
  // only when the admin edited the rows, and only rows with a typed value
  // survive it.
  if (headersTouched) {
    patch.headers = input.headers;
  }
  return patch;
}

function selectValue(event: Event): string {
  return (event.target as HTMLSelectElement).value;
}
