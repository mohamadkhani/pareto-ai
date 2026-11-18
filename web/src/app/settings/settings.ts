import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { ApiError } from '../api';
import { AuthService } from '../auth/auth.service';
import { MIN_PASSWORD_LEN } from '../users/users.types';
import { SetupService } from '../setup/setup.service';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiCard } from '../ui/card';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';

/** Turns an unknown failure into a user-facing message. */
function describeError(err: unknown): string {
  return err instanceof ApiError ? err.message : 'The request failed unexpectedly. Try again.';
}

/**
 * Account settings (admin only): change the signed-in password, set the first
 * password while the gateway is unprotected, and read the exposure facts.
 */
@Component({
  selector: 'app-settings',
  imports: [UiBanner, UiButton, UiCard, UiEmpty, UiField, UiInput],
  templateUrl: './settings.html',
  styleUrl: './settings.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Settings {
  private readonly auth = inject(AuthService);
  private readonly setup = inject(SetupService);

  protected readonly minLength = MIN_PASSWORD_LEN;
  protected readonly forbidden = computed(() => !this.auth.isAdmin());
  protected readonly status = this.setup.status;
  protected readonly unprotected = computed(() => this.status()?.mode === 'unprotected');

  protected readonly modeLabel = computed(() => {
    switch (this.status()?.mode) {
      case 'setup':
        return 'Setup — no account exists yet';
      case 'unprotected':
        return 'Unprotected — no password';
      case 'protected':
        return 'Protected — passwords required';
      default:
        return 'Unknown';
    }
  });

  /** One line describing automatic HTTPS, including why it is off. */
  protected readonly tlsLabel = computed(() => {
    const tls = this.status()?.tls;
    if (!tls) {
      return 'Unknown';
    }
    if (tls.enabled) {
      const expiry = tls.expires_at ? `, expires ${tls.expires_at.slice(0, 10)}` : '';
      return `HTTPS — ${tls.domain ?? 'enabled'}${expiry}`;
    }
    switch (tls.reason) {
      case 'local':
        return 'Off — plain HTTP (local bind)';
      case 'off':
        return 'Off — plain HTTP (GATEWAY_TLS=off)';
      case 'no_domain':
        return 'Off — plain HTTP (set GATEWAY_PUBLIC_DOMAIN to enable HTTPS)';
      case 'pending':
        return `Obtaining a certificate for ${tls.domain ?? 'this domain'}…`;
      default:
        return tls.error !== null ? 'Failing — plain HTTP (see below)' : 'Off — plain HTTP';
    }
  });

  protected readonly notice = signal<string | null>(null);
  protected readonly errorMessage = signal<string | null>(null);

  // Change-password form.
  protected readonly changing = signal(false);
  protected readonly changeAttempted = signal(false);
  protected readonly currentPassword = signal('');
  protected readonly newPassword = signal('');
  protected readonly newConfirm = signal('');

  // First-password form (unprotected mode only).
  protected readonly settingPassword = signal(false);
  protected readonly firstAttempted = signal(false);
  protected readonly firstEmail = signal('');
  protected readonly firstPassword = signal('');
  protected readonly firstConfirm = signal('');

  protected readonly currentPasswordError = computed(() =>
    this.changeAttempted() && this.currentPassword().length === 0
      ? 'Your current password is required.'
      : '',
  );

  protected readonly newPasswordError = computed(() => {
    if (!this.changeAttempted()) {
      return '';
    }
    if (this.newPassword().length === 0) {
      return 'A new password is required.';
    }
    return this.newPassword().length < MIN_PASSWORD_LEN
      ? `Use at least ${MIN_PASSWORD_LEN} characters.`
      : '';
  });

  protected readonly newConfirmError = computed(() => {
    if (!this.changeAttempted()) {
      return '';
    }
    if (this.newConfirm() !== this.newPassword()) {
      return 'The two passwords do not match.';
    }
    return '';
  });

  protected readonly firstEmailError = computed(() => {
    if (!this.firstAttempted()) {
      return '';
    }
    return this.firstEmail().trim().length === 0 ? 'An admin email is required.' : '';
  });

  protected readonly firstPasswordError = computed(() => {
    if (!this.firstAttempted()) {
      return '';
    }
    return this.firstPassword().length < MIN_PASSWORD_LEN
      ? `Use at least ${MIN_PASSWORD_LEN} characters.`
      : '';
  });

  protected readonly firstConfirmError = computed(() => {
    if (!this.firstAttempted()) {
      return '';
    }
    return this.firstConfirm() !== this.firstPassword() ? 'The two passwords do not match.' : '';
  });

  constructor() {
    // The shell (or the guard) has usually loaded the status already; a direct
    // navigation loads it here.
    void this.setup.load().catch(() => undefined);
  }

  protected async changePassword(event?: Event): Promise<void> {
    event?.preventDefault();
    if (this.changing()) {
      return;
    }
    this.changeAttempted.set(true);
    this.notice.set(null);
    this.errorMessage.set(null);
    if (
      this.currentPasswordError() !== '' ||
      this.newPasswordError() !== '' ||
      this.newConfirmError() !== ''
    ) {
      return;
    }

    this.changing.set(true);
    try {
      await this.auth.changePassword(this.currentPassword(), this.newPassword());
    } catch (err) {
      this.errorMessage.set(
        err instanceof ApiError && err.code === 'invalid_credentials'
          ? 'Your current password is not correct.'
          : describeError(err),
      );
      this.changing.set(false);
      return;
    }
    this.changing.set(false);
    this.currentPassword.set('');
    this.newPassword.set('');
    this.newConfirm.set('');
    this.changeAttempted.set(false);
    this.notice.set(
      'Password changed. Your other sessions were signed out; your API keys still work.',
    );
  }

  protected async setFirstPassword(event?: Event): Promise<void> {
    event?.preventDefault();
    if (this.settingPassword()) {
      return;
    }
    this.firstAttempted.set(true);
    this.notice.set(null);
    this.errorMessage.set(null);
    if (
      this.firstEmailError() !== '' ||
      this.firstPasswordError() !== '' ||
      this.firstConfirmError() !== ''
    ) {
      return;
    }

    this.settingPassword.set(true);
    try {
      await this.setup.createAdmin(this.firstEmail().trim(), this.firstPassword());
    } catch (err) {
      this.errorMessage.set(
        err instanceof ApiError && err.code === 'setup_already_complete'
          ? 'An account already exists. Reload the page to see the current state.'
          : describeError(err),
      );
      this.settingPassword.set(false);
      return;
    }
    this.settingPassword.set(false);
    this.firstPassword.set('');
    this.firstConfirm.set('');
    this.firstAttempted.set(false);
    this.notice.set('Password set. This gateway now requires a password to sign in.');
  }
}
