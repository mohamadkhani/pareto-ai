import {
  BALANCE_EXAMPLE_RAW,
  balanceExample,
  balanceFactor,
  convertBalance,
  convertPrice,
  detectTemplateId,
  detectTier,
  entryRelativePointer,
  formatConverted,
  isPath,
  expressionIssue,
  pointerFromPath,
  resolvePointer,
  rewriteTier,
  splitPointer,
  unitExample,
} from './connector-form';
import type { BalanceUnit, ConnectorTemplate, PriceUnit } from './connectors.types';

function unit(overrides: Partial<PriceUnit> = {}): PriceUnit {
  return {
    source: 'per_million_tokens',
    multiplier: 1,
    currency: 'USD',
    fx_to_usd: null,
    display: { symbol: '$', decimals: 4 },
    note: null,
    ...overrides,
  };
}

describe('paths (D61)', () => {
  it('builds and splits $. paths with bracket indexes', () => {
    expect(pointerFromPath(['data', 0, 'pricing', 'input'])).toBe('$.data[0].pricing.input');
    expect(splitPointer('$.data[0].pricing.input')).toEqual(['data', '0', 'pricing', 'input']);
    expect(pointerFromPath([])).toBe('$');
    expect(splitPointer('$')).toEqual([]);
    expect(splitPointer('/data/0/id')).toEqual([]);
  });

  it('resolves objects and array indexes and rejects bad steps', () => {
    const document = { data: [{ id: 'm1', pricing: { input: 0.5 } }] };
    expect(resolvePointer(document, '$.data[0].id')).toBe('m1');
    expect(resolvePointer(document, '$.data[0].pricing.input')).toBe(0.5);
    expect(resolvePointer(document, '$')).toBe(document);
    expect(resolvePointer(document, '$.data[9].id')).toBeUndefined();
    expect(resolvePointer(document, '$.data[x].id')).toBeUndefined();
    expect(resolvePointer(document, '/data/0/id')).toBeUndefined();
    expect(resolvePointer(document, '$.data[0].id * 2')).toBeUndefined();
  });

  it('flags an arithmetic expression as not a lone path', () => {
    expect(isPath('$.pricing.prompt')).toBe(true);
    expect(isPath('$.pricing.prompt * 1000000')).toBe(true);
    expect(resolvePointer({ pricing: { prompt: 1 } }, '$.pricing.prompt * 2')).toBeUndefined();
  });
});

describe('entry-relative paths', () => {
  it('drops the entries prefix so a price path is relative to one model', () => {
    expect(entryRelativePointer('$.data', '$.data[0].pricing.input')).toBe('$.pricing.input');
  });

  it('drops a direct entry prefix and leaves unrelated paths alone', () => {
    expect(entryRelativePointer('$.data[0]', '$.data[0].pricing.input')).toBe('$.pricing.input');
    expect(entryRelativePointer('$.data', '$.pricing.input')).toBe('$.pricing.input');
    expect(entryRelativePointer('$.data', '$.id')).toBe('$.id');
  });
});

describe('client-side field checks (D61)', () => {
  it('accepts the documented forms and flags the obvious mistakes', () => {
    expect(expressionIssue('')).toBeNull();
    expect(expressionIssue('$.pricing.prompt')).toBeNull();
    expect(expressionIssue('$.pricing.prompt * 1000000')).toBeNull();
    expect(expressionIssue('($.a - $.b) * 0.01')).toBeNull();
    expect(expressionIssue('max($.balance, 0)')).toBeNull();

    expect(expressionIssue('/pricing/prompt')).toContain('not /');
    expect(expressionIssue('$.')).toContain('needs a key');
    expect(expressionIssue('$')).toContain('needs a key');
    expect(expressionIssue('$.a * (1 + 2')).toContain('not closed');
    expect(expressionIssue('$.a + 1)')).toContain('no opening');
    expect(expressionIssue('$.a % 2')).toContain('not part of an expression');
  });

  it('rejects an operator in a field that takes one path only', () => {
    expect(expressionIssue('$.data', true)).toBeNull();
    expect(expressionIssue('$.data[0].id', true)).toBeNull();
    expect(expressionIssue('$.a + 1', true)).toContain('one path only');
  });
});

describe('unit conversion (D44)', () => {
  it('multiplies per-token prices by one million and leaves per-million alone', () => {
    expect(convertPrice(0.0000005, unit({ source: 'per_token' }))).toBeCloseTo(0.5, 12);
    expect(convertPrice(0.07, unit())).toBeCloseTo(0.07, 12);
  });

  it('applies the custom multiplier (cents quoted per million)', () => {
    expect(convertPrice(0.07, unit({ source: 'custom', multiplier: 0.01 }))).toBeCloseTo(
      0.0007,
      12,
    );
    expect(unitExample(unit({ source: 'custom', multiplier: 0.01 }))).toBe('raw 0.07 → $0.0007/M');
  });

  it('uses the display symbol and decimals', () => {
    expect(formatConverted(0.0007, { symbol: '¢', decimals: 2 })).toBe('¢0.00');
    expect(formatConverted(1.23456, { symbol: '$', decimals: 4 })).toBe('$1.2346');
  });
});

describe('tier helper', () => {
  const sample = {
    data: [
      {
        id: 'm1',
        pricing_usd_per_million_tokens: {
          asap: { input: 1, output: 2 },
          balanced: { input: 3, output: 4 },
          flex: { input: 5, output: 6 },
        },
      },
    ],
  };

  it('finds the shared tier segment and its siblings in the sample', () => {
    const info = detectTier(sample, [
      '$.pricing_usd_per_million_tokens.balanced.input',
      '$.pricing_usd_per_million_tokens.balanced.output',
    ]);
    expect(info).not.toBeNull();
    expect(info?.index).toBe(1);
    expect(info?.parentPointer).toBe('$.pricing_usd_per_million_tokens');
    expect(info?.current).toBe('balanced');
    expect(info?.options).toEqual(['asap', 'balanced', 'flex']);
  });

  it('rewrites only the shared segment on every matching pointer', () => {
    const info = detectTier(sample, [
      '$.pricing_usd_per_million_tokens.balanced.input',
      '$.pricing_usd_per_million_tokens.balanced.output',
      '$.id',
    ]);
    if (info === null) {
      throw new Error('expected a tier');
    }
    const rewritten = rewriteTier(
      [
        { field: 'input', pointer: '$.pricing_usd_per_million_tokens.balanced.input' },
        { field: 'output', pointer: '$.pricing_usd_per_million_tokens.balanced.output' },
        { field: 'model_id', pointer: '$.id' },
      ],
      info,
      'flex',
    );
    expect(rewritten.map((assignment) => assignment.pointer)).toEqual([
      '$.pricing_usd_per_million_tokens.flex.input',
      '$.pricing_usd_per_million_tokens.flex.output',
      '$.id',
    ]);
  });

  it('falls back to the known tiers when no sample is loaded', () => {
    const info = detectTier(undefined, ['$.pricing.balanced.input', '$.pricing.balanced.output']);
    expect(info?.current).toBe('balanced');
    expect(info?.options).toEqual(['asap', 'balanced', 'flex']);
  });
});

describe('template detection', () => {
  const sail: ConnectorTemplate = {
    id: 'sailresearch',
    label: 'SailResearch',
    description: '',
    configs: {
      catalog: {
        url: 'https://api.sailresearch.com/v1/models',
        headers: {},
        allow_private: false,
        catalog: {
          entries: '$.data',
          model_id: '$.id',
          input: '$.pricing_usd_per_million_tokens.balanced.input',
          output: '$.pricing_usd_per_million_tokens.balanced.output',
          cache_read: null,
          cache_write: null,
          reasoning: null,
          unit: unit({ source: 'custom', multiplier: 0.01 }),
          modalities_input: null,
          modalities_output: null,
          context_length: null,
          max_output: null,
          apply_on_discovery: true,
          refresh_seconds: null,
        },
      },
    },
  };

  it('detects the template whose pointers resolve against the sample', () => {
    const sample = {
      data: [
        {
          id: 'm1',
          pricing_usd_per_million_tokens: {
            balanced: { input: 1, output: 2 },
          },
        },
      ],
    };
    expect(detectTemplateId([sail], 'catalog', sample)).toBe('sailresearch');
    expect(detectTemplateId([sail], 'catalog', { data: [] })).toBeNull();
  });
});

describe('balance unit conversion (D52)', () => {
  function balanceUnit(overrides: Partial<BalanceUnit> = {}): BalanceUnit {
    return {
      multiplier: 1,
      currency: 'USD',
      fx_to_usd: null,
      display: { symbol: '$', decimals: 2 },
      note: null,
      ...overrides,
    };
  }

  it('applies the multiplier before any FX rate', () => {
    const unit = balanceUnit({ multiplier: 0.01 });
    expect(convertBalance(136_275, unit)).toBeCloseTo(1362.75, 9);
    expect(balanceExample(unit)).toBe('raw 136275 → $1362.75');
    expect(balanceExample(balanceUnit())).toBe('raw 136275 → $136275.00');
  });

  it('uses FX only for a non-USD currency and ignores a non-positive rate', () => {
    expect(balanceFactor(balanceUnit({ currency: 'EUR', fx_to_usd: 1.1 }))).toBeCloseTo(1.1, 9);
    expect(balanceFactor(balanceUnit({ currency: 'EUR', fx_to_usd: 0 }))).toBe(1);
    expect(balanceFactor(balanceUnit({ currency: 'eur', fx_to_usd: -2 }))).toBe(1);
    expect(balanceFactor(balanceUnit({ multiplier: 0 }))).toBe(0);
    expect(BALANCE_EXAMPLE_RAW).toBe(136_275);
  });
});
