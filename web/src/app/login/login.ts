import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { ActivatedRoute, Router } from '@angular/router';
import { ApiError } from '../api';
import { AuthService } from '../auth/auth.service';

type Field = 'email' | 'password';

/** Only same-origin paths may be used as a return URL (no open redirects). */
function safeReturnUrl(value: string | null): string {
  return value !== null && value.startsWith('/') && !value.startsWith('//') ? value : '/';
}

/** Turns an unknown failure into a user-facing message. */
function describeError(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.code === 'login_locked' && err.retryAfterSeconds !== null) {
      // The countdown goes on its own line; the banner uses `pre-line`.
      return `${err.message}\nTry again in ${err.retryAfterSeconds} seconds.`;
    }
    return err.message;
  }
  return 'Sign-in failed unexpectedly. Please try again.';
}

@Component({
  selector: 'app-login',
  imports: [ReactiveFormsModule],
  templateUrl: './login.html',
  styleUrl: './login.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Login {
  private readonly auth = inject(AuthService);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);

  protected readonly pending = signal(false);
  protected readonly attempted = signal(false);
  protected readonly errorMessage = signal<string | null>(null);

  protected readonly form = new FormGroup({
    email: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required, Validators.email],
    }),
    password: new FormControl('', {
      nonNullable: true,
      validators: [Validators.required],
    }),
  });

  private readonly returnUrl = safeReturnUrl(this.route.snapshot.queryParamMap.get('returnUrl'));

  protected async submit(): Promise<void> {
    this.attempted.set(true);
    this.errorMessage.set(null);
    if (this.form.invalid) {
      return;
    }
    this.pending.set(true);
    const { email, password } = this.form.getRawValue();
    try {
      await this.auth.login(email, password);
      await this.router.navigateByUrl(this.returnUrl);
    } catch (err) {
      this.errorMessage.set(describeError(err));
    } finally {
      this.pending.set(false);
    }
  }

  protected showError(field: Field): boolean {
    const control = this.form.controls[field];
    return control.invalid && (control.touched || this.attempted());
  }

  protected fieldError(field: Field): string {
    const control = this.form.controls[field];
    if (control.hasError('required')) {
      return field === 'email' ? 'Email is required.' : 'Password is required.';
    }
    if (control.hasError('email')) {
      return 'Enter a valid email address.';
    }
    return '';
  }
}
