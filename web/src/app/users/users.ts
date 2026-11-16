import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import {
  AbstractControl,
  FormControl,
  FormGroup,
  ReactiveFormsModule,
  Validators,
} from '@angular/forms';
import { AuthService } from '../auth/auth.service';
import { DeleteConfirm } from '../catalog/delete-confirm';
import { describeError, isForbidden } from '../catalog/errors';
import { formatTime } from '../catalog/format';
import { UsersService } from './users.service';
import { MIN_PASSWORD_LEN, type UserPatch, type UserView } from './users.types';

type FormState = { mode: 'create' } | { mode: 'edit'; user: UserView };

/** Validated fields of the create form. */
type CreateField = 'email' | 'display_name' | 'password';

/** The validation message for one form control, or '' when it is valid. */
function fieldErrorText(control: AbstractControl, field: string): string {
  if (control.hasError('required')) {
    return field === 'email'
      ? 'Email is required.'
      : field === 'display_name'
        ? 'Display name is required.'
        : field === 'password'
          ? 'A password is required.'
          : 'This field is required.';
  }
  if (control.hasError('email')) {
    return 'Enter a valid email address.';
  }
  if (control.hasError('minlength')) {
    return `Use at least ${MIN_PASSWORD_LEN} characters.`;
  }
  return '';
}

/** The user directory: create, edit, and force password resets. Admin only. */
@Component({
  selector: 'app-users',
  imports: [ReactiveFormsModule, DeleteConfirm],
  templateUrl: './users.html',
  styleUrl: './users.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Users {
  private readonly auth = inject(AuthService);
  private readonly usersService = inject(UsersService);

  protected readonly users = signal<UserView[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly actionError = signal<string | null>(null);
  protected readonly formState = signal<FormState | null>(null);
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly resetUser = signal<UserView | null>(null);
  protected readonly resetting = signal(false);

  protected readonly formatTime = formatTime;
  protected readonly minLength = MIN_PASSWORD_LEN;

  protected readonly createForm = new FormGroup({
    email: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required, Validators.email],
    }),
    display_name: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    role: new FormControl<'admin' | 'user'>('user', {
      nonNullable: true,
      validators: [Validators.required],
    }),
    password: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required, Validators.minLength(MIN_PASSWORD_LEN)],
    }),
    must_change_password: new FormControl(true, { nonNullable: true }),
  });

  protected readonly editForm = new FormGroup({
    display_name: new FormControl('', { nonNullable: true, validators: [Validators.required] }),
    role: new FormControl<'admin' | 'user'>('user', {
      nonNullable: true,
      validators: [Validators.required],
    }),
    is_active: new FormControl(true, { nonNullable: true }),
  });

  protected readonly resetForm = new FormGroup({
    password: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required, Validators.minLength(MIN_PASSWORD_LEN)],
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
      this.users.set(await this.usersService.list());
    } catch (err) {
      this.handleError(err);
    } finally {
      this.loading.set(false);
    }
  }

  protected openCreate(): void {
    this.createForm.reset({
      email: '',
      display_name: '',
      role: 'user',
      password: '',
      must_change_password: true,
    });
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'create' });
  }

  protected openEdit(user: UserView): void {
    this.editForm.reset({
      display_name: user.display_name,
      role: user.role,
      is_active: user.is_active,
    });
    this.attempted.set(false);
    this.actionError.set(null);
    this.formState.set({ mode: 'edit', user });
  }

  protected closeForm(): void {
    this.formState.set(null);
  }

  protected async save(): Promise<void> {
    this.attempted.set(true);
    this.actionError.set(null);
    const state = this.formState();
    if (state === null) {
      return;
    }
    const form = state.mode === 'create' ? this.createForm : this.editForm;
    if (form.invalid) {
      return;
    }
    this.saving.set(true);
    try {
      if (state.mode === 'create') {
        const input = this.createForm.getRawValue();
        await this.usersService.create({
          email: input.email.trim(),
          display_name: input.display_name.trim(),
          role: input.role,
          password: input.password,
          must_change_password: input.must_change_password,
        });
      } else {
        // Only the fields the admin changed (null keeps on the wire).
        const patch: UserPatch = {};
        const input = this.editForm.getRawValue();
        if (input.display_name.trim() !== state.user.display_name) {
          patch.display_name = input.display_name.trim();
        }
        if (input.role !== state.user.role) {
          patch.role = input.role;
        }
        if (input.is_active !== state.user.is_active) {
          patch.is_active = input.is_active;
        }
        if (Object.keys(patch).length > 0) {
          await this.usersService.update(state.user.id, patch);
        }
      }
      this.formState.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Saving the account failed.'));
    } finally {
      this.saving.set(false);
    }
  }

  /** Opens the forced-rotation panel for one account. */
  protected openReset(user: UserView): void {
    this.resetForm.reset({ password: '' });
    this.attempted.set(false);
    this.actionError.set(null);
    this.resetUser.set(user);
  }

  protected closeReset(): void {
    this.resetUser.set(null);
  }

  /**
   * Resets the account's password (D23: this revokes the account's API keys
   * and other sessions and forces rotation at next sign-in).
   */
  protected async resetPassword(): Promise<void> {
    const user = this.resetUser();
    this.attempted.set(true);
    this.actionError.set(null);
    if (user === null || this.resetForm.invalid) {
      return;
    }
    this.resetting.set(true);
    try {
      await this.usersService.update(user.id, { password: this.resetForm.getRawValue().password });
      this.resetUser.set(null);
      await this.reload();
    } catch (err) {
      this.actionError.set(describeError(err, 'Resetting the password failed.'));
    } finally {
      this.resetting.set(false);
    }
  }

  protected showCreateError(field: CreateField): boolean {
    const control = this.createForm.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected createFieldError(field: CreateField): string {
    return fieldErrorText(this.createForm.controls[field], field);
  }

  protected showEditError(): boolean {
    const control = this.editForm.controls.display_name;
    return control.invalid && (control.touched || this.attempted());
  }

  protected editFieldError(): string {
    return fieldErrorText(this.editForm.controls.display_name, 'display_name');
  }

  protected showResetError(): boolean {
    const control = this.resetForm.controls.password;
    return control.invalid && (control.touched || this.attempted());
  }

  protected resetFieldError(): string {
    return fieldErrorText(this.resetForm.controls.password, 'password');
  }
  private handleError(err: unknown): void {
    if (isForbidden(err)) {
      this.forbidden.set(true);
      return;
    }
    this.loadError.set(describeError(err, 'Loading accounts failed.'));
  }
}
