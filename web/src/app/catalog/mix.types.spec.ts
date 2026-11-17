import type { MixProfile } from './mix.types';
import {
  mixBreakdown,
  profileShareTotal,
  profileSummary,
  type MixComponentPrices,
} from './mix.types';

function profile(overrides: Partial<MixProfile> = {}): MixProfile {
  return {
    id: 'mix-code-agent',
    name: 'code-agent',
    kind: 'preset',
    input_share: 8.1,
    output_share: 0.4,
    cache_read_share: 91.5,
    cache_write_share: 0,
    reasoning_share: 0,
    is_builtin: true,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function prices(overrides: Partial<MixComponentPrices> = {}): MixComponentPrices {
  return {
    input_per_m: 3,
    output_per_m: 15,
    cache_read_per_m: 0.3,
    cache_write_per_m: null,
    reasoning_per_m: null,
    ...overrides,
  };
}

describe('mix price helpers', () => {
  it('sums the raw stored shares and renders the three-component summary', () => {
    expect(profileShareTotal(profile())).toBe(100);
    expect(profileSummary(profile())).toBe('8.1 / 0.4 / 91.5');
    expect(profileSummary(profile({ cache_write_share: 1.2, reasoning_share: 0.6 }))).toBe(
      '8.1 / 0.4 / 91.5 / w 1.2 / r 0.6',
    );
  });

  it('blends at the profile shares with cache read billed at the cache price', () => {
    const breakdown = mixBreakdown(profile(), prices());
    expect(breakdown).not.toBeNull();
    if (breakdown === null) {
      return;
    }
    // 0.081*3 + 0.004*15 + 0.915*0.3
    expect(breakdown.per_m).toBeCloseTo(0.5775, 10);
    expect(breakdown.rows.map((row) => row.component)).toEqual(['input', 'output', 'cache_read']);
    expect(breakdown.rows[2].price_per_m).toBe(0.3);
    expect(breakdown.rows[2].inherited_from).toBeNull();
  });

  it('bills the cache share at the input price when no cache price exists', () => {
    const breakdown = mixBreakdown(profile(), prices({ cache_read_per_m: null }));
    expect(breakdown).not.toBeNull();
    if (breakdown === null) {
      return;
    }
    const cache = breakdown.rows.find((row) => row.component === 'cache_read');
    expect(cache?.price_per_m).toBe(3);
    expect(cache?.inherited_from).toBe('input');
    // 0.081*3 + 0.004*15 + 0.915*3 = 3.048 (the nocache identity)
    expect(breakdown.per_m).toBeCloseTo(3.048, 10);
  });

  it('normalises shares that do not sum to 100 before blending', () => {
    const breakdown = mixBreakdown(
      profile({ input_share: 1, output_share: 1, cache_read_share: 0, cache_write_share: 2 }),
      prices({ cache_write_per_m: 4 }),
    );
    expect(breakdown?.rows.map((row) => row.share)).toEqual([0.25, 0.25, 0.5]);
    expect(breakdown?.rows[2].price_per_m).toBe(4);
    expect(breakdown?.per_m).toBeCloseTo(0.25 * 3 + 0.25 * 15 + 0.5 * 4, 10);
  });

  it('returns null for an impossible blend or all-zero shares', () => {
    expect(mixBreakdown(profile(), prices({ input_per_m: null }))).toBeNull();
    expect(
      mixBreakdown(profile({ input_share: 0, output_share: 0, cache_read_share: 0 }), prices()),
    ).toBeNull();
  });

  it('skips zero-share components and inherits reasoning from output', () => {
    const breakdown = mixBreakdown(
      profile({
        input_share: 0,
        output_share: 10,
        cache_read_share: 0,
        reasoning_share: 10,
      }),
      prices({ reasoning_per_m: null }),
    );
    expect(breakdown?.rows.map((row) => row.component)).toEqual(['output', 'reasoning']);
    expect(breakdown?.rows[1].inherited_from).toBe('output');
    expect(breakdown?.per_m).toBeCloseTo(0.5 * 15 + 0.5 * 15, 10);
  });
});
