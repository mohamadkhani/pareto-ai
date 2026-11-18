/// <reference types="vite/client" />

/**
 * Guardrail: pages must use the design-system controls, never the browser's
 * default `<select>`/`<input>` chrome. The primitives and the styleguide are
 * the only places allowed to render the native elements directly.
 */
interface AllowlistedRawControl {
  /** Path relative to `src/app`, e.g. `keys/keys.html`. */
  readonly file: string;
  /** Why this raw control cannot use a primitive today. */
  readonly reason: string;
}

/**
 * Empty today: every page uses `ui-input`/`ui-select`. Add an entry only when
 * a native control is genuinely required, with the reason it cannot be a
 * primitive; the test prints the allowlist so stale entries are visible.
 */
const ALLOWLIST: readonly AllowlistedRawControl[] = [];

const HTML = import.meta.glob('./**/*.html', {
  eager: true,
  query: '?raw',
  import: 'default',
}) as Record<string, string>;

function relativePath(path: string): string {
  return path.replace(/^\.\//, '');
}

function isExcluded(path: string): boolean {
  const file = relativePath(path);
  return file.startsWith('ui/') || file.startsWith('styleguide/');
}

/** Raw native controls, one entry per matching line. */
function rawControls(): string[] {
  const found: string[] = [];
  for (const [path, contents] of Object.entries(HTML)) {
    if (isExcluded(path)) {
      continue;
    }
    const file = relativePath(path);
    contents.split('\n').forEach((line, index) => {
      if (/<\s*(select|input)\b/i.test(line)) {
        found.push(`${file}:${index + 1} ${line.trim()}`);
      }
    });
  }
  return found;
}

describe('control markup guardrail', () => {
  it('has no raw select or input outside the primitives and styleguide', () => {
    const offenders = rawControls().filter(
      (entry) => !ALLOWLIST.some((allowed) => entry.startsWith(`${allowed.file}:`)),
    );

    expect(
      offenders,
      `Use ui-input/ui-select instead of native controls:\n${offenders.join('\n')}\n` +
        `Allowlist: ${ALLOWLIST.map((entry) => `${entry.file} (${entry.reason})`).join(', ')}`,
    ).toEqual([]);
  });

  it('finds the page templates it is meant to scan', () => {
    // A glob that silently matches nothing would make the check vacuous.
    expect(Object.keys(HTML).filter((path) => !isExcluded(path)).length).toBeGreaterThan(10);
  });
});
