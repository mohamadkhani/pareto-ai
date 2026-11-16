import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import {
  AbstractControl,
  FormControl,
  FormGroup,
  ReactiveFormsModule,
  ValidationErrors,
  Validators,
} from '@angular/forms';
import { Router } from '@angular/router';
import { ApiError } from '../api';
import { AuthService } from '../auth/auth.service';
import { MIN_PASSWORD_LEN } from '../users/users.types';

type Field = 'current_password' | 'new_password' | 'confirm_password';

/** Requires `confirm_password` to equal `new_password`. */
function passwordsMatch(group: AbstractControl): ValidationErrors | null {
  const password: unknown = group.get('new_password')?.value;
  const confirm: unknown = group.get('confirm_password')?.value;
  return password === confirm ? null : { passwordsMismatch: true };
}

/** Turns an unknown failure into a user-facing message. */
function describeError(err: unknown): string {
  if (err instanceof ApiError) {
    // The server's generic 401 text ("invalid email or password") would be
    // confusing here: on this screen it only ever means the current password.
    return err.code === 'invalid_credentials'
      ? 'Your current password is not correct.'
      : err.message;
  }
  return 'Changing the password failed unexpectedly. Please try again.';
}

/**
 * The forced-rotation screen (review finding 7): an account flagged
 * `must_change_password` is held here until it sets a new password. `authGuard`
 * keeps every other screen unreachable meanwhile.
 */
@Component({
  selector: 'app-change-password',
  imports: [ReactiveFormsModule],
  templateUrl: './change-password.html',
  styleUrl: './change-password.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ChangePassword {
  private readonly auth = inject(AuthService);
  private readonly router = inject(Router);

  protected readonly pending = signal(false);
  protected readonly attempted = signal(false);
  protected readonly errorMessage = signal<string | null>(null);
  protected readonly minLength = MIN_PASSWORD_LEN;

  protected readonly form = new FormGroup(
    {
      current_password: new FormControl('', {
        nonNullable: true,
        validators: [Validators.required],
      }),
      new_password: new FormControl('', {
        nonNullable: true,
        validators: [Validators.required, Validators.minLength(MIN_PASSWORD_LEN)],
      }),
      confirm_password: new FormControl('', {
        nonNullable: true,
        validators: [Validators.required],
      }),
    },
    { validators: passwordsMatch },
  );

  protected async submit(): Promise<void> {
    this.attempted.set(true);
    this.errorMessage.set(null);
    if (this.form.invalid) {
      return;
    }
    this.pending.set(true);
    const { current_password, new_password } = this.form.getRawValue();
    try {
      await this.auth.changePassword(current_password, new_password);
      // Re-read the session: the rotation flag has cleared server-side.
      await this.auth.loadMe(true);
      await this.router.navigateByUrl('/');
    } catch (err) {
      this.errorMessage.set(describeError(err));
    } finally {
      this.pending.set(false);
    }
  }

  /** Signs out so a different account can sign in instead. */
  protected async signOut(): Promise<void> {
    try {
      await this.auth.logout();
    } catch {
      // Local session state is cleared even when the server call fails.
    }
    await this.router.navigateByUrl('/login');
  }

  protected showError(field: Field): boolean {
    const control = this.form.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected fieldError(field: Field): string {
    const control = this.form.controls[field];
    if (control.hasError('required')) {
      return field === 'current_password'
        ? 'Your current password is required.'
        : field === 'new_password'
          ? 'A new password is required.'
          : 'Confirm the new password.';
    }
    if (control.hasError('minlength')) {
      return `Use at least ${MIN_PASSWORD_LEN} characters.`;
    }
    return '';
  }

  /** The group-level mismatch is reported under the confirm field. */
  protected mismatch(): boolean {
    const confirm = this.form.controls.confirm_password;
    return this.form.hasError('passwordsMismatch') && (confirm.touched || this.attempted());
  }
}
