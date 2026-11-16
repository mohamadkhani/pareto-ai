import { formatAge, formatTime, formatUsd, joinTags, parseTags } from './format';

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

  it('formatTime falls back to the raw value for invalid timestamps', () => {
    expect(formatTime('not-a-date')).toBe('not-a-date');
    expect(formatTime(null)).toBe('—');
  });

  it('parseTags and joinTags round-trip comma-separated tags', () => {
    expect(parseTags(' text, image ,, ')).toEqual(['text', 'image']);
    expect(joinTags(['text', 'image'])).toBe('text, image');
  });
});
