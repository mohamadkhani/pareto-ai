import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';
import { ApiError } from '../api';
import { MIN_PASSWORD_LEN } from '../users/users.types';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';
import { SetupService } from './setup.service';

/** Turns an unknown failure into a user-facing message. */
function describeError(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.code === 'setup_local_only') {
      return 'Setup can only be finished from the machine running the gateway.';
    }
    if (err.code === 'setup_already_complete') {
      return 'An account already exists. Sign in instead.';
    }
    return err.message;
  }
  return 'Setup failed unexpectedly. Please try again.';
}

/** A pragmatic email check; the server is the authority. */
const EMAIL_PATTERN = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

/**
 * The first-run wizard: create the first admin, or skip to run unprotected on
 * this machine. `/setup` is the only unauthenticated screen besides login.
 */
@Component({
  selector: 'app-setup',
  imports: [UiBanner, UiButton, UiField, UiInput],
  templateUrl: './setup.html',
  styleUrl: './setup.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Setup {
  private readonly setup = inject(SetupService);
  private readonly router = inject(Router);

  protected readonly minLength = MIN_PASSWORD_LEN;
  protected readonly pending = signal(false);
  protected readonly skipping = signal(false);
  protected readonly attempted = signal(false);
  protected readonly errorMessage = signal<string | null>(null);

  protected readonly email = signal('');
  protected readonly password = signal('');
  protected readonly confirm = signal('');

  /** `is_local` decides whether the mutations are available at all. */
  protected readonly isLocal = computed(() => this.setup.status()?.is_local !== false);
  protected readonly bind = computed(() => this.setup.status()?.bind ?? 'the gateway');

  protected readonly emailError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    const email = this.email().trim();
    if (email.length === 0) {
      return 'An admin email is required.';
    }
    return EMAIL_PATTERN.test(email) ? '' : 'Enter a valid email address.';
  });

  protected readonly passwordError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    if (this.password().length === 0) {
      return 'A password is required, or choose “Skip for now”.';
    }
    return this.password().length < MIN_PASSWORD_LEN
      ? `Use at least ${MIN_PASSWORD_LEN} characters.`
      : '';
  });

  protected readonly confirmError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    if (this.confirm().length === 0) {
      return 'Confirm the password.';
    }
    return this.confirm() === this.password() ? '' : 'The two passwords do not match.';
  });

  constructor() {
    // The `setupPageGuard` already loaded the status; this keeps a direct
    // navigation in a fresh tab consistent.
    void this.setup.load().catch(() => undefined);
  }

  protected async submit(event?: Event): Promise<void> {
    event?.preventDefault();
    if (this.pending() || this.skipping()) {
      return;
    }
    this.attempted.set(true);
    this.errorMessage.set(null);
    if (this.emailError() !== '' || this.passwordError() !== '' || this.confirmError() !== '') {
      return;
    }

    this.pending.set(true);
    try {
      await this.setup.createAdmin(this.email().trim(), this.password());
    } catch (err) {
      this.errorMessage.set(describeError(err));
      this.pending.set(false);
      return;
    }
    this.pending.set(false);
    await this.router.navigateByUrl('/');
  }

  protected async skip(): Promise<void> {
    if (this.pending() || this.skipping()) {
      return;
    }
    this.errorMessage.set(null);
    this.skipping.set(true);
    try {
      await this.setup.skip();
    } catch (err) {
      this.errorMessage.set(describeError(err));
      this.skipping.set(false);
      return;
    }
    this.skipping.set(false);
    await this.router.navigateByUrl('/');
  }
}
