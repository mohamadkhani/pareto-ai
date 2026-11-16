/**
 * Wire types for the M4 API-key API (`/api/users/{id}/keys`, `/api/keys/{id}`).
 *
 * The plaintext key exists only in the `POST` response (`CreatedKey.key`) and
 * is never recoverable afterwards; every other shape carries only the visible
 * `prefix`. Failures use the `{ error: { code, message, request_id } }`
 * envelope; mutations require the CSRF token.
 */

/** One key as lists return it. Never carries the key itself. */
export interface ApiKeyView {
  id: string;
  user_id: string;
  name: string;
  /** Visible head of the key (e.g. `pk_live_ab12`); useless for auth. */
  prefix: string;
  /** Stored scope names as a JSON array string (informational in v1). */
  scopes: string | null;
  expires_at: string | null;
  revoked_at: string | null;
  last_used_at: string | null;
  /** NULL means the server defaults (60 RPM / large TPM). */
  rate_limit_rpm: number | null;
  rate_limit_tpm: number | null;
  created_at: string;
  updated_at: string;
}

/**
 * `POST /api/users/{id}/keys` (status 201): the view plus the plaintext key,
 * which exists **only** in this response. The UI must show it once and never
 * persist it anywhere.
 */
export interface CreatedKey {
  /** The full key (`pk_live_...`); the only copy the client ever gets. */
  key: string;
  api_key: ApiKeyView;
}

/** `POST /api/users/{id}/keys`. */
export interface ApiKeyInput {
  name: string;
  /** RFC-3339 expiry; omitted means "never expires". */
  expires_at?: string;
  /** Scope names (informational in v1, not enforced). */
  scopes?: string[];
  /** Requests per minute; omitted means the server default. */
  rate_limit_rpm?: number;
  /** Tokens per minute; omitted means the server default. */
  rate_limit_tpm?: number;
}

/**
 * `PATCH /api/keys/{id}` (decision D20): `null`/omitted fields stay
 * unchanged. The wire cannot clear an expiry or rate limit back to null —
 * only concrete values go out.
 */
export interface ApiKeyPatch {
  name?: string;
  expires_at?: string;
  rate_limit_rpm?: number;
  rate_limit_tpm?: number;
}
