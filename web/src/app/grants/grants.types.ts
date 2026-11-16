/**
 * Wire types for the M4 access-grant API (`/api/grants`).
 *
 * A grant binds a subject (user or API key) to a resource (model, offering, or
 * credential) in `allow` or `deny` mode; `deny` beats `allow` and a `user`
 * role without grants is denied. A duplicate (subject, resource) pair answers
 * 409 `conflict`. Failures use the `{ error: { code, message, request_id } }`
 * envelope; mutations require the CSRF token.
 */

/** Who a grant applies to. */
export type GrantSubjectType = 'user' | 'api_key';

/** What a grant covers. */
export type GrantResourceType = 'model' | 'offering' | 'credential';

/** `deny` beats `allow` in the effective access matrix. */
export type GrantMode = 'allow' | 'deny';

/** One grant as the API returns it. */
export interface GrantView {
  id: string;
  subject_type: GrantSubjectType;
  subject_id: string;
  resource_type: GrantResourceType;
  resource_id: string;
  mode: GrantMode;
  created_by: string | null;
  created_at: string;
}

/** `POST /api/grants` (status 201). */
export interface GrantInput {
  subject_type: GrantSubjectType;
  subject_id: string;
  resource_type: GrantResourceType;
  resource_id: string;
  mode: GrantMode;
}

/** `GET /api/grants` query: narrow to one subject. */
export interface GrantFilter {
  subject_type?: GrantSubjectType;
  subject_id?: string;
}
