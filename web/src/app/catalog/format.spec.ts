import {
  formatAge,
  formatCount,
  formatPricePerM,
  formatTime,
  formatUsd,
  formatUsdPrecise,
  joinTags,
  parseTags,
} from './format';

describe('catalog format helpers', () => {
  it('formatAge renders seconds, minutes, hours and days ago', () => {
    const now = Date.parse('2026-01-01T12:00:00.000Z');
    expect(formatAge('2026-01-01T11:59:30.000Z', now)).toBe('30s ago');
    expect(formatAge('2026-01-01T11:55:00.000Z', now)).toBe('5m ago');
    expect(formatAge('2026-01-01T09:00:00.000Z', now)).toBe('3h ago');
    expect(formatAge('2025-12-29T12:00:00.000Z', now)).toBe('3d ago');
  });

  it('formatAge and formatUsd keep unknown values visible as missing', () => {
    expect(formatAge(null)).toBe('—');
    expect(formatUsd(null)).toBe('—');
  });

  it('formatUsd renders two decimal places', () => {
    expect(formatUsd(12.5)).toBe('$12.50');
  });

  it('formatUsdPrecise adapts precision and keeps unknown as missing', () => {
    expect(formatUsdPrecise(null)).toBe('—');
    expect(formatUsdPrecise(2.1)).toBe('$2.10');
    expect(formatUsdPrecise(12.5)).toBe('$12.50');
    expect(formatUsdPrecise(0.0234)).toBe('$0.0234');
    expect(formatUsdPrecise(0.000123)).toBe('$0.000123');
    expect(formatUsdPrecise(0)).toBe('$0');
    expect(formatUsdPrecise(0.0000001)).toBe('$0');
  });

  it('formatPricePerM keeps sub-cent prices visible and unknown as missing', () => {
    expect(formatPricePerM(null)).toBe('—');
    expect(formatPricePerM(1.25)).toBe('$1.25');
    expect(formatPricePerM(5)).toBe('$5.00');
    expect(formatPricePerM(0.00015)).toBe('$0.00015');
  });

  it('formatCount groups thousands and keeps unknown as missing', () => {
    expect(formatCount(null)).toBe('—');
    expect(formatCount(1048576)).toBe('1,048,576');
  });

  it('formatTime falls back to the raw value for invalid timestamps', () => {
    expect(formatTime('not-a-date')).toBe('not-a-date');
    expect(formatTime(null)).toBe('—');
  });

  it('parseTags and joinTags round-trip comma-separated tags', () => {
    expect(parseTags(' text, image ,, ')).toEqual(['text', 'image']);
    expect(joinTags(['text', 'image'])).toBe('text, image');
  });
});
