import { Injectable, computed, signal } from '@angular/core';
import { requestJson, requestNoContent } from '../api';

/** The signed-in account. Never carries secrets. */
export interface User {
  id: string;
  email: string;
  display_name: string;
  role: 'admin' | 'user';
  /** Forced rotation is pending: the UI must block everything else. */
  must_change_password: boolean;
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
  /** Increments per restore so a superseded response cannot overwrite newer state. */
  private restoreGeneration = 0;

  /**
   * Restores the session from `GET /api/auth/me`. Concurrent calls share one
   * request; a failed restore resolves to "not signed in" and never throws.
   * `force` re-reads the server state even when a session is already cached
   * (used after a password change to pick up `must_change_password`): it always
   * starts a fresh request instead of joining one that began before the change.
   */
  loadMe(force = false): Promise<boolean> {
    if (!force && this.restoreInFlight !== null) {
      return this.restoreInFlight;
    }
    if (!force && this.status() === 'authenticated') {
      return Promise.resolve(true);
    }
    if (!force && this.status() === 'anonymous') {
      return Promise.resolve(false);
    }
    const generation = ++this.restoreGeneration;
    this.status.set('loading');
    const restore = requestJson<AuthSession>('/api/auth/me')
      .then((session) => {
        if (generation !== this.restoreGeneration) {
          // A newer restore already answered; do not overwrite its state.
          return this.status() === 'authenticated';
        }
        this.user.set(session.user);
        this.csrfToken.set(session.csrf_token);
        this.status.set('authenticated');
        return true;
      })
      .catch(() => {
        // 401 just means "not signed in"; network trouble surfaces on login.
        if (generation === this.restoreGeneration) {
          this.status.set('anonymous');
        }
        return this.status() === 'authenticated';
      })
      .finally(() => {
        if (generation === this.restoreGeneration) {
          this.restoreInFlight = null;
        }
      });
    this.restoreInFlight = restore;
    return restore;
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
   * Changes the caller's own password (`POST /api/auth/change-password`),
   * clearing the forced-rotation flag server-side. Throws `ApiError`
   * (`invalid_credentials` when the current password is wrong, `invalid_request`
   * for a too-short new password) on failure.
   *
   * The local user is updated on success so the guard lets the account through
   * immediately, even before (or without) a following `loadMe(true)`.
   */
  async changePassword(currentPassword: string, newPassword: string): Promise<void> {
    await requestNoContent('/api/auth/change-password', {
      method: 'POST',
      body: { current_password: currentPassword, new_password: newPassword },
      csrfToken: this.csrfToken(),
    });
    const user = this.user();
    if (user?.must_change_password) {
      this.user.set({ ...user, must_change_password: false });
    }
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
