import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';
import { ApiError } from '../api';
import { AuthService } from '../auth/auth.service';
import { MIN_PASSWORD_LEN } from '../users/users.types';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';

type Field = 'current_password' | 'new_password' | 'confirm_password';

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
  imports: [UiBanner, UiButton, UiField, UiInput],
  templateUrl: './change-password.html',
  styleUrl: './change-password.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ChangePassword {
  private readonly auth = inject(AuthService);
  private readonly router = inject(Router);

  protected readonly pending = signal(false);
  /** The password is already changed; a second submit must not re-post it. */
  private readonly completed = signal(false);
  protected readonly attempted = signal(false);
  protected readonly errorMessage = signal<string | null>(null);
  protected readonly minLength = MIN_PASSWORD_LEN;

  protected readonly currentPassword = signal('');
  protected readonly newPassword = signal('');
  protected readonly confirmPassword = signal('');

  /** Field errors returned by the server, cleared on the next submit. */
  private readonly serverErrors = signal<Partial<Record<Field, string>>>({});

  protected readonly currentPasswordError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    if (this.currentPassword().length === 0) {
      return 'Your current password is required.';
    }
    return this.serverErrors().current_password ?? '';
  });

  protected readonly newPasswordError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    if (this.newPassword().length === 0) {
      return 'A new password is required.';
    }
    if (this.newPassword().length < MIN_PASSWORD_LEN) {
      return `Use at least ${MIN_PASSWORD_LEN} characters.`;
    }
    return this.serverErrors().new_password ?? '';
  });

  protected readonly confirmPasswordError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    if (this.confirmPassword().length === 0) {
      return 'Confirm the new password.';
    }
    if (this.confirmPassword() !== this.newPassword()) {
      return 'The two passwords do not match.';
    }
    return '';
  });

  /**
   * Submits the change. The pending state ends as soon as the server confirms
   * the change, before the session refresh and navigation: those steps can be
   * slow or fail and must never leave the button stuck on "Changing…".
   */
  protected async submit(event?: Event): Promise<void> {
    event?.preventDefault();
    if (this.pending() || this.completed()) {
      return;
    }
    this.attempted.set(true);
    this.errorMessage.set(null);
    this.serverErrors.set({});
    if (
      this.currentPasswordError() !== '' ||
      this.newPasswordError() !== '' ||
      this.confirmPasswordError() !== ''
    ) {
      return;
    }

    this.pending.set(true);
    try {
      await this.auth.changePassword(this.currentPassword(), this.newPassword());
    } catch (err) {
      this.reportError(err);
      this.pending.set(false);
      return;
    }
    this.pending.set(false);
    this.completed.set(true);

    // The server cleared the flag; re-read the session so the rest of the app
    // (role, display name, CSRF token) matches. A failed refresh must not block
    // the user: the local user is already marked as rotated.
    await this.auth.loadMe(true);
    await this.router.navigateByUrl('/');
  }

  private reportError(err: unknown): void {
    if (err instanceof ApiError && err.code === 'invalid_credentials') {
      this.serverErrors.set({ current_password: describeError(err) });
      return;
    }
    if (err instanceof ApiError && err.code === 'invalid_request') {
      this.serverErrors.set({ new_password: err.message });
      return;
    }
    this.errorMessage.set(describeError(err));
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
}
