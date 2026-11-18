import {
  ChangeDetectionStrategy,
  Component,
  afterNextRender,
  computed,
  inject,
  signal,
} from '@angular/core';
import { DOCUMENT } from '@angular/common';
import { ActivatedRoute, Router } from '@angular/router';
import { ApiError } from '../api';
import { AuthService } from '../auth/auth.service';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';

/** Only same-origin paths may be used as a return URL (no open redirects). */
function safeReturnUrl(value: string | null): string {
  return value !== null && value.startsWith('/') && !value.startsWith('//') ? value : '/';
}

/** Turns an unknown failure into a user-facing message. */
function describeError(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.code === 'login_locked' && err.retryAfterSeconds !== null) {
      return `${err.message} Try again in ${err.retryAfterSeconds} seconds.`;
    }
    return err.message;
  }
  return 'Sign-in failed unexpectedly. Please try again.';
}

/** A pragmatic email check; the server is the authority. */
const EMAIL_PATTERN = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

@Component({
  selector: 'app-login',
  imports: [UiBanner, UiButton, UiField, UiInput],
  templateUrl: './login.html',
  styleUrl: './login.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Login {
  private readonly auth = inject(AuthService);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);

  protected readonly pending = signal(false);
  protected readonly attempted = signal(false);
  protected readonly errorMessage = signal<string | null>(null);

  protected readonly email = signal('');
  protected readonly password = signal('');

  private readonly document = inject(DOCUMENT);

  protected readonly emailError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    const email = this.email().trim();
    if (email.length === 0) {
      return 'Email is required.';
    }
    return EMAIL_PATTERN.test(email) ? '' : 'Enter a valid email address.';
  });

  protected readonly passwordError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    return this.password().length === 0 ? 'Password is required.' : '';
  });

  private readonly returnUrl = safeReturnUrl(this.route.snapshot.queryParamMap.get('returnUrl'));

  constructor() {
    // Put the cursor where the user starts typing; the field is the first control.
    afterNextRender(() => {
      this.document.querySelector<HTMLInputElement>('#login-email')?.focus();
    });
  }

  protected async submit(event?: Event): Promise<void> {
    event?.preventDefault();
    if (this.pending()) {
      return;
    }
    this.attempted.set(true);
    this.errorMessage.set(null);
    if (this.emailError() !== '' || this.passwordError() !== '') {
      return;
    }

    this.pending.set(true);
    try {
      await this.auth.login(this.email().trim(), this.password());
    } catch (err) {
      this.errorMessage.set(describeError(err));
      this.pending.set(false);
      return;
    }
    this.pending.set(false);
    await this.router.navigateByUrl(this.returnUrl);
  }
}
