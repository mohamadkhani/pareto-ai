/**
 * A scripted `fetch` double for tests. Install it with
 * `vi.stubGlobal('fetch', mock.fetch)` and queue responses with `respond()`.
 * Kept free of test-runner imports so the app typecheck stays clean.
 */

/** One recorded call. */
export interface RecordedRequest {
  url: string;
  method: string;
  headers: Headers;
  credentials: RequestCredentials | undefined;
  /** Parsed JSON request body, or null when the call sent none. */
  body: unknown;
}

/** Replays queued responses and records every call. */
export class FetchMock {
  readonly requests: RecordedRequest[] = [];
  private readonly responses: Response[] = [];

  /** Queues the response for the next call. */
  respond(response: Response): void {
    this.responses.push(response);
  }

  /** The fake fetch to install as `globalThis.fetch`. */
  readonly fetch = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const body: unknown = typeof init?.body === 'string' ? JSON.parse(init.body) : null;
    this.requests.push({
      url: String(input),
      method: init?.method ?? 'GET',
      headers: new Headers(init?.headers),
      credentials: init?.credentials,
      body,
    });
    return Promise.resolve(this.responses.shift() ?? new Response(null, { status: 500 }));
  };
}

/** A JSON response with the admin API's content type. */
export function jsonResponse(
  status: number,
  body: unknown,
  headers: Record<string, string> = {},
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  });
}

/** A response in the admin API's error envelope. */
export function errorResponse(
  status: number,
  code: string,
  message: string,
  headers: Record<string, string> = {},
): Response {
  return jsonResponse(status, { error: { code, message, request_id: 'test-request' } }, headers);
}

/** A 204 like `POST /api/auth/logout` returns. */
export function noContentResponse(): Response {
  return new Response(null, { status: 204 });
}
