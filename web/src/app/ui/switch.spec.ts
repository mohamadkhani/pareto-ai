import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { UiSwitch } from './switch';

describe('UiSwitch', () => {
  async function create(): Promise<ComponentFixture<UiSwitch>> {
    TestBed.configureTestingModule({ imports: [UiSwitch] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(UiSwitch);
    fixture.componentRef.setInput('label', 'Offering enabled');
    fixture.detectChanges();
    return fixture;
  }

  it('exposes role switch, aria-checked, and the visible label', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const button = root.querySelector('button');

    expect(button?.getAttribute('role')).toBe('switch');
    expect(button?.getAttribute('aria-checked')).toBe('false');
    expect(button?.getAttribute('aria-label')).toBe('Offering enabled');
    expect(root.textContent).toContain('Offering enabled');
  });

  it('toggles through the two-way checked model', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const button = root.querySelector('button');
    button?.click();
    fixture.detectChanges();

    expect(fixture.componentInstance.checked()).toBe(true);
    expect(button?.getAttribute('aria-checked')).toBe('true');
  });

  it('ignores clicks while disabled', async () => {
    const fixture = await create();
    fixture.componentRef.setInput('disabled', true);
    fixture.detectChanges();
    const button = (fixture.nativeElement as HTMLElement).querySelector('button');
    button?.click();

    expect(fixture.componentInstance.checked()).toBe(false);
    expect(button?.disabled).toBe(true);
  });
});
