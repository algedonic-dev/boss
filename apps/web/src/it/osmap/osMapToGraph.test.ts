import { describe, expect, test } from 'bun:test';
import {
  edgeId,
  handoffDeltas,
  isSimulated,
  layout,
  strokeWidth,
  toEdges,
  type OsMapEdge,
  type OsMapNode,
} from './osMapToGraph';

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

describe('handoffDeltas — what moved since the last read', () => {
  const e = (source: string, target: string, handoffs: number, simulated = 0) => ({
    source,
    target,
    handoffs,
    simulated,
  });

  test('reports nothing on the first read', () => {
    // A page load must not fire a pulse for every handoff in the
    // window — the map would erupt on open and mean nothing.
    expect([...handoffDeltas(null, [e('a', 'b', 1800)]).entries()]).toEqual([]);
  });

  test('reports only the increase, not the total', () => {
    const d = handoffDeltas([e('a', 'b', 10)], [e('a', 'b', 13)]);
    expect(d.get('a->b')).toBe(3);
  });

  test('ignores a route that did not move', () => {
    expect([...handoffDeltas([e('a', 'b', 10)], [e('a', 'b', 10)]).keys()]).toEqual([]);
  });

  test('ignores a DECREASE rather than treating it as movement', () => {
    // The map reads a trailing window, so old traffic ages out and a
    // count can fall. A shrinking route is not a handoff happening —
    // counting it would make the map pulse hardest exactly when the
    // brewery went quiet.
    expect([...handoffDeltas([e('a', 'b', 40)], [e('a', 'b', 12)]).keys()]).toEqual([]);
  });

  test('treats a brand-new route as all movement', () => {
    const d = handoffDeltas([e('a', 'b', 5)], [e('a', 'b', 5), e('c', 'd', 2)]);
    expect(d.get('c->d')).toBe(2);
    expect(d.has('a->b')).toBe(false);
  });

  test('keys a self-loop the same way the edge does', () => {
    // The dispatcher's own loop is the busiest route on the map. If
    // the delta key and the edge id disagreed, the busiest edge would
    // be the one that never pulsed.
    const d = handoffDeltas([e('dispatcher', 'dispatcher', 1)], [e('dispatcher', 'dispatcher', 4)]);
    expect(d.get(edgeId({ source: 'dispatcher', target: 'dispatcher' }))).toBe(3);
  });
});

describe('toEdges — pulses ride along with the history', () => {
  test('keeps stroke width for cumulative volume and puts the delta in data', () => {
    const edges = toEdges(
      [
        { source: 'a', target: 'b', handoffs: 100, simulated: 0 },
        { source: 'c', target: 'd', handoffs: 1, simulated: 1 },
      ],
      new Map([['a->b', 3]]),
      12345,
    );
    const ab = edges.find((x) => x.id === 'a->b')!;
    const cd = edges.find((x) => x.id === 'c->d')!;
    // History: the busier route is visibly thicker.
    expect(ab.style).toContain('stroke-width');
    expect(cd.style).toContain('stroke-width');
    // Moment: only the route that moved carries pulses.
    expect((ab.data as { pulses: number }).pulses).toBe(3);
    expect((cd.data as { pulses: number }).pulses).toBe(0);
    // The token forces the animation to replay on a new read.
    expect((ab.data as { pulseToken: number }).pulseToken).toBe(12345);
  });

  test('routes every edge through the custom type, self-loops included', () => {
    // Self-loops used to fall back to 'smoothstep'. They are the
    // commonest edge in the data, so they cannot be the ones that go
    // un-animated.
    const edges = toEdges([{ source: 'x', target: 'x', handoffs: 9, simulated: 0 }]);
    expect(edges[0]!.type).toBe('pulse');
  });
});
