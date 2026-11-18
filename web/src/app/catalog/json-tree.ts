import { ChangeDetectionStrategy, Component, computed, input, output, signal } from '@angular/core';
import { pointerFromPath } from './connector-form';

/** One visible row of the flattened JSON tree. */
export interface JsonRow {
  /** Path segments from the document root. */
  path: (string | number)[];
  pointer: string;
  key: string;
  value: unknown;
  depth: number;
  container: 'object' | 'array' | null;
  childCount: number;
}

export interface JsonRowOptions {
  /** Explicit expand/collapse choices; absent uses `defaultDepth`. */
  overrides: ReadonlyMap<string, boolean>;
  /** Depth whose children are expanded unless an override says otherwise. */
  defaultDepth?: number;
}

/** Flattens a parsed document into ordered rows for a collapsible tree. */
export function flattenJson(value: unknown, options: JsonRowOptions): JsonRow[] {
  const rows: JsonRow[] = [];
  const defaultDepth = options.defaultDepth ?? 6;
  const isExpanded = (pointer: string, depth: number): boolean =>
    options.overrides.get(pointer) ?? depth < defaultDepth;
  const visit = (current: unknown, path: (string | number)[], key: string, depth: number): void => {
    const pointer = pointerFromPath(path);
    const container = Array.isArray(current) ? 'array' : isRecord(current) ? 'object' : null;
    const childCount =
      container === 'array'
        ? (current as unknown[]).length
        : container === 'object'
          ? Object.keys(current as Record<string, unknown>).length
          : 0;
    if (path.length > 0) {
      rows.push({ path, pointer, key, value: current, depth, container, childCount });
    }
    if (container === null || childCount === 0 || !isExpanded(pointer, depth)) {
      return;
    }
    if (container === 'array') {
      (current as unknown[]).forEach((child, index) => {
        visit(child, [...path, index], `[${index}]`, depth + 1);
      });
    } else {
      for (const [childKey, child] of Object.entries(current as Record<string, unknown>)) {
        visit(child, [...path, childKey], childKey, depth + 1);
      }
    }
  };
  if (value !== undefined) {
    visit(value, [], '', 0);
  }
  return rows;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Collapsible JSON tree. Container rows expand and collapse; number and string
 * leaves are buttons that emit their RFC 6901 pointer when `pickable`.
 */
@Component({
  selector: 'app-json-tree',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (rows().length === 0) {
      <p class="empty">No JSON to show.</p>
    } @else {
      <ul class="tree" role="tree" aria-label="JSON sample">
        @for (row of rows(); track row.pointer) {
          <li
            [class]="'row depth-' + depthClass(row.depth)"
            role="treeitem"
            [attr.aria-level]="row.depth + 1"
            [attr.aria-expanded]="row.container !== null ? isExpanded(row) : null"
          >
            @if (row.container !== null) {
              <button
                type="button"
                class="toggle"
                [attr.aria-label]="(isExpanded(row) ? 'Collapse ' : 'Expand ') + row.pointer"
                (click)="toggle(row)"
              >
                {{ isExpanded(row) ? '▾' : '▸' }}
              </button>
              <code class="key">{{ row.key }}</code>
              <span class="meta"
                >{{ row.container }} · {{ row.childCount }}
                {{ row.container === 'array' ? 'items' : 'keys' }}</span
              >
            } @else if (pickable() && isLeafValue(row.value)) {
              <button
                type="button"
                class="leaf"
                [class.is-picked]="pickedPointer() === row.pointer"
                [attr.aria-label]="'Pick ' + row.pointer"
                (click)="pick(row)"
              >
                <code class="key">{{ row.key }}</code>
                <code class="value">{{ displayValue(row.value) }}</code>
                @if (pickedPointer() === row.pointer) {
                  <span class="picked">picked</span>
                }
              </button>
            } @else {
              <span class="leaf-static">
                <code class="key">{{ row.key }}</code>
                <code class="value">{{ displayValue(row.value) }}</code>
              </span>
            }
          </li>
        }
      </ul>
    }
  `,
  styleUrl: './json-tree.sass',
})
export class JsonTree {
  /** Parsed JSON document (not a string). */
  readonly value = input<unknown>(null);
  /** Allows clicking number/string leaves. */
  readonly pickable = input(false);
  /** The pointer currently selected in the form, for the highlight. */
  readonly pickedPointer = input<string | null>(null);

  readonly picked = output<string>();

  private readonly overrides = signal<ReadonlyMap<string, boolean>>(new Map());

  protected readonly rows = computed(() =>
    flattenJson(this.value(), { overrides: this.overrides() }),
  );

  protected isExpanded(row: JsonRow): boolean {
    return row.container !== null && (this.overrides().get(row.pointer) ?? row.depth < 6);
  }

  protected depthClass(depth: number): number {
    return Math.min(6, depth);
  }

  protected isLeafValue(value: unknown): boolean {
    return typeof value === 'number' || typeof value === 'string';
  }

  protected displayValue(value: unknown): string {
    if (typeof value === 'string') {
      return JSON.stringify(value.length > 60 ? `${value.slice(0, 60)}…` : value);
    }
    if (value === null) {
      return 'null';
    }
    if (typeof value === 'object') {
      return Array.isArray(value) ? '[…]' : '{…}';
    }
    return String(value);
  }

  protected toggle(row: JsonRow): void {
    this.overrides.update((current) => {
      const next = new Map(current);
      next.set(row.pointer, !this.isExpanded(row));
      return next;
    });
  }

  protected pick(row: JsonRow): void {
    if (this.pickable() && this.isLeafValue(row.value)) {
      this.picked.emit(row.pointer);
    }
  }
}
