import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { ApiError } from '../api';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from '../catalog/catalog.service';
import type { Credential, Model, Offering, Provider } from '../catalog/catalog.types';
import { describeError, isForbidden } from '../catalog/errors';
import { formatTime } from '../catalog/format';
import { KeysService } from '../keys/keys.service';
import type { ApiKeyView } from '../keys/keys.types';
import { UiBadge } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { type ComboboxOption, UiCombobox } from '../ui/combobox';
import { UiDialog } from '../ui/dialog';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiSelect } from '../ui/select';
import { UiTable } from '../ui/table';
import { UsersService } from '../users/users.service';
import type { UserView } from '../users/users.types';
import { GrantsService } from './grants.service';
import type { GrantMode, GrantResourceType, GrantSubjectType, GrantView } from './grants.types';

/** A readable API-key subject: name, visible prefix, and its owner. */
interface KeyLabel {
  name: string;
  prefix: string;
  ownerEmail: string;
}

/** A 409 duplicate gets its own message so the admin sees why it failed. */
function describeGrantError(err: unknown): string {
  if (err instanceof ApiError && err.code === 'conflict') {
    return `Duplicate grant — ${err.message}.`;
  }
  return describeError(err, 'Saving the grant failed.');
}

/** Access grants: who may use which models, offerings, and credentials. */
@Component({
  selector: 'app-grants',
  imports: [
    ReactiveFormsModule,
    UiBadge,
    UiBanner,
    UiButton,
    UiCombobox,
    UiDialog,
    UiEmpty,
    UiField,
    UiSelect,
    UiTable,
  ],
  templateUrl: './grants.html',
  styleUrl: './grants.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Grants {
  private readonly auth = inject(AuthService);
  private readonly grantsService = inject(GrantsService);
  private readonly usersService = inject(UsersService);
  private readonly keysService = inject(KeysService);
  private readonly catalog = inject(CatalogService);

  protected readonly grants = signal<GrantView[]>([]);
  private readonly users = signal<UserView[]>([]);
  private readonly keys = signal<ApiKeyView[]>([]);
  /** Key-id → readable label, for grants whose subject is an API key. */
  private readonly keyLabels = signal<ReadonlyMap<string, KeyLabel>>(new Map());
  private readonly models = signal<Model[]>([]);
  private readonly offerings = signal<Offering[]>([]);
  private readonly credentials = signal<Credential[]>([]);
  private readonly providers = signal<Provider[]>([]);

  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly deletingId = signal<string | null>(null);
  /** The grant the delete confirmation dialog is open for. */
  protected readonly deleteTarget = signal<GrantView | null>(null);
  protected readonly showForm = signal(false);

  protected readonly formatTime = formatTime;

  protected readonly form = new FormGroup({
    subject_type: new FormControl<GrantSubjectType>('user', { nonNullable: true }),
    /** Owner of the keys offered when an API-key subject is picked. */
    key_owner_id: new FormControl('', { nonNullable: true }),
    subject_id: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    resource_type: new FormControl<GrantResourceType>('model', { nonNullable: true }),
    resource_id: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    mode: new FormControl<GrantMode>('allow', { nonNullable: true }),
  });

  private readonly subjectType = toSignal(this.form.controls.subject_type.valueChanges, {
    initialValue: 'user' as GrantSubjectType,
  });
  private readonly resourceType = toSignal(this.form.controls.resource_type.valueChanges, {
    initialValue: 'model' as GrantResourceType,
  });

  /** Subjects of the picked kind: every user, or the picked owner's keys. */
  protected readonly subjectOptions = computed<ComboboxOption[]>(() => {
    if (this.subjectType() === 'user') {
      return this.users().map((user) => ({ value: user.id, label: user.email }));
    }
    return this.keys().map((key) => ({
      value: key.id,
      label: `${key.name} (${key.prefix})`,
    }));
  });

  /** Key owners offered when an API-key subject is picked. */
  protected readonly ownerOptions = computed<ComboboxOption[]>(() =>
    this.users().map((user) => ({ value: user.id, label: user.email })),
  );

  /** Resources of the picked kind, with friendly names. */
  protected readonly resourceOptions = computed<ComboboxOption[]>(() => {
    switch (this.resourceType()) {
      case 'model':
        return this.models().map((model) => ({
          value: model.id,
          label: model.canonical_name,
          searchText: model.family ?? '',
        }));
      case 'offering':
        return this.offerings().map((offering) => ({
          value: offering.id,
          label: `${this.providerName(offering.provider_id)} → ${this.modelName(
            offering.model_id,
          )} (${offering.upstream_model_id})`,
        }));
      case 'credential':
        return this.credentials().map((credential) => ({
          value: credential.id,
          label: `${this.providerName(credential.provider_id)} · ${credential.label}`,
        }));
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
      const [grants, users, models, offerings, providers] = await Promise.all([
        this.grantsService.list(),
        this.usersService.list(),
        this.catalog.listModels(),
        this.catalog.listOfferings(),
        this.catalog.listProviders(),
      ]);
      // Credentials list per provider; flatten into one picker.
      const credentials = (
        await Promise.all(providers.map((provider) => this.catalog.listCredentials(provider.id)))
      ).flat();
      this.grants.set(grants);
      this.users.set(users);
      this.models.set(models);
      this.offerings.set(offerings);
      this.providers.set(providers);
      this.credentials.set(credentials);
      await this.loadKeyLabels(grants, users);
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  /**
   * Resolves API-key grant subjects through the per-user key endpoints; the
   * grants list carries only the key id. Only runs when such a grant exists,
   * and a failing owner read leaves that key's label to the graceful fallback.
   */
  private async loadKeyLabels(grants: GrantView[], users: UserView[]): Promise<void> {
    if (!grants.some((grant) => grant.subject_type === 'api_key')) {
      this.keyLabels.set(new Map());
      return;
    }
    const lists = await Promise.all(
      users.map((user) => this.keysService.list(user.id).catch(() => [] as ApiKeyView[])),
    );
    const labels = new Map<string, KeyLabel>();
    users.forEach((user, index) => {
      for (const key of lists[index]) {
        labels.set(key.id, { name: key.name, prefix: key.prefix, ownerEmail: user.email });
      }
    });
    this.keyLabels.set(labels);
  }

  /** Switching the subject kind resets the picked subject and its key list. */
  protected onSubjectTypeChange(): void {
    this.form.controls.key_owner_id.setValue('');
    this.form.controls.subject_id.setValue('');
    this.keys.set([]);
  }

  /** Key subjects are fetched per owner only when an API-key subject is used. */
  protected async onKeyOwnerChange(): Promise<void> {
    this.form.controls.subject_id.setValue('');
    const ownerId = this.form.controls.key_owner_id.value;
    if (ownerId === '') {
      this.keys.set([]);
      return;
    }
    try {
      this.keys.set(await this.keysService.list(ownerId));
    } catch (err) {
      this.actionError.set(describeError(err, 'Loading the account’s keys failed.'));
    }
  }

  /** Switching the resource kind resets the picked resource. */
  protected onResourceTypeChange(): void {
    this.form.controls.resource_id.setValue('');
  }

  protected openForm(): void {
    this.form.reset({
      subject_type: 'user',
      key_owner_id: '',
      subject_id: '',
      resource_type: 'model',
      resource_id: '',
      mode: 'allow',
    });
    this.keys.set([]);
    this.attempted.set(false);
    this.actionError.set(null);
    this.showForm.set(true);
  }

  protected closeForm(): void {
    this.showForm.set(false);
    this.actionError.set(null);
  }

  protected async save(): Promise<void> {
    this.attempted.set(true);
    this.actionError.set(null);
    if (this.form.invalid) {
      return;
    }
    this.saving.set(true);
    try {
      const input = this.form.getRawValue();
      await this.grantsService.create({
        subject_type: input.subject_type,
        subject_id: input.subject_id,
        resource_type: input.resource_type,
        resource_id: input.resource_id,
        mode: input.mode,
      });
      this.form.controls.subject_id.setValue('');
      this.form.controls.resource_id.setValue('');
      this.attempted.set(false);
      this.showForm.set(false);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeGrantError(err));
    } finally {
      this.saving.set(false);
    }
  }

  protected async remove(grant: GrantView): Promise<void> {
    this.deletingId.set(grant.id);
    this.actionError.set(null);
    try {
      await this.grantsService.remove(grant.id);
      this.deleteTarget.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Deleting the grant failed.'));
    } finally {
      this.deletingId.set(null);
    }
  }

  /** Opens the delete confirmation for one grant. */
  protected openDelete(grant: GrantView): void {
    this.actionError.set(null);
    this.deleteTarget.set(grant);
  }

  protected closeDelete(): void {
    this.deleteTarget.set(null);
  }

  protected confirmDelete(): void {
    const grant = this.deleteTarget();
    if (grant !== null) {
      void this.remove(grant);
    }
  }

  /** Human-readable subject: the user's email or the key's name and prefix. */
  protected subjectLabel(grant: GrantView): string {
    if (grant.subject_type === 'user') {
      return this.users().find((user) => user.id === grant.subject_id)?.email ?? grant.subject_id;
    }
    const key = this.keyLabels().get(grant.subject_id);
    if (key === undefined) {
      // The key was deleted after the grant was written; keep the row readable.
      return `deleted key ${grant.subject_id.slice(0, 8)}…`;
    }
    return `${key.name} (${key.prefix}) · ${key.ownerEmail}`;
  }

  /** Human-readable resource: canonical model, offering, or credential label. */
  protected resourceLabel(grant: GrantView): string {
    switch (grant.resource_type) {
      case 'model':
        return this.modelName(grant.resource_id);
      case 'offering': {
        const offering = this.offerings().find((item) => item.id === grant.resource_id);
        return offering === undefined
          ? grant.resource_id
          : `${this.providerName(offering.provider_id)} → ${this.modelName(
              offering.model_id,
            )} (${offering.upstream_model_id})`;
      }
      case 'credential': {
        const credential = this.credentials().find((item) => item.id === grant.resource_id);
        return credential === undefined
          ? grant.resource_id
          : `${this.providerName(credential.provider_id)} · ${credential.label}`;
      }
    }
  }

  protected showError(field: 'subject_id' | 'resource_id'): boolean {
    const control = this.form.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected fieldError(field: 'subject_id' | 'resource_id'): string {
    return field === 'subject_id'
      ? this.subjectType() === 'user'
        ? 'Choose an account.'
        : 'Choose an API key.'
      : 'Choose a resource.';
  }

  private modelName(id: string): string {
    return this.models().find((model) => model.id === id)?.canonical_name ?? id;
  }

  private providerName(id: string): string {
    return this.providers().find((provider) => provider.id === id)?.name ?? id;
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading grants failed.'));
  }
}
