/**
 * Wire types for the M4 user-admin API (`/api/users`) and the self-service
 * password change (`/api/auth/change-password`).
 *
 * Every request/response shape lives here so that aligning the UI with the
 * server is a one-file change. Failures use the `{ error: { code, message,
 * request_id } }` envelope (parsed by `api.ts`), timestamps are RFC-3339 UTC
 * strings ending in `Z`, and mutations require the CSRF token.
 */

/** Account roles; the server rejects anything else. */
export type UserRole = 'admin' | 'user';

/** One account as `GET /api/users` returns it. Never carries the password. */
export interface UserView {
  id: string;
  email: string;
  display_name: string;
  role: UserRole;
  is_active: boolean;
  /** Forced rotation is pending (review finding 7). */
  must_change_password: boolean;
  created_at: string;
  updated_at: string;
  last_login_at: string | null;
}

/** `POST /api/users`: the initial password is at least 12 characters. */
export interface UserInput {
  email: string;
  display_name: string;
  role: UserRole;
  /** Initial password; the account is flagged for rotation unless overridden. */
  password: string;
  /** Defaults to true server-side: an admin-set password must be rotated. */
  must_change_password?: boolean;
}

/**
 * `PATCH /api/users/{id}` (decision D20): `null`/omitted fields stay
 * unchanged. A `password` resets the account's password and — unless
 * `must_change_password` says otherwise — flags forced rotation (D23: the
 * reset also revokes the account's API keys and other sessions).
 */
export interface UserPatch {
  display_name?: string;
  role?: UserRole;
  is_active?: boolean;
  password?: string;
  must_change_password?: boolean;
}

/** Shortest password the API accepts (server `MIN_PASSWORD_LEN`). */
export const MIN_PASSWORD_LEN = 12;
