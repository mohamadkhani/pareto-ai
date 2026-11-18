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

/** A URL-prefix response that is always available, not queued. */
interface Route {
  prefix: string;
  respond: () => Response;
  /** When false the call is answered but not recorded (background reads). */
  record: boolean;
}

/** Replays queued responses and records every call. */
export class FetchMock {
  readonly requests: RecordedRequest[] = [];
  private readonly responses: Response[] = [];
  private readonly routes: Route[] = [];

  /** Queues the response for the next call. */
  respond(response: Response): void {
    this.responses.push(response);
  }

  /**
   * Answers every call whose URL starts with `prefix`, without consuming the
   * FIFO queue. `record: false` (the default) keeps incidental background calls
   * out of the assertion array; pass `record: true` when the call is the
   * subject of the test.
   */
  respondTo(prefix: string, respond: () => Response, record = false): void {
    this.routes.push({ prefix, respond, record });
  }

  /** The fake fetch to install as `globalThis.fetch`. */
  readonly fetch = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input);
    const body: unknown = typeof init?.body === 'string' ? JSON.parse(init.body) : null;
    const record = (): void => {
      this.requests.push({
        url,
        method: init?.method ?? 'GET',
        headers: new Headers(init?.headers),
        credentials: init?.credentials,
        body,
      });
    };
    const route = this.routes.find((candidate) => url.startsWith(candidate.prefix));
    if (route !== undefined) {
      if (route.record) {
        record();
      }
      return Promise.resolve(route.respond());
    }
    record();
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
