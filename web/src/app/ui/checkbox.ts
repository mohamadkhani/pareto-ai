import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  effect,
  input,
  model,
  viewChild,
} from '@angular/core';

/**
 * Native checkbox with a token-styled box. The visible `label` is also the
 * accessible name; `ariaLabel` supplies one when no label is rendered (a table
 * selection column). `indeterminate` renders the mixed state a "select all"
 * control shows while only some rows are checked.
 */
@Component({
  selector: 'ui-checkbox',
  templateUrl: './checkbox.html',
  styleUrl: './checkbox.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class.is-disabled]': 'disabled()' },
})
export class UiCheckbox {
  readonly checked = model(false);
  readonly disabled = input(false);
  readonly label = input<string>('');
  /** Accessible name when the checkbox renders without a visible label. */
  readonly ariaLabel = input<string>('');
  /** True while only part of a group is selected; the box shows a dash. */
  readonly indeterminate = input(false);

  private readonly boxRef = viewChild<ElementRef<HTMLInputElement>>('box');

  constructor() {
    // `indeterminate` is a DOM property, not an attribute, so it is set here
    // rather than bound. The native state clears on a click; a header click
    // always ends with a definite state, so the effect re-runs and re-syncs it.
    effect(() => {
      const el = this.boxRef()?.nativeElement;
      if (el === undefined) {
        return;
      }
      el.indeterminate = this.indeterminate();
    });
  }

  protected onChange(event: Event): void {
    if (event.target instanceof HTMLInputElement) {
      this.checked.set(event.target.checked);
    }
  }
}
