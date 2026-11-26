import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { UiCheckbox } from './checkbox';

describe('UiCheckbox', () => {
  async function create(): Promise<ComponentFixture<UiCheckbox>> {
    TestBed.configureTestingModule({ imports: [UiCheckbox] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(UiCheckbox);
    fixture.detectChanges();
    return fixture;
  }

  function box(fixture: ComponentFixture<UiCheckbox>): HTMLInputElement {
    const input = (fixture.nativeElement as HTMLElement).querySelector('input');
    if (!(input instanceof HTMLInputElement)) {
      throw new Error('no checkbox input');
    }
    return input;
  }

  it('reflects the checked model and emits on change', async () => {
    const fixture = await create();
    const input = box(fixture);

    expect(input.checked).toBe(false);

    input.checked = true;
    input.dispatchEvent(new Event('change'));
    fixture.detectChanges();

    expect(fixture.componentInstance.checked()).toBe(true);
  });

  it('uses ariaLabel when no visible label is rendered', async () => {
    const fixture = await create();
    fixture.componentRef.setInput('ariaLabel', 'Select all models');
    fixture.detectChanges();

    expect(box(fixture).getAttribute('aria-label')).toBe('Select all models');
  });

  it('sets the native indeterminate property from the input', async () => {
    const fixture = await create();

    expect(box(fixture).indeterminate).toBe(false);

    fixture.componentRef.setInput('indeterminate', true);
    fixture.detectChanges();

    expect(box(fixture).indeterminate).toBe(true);
  });
});
