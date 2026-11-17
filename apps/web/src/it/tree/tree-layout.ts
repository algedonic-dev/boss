// WHERE EACH CARD STANDS, AND THE LINES BETWEEN THEM (T7 of design
// 4b27a65a, backlog 94a9dad4).
//
// THE LAYOUT IS A FUNCTION OF THE TREE READ, NOT A LIST. A card stands
// where the tree puts it — inside its parent's card, beside its siblings
// in the server's order, shelf by shelf — so re-parenting a station is a
// row edit and the floor follows it with no web change (handoff §10).
// Past the depth the viewer asks for, a station collapses into its
// nearest drawn ancestor, which says how many stand inside it.
//
// EVERY CARD HAS A RING, on its header beside its name; a station row's
// declared edge joins two rings. No pair is written here.
//
// Pure functions, unit-pinned in tree-layout.test.ts.

import { type StationTree } from './station-tree';

/** A point on the floor, in CSS pixels. */
export type Point = Readonly<{ x: number; y: number }>;

/** A card's measures, in the floor's own units (CSS pixels). */
export const CARD = {
  /** The header band: the ring, the name, the plate. */
  head: 28,
  /** One line of the card's body (station-tree.ts `stationCard` lines). */
  line: 16,
  /** Inside a card, around its body and its children. */
  pad: 8,
  /** Between sibling cards. */
  gap: 10,
  /** The narrowest a card is drawn: design c4854fd4 E1's card, wide
   *  enough for a 32-cell pool, one cell per unit. */
  leaf: 296,
  /** The ring's distance in from the card's left edge. */
  ring: 12,
} as const;

/** One drawn card: where it stands on the floor. */
export type Box = Readonly<{
  name: string;
  x: number;
  y: number;
  w: number;
  h: number;
  /** Steps below the subtree's top: the top is 0. */
  level: number;
  /** The drawn card this one stands in; null for the top. */
  parent: string | null;
  /** Whether its children are drawn inside it at this depth. */
  kidsDrawn: boolean;
}>;

export type TreeLayout = Readonly<{
  /** Top first, then depth-first — parents before their children. */
  boxes: ReadonlyArray<Box>;
  width: number;
  height: number;
  /** The drawn card a station stands in: itself when drawn, else its
   *  nearest drawn ancestor; null for a name this read does not hold. */
  drawn: (name: string) => string | null;
  /** Where a station's ring is — its drawn card's. */
  ring: (name: string) => Point | null;
  /** Children the read names but does not carry. */
  missing: ReadonlyArray<string>;
  /** Rows the read carries that no station of it contains. */
  unreached: ReadonlyArray<string>;
}>;

type Measured = Readonly<{
  name: string;
  w: number;
  h: number;
  level: number;
  kidsDrawn: boolean;
  kids: ReadonlyArray<Readonly<{ m: Measured; dx: number; dy: number }>>;
}>;

type LayoutOptions = Readonly<{
  /** The floor's width; the top card wraps its children to it. */
  width: number;
  /** How many levels below the top draw their children. */
  depth: number;
  /** How many body lines a card draws (station-tree.ts `stationCard`). */
  lines: (name: string, kidsDrawn: boolean) => number;
}>;

function measure(tree: StationTree, name: string, level: number, maxW: number, o: LayoutOptions, seen: Set<string>): Measured {
  seen.add(name);
  const kidNames = (tree.byName.get(name)?.children ?? []).filter((c) => tree.byName.has(c) && !seen.has(c));
  // A read that names a child twice, or a cycle, is drawn once: the
  // first visit takes it.
  for (const c of kidNames) seen.add(c);
  const kidsDrawn = level < o.depth && kidNames.length > 0;
  const bodyH = CARD.head + o.lines(name, kidsDrawn) * CARD.line + CARD.pad;
  if (!kidsDrawn) return { name, w: CARD.leaf, h: bodyH, level, kidsDrawn: false, kids: [] };
  const inner = Math.max(CARD.leaf, maxW - 2 * CARD.pad);
  const measured = kidNames.map((c) => {
    seen.delete(c);
    return measure(tree, c, level + 1, inner, o, seen);
  });
  // Shelves: left to right, a new shelf when the next card would cross
  // the inner edge (a card wider than the shelf takes one of its own).
  type Shelf = Readonly<{ x: number; y: number; rowH: number; widest: number; kids: Measured['kids'] }>;
  const packed = measured.reduce<Shelf>((s, m) => {
    const wrap = s.x > 0 && s.x + m.w > inner;
    const x = wrap ? 0 : s.x;
    const y = wrap ? s.y + s.rowH + CARD.gap : s.y;
    const rowH = wrap ? m.h : Math.max(s.rowH, m.h);
    return {
      x: x + m.w + CARD.gap,
      y,
      rowH,
      widest: Math.max(s.widest, x + m.w),
      kids: [...s.kids, { m, dx: CARD.pad + x, dy: bodyH + y }],
    };
  }, { x: 0, y: 0, rowH: 0, widest: 0, kids: [] });
  return {
    name,
    w: Math.max(CARD.leaf, packed.widest + 2 * CARD.pad),
    h: bodyH + packed.y + packed.rowH + CARD.pad,
    level,
    kidsDrawn: true,
    kids: packed.kids,
  };
}

function flatten(m: Measured, x: number, y: number, parent: string | null): ReadonlyArray<Box> {
  const box: Box = { name: m.name, x, y, w: m.w, h: m.h, level: m.level, parent, kidsDrawn: m.kidsDrawn };
  return [box, ...m.kids.flatMap((k) => flatten(k.m, x + k.dx, y + k.dy, m.name))];
}

/** The tree read, laid out: one box per drawn station. */
export function layoutTree(tree: StationTree, o: LayoutOptions): TreeLayout {
  const top = measure(tree, tree.top, 0, Math.max(CARD.leaf, o.width), o, new Set());
  const boxes = flatten(top, 0, 0, null);
  const byName = new Map(boxes.map((b) => [b.name, b] as const));
  // Each station's parent by the read's own children lists — the same
  // edges the boxes were nested by, so collapse follows the drawing.
  const parentOf = new Map<string, string>();
  for (const n of tree.nodes) for (const c of n.children) if (!parentOf.has(c) && c !== n.name) parentOf.set(c, n.name);
  const drawn = (name: string): string | null => {
    // Bounded by the read's size, so a malformed cycle cannot hang it.
    let at: string | undefined = tree.byName.has(name) ? name : undefined;
    for (let i = 0; at !== undefined && i <= tree.nodes.length; i += 1) {
      if (byName.has(at)) return at;
      at = parentOf.get(at);
    }
    return null;
  };
  const ring = (name: string): Point | null => {
    const b = byName.get(drawn(name) ?? '');
    return b === undefined ? null : { x: b.x + CARD.ring, y: b.y + CARD.head / 2 };
  };
  const missing = [...new Set(tree.nodes.flatMap((n) => n.children).filter((c) => !tree.byName.has(c)))];
  const unreached = tree.nodes.map((n) => n.name).filter((n) => drawn(n) === null);
  return { boxes, width: top.w, height: top.h, drawn, ring, missing, unreached };
}

// ---------------------------------------------------------------------
// The lines between rings.
// ---------------------------------------------------------------------

/** One station-edge line: a station row's `edges` (design ccfe87ff pass
 *  4), between the two drawn cards it joins. */
export type TreeEdge = Readonly<{ key: string; from: string; to: string; d: string }>;

const r1 = (n: number): number => Math.round(n * 10) / 10;

/** Ring to ring on the schematic grammar: along, across, along. */
function elbow(a: Point, b: Point): string {
  if (Math.abs(a.y - b.y) < 0.5) return `M${r1(a.x)} ${r1(a.y)} L${r1(b.x)} ${r1(b.y)}`;
  // One above the other: step out to the left so the line is not hidden
  // under the card edge both rings sit beside.
  const mid = Math.abs(a.x - b.x) < 0.5 ? a.x - CARD.ring : (a.x + b.x) / 2;
  return `M${r1(a.x)} ${r1(a.y)} H${r1(mid)} V${r1(b.y)} H${r1(b.x)}`;
}

/** Every line drawn on the floor: each station row's edges, between the
 *  drawn cards they join. A line exists because a station row declares
 *  it, never because something crossed it. The routes GET
 *  /api/yard/routes serves are keyed by the old map's regions, and the
 *  table that stood a region on a station (`REGION_AT`) retired with the
 *  transit map (R3, backlog 23a4b4d7), so they are not drawn here. A pair
 *  declared twice is drawn once; a pair inside one drawn card is not a
 *  line. */
export function treeEdges(tree: StationTree, layout: TreeLayout): ReadonlyArray<TreeEdge> {
  const seen = new Set<string>();
  return tree.nodes.flatMap((n) => n.edges.map((e) => [layout.drawn(n.name), layout.drawn(e)] as const))
    .flatMap(([from, to]): TreeEdge[] => {
      if (from === null || to === null || from === to) return [];
      const key = `${from}→${to}`;
      if (seen.has(key)) return [];
      seen.add(key);
      const a = layout.ring(from);
      const b = layout.ring(to);
      return a === null || b === null ? [] : [{ key, from, to, d: elbow(a, b) }];
    });
}
