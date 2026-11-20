import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';

/**
 * Small rounded pill for a compact secondary list, such as the providers of a
 * model. It is a button, because every chip does something (filter, expand);
 * for non-interactive status text use `<ui-badge>` instead.
 */
@Component({
  selector: 'ui-chip',
  template: `<button
    type="button"
    class="ui-chip"
    [class.is-mono]="mono()"
    [attr.title]="label() || null"
    [attr.aria-label]="label() || null"
    (click)="clicked.emit($event)"
  >
    <ng-content />
  </button>`,
  styleUrl: './chip.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiChip {
  /** Tooltip and accessible name; the projected text is usually shorter. */
  readonly label = input<string>('');

  /** Monospace numerals, for a count such as "+2". */
  readonly mono = input(false);

  readonly clicked = output<MouseEvent>();
}
