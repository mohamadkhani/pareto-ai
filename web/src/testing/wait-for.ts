/**
 * Polling helpers for component tests. `fixture.whenStable()` only waits for
 * Angular's own pending tasks, not for arbitrary promises such as a mocked
 * `fetch` chain, so tests that act after an async operation must wait for the
 * rendered result instead of assuming one change-detection pass is enough.
 */

/** Anything that can run change detection: a fixture or its component ref. */
export interface ChangeDetectable {
  detectChanges(): void;
}

const DEFAULT_TIMEOUT_MS = 2000;
const POLL_INTERVAL_MS = 5;

/** Polls `probe` after change detection until it returns a truthy value. */
export async function waitFor<T>(
  fixture: ChangeDetectable,
  probe: () => T | null | undefined | false,
  description: string,
  timeoutMs = DEFAULT_TIMEOUT_MS,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    fixture.detectChanges();
    const value = probe();
    if (value !== null && value !== undefined && value !== false) {
      return value;
    }
    if (Date.now() >= deadline) {
      throw new Error(`Timed out after ${timeoutMs} ms waiting for ${description}`);
    }
    await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
  }
}

/** Waits until the rendered text contains `text`. */
export async function waitForText(
  fixture: ChangeDetectable,
  root: ParentNode,
  text: string,
  timeoutMs?: number,
): Promise<void> {
  await waitFor(
    fixture,
    () => (root.textContent ?? '').includes(text),
    `text ${JSON.stringify(text)}`,
    timeoutMs,
  );
}

/** Waits until a button whose trimmed label equals `text` is rendered. */
export function waitForButton(
  fixture: ChangeDetectable,
  root: ParentNode,
  text: string,
  timeoutMs?: number,
): Promise<HTMLButtonElement> {
  return waitFor(
    fixture,
    () =>
      Array.from(root.querySelectorAll('button')).find(
        (candidate) => candidate.textContent?.trim() === text,
      ),
    `button labelled ${JSON.stringify(text)}`,
    timeoutMs,
  );
}
