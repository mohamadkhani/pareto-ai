import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from '../catalog/catalog.service';
import type { Model, Offering, Provider } from '../catalog/catalog.types';
import { DeleteConfirm } from '../catalog/delete-confirm';
import { describeError, isForbidden } from '../catalog/errors';
import { CombosService } from './combos.service';
import type {
  Combo,
  ComboInput,
  ComboStrategy,
  ComboWeightsInput,
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

/** The four custom weight inputs, in form order. */
const WEIGHT_FIELDS: readonly WeightComponent[] = ['price', 'speed', 'quality', 'reliability'];

type Form = FormGroup<{
  name: FormControl<string>;
  strategy: FormControl<ComboStrategy>;
  weights_mode: FormControl<WeightsMode>;
  price: FormControl<number>;
  speed: FormControl<number>;
  quality: FormControl<number>;
  reliability: FormControl<number>;
  enabled: FormControl<boolean>;
}>;

/**
 * Combos: named offering pools addressable as `combo:<name>`, with the
 * strategy/weights editor and the ordered member picker. Members are
 * offerings; the stored order is the fallback chain for `ordered`.
 */
@Component({
  selector: 'app-combos',
  imports: [ReactiveFormsModule, DeleteConfirm],
  templateUrl: './combos.html',
  styleUrl: './combos.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Combos {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);
  private readonly combosService = inject(CombosService);

  protected readonly combos = signal<Combo[]>([]);
  protected readonly providers = signal<Provider[]>([]);
  protected readonly models = signal<Model[]>([]);
  protected readonly offerings = signal<Offering[]>([]);
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
  protected readonly members = signal<MemberRow[]>([]);

  protected readonly form: Form = new FormGroup({
    name: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required, Validators.pattern(/\S/)],
    }),
    strategy: new FormControl<ComboStrategy>('weighted', { nonNullable: true }),
    weights_mode: new FormControl<WeightsMode>('balanced', { nonNullable: true }),
    price: new FormControl(0.25, { nonNullable: true, validators: [Validators.min(0)] }),
    speed: new FormControl(0.3, { nonNullable: true, validators: [Validators.min(0)] }),
    quality: new FormControl(0.3, { nonNullable: true, validators: [Validators.min(0)] }),
    reliability: new FormControl(0.15, { nonNullable: true, validators: [Validators.min(0)] }),
    enabled: new FormControl(true, { nonNullable: true }),
  });

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
      const [combos, offerings, providers, models] = await Promise.all([
        this.combosService.list(),
        this.catalog.listOfferings(),
        this.catalog.listProviders(),
        this.catalog.listModels(),
      ]);
      this.combos.set(combos);
      this.offerings.set(offerings);
      this.providers.set(providers);
      this.models.set(models);
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

  protected strategyHint(): string {
    const strategy = this.form.controls.strategy.value;
    if (strategy === 'ordered') {
      return 'Members are tried top to bottom — the stored order is the fallback chain.';
    }
    if (strategy === 'round_robin') {
      return 'Requests rotate over the members, starting at the top.';
    }
    return 'The pool is scored; a member weight multiplies its score.';
  }

  protected openCreate(): void {
    this.form.reset({
      name: '',
      strategy: 'weighted',
      weights_mode: 'balanced',
      price: 0.25,
      speed: 0.3,
      quality: 0.3,
      reliability: 0.15,
      enabled: true,
    });
    this.form.controls.name.enable();
    this.members.set([]);
    this.attempted.set(false);
    this.actionError.set(null);
    this.notice.set(null);
    this.formState.set({ mode: 'create' });
  }

  protected openEdit(combo: Combo): void {
    this.form.reset({
      name: combo.name,
      strategy: combo.strategy,
      // A stored custom weighting comes back without a preset name.
      weights_mode: combo.preset ?? 'custom',
      price: combo.weights.price,
      speed: combo.weights.speed,
      quality: combo.weights.quality,
      reliability: combo.weights.reliability,
      enabled: combo.enabled,
    });
    // The name is immutable over the wire.
    this.form.controls.name.disable();
    this.members.set(combo.members.map((member) => ({ ...member })));
    this.attempted.set(false);
    this.actionError.set(null);
    this.notice.set(null);
    this.formState.set({ mode: 'edit', combo });
  }

  protected closeForm(): void {
    this.formState.set(null);
  }

  protected addMember(event: Event): void {
    const select = event.target;
    if (!(select instanceof HTMLSelectElement) || select.value === '') {
      return;
    }
    this.members.set([...this.members(), { offering_id: select.value, weight: 1 }]);
    // The picker offers the remaining offerings; reset it for the next pick.
    select.value = '';
  }

  protected removeMember(index: number): void {
    this.members.set(this.members().filter((_, i) => i !== index));
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

  protected setMemberWeight(index: number, event: Event): void {
    const input = event.target;
    if (!(input instanceof HTMLInputElement)) {
      return;
    }
    const weight = Number(input.value);
    this.members.set(
      this.members().map((member, i) => (i === index ? { ...member, weight } : member)),
    );
  }

  /** A member weight must be a positive finite number. */
  protected memberWeightsInvalid(): boolean {
    return this.members().some((member) => !Number.isFinite(member.weight) || member.weight <= 0);
  }

  /** Custom weights must not all be zero (the wire rejects an all-zero set). */
  protected customWeightsInvalid(): boolean {
    return WEIGHT_FIELDS.every((field) => this.form.controls[field].value <= 0);
  }

  protected showError(field: 'name'): boolean {
    const control = this.form.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected fieldError(field: 'name'): string {
    const control = this.form.controls[field];
    if (control.hasError('required') || control.hasError('pattern')) {
      return 'This field is required.';
    }
    return '';
  }

  protected async save(): Promise<void> {
    this.attempted.set(true);
    this.actionError.set(null);
    this.notice.set(null);
    const state = this.formState();
    if (
      state === null ||
      this.form.invalid ||
      this.members().length === 0 ||
      this.memberWeightsInvalid() ||
      (this.form.controls.weights_mode.value === 'custom' && this.customWeightsInvalid())
    ) {
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
      this.actionError.set(describeError(err, 'Saving the combo failed.'));
    } finally {
      this.saving.set(false);
    }
  }

  protected async toggleEnabled(combo: Combo): Promise<void> {
    this.togglingId.set(combo.id);
    this.actionError.set(null);
    this.notice.set(null);
    try {
      await this.combosService.update(combo.id, comboInputOf(combo, !combo.enabled));
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Changing the combo failed.'));
    } finally {
      this.togglingId.set(null);
    }
  }

  protected async deleteCombo(combo: Combo): Promise<void> {
    this.deletingId.set(combo.id);
    this.actionError.set(null);
    this.notice.set(null);
    try {
      await this.combosService.remove(combo.id);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Deleting the combo failed.'));
    } finally {
      this.deletingId.set(null);
    }
  }

  private buildInput(): ComboInput {
    const { name, strategy, weights_mode, enabled } = this.form.getRawValue();
    return {
      name: name.trim(),
      strategy,
      weights: this.buildWeights(weights_mode),
      members: this.members().map((member) => ({
        offering_id: member.offering_id,
        weight: member.weight,
      })),
      enabled,
    };
  }

  /** A named preset goes out as `{"preset": …}`, custom as the four parts. */
  private buildWeights(mode: WeightsMode): ComboWeightsInput {
    if (mode !== 'custom') {
      return { preset: mode };
    }
    return {
      price: this.form.controls.price.value,
      speed: this.form.controls.speed.value,
      quality: this.form.controls.quality.value,
      reliability: this.form.controls.reliability.value,
    };
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading combos failed.'));
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
  };
}
