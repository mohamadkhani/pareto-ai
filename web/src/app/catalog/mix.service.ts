import { Injectable, inject } from '@angular/core';
import { requestJson, requestNoContent } from '../api';
import { AuthService } from '../auth/auth.service';
import type { MixProfile, MixProfileInput, PricingSettings } from './mix.types';

/**
 * HTTP calls for token-mix profiles and the pricing setting (D34), all
 * admin-only. Mutations send the session cookie and the CSRF token; a
 * built-in profile edit or delete is a 400 and an unknown selector a 404.
 */
@Injectable({ providedIn: 'root' })
export class MixService {
  private readonly auth = inject(AuthService);

  /** Lists every profile, built-ins first. */
  listProfiles(): Promise<MixProfile[]> {
    return requestJson<MixProfile[]>('/api/mix-profiles');
  }

  /** Creates a custom profile (status 201). */
  createProfile(input: MixProfileInput): Promise<MixProfile> {
    return requestJson<MixProfile>('/api/mix-profiles', {
      method: 'POST',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Replaces a profile; built-ins reject with 400. */
  updateProfile(id: string, input: MixProfileInput): Promise<MixProfile> {
    return requestJson<MixProfile>(`/api/mix-profiles/${id}`, {
      method: 'PUT',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Deletes a profile; built-ins reject with 400. */
  deleteProfile(id: string): Promise<void> {
    return requestNoContent(`/api/mix-profiles/${id}`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Reads the stored default and the effective profile (env wins). */
  pricingSettings(): Promise<PricingSettings> {
    return requestJson<PricingSettings>('/api/settings/pricing');
  }

  /**
   * Sets the default profile by id or name; null clears the setting so the
   * built-in `code-agent` applies again. Returns the refreshed settings.
   */
  setDefaultProfile(selector: string | null): Promise<PricingSettings> {
    return requestJson<PricingSettings>('/api/settings/pricing', {
      method: 'PUT',
      body: { default_mix_profile: selector },
      csrfToken: this.auth.csrfToken(),
    });
  }
}
