import { Injectable, inject } from '@angular/core';
import { requestJson } from '../api';
import { AuthService } from '../auth/auth.service';
import type { UserInput, UserPatch, UserView } from './users.types';

/**
 * All HTTP calls for the M4 user-admin API. Admin only: the server answers
 * 403 to non-admins and audits every read and write. Mutations send the
 * session cookie and the CSRF token.
 */
@Injectable({ providedIn: 'root' })
export class UsersService {
  private readonly auth = inject(AuthService);

  /** Lists every account. Throws `ApiError` on failure. */
  list(): Promise<UserView[]> {
    return requestJson<UserView[]>('/api/users');
  }

  /** Creates an account (status 201) and returns the created row. */
  create(input: UserInput): Promise<UserView> {
    return requestJson<UserView>('/api/users', {
      method: 'POST',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /**
   * Applies a partial account update and returns the updated row. Omitted
   * fields keep their values; a `password` forces rotation (D23).
   */
  update(id: string, patch: UserPatch): Promise<UserView> {
    return requestJson<UserView>(`/api/users/${id}`, {
      method: 'PATCH',
      body: patch,
      csrfToken: this.auth.csrfToken(),
    });
  }
}
