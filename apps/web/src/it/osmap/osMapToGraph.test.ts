import { describe, expect, test } from 'bun:test';
import { isSimulated, layout, strokeWidth, type OsMapEdge, type OsMapNode } from './osMapToGraph';

const node = (id: string, kind: OsMapNode['kind'], touched: number): OsMapNode => ({
  id,
  label: id,
  kind,
  touched,
});

const edge = (handoffs: number, simulated: number): OsMapEdge => ({
  source: 'a',
  target: 'b',
  handoffs,
  simulated,
});

describe('layout', () => {
  // A live view polls. A force-directed layout would reshuffle every
  // few seconds as counts shift, so movement on screen would mean
  // "the physics settled differently" rather than "work moved" —
  // which destroys the only thing a live instrument is for.
  test('is deterministic across renders', () => {
    const nodes = [node('production', 'department', 900), node('qa', 'department', 60)];
    expect(layout(nodes)).toEqual(layout(nodes));
  });

  test('pins the dispatcher to the centre', () => {
    const placed = layout([
      node('dispatcher', 'dispatcher', 2600),
      node('production', 'department', 900),
    ]);
    const d = placed.find((n) => n.id === 'dispatcher')!;
    expect(d.position).toEqual({ x: 0, y: 0 });
    // Everything else is off-centre, or the map is a pile.
    const p = placed.find((n) => n.id === 'production')!;
    expect(p.position.x !== 0 || p.position.y !== 0).toBe(true);
  });

  test('orders the rim by traffic, not by name', () => {
    const placed = layout([
      node('aaa', 'department', 1),
      node('zzz', 'department', 999),
    ]);
    // Busiest goes first (top), so the map reads the same way twice
    // running even as quiet departments come and go.
    expect(placed[0]!.id).toBe('zzz');
  });
});

describe('strokeWidth', () => {
  // The playground's busiest edge carries 1,821 handoffs and its
  // quietest carries 1. Linear scaling renders everything but the top
  // two as a hairline, hiding the small flows worth noticing.
  test('keeps a small flow visible next to a huge one', () => {
    const small = strokeWidth(1, 1821);
    const large = strokeWidth(1821, 1821);
    expect(small).toBeGreaterThan(1);
    expect(large).toBeGreaterThan(small);
    // Log scale: the ratio is nothing like 1821:1.
    expect(large / small).toBeLessThan(10);
  });

  test('handles a degenerate single-edge map', () => {
    expect(strokeWidth(1, 1)).toBeGreaterThan(0);
    expect(Number.isFinite(strokeWidth(0, 0))).toBe(true);
  });
});

describe('isSimulated', () => {
  test('only when every handoff on the edge is simulated', () => {
    expect(isSimulated(edge(10, 10))).toBe(true);
    expect(isSimulated(edge(10, 0))).toBe(false);
  });

  // A mixed edge is real work that happens to include some synthetic.
  // Colouring it as simulated would understate what people did — on a
  // map someone might make staffing decisions from.
  test('a mixed edge counts as real', () => {
    expect(isSimulated(edge(10, 9))).toBe(false);
  });

  test('an empty edge is not simulated', () => {
    expect(isSimulated(edge(0, 0))).toBe(false);
  });
});
