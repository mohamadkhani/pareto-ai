import { type ComponentFixture, TestBed } from '@angular/core/testing';
import type { CandidateScore, DecisionCandidate, DecisionRecord } from './routing.types';
import { DecisionPanel } from './decision-panel';

function score(overrides: Partial<CandidateScore> = {}): CandidateScore {
  return {
    price: 0.8,
    speed: 0.6,
    quality: 0.9,
    reliability: 0.7,
    penalties: 0.05,
    total: 0.75,
    imputed: [],
    shrunk: [],
    ...overrides,
  };
}

function candidate(overrides: Partial<DecisionCandidate> = {}): DecisionCandidate {
  return {
    id: 'off1',
    provider: 'openrouter',
    model: 'openai/gpt-5',
    upstream_model_id: 'openai/gpt-5',
    price: null,
    credential_id: 'c1',
    outcome: 'chosen',
    error_class: null,
    status: 200,
    score: score(),
    mix_price: null,
    price_basis: 'unpriced',
    explanation: 'chosen: served the request',
    ...overrides,
  };
}

function record(overrides: Partial<DecisionRecord> = {}): DecisionRecord {
  return {
    candidates: [candidate()],
    chosen_offering_id: 'off1',
    reason: 'off1 served the request',
    routing: {
      strategy: 'weighted',
      combo: 'shared-fast',
      weights: { price: 0.25, speed: 0.3, quality: 0.3, reliability: 0.15 },
      preset: 'balanced',
      mix_profile: null,
      rr_cursor: null,
    },
    sticky: {
      enabled: false,
      key_source: null,
      pinned: null,
      age_s: null,
      replaced_why: null,
    },
    ...overrides,
  };
}

async function create(decision: DecisionRecord): Promise<ComponentFixture<DecisionPanel>> {
  TestBed.configureTestingModule({ imports: [DecisionPanel] });
  await TestBed.compileComponents();
  const fixture = TestBed.createComponent(DecisionPanel);
  fixture.componentRef.setInput('decision', decision);
  fixture.detectChanges();
  return fixture;
}

describe('DecisionPanel', () => {
  it('highlights the chosen candidate and renders its score breakdown', async () => {
    const fixture = await create(
      record({
        candidates: [
          candidate({ id: 'off1', outcome: 'chosen' }),
          candidate({
            id: 'off2',
            outcome: 'not_tried',
            explanation: 'not reached: off1 served before this candidate',
          }),
        ],
      }),
    );
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('tr.chosen')).not.toBeNull();
    expect(root.querySelector('tr.chosen')?.textContent ?? '').toContain('off1');
    expect(root.textContent).toContain('off1 served the request');
    // The score components render at three decimals.
    expect(root.querySelector('tr.chosen')?.textContent ?? '').toContain('0.800');
    expect(root.querySelector('tr.chosen')?.textContent ?? '').toContain('0.750');
    // The routing block names the strategy and the combo.
    expect(root.textContent).toContain('weighted');
    expect(root.textContent).toContain('combo:shared-fast');
    expect(root.textContent).toContain('balanced');
  });

  it('marks imputed and shrunk components with tooltip badges', async () => {
    const fixture = await create(
      record({
        candidates: [
          candidate({
            score: score({ imputed: ['speed'], shrunk: ['reliability'] }),
          }),
        ],
      }),
    );
    const root = fixture.nativeElement as HTMLElement;
    const row = root.querySelector('tbody tr');
    const markers = Array.from((row ?? root).querySelectorAll('sup.marker'));

    expect(markers).toHaveLength(2);
    expect(markers[0].textContent?.trim()).toBe('imp');
    expect(markers[0].getAttribute('title') ?? '').toContain('imputed');
    expect(markers[1].textContent?.trim()).toBe('shr');
    expect(markers[1].getAttribute('title') ?? '').toContain('shrunk');
  });

  it('renders a negative total as-is (the total is not clamped)', async () => {
    const fixture = await create(
      record({ candidates: [candidate({ score: score({ total: -0.125 }) })] }),
    );
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('-0.125');
  });

  it('shows unscored candidates without a breakdown', async () => {
    const fixture = await create(
      record({
        candidates: [candidate({ id: 'off2', outcome: 'skipped_breaker_open', score: null })],
        chosen_offering_id: null,
      }),
    );
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('not scored');
    expect(root.textContent).toContain('skipped');
    expect(root.querySelectorAll('sup.marker')).toHaveLength(2); // legend only
  });

  it('renders the mix profile and per-candidate mix price with its basis', async () => {
    const fixture = await create(
      record({
        candidates: [
          candidate({
            mix_price: {
              per_m: 0.578,
              profile_id: 'mix-code-agent',
              profile_name: 'code-agent',
            },
            price_basis: 'mix',
            explanation: 'price from mix $0.578/M at code-agent (8.1/0.4/91.5)',
          }),
          candidate({
            id: 'off2',
            outcome: 'not_tried',
            mix_price: null,
            price_basis: 'component_sum',
            explanation: 'price from component sum $6.000/M (input + output; no mix price)',
          }),
        ],
        routing: {
          strategy: 'weighted',
          combo: 'shared-fast',
          weights: { price: 0.25, speed: 0.3, quality: 0.3, reliability: 0.15 },
          preset: 'balanced',
          mix_profile: { profile_id: 'mix-code-agent', profile_name: 'code-agent' },
          rr_cursor: null,
        },
      }),
    );
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('Mix profile');
    expect(text).toContain('code-agent');
    expect(text).toContain('mix-code-agent');
    expect(text).toContain('$0.5780');
    expect(text).toContain('component sum');
    // Unpriced/fallback candidates never render as $0.00.
    expect(text).not.toContain('$0.0000');
  });

  it('renders the sticky block with its pin, source, age, and replacement reason', async () => {
    const fixture = await create(
      record({
        candidates: [
          candidate({ id: 'off1', provider: 'openrouter', model: 'openai/gpt-5' }),
          candidate({ id: 'off2', outcome: 'not_tried' }),
        ],
        sticky: {
          enabled: true,
          key_source: 'header',
          pinned: 'off1',
          age_s: 42,
          replaced_why: 'expired',
        },
      }),
    );
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('Stickiness');
    expect(text).toContain('enabled');
    expect(text).toContain('key from header');
    expect(text).toContain('Pinned');
    expect(text).toContain('openrouter · openai/gpt-5');
    expect(text).toContain('42s');
    expect(text).toContain('replaced: expired');
  });

  it('treats a fresh pin as zero seconds', async () => {
    const fixture = await create(
      record({
        sticky: {
          enabled: true,
          key_source: 'auto',
          pinned: 'off1',
          age_s: 0,
          replaced_why: null,
        },
      }),
    );
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('fresh pin (picked by this request)');
    expect(text).toContain('key from auto');
  });

  it('hides the sticky block for records written before stickiness existed', async () => {
    const fixture = await create(record());
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('Stickiness');
  });
});
