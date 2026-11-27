import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';

/**
 * A text action that looks like a link but behaves like a button: no box and no
 * minimum height, and it inherits the surrounding font, so it sits inline with
 * plain text at exactly the same size (a table cell). The projected text is the
 * accessible name; set `label` when the visible text alone would not tell a
 * screen-reader user what the action does.
 */
@Component({
  selector: 'ui-link-button',
  templateUrl: './link-button.html',
  styleUrl: './link-button.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiLinkButton {
  /**
   * An explicit accessible name for the inner `<button>`. Empty (the default)
   * leaves the projected text as the accessible name.
   */
  readonly label = input<string>('');
  readonly clicked = output<MouseEvent>();
}
