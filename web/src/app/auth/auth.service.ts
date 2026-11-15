import { Injectable, computed, signal } from '@angular/core';
import { requestJson, requestNoContent } from '../api';

/** The signed-in account. Never carries secrets. */
export interface User {
  id: string;
  email: string;
  display_name: string;
  role: 'admin' | 'user';
}

/** What `POST /api/auth/login` and `GET /api/auth/me` return. */
export interface AuthSession {
  user: User;
  csrf_token: string;
}

/** Where the session restore currently stands. */
export type AuthStatus = 'unknown' | 'loading' | 'authenticated' | 'anonymous';

/**
 * Holds the signed-in user and the CSRF token in signals.
 *
 * The session itself lives in an HttpOnly cookie; this service only mirrors
 * its state and forwards the CSRF token on state-changing calls.
 */
@Injectable({ providedIn: 'root' })
export class AuthService {
  readonly user = signal<User | null>(null);
  readonly csrfToken = signal<string | null>(null);
  readonly status = signal<AuthStatus>('unknown');
  readonly isAdmin = computed(() => this.user()?.role === 'admin');

  private restoreInFlight: Promise<boolean> | null = null;

  /**
   * Restores the session from `GET /api/auth/me`. Concurrent calls share one
   * request; a failed restore resolves to "not signed in" and never throws.
   */
  loadMe(): Promise<boolean> {
    if (this.restoreInFlight !== null) {
      return this.restoreInFlight;
    }
    if (this.status() === 'authenticated') {
      return Promise.resolve(true);
    }
    if (this.status() === 'anonymous') {
      return Promise.resolve(false);
    }
    this.status.set('loading');
    this.restoreInFlight = requestJson<AuthSession>('/api/auth/me')
      .then((session) => {
        this.user.set(session.user);
        this.csrfToken.set(session.csrf_token);
        this.status.set('authenticated');
        return true;
      })
      .catch(() => {
        // 401 just means "not signed in"; network trouble surfaces on login.
        this.status.set('anonymous');
        return false;
      })
      .finally(() => {
        this.restoreInFlight = null;
      });
    return this.restoreInFlight;
  }

  /**
   * Signs in and stores the returned user and CSRF token.
   * Throws `ApiError` (e.g. `invalid_credentials`, `login_locked`) on failure.
   */
  async login(email: string, password: string): Promise<void> {
    const session = await requestJson<AuthSession>('/api/auth/login', {
      method: 'POST',
      body: { email, password },
      csrfToken: this.csrfToken(),
    });
    this.user.set(session.user);
    this.csrfToken.set(session.csrf_token);
    this.status.set('authenticated');
  }

  /**
   * Signs out. Always clears the local session state, but rethrows an API or
   * network failure so callers can decide whether to surface it.
   */
  async logout(): Promise<void> {
    try {
      await requestNoContent('/api/auth/logout', {
        method: 'POST',
        csrfToken: this.csrfToken(),
      });
    } finally {
      this.user.set(null);
      this.csrfToken.set(null);
      this.status.set('anonymous');
    }
  }
}
