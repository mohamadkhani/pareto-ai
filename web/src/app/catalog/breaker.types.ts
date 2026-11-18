/**
 * Wire types for the breaker view (D50): the live per-(credential, offering)
 * state plus the manual reset input. Trip history lives in the database and is
 * read through the same admin session.
 */

/** One pair's live breaker state. */
export interface BreakerEntry {
  credential_id: string;
  offering_id: string;
  /** `closed`, `open`, or `half_open`. */
  status: 'closed' | 'open' | 'half_open';
  /** Consecutive counted failures with no success in between. */
  consecutive: number;
  /** How many times this pair has opened since the process started. */
  trips: number;
  /** Remaining milliseconds of the open window; null when not open. */
  open_for_ms: number | null;
  /** The taxonomy class of the last counted failure. */
  last_error_class: string | null;
  /** The bounded, redacted provider text of the last counted failure (D49). */
  last_error_detail: string | null;
}

/** `GET /api/breaker`. */
export interface BreakerListResponse {
  entries: BreakerEntry[];
}

/** `POST /api/breaker/reset`. */
export interface BreakerResetInput {
  credential_id: string;
  offering_id: string;
}
