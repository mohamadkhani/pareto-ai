import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { DecisionPanel } from './decision-panel';
import { parseDecision } from './routing.types';

/**
 * Rendering against **real** `decision_json` records: these four strings are
 * verbatim `request_logs.decision_json` values produced by the M5 gateway
 * (commit 03c0c3d, 2026-11-16) through `/v1/chat/completions` against a stub
 * upstream and read back via `GET /api/logs`. They pin the wire shape the
 * panel must keep rendering: weighted scores with imputed/shrunk markers and
 * unclamped (negative) totals, an ordered failover with a failed attempt, and
 * a round-robin rotation carrying its cursor.
 */

/** Weighted combo over tiny weights: penalties take both totals below zero. */
const WEIGHTED_NEGATIVE = `{"candidates":[{"credential_id":"76f26519-1689-4c01-8e63-e85cd9fef62f","error_class":null,"explanation":"scored #1 of 2: best weighted score -0.0979 (price 0.50, speed 0.50, quality 0.50, reliability 0.64, penalties 0.10, member weight 1); imputed at 0.5 (no data): price, quality; shrunk toward 0.5 (fewer than 10 samples): speed, reliability; quality from model_default","id":"687fdef0-f04c-4188-a3bf-902cb536a313","model":"gpt-4o","outcome":"chosen","price":null,"provider":"pb","score":{"imputed":["price","quality"],"penalties":0.1,"price":0.5,"quality":0.5,"reliability":0.6428571428571428,"shrunk":["speed","reliability"],"speed":0.5,"total":-0.09785714285714286},"status":null,"upstream_model_id":"up-b"},{"credential_id":"4d348010-b9e7-47b4-b675-be097061e4bb","error_class":null,"explanation":"scored #2 of 2: weighted score -0.1980 below the leader -0.0979 (price 0.50, speed 0.50, quality 0.50, reliability 0.45, penalties 0.20, member weight 1); imputed at 0.5 (no data): price, speed, quality; shrunk toward 0.5 (fewer than 10 samples): reliability; quality from model_default","id":"81fd6b4b-dfa6-42b0-8350-f266bcb2db76","model":"gpt-4o","outcome":"not_tried","price":null,"provider":"pf","score":{"imputed":["price","speed","quality"],"penalties":0.2,"price":0.5,"quality":0.5,"reliability":0.45454545454545453,"shrunk":["reliability"],"speed":0.5,"total":-0.19804545454545455},"status":null,"upstream_model_id":"up-f"}],"chosen_offering_id":"687fdef0-f04c-4188-a3bf-902cb536a313","reason":"687fdef0-f04c-4188-a3bf-902cb536a313 chosen (best weighted score -0.0979 of 2 scored candidates)","routing":{"combo":"w-tiny","preset":null,"rr_cursor":null,"strategy":"weighted","weights":{"price":0.001,"quality":0.001,"reliability":0.001,"speed":0.001}}}`;

/** Weighted combo with the `balanced` preset and thin-window telemetry. */
const WEIGHTED_BALANCED = `{"candidates":[{"credential_id":"9c0f6862-ce10-492d-b4c5-dfcc29f22ef0","error_class":null,"explanation":"scored #1 of 2: best weighted score 0.4500 (price 0.50, speed 0.50, quality 0.50, reliability 0.50, penalties 0.05, member weight 1); imputed at 0.5 (no data): price, quality; shrunk toward 0.5 (fewer than 10 samples): speed, reliability; quality from model_default","id":"50f77e39-f97d-4db5-83f8-16e6593e4849","model":"gpt-4o","outcome":"chosen","price":null,"provider":"pa","score":{"imputed":["price","quality"],"penalties":0.05000000000000002,"price":0.5,"quality":0.5,"reliability":0.5,"shrunk":["speed","reliability"],"speed":0.5,"total":0.44999999999999996},"status":null,"upstream_model_id":"up-a"},{"credential_id":"76f26519-1689-4c01-8e63-e85cd9fef62f","error_class":null,"explanation":"scored #2 of 2: weighted score 0.4000 below the leader 0.4500 (price 0.50, speed 0.50, quality 0.50, reliability 0.50, penalties 0.10, member weight 1); imputed at 0.5 (no data): price, quality; shrunk toward 0.5 (fewer than 10 samples): speed, reliability; quality from model_default","id":"687fdef0-f04c-4188-a3bf-902cb536a313","model":"gpt-4o","outcome":"not_tried","price":null,"provider":"pb","score":{"imputed":["price","quality"],"penalties":0.1,"price":0.5,"quality":0.5,"reliability":0.5,"shrunk":["speed","reliability"],"speed":0.5,"total":0.4},"status":null,"upstream_model_id":"up-b"}],"chosen_offering_id":"50f77e39-f97d-4db5-83f8-16e6593e4849","reason":"50f77e39-f97d-4db5-83f8-16e6593e4849 chosen (best weighted score 0.4500 of 2 scored candidates)","routing":{"combo":"w-bal","preset":"balanced","rr_cursor":null,"strategy":"weighted","weights":{"price":0.25,"quality":0.3,"reliability":0.15,"speed":0.3}}}`;

/** Ordered combo: the first member failed and failover served the second. */
const ORDERED_FAILOVER = `{"candidates":[{"credential_id":"4d348010-b9e7-47b4-b675-be097061e4bb","error_class":"upstream_error","explanation":"tried and failed with upstream_error (HTTP 500)","id":"81fd6b4b-dfa6-42b0-8350-f266bcb2db76","model":"gpt-4o","outcome":"failed","price":null,"provider":"pf","score":null,"status":500,"upstream_model_id":"up-f"},{"credential_id":"76f26519-1689-4c01-8e63-e85cd9fef62f","error_class":null,"explanation":"chosen: served the request","id":"687fdef0-f04c-4188-a3bf-902cb536a313","model":"gpt-4o","outcome":"chosen","price":null,"provider":"pb","score":null,"status":null,"upstream_model_id":"up-b"}],"chosen_offering_id":"687fdef0-f04c-4188-a3bf-902cb536a313","reason":"81fd6b4b-dfa6-42b0-8350-f266bcb2db76 failed with upstream_error; 687fdef0-f04c-4188-a3bf-902cb536a313 chosen (first eligible candidate in fixed order)","routing":{"combo":"ord-fail","preset":null,"rr_cursor":null,"strategy":"ordered","weights":null}}`;

/** Round-robin combo: the second request consumed rotation cursor 2. */
const ROUND_ROBIN = `{"candidates":[{"credential_id":"76f26519-1689-4c01-8e63-e85cd9fef62f","error_class":null,"explanation":"chosen: served the request","id":"687fdef0-f04c-4188-a3bf-902cb536a313","model":"gpt-4o","outcome":"chosen","price":null,"provider":"pb","score":null,"status":null,"upstream_model_id":"up-b"},{"credential_id":"9c0f6862-ce10-492d-b4c5-dfcc29f22ef0","error_class":null,"explanation":"not reached: 687fdef0-f04c-4188-a3bf-902cb536a313 served before this candidate","id":"50f77e39-f97d-4db5-83f8-16e6593e4849","model":"gpt-4o","outcome":"not_tried","price":null,"provider":"pa","score":null,"status":null,"upstream_model_id":"up-a"}],"chosen_offering_id":"687fdef0-f04c-4188-a3bf-902cb536a313","reason":"687fdef0-f04c-4188-a3bf-902cb536a313 chosen (next in the round-robin rotation)","routing":{"combo":"rr","preset":null,"rr_cursor":2,"strategy":"round_robin","weights":null}}`;

/** A plain model decided at the env-overridden `chat` mix profile (D34). */
const MIX_FIXED_ORDER = `{"candidates":[{"credential_id":"71ec5162-33ff-4df0-953c-e60a5fa6f623","error_class":null,"explanation":"chosen: served the request","id":"d0a6959f-106f-4673-8a02-7f2579e6f103","mix_price":{"per_m":0.1668,"profile_id":"mix-chat","profile_name":"chat","shares":{"cache_read":0.0,"cache_write":0.0,"input":0.74,"output":0.26,"reasoning":0.0}},"model":"mock/cheap","outcome":"chosen","price":{"cache_read_per_m":0.03,"cache_write_per_m":null,"currency":"USD","input_per_m":0.12,"output_per_m":0.3,"reasoning_per_m":null,"request_fee":null},"price_basis":"mix","provider":"mock","score":null,"status":null,"upstream_model_id":"cheap"}],"chosen_offering_id":"d0a6959f-106f-4673-8a02-7f2579e6f103","reason":"d0a6959f-106f-4673-8a02-7f2579e6f103 chosen (first eligible candidate in fixed order); mix $0.167/M at chat (74.0/26.0/0.0)","routing":{"combo":null,"mix_profile":{"profile_id":"mix-chat","profile_name":"chat"},"preset":null,"rr_cursor":null,"strategy":"fixed_order","weights":null},"sticky":{"age_s":null,"enabled":false,"key_source":null,"pinned":null,"replaced_why":null}}`;

/** A rule combo (`pool = all`): one scored candidate plus constraint exclusions. */
const RULE_COMBO = `{"candidates":[{"credential_id":"71ec5162-33ff-4df0-953c-e60a5fa6f623","error_class":null,"explanation":"scored #1 of 1: best weighted score 0.3679 (price 0.50, speed 0.50, quality 0.50, reliability 0.50, penalties 0.13, member weight 1); shrunk toward 0.5 (fewer than 10 samples): speed, reliability; quality from override; price from mix $0.167/M at chat (74.0/26.0/0.0)","id":"d0a6959f-106f-4673-8a02-7f2579e6f103","mix_price":{"per_m":0.1668,"profile_id":"mix-chat","profile_name":"chat","shares":{"cache_read":0.0,"cache_write":0.0,"input":0.74,"output":0.26,"reasoning":0.0}},"model":"mock/cheap","outcome":"chosen","price":{"cache_read_per_m":0.03,"cache_write_per_m":null,"currency":"USD","input_per_m":0.12,"output_per_m":0.3,"reasoning_per_m":null,"request_fee":null},"price_basis":"mix","provider":"mock","score":{"imputed":[],"penalties":0.13214285714285717,"price":0.5,"quality":0.5,"reliability":0.5,"shrunk":["speed","reliability"],"speed":0.5,"total":0.3678571428571428},"status":null,"upstream_model_id":"cheap"},{"credential_id":"71ec5162-33ff-4df0-953c-e60a5fa6f623","error_class":null,"explanation":"mix price $1.5200/M is above the maximum $0.3000/M","id":"4f116ade-6837-414d-ae7f-b104ac922420","mix_price":{"per_m":1.52,"profile_id":"mix-chat","profile_name":"chat","shares":{"cache_read":0.0,"cache_write":0.0,"input":0.74,"output":0.26,"reasoning":0.0}},"model":"mock/fast","outcome":"above_max_price","price":{"cache_read_per_m":0.3,"cache_write_per_m":null,"currency":"USD","input_per_m":1.0,"output_per_m":3.0,"reasoning_per_m":null,"request_fee":null},"price_basis":"mix","provider":"mock","score":null,"status":null,"upstream_model_id":"fast"},{"credential_id":"71ec5162-33ff-4df0-953c-e60a5fa6f623","error_class":null,"explanation":"mix price $6.0800/M is above the maximum $0.3000/M","id":"c2740e4c-cd72-49ec-a458-87a8de414ed2","mix_price":{"per_m":6.08,"profile_id":"mix-chat","profile_name":"chat","shares":{"cache_read":0.0,"cache_write":0.0,"input":0.74,"output":0.26,"reasoning":0.0}},"model":"mock/good","outcome":"above_max_price","price":{"cache_read_per_m":1.2,"cache_write_per_m":null,"currency":"USD","input_per_m":4.0,"output_per_m":12.0,"reasoning_per_m":null,"request_fee":null},"price_basis":"mix","provider":"mock","score":null,"status":null,"upstream_model_id":"good"},{"credential_id":"71ec5162-33ff-4df0-953c-e60a5fa6f623","error_class":null,"explanation":"no mix price is computable and the combo caps it at $0.3000/M","id":"04d7169d-234a-49e8-bc9b-9c4fdf616e29","mix_price":null,"model":"mock/unpriced","outcome":"unpriced","price":null,"price_basis":"unpriced","provider":"mock","score":null,"status":null,"upstream_model_id":"unpriced"}],"chosen_offering_id":"d0a6959f-106f-4673-8a02-7f2579e6f103","reason":"d0a6959f-106f-4673-8a02-7f2579e6f103 chosen (best weighted score 0.3679 of 1 scored candidates); mix $0.167/M at chat (74.0/26.0/0.0)","routing":{"combo":"cheap-only","mix_profile":{"profile_id":"mix-chat","profile_name":"chat"},"preset":"balanced","rr_cursor":null,"strategy":"weighted","weights":{"price":0.25,"quality":0.3,"reliability":0.15,"speed":0.3}},"sticky":{"age_s":0,"enabled":true,"key_source":"header","pinned":"d0a6959f-106f-4673-8a02-7f2579e6f103","replaced_why":null}}`;

async function render(raw: string): Promise<ComponentFixture<DecisionPanel>> {
  const decision = parseDecision(raw);
  expect(decision).not.toBeNull();
  TestBed.configureTestingModule({ imports: [DecisionPanel] });
  await TestBed.compileComponents();
  const fixture = TestBed.createComponent(DecisionPanel);
  fixture.componentRef.setInput('decision', decision);
  fixture.detectChanges();
  return fixture;
}

describe('DecisionPanel on real gateway records', () => {
  it('renders a weighted decision with unclamped negative totals', async () => {
    const fixture = await render(WEIGHTED_NEGATIVE);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('best weighted score -0.0979 of 2 scored candidates');
    // The score rows show the real totals at three decimals, negative as-is.
    expect(root.textContent).toContain('-0.098');
    expect(root.textContent).toContain('-0.198');
    expect(root.querySelectorAll('td.total.negative')).toHaveLength(2);
    // The chosen row is the one with the higher (less negative) total.
    expect(root.querySelector('tr.chosen')?.textContent ?? '').toContain('-0.098');
    // Real marker placement: imputed price/quality, shrunk speed/reliability.
    const row = root.querySelector('tr.chosen');
    expect((row ?? root).querySelectorAll('sup.marker')).toHaveLength(4);
    expect(root.textContent).toContain('combo:w-tiny');
  });

  it('renders the balanced preset, its weights, and shrunk markers', async () => {
    const fixture = await render(WEIGHTED_BALANCED);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('combo:w-bal');
    expect(root.textContent).toContain('balanced');
    for (const part of ['price 0.250', 'speed 0.300', 'quality 0.300', 'reliability 0.150']) {
      expect(root.textContent).toContain(part);
    }
    expect(root.textContent).toContain('0.450');
    const row = root.querySelector('tr.chosen');
    const markers = Array.from((row ?? root).querySelectorAll('sup.marker'));
    // Column order is price/speed/quality/reliability.
    expect(markers.map((m) => m.textContent?.trim())).toEqual(['imp', 'shr', 'imp', 'shr']);
  });

  it('renders an ordered failover with the failed attempt and no breakdown', async () => {
    const fixture = await render(ORDERED_FAILOVER);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('failed with upstream_error');
    expect(root.textContent).toContain('combo:ord-fail');
    expect(root.textContent).toContain('ordered');
    // Unscored strategies carry no score breakdown.
    expect(root.textContent).toContain('not scored');
    expect(root.querySelector('tr.chosen')?.textContent ?? '').toContain('chosen');
  });

  it('renders the round-robin rotation cursor', async () => {
    const fixture = await render(ROUND_ROBIN);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('round-robin cursor 2');
    expect(root.textContent).toContain('next in the round-robin rotation');
  });

  it('renders a mix-driven decision with its basis and mix price', async () => {
    const fixture = await render(MIX_FIXED_ORDER);
    const root = fixture.nativeElement as HTMLElement;

    // The routing block names the effective profile (D34).
    expect(root.textContent).toContain('Mix profile');
    expect(root.textContent).toContain('chat');
    expect(root.textContent).toContain('mix-chat');
    // The candidate's basis badge and mix price come from the decision itself.
    expect(root.textContent).toContain('mix');
    expect(root.textContent).toContain('$0.1668');
    expect(root.textContent).toContain('mix $0.167/M at chat (74.0/26.0/0.0)');
    expect(root.querySelector('tr.chosen')?.textContent ?? '').toContain('chosen');
    // Sticky was off for this plain-model decision (the real record says so).
    expect(root.textContent).not.toContain('Stickiness');
  });

  it('renders a rule-combo decision, its weights, and constraint exclusions', async () => {
    const fixture = await render(RULE_COMBO);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('combo:cheap-only');
    expect(root.textContent).toContain('balanced');
    expect(root.textContent).toContain('mix-chat');
    // Excluded catalog candidates carry their reason as the outcome.
    expect(root.textContent).toContain('above_max_price');
    expect(root.textContent).toContain('unpriced');
    expect(root.textContent).toContain('not scored');
    // The scored winner shows its breakdown.
    const chosen = root.querySelector('tr.chosen');
    expect(chosen?.textContent ?? '').toContain('0.368');
    // D35: the real record pinned d0a6… via the header, freshly on this request.
    expect(root.textContent).toContain('Stickiness');
    expect(root.textContent).toContain('key from header');
    expect(root.textContent).toContain('fresh pin (picked by this request)');
    expect(root.textContent).toContain('mock · mock/cheap');
  });
});
