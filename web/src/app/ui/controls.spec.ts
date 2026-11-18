import { Component, signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FormControl, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { UiField } from './field';
import { UiInput } from './input';
import { UiSelect } from './select';

@Component({
  imports: [ReactiveFormsModule, UiField, UiInput, UiSelect],
  template: `
    <ui-field label="Amount" [controlId]="'amount'">
      <ui-input
        [id]="'amount'"
        type="number"
        min="0"
        max="10"
        step="0.5"
        inputmode="decimal"
        [formControl]="form.controls.amount"
      />
    </ui-field>

    <ui-field label="Starts on" [controlId]="'starts'">
      <ui-input [id]="'starts'" type="datetime-local" [formControl]="form.controls.starts" />
    </ui-field>

    <ui-field label="Provider" [controlId]="'provider'">
      <ui-select [id]="'provider'" [formControl]="form.controls.provider">
        <option value="openrouter">openrouter</option>
        <option value="azure">azure</option>
      </ui-select>
    </ui-field>

    <ui-field label="Locked" [controlId]="'locked'">
      <ui-input [id]="'locked'" [formControl]="form.controls.locked" />
    </ui-field>

    <ui-input [type]="'text'" [(value)]="twoWay" ariaLabel="Two-way" />
  `,
})
class ControlsHost {
  readonly form = new FormGroup({
    amount: new FormControl<number | null>(null, { validators: [Validators.min(0)] }),
    starts: new FormControl('', { nonNullable: true }),
    provider: new FormControl('azure', { nonNullable: true }),
    locked: new FormControl({ value: 'read only', disabled: true }, { nonNullable: true }),
  });
  readonly twoWay = signal('draft');
}

describe('Input and select primitives', () => {
  async function create(): Promise<ComponentFixture<ControlsHost>> {
    TestBed.configureTestingModule({ imports: [ControlsHost] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(ControlsHost);
    fixture.detectChanges();
    fixture.detectChanges();
    return fixture;
  }

  function native<T extends HTMLElement>(root: HTMLElement, selector: string): T {
    const element = root.querySelector<T>(selector);
    if (element === null) {
      throw new Error(`Expected ${selector} in the fixture`);
    }
    return element;
  }

  it('passes type, step, min, max, and inputmode to the native input', async () => {
    const fixture = await create();
    const input = native<HTMLInputElement>(fixture.nativeElement, '#amount');

    expect(input.type).toBe('number');
    expect(input.getAttribute('step')).toBe('0.5');
    expect(input.getAttribute('min')).toBe('0');
    expect(input.getAttribute('max')).toBe('10');
    expect(input.getAttribute('inputmode')).toBe('decimal');
  });

  it('gives a number form control a number, and null when cleared', async () => {
    const fixture = await create();
    const input = native<HTMLInputElement>(fixture.nativeElement, '#amount');
    const control = fixture.componentInstance.form.controls.amount;

    input.value = '2.5';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    expect(control.value).toBe(2.5);
    expect(control.valid).toBe(true);

    input.value = '';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    expect(control.value).toBeNull();
  });

  it('writes control values into the native elements', async () => {
    const fixture = await create();
    fixture.componentInstance.form.controls.starts.setValue('2026-11-18T09:30');
    fixture.componentInstance.form.controls.provider.setValue('openrouter');
    fixture.detectChanges();

    const root = fixture.nativeElement as HTMLElement;
    expect(native<HTMLInputElement>(root, '#starts').value).toBe('2026-11-18T09:30');
    expect(native<HTMLSelectElement>(root, '#provider').value).toBe('openrouter');
  });

  it('reports select changes to the form and keeps the two-way value', async () => {
    const fixture = await create();
    const select = native<HTMLSelectElement>(fixture.nativeElement, '#provider');

    select.value = 'openrouter';
    select.dispatchEvent(new Event('change'));
    fixture.detectChanges();

    expect(fixture.componentInstance.form.controls.provider.value).toBe('openrouter');
    expect(select.classList.contains('select')).toBe(true);
  });

  it('disables the native control when the form disables it', async () => {
    const fixture = await create();
    const input = native<HTMLInputElement>(fixture.nativeElement, '#locked');

    expect(fixture.componentInstance.form.controls.locked.disabled).toBe(true);
    expect(input.disabled).toBe(true);
  });

  it('keeps the two-way value in sync without a form', async () => {
    const fixture = await create();
    const input = native<HTMLInputElement>(
      fixture.nativeElement as HTMLElement,
      'input[aria-label="Two-way"]',
    );

    input.value = 'typed';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();

    expect(fixture.componentInstance.twoWay()).toBe('typed');
  });
});
