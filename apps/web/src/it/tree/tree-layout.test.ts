import { describe, expect, test } from 'bun:test';
import { chainRows, treeRead } from '../../../tests/fixtures/station-tree';
import { parseTree, type StationTree } from './station-tree';
import { CARD, layoutTree, treeEdges, type Box, type TreeLayout } from './tree-layout';

// THE TREE, LAID OUT — the pure function from a tree read to where each
// card stands, where its ring is, and which station edges are drawn
// between rings (backlog 94a9dad4, T7 of design 4b27a65a). The record
// routes and the live motion that rode them retired with the transit
// map (R3, backlog 23a4b4d7). Every rule a reader could check by eye is
// pinned here, so the page owns only strokes.

const lines = () => 3;
const lay = (t: StationTree, depth = 9, width = 1200): TreeLayout => layoutTree(t, { width, depth, lines });
const tree = () => parseTree(treeRead());
const box = (l: TreeLayout, name: string): Box => {
  const b = l.boxes.find((x) => x.name === name);
  if (b === undefined) throw new Error(`no box for ${name}`);
  return b;
};
const inside = (a: Box, b: Box) => a.x >= b.x && a.y >= b.y && a.x + a.w <= b.x + b.w && a.y + a.h <= b.y + b.h;
const overlap = (a: Box, b: Box) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;

function nestsAndNeverOverlaps(l: TreeLayout): void {
  for (const b of l.boxes) {
    if (b.parent !== null) expect(inside(b, box(l, b.parent))).toBe(true);
    for (const s of l.boxes.filter((x) => x.parent === b.parent && x.name !== b.name)) expect(overlap(b, s)).toBe(false);
  }
}

describe('the layout', () => {
  test('one card per station, each inside its parent, siblings apart', () => {
    const l = lay(tree());
    expect(l.boxes).toHaveLength(16);
    expect(box(l, 'company')).toMatchObject({ x: 0, y: 0, parent: null, level: 0 });
    expect(box(l, 'gates').parent).toBe('engineering');
    nestsAndNeverOverlaps(l);
    expect(l.width).toBe(box(l, 'company').w);
    expect(l.height).toBe(box(l, 'company').h);
  });

  test('a card is tall enough for its lines, and wide enough for a leaf', () => {
    const l = layoutTree(tree(), { width: 1200, depth: 9, lines: (name) => (name === 'gates' ? 7 : 1) });
    expect(box(l, 'gates').h).toBe(CARD.head + 7 * CARD.line + CARD.pad);
    expect(box(l, 'probe-shed').h).toBe(CARD.head + CARD.line + CARD.pad);
    expect(l.boxes.every((b) => b.w >= CARD.leaf)).toBe(true);
  });

  test('the ring is where the card is named: on its header, inside the card', () => {
    const l = lay(tree());
    const g = box(l, 'gates');
    expect(l.ring('gates')).toEqual({ x: g.x + CARD.ring, y: g.y + CARD.head / 2 });
    expect(l.ring('not-a-station')).toBeNull();
  });

  test('past the depth asked, a station collapses into its nearest drawn ancestor', () => {
    const l = lay(tree(), 1);
    expect(l.boxes.map((b) => b.name).sort()).toEqual(['company', 'engineering', 'executive', 'operations']);
    expect(box(l, 'engineering').kidsDrawn).toBe(false);
    expect(l.drawn('gates')).toBe('engineering');
    expect(l.drawn('forge-ci')).toBe('engineering');
    expect(l.ring('gates')).toEqual(l.ring('engineering'));
  });

  test('a subtree read lays out from its own top', () => {
    const l = lay(parseTree(treeRead('engineering')));
    expect(box(l, 'engineering')).toMatchObject({ x: 0, y: 0, parent: null, level: 0 });
    expect(l.boxes).toHaveLength(6);
    expect(l.drawn('deploy')).toBeNull();
  });

  test('a deep tree lays out every level, nested, wider than the view rather than refused', () => {
    const t = parseTree(treeRead('s0', chainRows(40)));
    const l = lay(t, 99, 600);
    expect(l.boxes).toHaveLength(40);
    nestsAndNeverOverlaps(l);
    expect(box(l, 's39').w).toBeGreaterThanOrEqual(CARD.leaf);
  });

  test('a child the read names but does not carry is a gap, and a cycle cannot loop the layout', () => {
    const raw = treeRead();
    const data = raw.data.map((n) =>
      n.station.name === 'forge-ci' ? { ...n, children: ['company', 'ghost'] } : n,
    );
    const l = lay(parseTree({ ...raw, data }));
    expect(l.missing).toEqual(['ghost']);
    expect(l.boxes.filter((b) => b.name === 'company')).toHaveLength(1);
  });
});

describe('the station edges between rings', () => {
  test('each station row edge is drawn ring to ring, once', () => {
    const t = tree();
    const l = lay(t);
    const r = treeEdges(t, l);
    expect(r.map((x) => x.key).sort()).toEqual(
      ['design-review→design-decided', 'gates→loading-dock', 'loading-dock→probe-shed', 'repair→loading-dock'].sort(),
    );
    const g = r.find((x) => x.key === 'gates→loading-dock');
    const a = l.ring('gates');
    const b = l.ring('loading-dock');
    expect(g?.d.startsWith(`M${a?.x} ${a?.y}`)).toBe(true);
    expect(g?.d.endsWith(`${b?.x}`) || g?.d.endsWith(`${b?.y}`)).toBe(true);
  });

  test('an edge inside one collapsed card is not a line', () => {
    const t = tree();
    const keys = treeEdges(t, lay(t, 1)).map((x) => x.key);
    expect(keys.some((k) => k.startsWith('engineering→engineering'))).toBe(false);
    expect(keys.some((k) => k.startsWith('gates'))).toBe(false);
  });
});
