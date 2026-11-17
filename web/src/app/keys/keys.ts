import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';
import { AuthService } from '../auth/auth.service';
import { describeError, isForbidden } from '../catalog/errors';
import { formatTime } from '../catalog/format';
import { positiveInteger } from '../catalog/validators';
import { UiBadge } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiDialog } from '../ui/dialog';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiTable } from '../ui/table';
import { KeysService } from './keys.service';
import type { ApiKeyPatch, ApiKeyView, CreatedKey } from './keys.types';

type FormState = { mode: 'create' } | { mode: 'edit'; key: ApiKeyView };

/** The plaintext key on display until the owner dismisses it. */
interface ShownKey {
  name: string;
  key: string;
}

/** Formats an RFC-3339 instant for `<input type="datetime-local">` (local time). */
function toLocalInput(iso: string | null): string {
  if (iso === null || iso === '') {
    return '';
  }
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) {
    return '';
  }
  const pad = (value: number): string => String(value).padStart(2, '0');
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}` +
    `T${pad(date.getHours())}:${pad(date.getMinutes())}`
  );
}

/** Parses a `datetime-local` value back to an RFC-3339 UTC instant. */
function fromLocalInput(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed === '') {
    return null;
  }
  const date = new Date(trimmed);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

/** True when two RFC-3339 instants (or nulls) denote the same moment. */
function sameInstant(a: string | null, b: string | null): boolean {
  if (a === null || b === null) {
    return a === b;
  }
  return Date.parse(a) === Date.parse(b);
}

/** Only the fields that changed; `null` keeps on the wire (decision D20). */
function buildPatch(key: ApiKeyView, raw: { name: string; expires_at: string }): ApiKeyPatch {
  const patch: ApiKeyPatch = {};
  const name = raw.name.trim();
  if (name !== '' && name !== key.name) {
    patch.name = name;
  }
  const expiresAt = fromLocalInput(raw.expires_at);
  // An empty input keeps the stored expiry: the wire cannot clear it.
  if (expiresAt !== null && !sameInstant(expiresAt, key.expires_at)) {
    patch.expires_at = expiresAt;
  }
  return patch;
}

/** The API-key page: list, issue, edit limits, revoke. Self or admin. */
@Component({
  selector: 'app-keys',
  imports: [ReactiveFormsModule, UiBadge, UiBanner, UiButton, UiDialog, UiEmpty, UiField, UiTable],
  templateUrl: './keys.html',
  styleUrl: './keys.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Keys {
  private readonly auth = inject(AuthService);
  private readonly keysService = inject(KeysService);
  private readonly route = inject(ActivatedRoute);

  private readonly params = toSignal(this.route.paramMap, {
    initialValue: this.route.snapshot.paramMap,
  });

  /** Whose keys are shown: the route's `:id` when an admin opens another account. */
  protected readonly targetUserId = computed(
    () => this.params().get('id') ?? this.auth.user()?.id ?? '',
  );
  protected readonly viewingSelf = computed(() => this.targetUserId() === this.auth.user()?.id);

  protected readonly keys = signal<ApiKeyView[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly formState = signal<FormState | null>(null);
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly revokingId = signal<string | null>(null);
  /** The key the revoke confirmation dialog is open for. */
  protected readonly revokeTarget = signal<ApiKeyView | null>(null);
  /** The just-issued plaintext key, shown exactly once. */
  protected readonly createdKey = signal<ShownKey | null>(null);
  protected readonly copied = signal(false);

  protected readonly formatTime = formatTime;

  protected readonly form = new FormGroup({
    name: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    expires_at: new FormControl('', { nonNullable: true }),
    rate_limit_rpm: new FormControl<number | null>(null, { validators: [positiveInteger] }),
    rate_limit_tpm: new FormControl<number | null>(null, { validators: [positiveInteger] }),
  });

  constructor() {
    void this.reload();
  }

  protected async reload(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      this.keys.set(await this.keysService.list(this.targetUserId()));
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected openCreate(): void {
    this.form.reset({ name: '', expires_at: '', rate_limit_rpm: null, rate_limit_tpm: null });
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'create' });
  }

  protected openEdit(key: ApiKeyView): void {
    this.form.reset({
      name: key.name,
      expires_at: toLocalInput(key.expires_at),
      rate_limit_rpm: key.rate_limit_rpm,
      rate_limit_tpm: key.rate_limit_tpm,
    });
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'edit', key });
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
      const raw = this.form.getRawValue();
      if (state.mode === 'create') {
        const created: CreatedKey = await this.keysService.create(this.targetUserId(), {
          name: raw.name.trim(),
          expires_at: fromLocalInput(raw.expires_at) ?? undefined,
          rate_limit_rpm: raw.rate_limit_rpm ?? undefined,
          rate_limit_tpm: raw.rate_limit_tpm ?? undefined,
        });
        // Hold on to the plaintext for exactly this one display.
        this.createdKey.set({ name: created.api_key.name, key: created.key });
        this.copied.set(false);
      } else {
        const patch = buildPatch(state.key, raw);
        // `null` means "keep" over the wire, so only concrete values go out.
        if (raw.rate_limit_rpm !== null && raw.rate_limit_rpm !== state.key.rate_limit_rpm) {
          patch.rate_limit_rpm = raw.rate_limit_rpm;
        }
        if (raw.rate_limit_tpm !== null && raw.rate_limit_tpm !== state.key.rate_limit_tpm) {
          patch.rate_limit_tpm = raw.rate_limit_tpm;
        }
        if (Object.keys(patch).length > 0) {
          await this.keysService.update(state.key.id, patch);
        }
      }
      this.formState.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the key failed.'));
    } finally {
      this.saving.set(false);
    }
  }

  /** Copies the shown key; the clipboard may be unavailable in tests. */
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

  /** Drops the plaintext from memory; it is never recoverable afterwards. */
  protected dismissKey(): void {
    this.createdKey.set(null);
    this.copied.set(false);
  }

  protected async revoke(key: ApiKeyView): Promise<void> {
    this.revokingId.set(key.id);
    this.actionError.set(null);
    try {
      await this.keysService.revoke(key.id);
      this.revokeTarget.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Revoking the key failed.'));
    } finally {
      this.revokingId.set(null);
    }
  }

  /** Opens the revoke confirmation for one key. */
  protected openRevoke(key: ApiKeyView): void {
    this.actionError.set(null);
    this.revokeTarget.set(key);
  }

  protected closeRevoke(): void {
    this.revokeTarget.set(null);
  }

  protected confirmRevoke(): void {
    const key = this.revokeTarget();
    if (key !== null) {
      void this.revoke(key);
    }
  }

  protected isExpired(key: ApiKeyView): boolean {
    return key.expires_at !== null && Date.parse(key.expires_at) <= Date.now();
  }

  protected showError(field: 'name' | 'rate_limit_rpm' | 'rate_limit_tpm'): boolean {
    const control = this.form.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected fieldError(field: 'name' | 'rate_limit_rpm' | 'rate_limit_tpm'): string {
    const control = this.form.controls[field];
    if (control.hasError('required')) {
      return 'A name is required.';
    }
    if (control.hasError('positiveInteger')) {
      return 'Use a whole number of 1 or more.';
    }
    return '';
  }

  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading API keys failed.'));
  }
}
