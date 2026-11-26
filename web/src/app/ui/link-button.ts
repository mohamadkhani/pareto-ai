import { ChangeDetectionStrategy, Component, output } from '@angular/core';

/**
 * A text action that looks like a link but behaves like a button: no box and no
 * minimum height, and it inherits the surrounding font, so it sits inline with
 * plain text at exactly the same size (a table cell). The projected text is the
 * accessible name.
 */
@Component({
  selector: 'ui-link-button',
  templateUrl: './link-button.html',
  styleUrl: './link-button.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiLinkButton {
  readonly clicked = output<MouseEvent>();
}
