/**
 * Thin typed wrapper over `fetch` for the admin API (`/api/*`).
 *
 * Every call sends the session cookie (`credentials: 'include'`), state-changing
 * calls carry the CSRF token in `X-CSRF-Token`, and failures are surfaced as
 * `ApiError` with the server's stable error code.
 */

/** What the server returns when a request fails. */
interface ErrorEnvelope {
  code: string;
  message: string;
  requestId: string;
}

/** Options for one admin API call. */
export interface ApiRequestOptions {
  method?: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';
  /** JSON-serializable request body. */
  body?: unknown;
  /** CSRF token to send as `X-CSRF-Token` on state-changing calls. */
  csrfToken?: string | null;
}

/** A failed API call, carrying the server's stable error code and message. */
export class ApiError extends Error {
  constructor(
    message: string,
    readonly code: string,
    readonly status: number,
    readonly requestId: string,
    readonly retryAfterSeconds: number | null = null,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

/** Performs a request and parses its JSON body. Throws `ApiError` on failure. */
export async function requestJson<T>(path: string, options: ApiRequestOptions = {}): Promise<T> {
  const response = await send(path, options);
  const body: unknown = await response.json();
  return body as T;
}

/** Performs a request with no response body. Throws `ApiError` on failure. */
export async function requestNoContent(
  path: string,
  options: ApiRequestOptions = {},
): Promise<void> {
  await send(path, options);
}

async function send(path: string, options: ApiRequestOptions): Promise<Response> {
  const method = options.method ?? 'GET';
  const headers = new Headers({ Accept: 'application/json' });
  if (options.body !== undefined) {
    headers.set('Content-Type', 'application/json');
  }
  if (options.csrfToken != null && method !== 'GET') {
    headers.set('X-CSRF-Token', options.csrfToken);
  }

  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      credentials: 'include',
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    });
  } catch {
    throw new ApiError(
      'Could not reach the gateway. Check your connection and try again.',
      'network_error',
      0,
      '',
    );
  }
  if (!response.ok) {
    throw await toApiError(response);
  }
  return response;
}

async function toApiError(response: Response): Promise<ApiError> {
  let body: unknown = null;
  try {
    body = await response.json();
  } catch {
    // A non-JSON error body still becomes a generic ApiError below.
  }
  const envelope = parseErrorEnvelope(body);
  return new ApiError(
    envelope?.message ?? `Request failed (HTTP ${response.status}).`,
    envelope?.code ?? 'unknown_error',
    response.status,
    envelope?.requestId ?? '',
    parseRetryAfter(response.headers.get('Retry-After')),
  );
}

function parseErrorEnvelope(body: unknown): ErrorEnvelope | null {
  if (typeof body !== 'object' || body === null || !('error' in body)) {
    return null;
  }
  const inner: unknown = body.error;
  if (typeof inner !== 'object' || inner === null || !('code' in inner) || !('message' in inner)) {
    return null;
  }
  const code: unknown = inner.code;
  const message: unknown = inner.message;
  const requestId: unknown = 'request_id' in inner ? inner.request_id : '';
  if (typeof code !== 'string' || typeof message !== 'string') {
    return null;
  }
  return { code, message, requestId: typeof requestId === 'string' ? requestId : '' };
}

function parseRetryAfter(value: string | null): number | null {
  if (value === null) {
    return null;
  }
  const seconds = Number(value);
  return Number.isFinite(seconds) && seconds >= 0 ? Math.ceil(seconds) : null;
}
