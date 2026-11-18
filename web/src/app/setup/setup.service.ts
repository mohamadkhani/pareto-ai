import { Injectable, inject, signal } from '@angular/core';
import { requestJson } from '../api';
import { AuthService, type AuthSession } from '../auth/auth.service';

/** Which authentication state the gateway is in. */
export type SetupMode = 'setup' | 'unprotected' | 'protected';

/** Why automatic HTTPS is off. `null` once a certificate is active. */
export type TlsOffReason = 'local' | 'off' | 'no_domain' | 'pending';

/** Automatic TLS state (decision D54). */
export interface TlsStatus {
  /** True only once a certificate is installed and HTTPS is being served. */
  enabled: boolean;
  /** Certificate hostname; configured even while issuance is failing. */
  domain: string | null;
  reason: TlsOffReason | null;
  /** The last issuance/renewal failure, kept until a retry succeeds. */
  error: string | null;
  /** Certificate expiry (ISO-8601) once enabled. */
  expires_at: string | null;
}

/** What `GET /api/setup/status` returns. */
export interface SetupStatus {
  mode: SetupMode;
  bind: string;
  is_local: boolean;
  setup_required: boolean;
  warning: string | null;
  tls: TlsStatus;
  /** `env` (`GATEWAY_MASTER_KEY`) or `file` (`<data>/master.key`). */
  master_key_source: 'env' | 'file';
}

/**
 * First-run setup and exposure state.
 *
 * The status is cached in a signal and shared by the wizard, the shell banner,
 * and Settings. `load()` de-duplicates concurrent reads; call it with
 * `force = true` after anything that can change the mode.
 */
@Injectable({ providedIn: 'root' })
export class SetupService {
  private readonly auth = inject(AuthService);

  readonly status = signal<SetupStatus | null>(null);

  private inFlight: Promise<SetupStatus> | null = null;

  /** Reads `GET /api/setup/status` (once unless `force`). Throws on failure. */
  load(force = false): Promise<SetupStatus> {
    if (!force && this.inFlight !== null) {
      return this.inFlight;
    }
    if (!force && this.status() !== null) {
      return Promise.resolve(this.status() as SetupStatus);
    }
    const pending = requestJson<SetupStatus>('/api/setup/status')
      .then((status) => {
        this.status.set(status);
        return status;
      })
      .finally(() => {
        if (this.inFlight === pending) {
          this.inFlight = null;
        }
      });
    this.inFlight = pending;
    return pending;
  }

  /**
   * Creates the first admin (`POST /api/setup/admin`), adopts the returned
   * session, and refreshes the status. Throws `ApiError` on failure.
   */
  async createAdmin(email: string, password: string): Promise<void> {
    const session = await requestJson<AuthSession>('/api/setup/admin', {
      method: 'POST',
      body: { email, password },
      csrfToken: this.auth.csrfToken(),
    });
    this.auth.adoptSession(session);
    await this.load(true);
  }

  /**
   * Skips setup (`POST /api/setup/skip`): the gateway keeps running without a
   * password as a trusted local operator. Throws `ApiError` on failure.
   */
  async skip(): Promise<void> {
    const status = await requestJson<SetupStatus>('/api/setup/skip', {
      method: 'POST',
      body: { confirm: true },
      csrfToken: this.auth.csrfToken(),
    });
    this.status.set(status);
    // The caller may still be cached as "anonymous" (a previous sign-out);
    // the unprotected gateway answers `/api/auth/me` with the local operator,
    // so the next guard check must ask again instead of trusting the cache.
    this.auth.forget();
  }
}
