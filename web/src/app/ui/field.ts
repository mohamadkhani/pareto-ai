import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  InjectionToken,
  Signal,
  afterEveryRender,
  computed,
  forwardRef,
  inject,
  input,
  signal,
} from '@angular/core';

/**
 * The label/hint/error wiring a projected control needs. Controls inject this
 * optionally so a standalone control still works outside a field.
 */
export interface FieldContext {
  readonly id: Signal<string>;
  readonly describedBy: Signal<string | null>;
  readonly invalid: Signal<boolean>;
}

export const UI_FIELD = new InjectionToken<FieldContext>('UI_FIELD');

let nextFieldId = 0;

/**
 * Label + hint + error wrapper for one control. The projected control picks up
 * the generated id (unless it sets its own) and the `aria-describedby` /
 * `aria-invalid` attributes.
 *
 * The label's `for` must name the control's real id. When `controlId` is set,
 * that id wins (it must match the control's id). Otherwise the field adopts
 * the id of the first projected control that carries one, and only falls back
 * to its generated id when the control has none. Adopting the control's id
 * keeps one source of truth: the control decides its id, the label follows it.
 */
@Component({
  selector: 'ui-field',
  templateUrl: './field.html',
  styleUrl: './field.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
  providers: [{ provide: UI_FIELD, useExisting: forwardRef(() => UiField) }],
  host: { class: 'ui-field-host' },
})
export class UiField implements FieldContext {
  readonly label = input<string>('');
  readonly hint = input<string>('');
  readonly error = input<string>('');
  readonly optional = input(false);
  /** Overrides the projected control's id; must match it. */
  readonly controlId = input<string>('');

  private readonly host = inject<ElementRef<HTMLElement>>(ElementRef);
  private readonly projectedId = signal<string | null>(null);
  private readonly generatedId = `ui-field-${++nextFieldId}`;
  readonly id = computed(() => this.controlId() || this.projectedId() || this.generatedId);
  readonly hintId = computed(() => `${this.id()}-hint`);
  readonly errorId = computed(() => `${this.id()}-error`);

  readonly invalid = computed(() => this.error().trim().length > 0);
  readonly describedBy = computed(() => {
    if (this.invalid()) {
      return this.errorId();
    }
    return this.hint().trim().length > 0 ? this.hintId() : null;
  });

  constructor() {
    // Projected content lands in the DOM after this component's own view is
    // checked, so an id lookup has to wait for the render to finish. The
    // label is also written directly, so it is correct without waiting for a
    // second change-detection pass; the signal keeps controls in sync when a
    // projected id changes later.
    afterEveryRender(() => {
      if (this.controlId() !== '') {
        return;
      }
      const control = this.host.nativeElement.querySelector<HTMLElement>(
        'input[id], select[id], textarea[id]',
      );
      const id = control?.id ?? null;
      if (id === null) {
        return;
      }
      if (id !== this.projectedId()) {
        this.projectedId.set(id);
      }
      const label =
        this.host.nativeElement.querySelector<HTMLLabelElement>(':scope > .field > label');
      if (label !== null && label.getAttribute('for') !== id) {
        label.setAttribute('for', id);
      }
    });
  }
}
