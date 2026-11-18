import { ChangeDetectionStrategy, Component, input } from '@angular/core';

export type CardPadding = 'none' | 'sm' | 'md';

/**
 * Surface with header/body/footer slots. Mark projected elements with
 * `ui-card-header` / `ui-card-footer`; anything else lands in the body.
 * `interactive` adds the shared hover/focus border emphasis for cards whose
 * header navigates somewhere.
 */
@Component({
  selector: 'ui-card',
  templateUrl: './card.html',
  styleUrl: './card.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class]': 'hostClass()' },
})
export class UiCard {
  readonly padding = input<CardPadding>('md');
  readonly raised = input(false);
  readonly interactive = input(false);

  protected hostClass(): string {
    return [
      'ui-card',
      `pad-${this.padding()}`,
      this.raised() ? 'is-raised' : '',
      this.interactive() ? 'is-interactive' : '',
    ].join(' ');
  }
}
