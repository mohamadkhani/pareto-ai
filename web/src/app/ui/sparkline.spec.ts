import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { UiSparkline } from './sparkline';

describe('UiSparkline', () => {
  async function create(values: number[]): Promise<ComponentFixture<UiSparkline>> {
    TestBed.configureTestingModule({ imports: [UiSparkline] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(UiSparkline);
    fixture.componentRef.setInput('label', 'Spend over the last 7 days');
    fixture.componentRef.setInput('values', values);
    fixture.detectChanges();
    return fixture;
  }

  function linePoints(root: HTMLElement): string {
    return root.querySelector('polyline')?.getAttribute('points') ?? '';
  }

  it('names the chart for assistive technology', async () => {
    const fixture = await create([1, 2, 3]);
    const svg = (fixture.nativeElement as HTMLElement).querySelector('svg');

    expect(svg?.getAttribute('role')).toBe('img');
    expect(svg?.getAttribute('aria-label')).toBe('Spend over the last 7 days');
  });

  it('draws one point per value from left to right', async () => {
    const fixture = await create([0, 1, 2]);
    const points = linePoints(fixture.nativeElement as HTMLElement).split(' ');

    expect(points).toHaveLength(3);
    expect(points[0]?.startsWith('0.00,')).toBe(true);
    expect(points[2]?.startsWith('100.00,')).toBe(true);
  });

  it('draws a flat series through the middle instead of at the top', async () => {
    const fixture = await create([5, 5, 5]);
    const y = linePoints(fixture.nativeElement as HTMLElement)
      .split(' ')
      .map((point) => point.split(',')[1]);

    expect(new Set(y)).toEqual(new Set(['16.00']));
  });

  it('renders an empty series without a line', async () => {
    const fixture = await create([]);
    expect(linePoints(fixture.nativeElement as HTMLElement)).toBe('');
  });
});
