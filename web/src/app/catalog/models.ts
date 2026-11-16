import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from './catalog.service';
import { DeleteConfirm } from './delete-confirm';
import { describeError, isForbidden } from './errors';
import type { Model, ModelInput, ModelPatch } from './catalog.types';
import { joinTags, parseTags } from './format';
import { positiveInteger } from './validators';

type FormState = { mode: 'create' } | { mode: 'edit'; model: Model };

/** The model catalog: canonical names, modalities, and limits. */
@Component({
  selector: 'app-models',
  imports: [ReactiveFormsModule, DeleteConfirm],
  templateUrl: './models.html',
  styleUrl: './models.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Models {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);

  protected readonly models = signal<Model[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly formState = signal<FormState | null>(null);
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly deletingId = signal<string | null>(null);

  protected readonly form = new FormGroup({
    canonical_name: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    family: new FormControl('', { nonNullable: true }),
    modalities_input: new FormControl('', { nonNullable: true }),
    modalities_output: new FormControl('', { nonNullable: true }),
    context_length: new FormControl<number | null>(null, { validators: [positiveInteger] }),
    max_output_tokens: new FormControl<number | null>(null, { validators: [positiveInteger] }),
    capabilities: new FormControl('', { nonNullable: true }),
  });

  protected readonly capabilityDisplay = capabilityDisplay;

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
      this.models.set(await this.catalog.listModels());
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected openCreate(): void {
    this.form.reset({
      canonical_name: '',
      family: '',
      modalities_input: '',
      modalities_output: '',
      context_length: null,
      max_output_tokens: null,
      capabilities: '',
    });
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'create' });
  }

  protected openEdit(model: Model): void {
    this.form.reset({
      canonical_name: model.canonical_name,
      family: model.family ?? '',
      modalities_input: joinTags(model.modalities_input),
      modalities_output: joinTags(model.modalities_output),
      context_length: model.context_length,
      max_output_tokens: model.max_output_tokens,
      capabilities: joinTags(capabilityTags(model.capabilities)),
    });
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'edit', model });
  }

  protected closeForm(): void {
    this.formState.set(null);
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
      const input = this.buildInput();
      if (state.mode === 'create') {
        await this.catalog.createModel(input);
      } else {
        const patch = buildPatch(state.model, input);
        if (Object.keys(patch).length > 0) {
          await this.catalog.updateModel(state.model.id, patch);
        }
      }
      this.formState.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the model failed.'));
    } finally {
      this.saving.set(false);
    }
  }

  protected async deleteModel(model: Model): Promise<void> {
    this.deletingId.set(model.id);
    this.actionError.set(null);
    try {
      await this.catalog.deleteModel(model.id);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Deleting the model failed.'));
    } finally {
      this.deletingId.set(null);
    }
  }

  protected showError(field: 'canonical_name' | 'context_length' | 'max_output_tokens'): boolean {
    const control = this.form.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected fieldError(field: 'canonical_name' | 'context_length' | 'max_output_tokens'): string {
    const control = this.form.controls[field];
    if (control.hasError('required')) {
      return 'Canonical name is required.';
    }
    if (control.hasError('positiveInteger')) {
      return 'Use a whole number of 1 or more.';
    }
    return '';
  }

  private buildInput(): ModelInput {
    const {
      canonical_name,
      family,
      modalities_input,
      modalities_output,
      context_length,
      max_output_tokens,
      capabilities,
    } = this.form.getRawValue();
    return {
      canonical_name: canonical_name.trim(),
      family: family.trim() === '' ? null : family.trim(),
      modalities_input: parseTags(modalities_input),
      modalities_output: parseTags(modalities_output),
      context_length,
      max_output_tokens,
      capabilities: parseTags(capabilities),
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

/** Only the fields the admin changed. */
function buildPatch(model: Model, input: ModelInput): ModelPatch {
  const patch: ModelPatch = {};
  if (input.canonical_name !== model.canonical_name) {
    patch.canonical_name = input.canonical_name;
  }
  if (input.family !== null && input.family !== model.family) {
    patch.family = input.family;
  }
  if (!sameTags(input.modalities_input, model.modalities_input)) {
    patch.modalities_input = input.modalities_input;
  }
  if (!sameTags(input.modalities_output, model.modalities_output)) {
    patch.modalities_output = input.modalities_output;
  }
  // Nullable columns cannot be cleared over the wire (null means "keep"), so
  // only concrete values go out.
  if (input.context_length !== null && input.context_length !== model.context_length) {
    patch.context_length = input.context_length;
  }
  if (input.max_output_tokens !== null && input.max_output_tokens !== model.max_output_tokens) {
    patch.max_output_tokens = input.max_output_tokens;
  }
  if (!sameTags(input.capabilities, capabilityTags(model.capabilities))) {
    patch.capabilities = input.capabilities;
  }
  return patch;
}

function sameTags(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((tag, i) => tag === b[i]);
}

/**
 * The tag list the UI edits: `capabilities` is free-form JSON on the wire and
 * only a string array is a tag list (synced models store structured objects).
 */
export function capabilityTags(value: unknown): string[] {
  return Array.isArray(value) && value.every((tag) => typeof tag === 'string')
    ? [...(value as string[])]
    : [];
}

/** Table text for capabilities: tags joined, structured JSON verbatim, or ''. */
export function capabilityDisplay(value: unknown): string {
  const tags = capabilityTags(value);
  if (tags.length > 0) {
    return tags.join(', ');
  }
  if (value === null || value === undefined || Array.isArray(value)) {
    return '';
  }
  return JSON.stringify(value);
}
