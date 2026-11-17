import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { ComboPreviewPanel } from './combo-preview';
import type { ComboPreview } from './combos.types';

function preview(overrides: Partial<ComboPreview> = {}): ComboPreview {
  return {
    profile: { profile_id: 'mix-code-agent', profile_name: 'code-agent' },
    scored: 1,
    candidates: [
      {
        offering_id: 'off1',
        model: 'anthropic/claude',
        provider: 'openrouter',
        quality: 1450,
        quality_source: 'benchmark',
        mix_price: 0.578,
        score: {
          price: 0.9,
          speed: 0.4,
          quality: 0.8,
          reliability: 0.7,
          penalties: 0.05,
          total: -0.12,
          imputed: ['price'],
          shrunk: ['speed'],
        },
        explanation: 'scored #1 of 2: best weighted score -0.1200',
      },
    ],
    excluded: [],
    sticky: null,
    ...overrides,
  };
}

async function create(value: ComboPreview): Promise<ComponentFixture<ComboPreviewPanel>> {
  TestBed.configureTestingModule({ imports: [ComboPreviewPanel] });
  await TestBed.compileComponents();
  const fixture = TestBed.createComponent(ComboPreviewPanel);
  fixture.componentRef.setInput('preview', value);
  fixture.detectChanges();
  return fixture;
}

describe('ComboPreviewPanel', () => {
  it('says it is the runtime decision and shows the effective profile', async () => {
    const fixture = await create(preview());
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('This is exactly what routing will do');
    expect(root.textContent).toContain('code-agent');
    expect(root.textContent).toContain('mix-code-agent');
    expect(root.textContent).toContain('1 scored');
    expect(root.textContent).toContain('0 excluded (1 total)');
  });

  it('prints scored, shown, excluded and total consistently when the list is truncated', async () => {
    const fixture = await create(
      preview({
        scored: 90,
        excluded: [
          {
            offering_id: 'off9',
            model: 'qwen/qwen3',
            provider: 'together',
            reason: 'unpriced',
            detail: 'no mix price under the profile',
          },
        ],
      }),
    );
    const root = fixture.nativeElement as HTMLElement;
    const pool = root.querySelectorAll('dd')[1]?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

    // The server scored 90, the request's limit showed 1, and 1 was excluded:
    // "#1 of 90" in the explanation now agrees with this header.
    expect(pool).toContain('90 scored');
    expect(pool).toContain('showing 1');
    expect(pool).toContain('1 excluded');
    expect(pool).toContain('91 total');
    expect(root.textContent).toContain('scored #1 of 2');
  });

  it('renders candidates in order with quality, mix price, and a clamped score bar', async () => {
    const fixture = await create(preview());
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelectorAll('.candidate')).toHaveLength(1);
    expect(root.textContent).toContain('anthropic/claude');
    expect(root.textContent).toContain('1450.0');
    expect(root.textContent).toContain('$0.5780');
    // A negative total prints as-is; only the bar width clamps.
    expect(root.textContent).toContain('-0.120');
    const fill = root.querySelector<HTMLElement>('.score-fill');
    expect(fill?.style.width).toBe('0%');
    expect(root.textContent).toContain('imputed at 0.5: price');
    expect(root.textContent).toContain('shrunk toward 0.5: speed');
  });

  it('groups exclusions by reason and keeps their details', async () => {
    const fixture = await create(
      preview({
        excluded: [
          {
            offering_id: 'off2',
            model: 'qwen/qwen3',
            provider: 'together',
            reason: 'below_min_quality',
            detail: 'quality 900 is below the minimum 1200',
          },
          {
            offering_id: 'off3',
            model: 'meta/llama',
            provider: 'together',
            reason: 'unpriced',
            detail: 'no mix price under the profile',
          },
          {
            offering_id: 'off4',
            model: 'mistral/large',
            provider: 'mistral',
            reason: 'below_min_quality',
            detail: 'quality 800 is below the minimum 1200',
          },
        ],
      }),
    );
    const root = fixture.nativeElement as HTMLElement;
    const headers = Array.from(root.querySelectorAll('.exclusion-group h4')).map((header) =>
      header.textContent?.replace(/\s+/g, ' ').trim(),
    );

    // First-seen order, one group per reason, with its count.
    expect(headers).toEqual(['Below minimum quality 2', 'Unpriced 1']);
    expect(root.textContent).toContain('quality 900 is below the minimum 1200');
    expect(root.textContent).toContain('quality 800 is below the minimum 1200');
    expect(root.textContent).toContain('no mix price under the profile');
  });

  it('shows the current sticky pin and its age', async () => {
    const fixture = await create(
      preview({
        sticky: {
          enabled: true,
          key_source: 'header',
          session_key: 's-1',
          pinned: 'off1',
          age_s: 125,
        },
      }),
    );
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('s-1');
    expect(text).toContain('off1');
    expect(text).toContain('2m ago');
    expect(text).toContain('key source: header');
  });

  it('explains an off switch and an empty pool', async () => {
    const fixture = await create(preview({ candidates: [], excluded: [] }));
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('No candidate passed the filters and constraints');
    expect(text).toContain('Nothing was excluded');
    expect(text).toContain('Stickiness is off');
  });
});
