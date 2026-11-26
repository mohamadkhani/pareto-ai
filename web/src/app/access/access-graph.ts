import { ChangeDetectionStrategy, Component, computed, input, output } from '@angular/core';

/** A node (a user on the left, a model on the right) in the access graph. */
export interface AccessGraphNode {
  id: string;
  label: string;
  /** A second line under the label (a role, a verdict reason). */
  sublabel?: string;
}

/** A connection from a left node to a right node, coloured by its verdict. */
export interface AccessGraphLink {
  id: string;
  /** Left node id (a user). */
  source: string;
  /** Right node id (a model). */
  target: string;
  verdict: 'allow' | 'deny';
  /** Why this verdict holds; shown on hover and in the detail panel. */
  reason: string;
}

/** One laid-out node with its vertical offset in pixels. */
interface PositionedNode<T> {
  node: T;
  top: number;
}

/** One laid-out link with its SVG path and CSS tone class. */
interface PositionedLink {
  link: AccessGraphLink;
  d: string;
  tone: string;
}

const ROW_HEIGHT = 46;
const PAD_Y = 12;
const LEFT_WIDTH = 200;
const RIGHT_WIDTH = 224;
const GAP = 72;
const NODE_GAP = 8;

/**
 * Two-column node-link diagram of access: users (or one user) on the left,
 * models on the right, and a curved SVG connector per pair coloured green for
 * `allow` and red for `deny`. The connector carries the deciding rule as a
 * native tooltip and on click; the layout is computed in pixels so the SVG and
 * the HTML labels share one coordinate system.
 */
@Component({
  selector: 'app-access-graph',
  templateUrl: './access-graph.html',
  styleUrl: './access-graph.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class AccessGraph {
  readonly leftNodes = input<readonly AccessGraphNode[]>([]);
  readonly rightNodes = input<readonly AccessGraphNode[]>([]);
  readonly links = input<readonly AccessGraphLink[]>([]);
  /** The currently highlighted link id (two-way bindable). */
  readonly selectedId = input<string | null>(null);

  readonly linkSelect = output<AccessGraphLink>();

  protected readonly rowHeight = ROW_HEIGHT;
  protected readonly nodeHeight = ROW_HEIGHT - NODE_GAP;
  protected readonly leftWidth = LEFT_WIDTH;
  protected readonly rightWidth = RIGHT_WIDTH;
  protected readonly gap = GAP;

  private readonly leftIndex = computed(() => {
    const map = new Map<string, number>();
    this.leftNodes().forEach((node, index) => map.set(node.id, index));
    return map;
  });
  private readonly rightIndex = computed(() => {
    const map = new Map<string, number>();
    this.rightNodes().forEach((node, index) => map.set(node.id, index));
    return map;
  });

  protected readonly width = LEFT_WIDTH + GAP + RIGHT_WIDTH;

  protected readonly height = computed(() => {
    const rows = Math.max(this.leftNodes().length, this.rightNodes().length, 1);
    return PAD_Y * 2 + rows * ROW_HEIGHT;
  });

  /** A single left node is centred vertically; otherwise rows top-align. */
  private topFor(index: number, count: number): number {
    if (count === 1) {
      return this.height() / 2 - this.nodeHeight / 2;
    }
    return PAD_Y + index * ROW_HEIGHT;
  }

  protected readonly left = computed<PositionedNode<AccessGraphNode>[]>(() => {
    const nodes = this.leftNodes();
    return nodes.map((node, index) => ({ node, top: this.topFor(index, nodes.length) }));
  });

  protected readonly right = computed<PositionedNode<AccessGraphNode>[]>(() => {
    const nodes = this.rightNodes();
    return nodes.map((node, index) => ({ node, top: this.topFor(index, nodes.length) }));
  });

  protected readonly positionedLinks = computed<PositionedLink[]>(() => {
    const left = this.leftIndex();
    const right = this.rightIndex();
    const leftCount = this.leftNodes().length;
    const rightCount = this.rightNodes().length;
    const sx = LEFT_WIDTH;
    const rx = LEFT_WIDTH + GAP;
    const bend = GAP * 0.5;

    return this.links().flatMap((link) => {
      const sourceIndex = left.get(link.source);
      const targetIndex = right.get(link.target);
      if (sourceIndex === undefined || targetIndex === undefined) {
        return [];
      }
      const sy = this.topFor(sourceIndex, leftCount) + this.nodeHeight / 2;
      const ty = this.topFor(targetIndex, rightCount) + this.nodeHeight / 2;
      const d = `M ${sx} ${sy} C ${sx + bend} ${sy}, ${rx - bend} ${ty}, ${rx} ${ty}`;
      return [{ link, d, tone: link.verdict === 'allow' ? 'is-allow' : 'is-deny' }];
    });
  });

  protected select(link: AccessGraphLink): void {
    this.linkSelect.emit(link);
  }

  protected isSelected(link: AccessGraphLink): boolean {
    return this.selectedId() === link.id;
  }
}
