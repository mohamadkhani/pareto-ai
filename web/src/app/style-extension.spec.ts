/// <reference types="vite/client" />

/**
 * Guardrail: the console is styled with indented Sass (`.sass`, D53). A `.css`
 * file is not compiled by the build here — it is silently dead weight — so any
 * reintroduced stylesheet would look applied in source review while never
 * reaching the page. Keep the allowlist empty; if a plain CSS file is ever
 * genuinely required, add it with the reason and delete it when that reason
 * goes away.
 */
const ALLOWLIST: readonly string[] = [];

const CSS_FILES = import.meta.glob('../**/*.css', {
  eager: true,
  query: '?raw',
  import: 'default',
}) as Record<string, string>;

describe('style extension guardrail', () => {
  it('has no stylesheet files under src that are not indented Sass', () => {
    const offenders = Object.keys(CSS_FILES)
      .map((path) => path.replace(/^\.\.\//, 'src/'))
      .filter((path) => !ALLOWLIST.includes(path));

    expect(offenders, 'convert these files to .sass (D53)').toEqual([]);
  });
});
