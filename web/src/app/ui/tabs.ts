import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  input,
  model,
  viewChildren,
} from '@angular/core';

export interface TabItem {
  readonly id: string;
  readonly label: string;
  readonly disabled?: boolean;
}

let nextTabsId = 0;

/**
 * Data-driven tab list. Arrow keys/Home/End move selection (skipping disabled
 * tabs) and keep one tab in the tab order (roving tabindex). Project the panel
 * content for the active tab; the component gives it the tabpanel role.
 */
@Component({
  selector: 'ui-tabs',
  templateUrl: './tabs.html',
  styleUrl: './tabs.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiTabs {
  readonly tabs = input<readonly TabItem[]>([]);
  readonly selected = model<string>('');
  readonly label = input<string>('');

  private readonly tabButtons = viewChildren<ElementRef<HTMLButtonElement>>('tabButton');
  private readonly instanceId = ++nextTabsId;

  protected readonly activeId = computed<string | null>(() => {
    const tabs = this.tabs();
    const selected = this.selected();
    if (tabs.some((tab) => tab.id === selected && !tab.disabled)) {
      return selected;
    }
    return tabs.find((tab) => !tab.disabled)?.id ?? null;
  });

  protected tabId(id: string): string {
    return `ui-tab-${this.instanceId}-${id}`;
  }

  protected panelLabel(): string | null {
    const id = this.activeId();
    return id === null ? null : this.tabId(id);
  }

  protected select(tab: TabItem): void {
    if (!tab.disabled) {
      this.selected.set(tab.id);
    }
  }

  protected onKeydown(event: KeyboardEvent, tab: TabItem): void {
    const tabs = this.tabs();
    const enabled = tabs.filter((candidate) => !candidate.disabled);
    if (enabled.length === 0) {
      return;
    }
    const current = enabled.indexOf(tab);
    let next: TabItem | undefined;
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        next = enabled[(current + 1) % enabled.length];
        break;
      case 'ArrowLeft':
      case 'ArrowUp':
        next = enabled[(current - 1 + enabled.length) % enabled.length];
        break;
      case 'Home':
        next = enabled[0];
        break;
      case 'End':
        next = enabled[enabled.length - 1];
        break;
      default:
        return;
    }
    if (next === undefined) {
      return;
    }
    event.preventDefault();
    this.selected.set(next.id);
    const index = tabs.indexOf(next);
    queueMicrotask(() => this.tabButtons()[index]?.nativeElement.focus());
  }
}
