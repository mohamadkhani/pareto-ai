import { Component, signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { UiField } from './field';
import { UiInput } from './input';
import { UiSelect } from './select';
import { UiTextarea } from './textarea';

@Component({
  imports: [UiField, UiInput, UiSelect, UiTextarea],
  template: `
    <ui-field label="Model name" hint="Canonical catalog name." [error]="error()">
      <ui-input [(value)]="name" />
    </ui-field>
    <ui-field label="Notes">
      <ui-textarea [(value)]="notes" />
    </ui-field>
    <ui-field label="Provider">
      <ui-select [(value)]="provider">
        <option value="openrouter">openrouter</option>
      </ui-select>
    </ui-field>
    <ui-field label="Custom id">
      <ui-input [id]="'custom-id'" [(value)]="custom" />
    </ui-field>
    <ui-field label="Raw control">
      <input id="raw-id" [value]="raw()" (input)="raw.set($any($event.target).value)" />
    </ui-field>
    <ui-field label="Default id">
      <ui-input [(value)]="defaulted" />
    </ui-field>
  `,
})
class FieldHost {
  readonly name = signal('openai/gpt-5');
  readonly notes = signal('');
  readonly provider = signal('openrouter');
  readonly custom = signal('custom');
  readonly raw = signal('raw');
  readonly defaulted = signal('');
  readonly error = signal('');
}

describe('Field and controls', () => {
  async function create(): Promise<ComponentFixture<FieldHost>> {
    TestBed.configureTestingModule({
      imports: [FieldHost, UiField, UiInput, UiSelect, UiTextarea],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(FieldHost);
    // The field adopts a projected control's id after content is checked, so
    // the label binding settles on the next pass.
    fixture.detectChanges();
    fixture.detectChanges();
    return fixture;
  }

  it('associates the label and hint with the projected input', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const label = root.querySelector('label');
    const input = root.querySelector('input');

    expect(label?.getAttribute('for')).toBeTruthy();
    expect(label?.getAttribute('for')).toBe(input?.id);
    const hint = root.querySelector('.hint');
    expect(hint?.textContent).toContain('Canonical catalog name.');
    expect(input?.getAttribute('aria-describedby')).toBe(hint?.id);
    expect(input?.getAttribute('aria-invalid')).toBeNull();
  });

  it('shows an error, marks the control invalid, and describes it by the error', async () => {
    const fixture = await create();
    fixture.componentInstance.error.set('A model name is required.');
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    const input = root.querySelector('input');

    const error = root.querySelector('.error');
    expect(error?.textContent).toContain('A model name is required.');
    expect(error?.getAttribute('role')).toBe('alert');
    expect(input?.getAttribute('aria-invalid')).toBe('true');
    expect(input?.getAttribute('aria-describedby')).toBe(error?.id);
  });

  it('writes typed values back through the model', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const input = root.querySelector('input');
    if (!(input instanceof HTMLInputElement)) {
      throw new Error('no input');
    }
    input.value = 'openai/gpt-5-mini';
    input.dispatchEvent(new Event('input'));

    expect(fixture.componentInstance.name()).toBe('openai/gpt-5-mini');
  });

  it('wires the textarea and select to their fields', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const textarea = root.querySelector('textarea');
    const select = root.querySelector('select');

    expect(textarea?.id).toBeTruthy();
    expect(textarea?.getAttribute('aria-describedby')).toBeNull();
    expect(select?.getAttribute('aria-invalid')).toBeNull();
    expect(root.querySelectorAll('label[for]')).toHaveLength(6);
  });

  it('labels a control that sets its own id exactly once', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const custom = root.querySelector<HTMLInputElement>('#custom-id');

    expect(custom).not.toBeNull();
    expect(custom?.labels).toHaveLength(1);
    expect(custom?.labels?.[0]?.textContent).toContain('Custom id');
  });

  it('labels a raw control with an explicit id', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const raw = root.querySelector<HTMLInputElement>('#raw-id');

    expect(raw?.labels).toHaveLength(1);
    expect(raw?.labels?.[0]?.textContent).toContain('Raw control');
  });

  it('labels a control that relies on the field-generated id', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const fields = root.querySelectorAll('ui-field');
    const generated = fields[5]?.querySelector<HTMLInputElement>('input');

    expect(generated?.id).toBeTruthy();
    expect(generated?.id.startsWith('ui-field-')).toBe(true);
    expect(generated?.labels).toHaveLength(1);
  });
});
