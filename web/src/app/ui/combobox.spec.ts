import { Component, signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FormControl, FormGroup, ReactiveFormsModule } from '@angular/forms';
import { type ComboboxOption, UiCombobox } from './combobox';
import { UiField } from './field';

const OPTIONS: readonly ComboboxOption[] = [
  { value: 'm1', label: 'openai/gpt-5', searchText: 'openrouter' },
  { value: 'm2', label: 'anthropic/claude', searchText: 'openrouter' },
  { value: 'm3', label: 'qwen/qwen3', searchText: 'sailresearch' },
];

@Component({
  imports: [ReactiveFormsModule, UiField, UiCombobox],
  template: `
    <ui-field label="Model" [controlId]="'model'">
      <ui-combobox
        [id]="'model'"
        [options]="options"
        placeholder="Search models…"
        [formControl]="control"
      />
    </ui-field>
    <ui-combobox [options]="options" ariaLabel="Two-way" [(value)]="twoWay" />
  `,
})
class ComboboxHost {
  readonly options = OPTIONS;
  readonly control = new FormControl('m2', { nonNullable: true });
  readonly twoWay = signal('m1');
}

describe('UiCombobox', () => {
  async function create(): Promise<ComponentFixture<ComboboxHost>> {
    TestBed.configureTestingModule({ imports: [ComboboxHost] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(ComboboxHost);
    fixture.detectChanges();
    return fixture;
  }

  function comboboxes(root: HTMLElement): HTMLElement[] {
    return Array.from(root.querySelectorAll<HTMLElement>('ui-combobox'));
  }

  function trigger(root: HTMLElement): HTMLInputElement {
    const input = comboboxes(root)[0]?.querySelector<HTMLInputElement>('input');
    if (!input) {
      throw new Error('no combobox input');
    }
    return input;
  }

  function list(root: HTMLElement): HTMLUListElement {
    const element = comboboxes(root)[0]?.querySelector<HTMLUListElement>('ul');
    if (!element) {
      throw new Error('no combobox list');
    }
    return element;
  }

  it('shows the selected label and opens a filtered listbox on focus', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const input = trigger(root);

    expect(input.value).toBe('anthropic/claude');
    expect(list(root).hidden).toBe(true);

    input.dispatchEvent(new FocusEvent('focus'));
    fixture.detectChanges();
    expect(input.getAttribute('aria-expanded')).toBe('true');
    expect(list(root).hidden).toBe(false);
    expect(list(root).querySelectorAll('[role="option"]')).toHaveLength(3);
  });

  it('filters options by label and by searchText', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const input = trigger(root);
    input.dispatchEvent(new FocusEvent('focus'));
    fixture.detectChanges();

    input.value = 'openrouter';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    let options = list(root).querySelectorAll('[role="option"]');
    expect(options).toHaveLength(2);

    input.value = 'qwen';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    options = list(root).querySelectorAll('[role="option"]');
    expect(options).toHaveLength(1);
    expect(options[0]?.textContent).toContain('qwen/qwen3');
  });

  it('writes the picked value through the form control', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const input = trigger(root);
    input.dispatchEvent(new FocusEvent('focus'));
    fixture.detectChanges();

    input.value = 'qwen';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    const option = list(root).querySelector<HTMLElement>('[role="option"]');
    option?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    fixture.detectChanges();

    expect(fixture.componentInstance.control.value).toBe('m3');
    expect(input.value).toBe('qwen/qwen3');
    expect(list(root).hidden).toBe(true);
  });

  it('navigates with the keyboard and commits with Enter', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const input = trigger(root);
    input.dispatchEvent(new FocusEvent('focus'));
    fixture.detectChanges();

    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown' }));
    fixture.detectChanges();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown' }));
    fixture.detectChanges();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    fixture.detectChanges();

    // First ArrowDown highlights index 0 (m1), second index 1 (m2); Enter picks m2.
    expect(fixture.componentInstance.control.value).toBe('m2');
  });

  it('closes on Escape without changing the value', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const input = trigger(root);
    input.dispatchEvent(new FocusEvent('focus'));
    fixture.detectChanges();

    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    fixture.detectChanges();

    expect(input.getAttribute('aria-expanded')).toBe('false');
    expect(fixture.componentInstance.control.value).toBe('m2');
    expect(input.value).toBe('anthropic/claude');
  });

  it('keeps the two-way value in sync without a form', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const second = comboboxes(root)[1];
    const input = second?.querySelector<HTMLInputElement>('input');
    if (!input) {
      throw new Error('no second combobox');
    }

    input.dispatchEvent(new FocusEvent('focus'));
    fixture.detectChanges();
    input.value = 'qwen';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    second
      ?.querySelector<HTMLElement>('[role="option"]')
      ?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    fixture.detectChanges();

    expect(fixture.componentInstance.twoWay()).toBe('m3');
  });
});
