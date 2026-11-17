import { type ComponentFixture, TestBed } from '@angular/core/testing';
import type { TabItem } from './tabs';
import { UiTabs } from './tabs';

const tabs: readonly TabItem[] = [
  { id: 'one', label: 'One' },
  { id: 'two', label: 'Two' },
  { id: 'three', label: 'Three', disabled: true },
  { id: 'four', label: 'Four' },
];

describe('UiTabs', () => {
  async function create(selected = 'one'): Promise<ComponentFixture<UiTabs>> {
    TestBed.configureTestingModule({ imports: [UiTabs] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(UiTabs);
    fixture.componentRef.setInput('tabs', tabs);
    fixture.componentRef.setInput('selected', selected);
    fixture.componentRef.setInput('label', 'Sections');
    fixture.detectChanges();
    return fixture;
  }

  function tabButtons(root: HTMLElement): HTMLButtonElement[] {
    return Array.from(root.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
  }

  it('marks the selected tab and keeps a roving tabindex', async () => {
    const fixture = await create('two');
    const root = fixture.nativeElement as HTMLElement;
    const buttons = tabButtons(root);

    expect(buttons.map((button) => button.getAttribute('aria-selected'))).toEqual([
      'false',
      'true',
      'false',
      'false',
    ]);
    expect(buttons.map((button) => button.getAttribute('tabindex'))).toEqual([
      '-1',
      '0',
      '-1',
      '-1',
    ]);
    expect(root.querySelector('[role="tablist"]')?.getAttribute('aria-label')).toBe('Sections');
    expect(root.querySelector('[role="tabpanel"]')).not.toBeNull();
  });

  it('selects a tab on click', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    tabButtons(root)[1]?.click();
    fixture.detectChanges();

    expect(fixture.componentInstance.selected()).toBe('two');
  });

  it('moves with arrow keys, skipping disabled tabs and wrapping', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const buttons = tabButtons(root);

    buttons[0]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    await Promise.resolve();
    fixture.detectChanges();
    expect(fixture.componentInstance.selected()).toBe('two');

    buttons[1]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    await Promise.resolve();
    fixture.detectChanges();
    // `three` is disabled, so selection lands on `four`.
    expect(fixture.componentInstance.selected()).toBe('four');
    expect(document.activeElement).toBe(buttons[3]);

    buttons[3]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    await Promise.resolve();
    fixture.detectChanges();
    expect(fixture.componentInstance.selected()).toBe('one');
  });

  it('moves to the first and last enabled tab with Home and End', async () => {
    const fixture = await create('two');
    const root = fixture.nativeElement as HTMLElement;
    const buttons = tabButtons(root);

    buttons[1]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'End', bubbles: true }));
    await Promise.resolve();
    expect(fixture.componentInstance.selected()).toBe('four');

    buttons[3]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true }));
    await Promise.resolve();
    expect(fixture.componentInstance.selected()).toBe('one');
  });
});
