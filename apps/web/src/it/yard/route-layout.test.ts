// THE ROUTE LAYOUT — design e765b3fc, car R3. The map draws exactly the
// routes /api/yard/routes serves, laid out by the octilinear router, and
// the moves of car M2 travel the same paths through `routePath`. These
// pin both halves: what is drawn is what is served, and how it is laid.
import { describe, expect, it } from 'bun:test';
import { ROUTES, routesPayload, type FixtureRoute } from '../../../tests/fixtures/yard';
import {
  ARC_ABOVE,
  ARC_BELOW,
  LANE,
  ledgerOf,
  linesOf,
  mapView,
  octilinear,
  routePath,
  sectionKey,
  sectionsOf,
  wordsSide,
  type Section,
} from './route-layout';
import { parseRoutes } from './routes';
import { STATIONS } from './transit';
import { pointAt } from './world-motion';

/** The fixture's routes, as the page parses them off the wire. */
const served = (routes: ReadonlyArray<FixtureRoute> = ROUTES) => parseRoutes(routesPayload(routes));
const laid = (routes?: ReadonlyArray<FixtureRoute>) => sectionsOf(served(routes)).sections;
const find = (key: string) => laid().find((s) => s.key === key)!;
const station = (name: string) => STATIONS.find((s) => s.name === name)!;

// THE EDGES ARE SERVED (design e765b3fc, car R3): the map draws exactly
// the routes /api/yard/routes answers — one drawn element per route,
// no more and no fewer — laid out by the octilinear router.
describe('the map draws exactly the routes the server serves', () => {
  it('one drawn element per served route, keyed by its two ends', () => {
    expect(laid().map((s) => s.key)).toEqual(ROUTES.map((r) => sectionKey(r.from, r.to)));
  });

  it('a route the server starts serving arrives on the map, and one it stops serving leaves it — no web change', () => {
    const more = laid([...ROUTES, { from: 'track', to: 'garage', declared: true, sources: [{ source: 'observed', moves: 1, last_at: '' }] }]);
    expect(more.map((s) => s.key)).toContain('track→garage');
    const fewer = laid(ROUTES.filter((r) => !(r.from === 'gates' && r.to === 'track')));
    expect(fewer.map((s) => s.key)).not.toContain('gates→track');
    expect(fewer).toHaveLength(ROUTES.length - 1);
  });

  it('draws nothing it was not served: no routes, no sections', () => {
    expect(laid([])).toEqual([]);
  });

  it('names a section, an exit and an entry by their ends, the key a move (car M2) finds its path by', () => {
    expect(find('gates→track').kind).toBe('section');
    expect(find('dock→').kind).toBe('exit');
    expect(find('→receiving').kind).toBe('entry');
    expect(sectionKey('dock', null)).toBe('dock→');
    expect(sectionKey(null, 'receiving')).toBe('→receiving');
  });

  it('says, rather than drops, a route to a region this layout has no station for', () => {
    const out = sectionsOf(served([...ROUTES, { from: 'gates', to: 'the-moon', declared: true, sources: [{ source: 'observed', moves: 1, last_at: '' }] }]));
    expect(out.unplaced).toEqual(['gates→the-moon']);
    expect(out.sections).toHaveLength(ROUTES.length);
  });

  it('carries whether a route is declared — observed-only routes are drawn apart (dashed red)', () => {
    expect(find('shed→arrivals').declared).toBe(false);
    expect(find('gates→track').declared).toBe(true);
  });

  it('puts a route on the line of the station it serves off the main line', () => {
    expect(find('receiving→marshalling').line).toBe('delivery');
    expect(find('gates→track').line).toBe('delivery');
    expect(find('gates→garage').line).toBe('siding');
    expect(find('garage→dock').line).toBe('siding');
    expect(linesOf(laid())).toEqual(['delivery', 'siding']);
  });
});

describe('the octilinear router', () => {
  it('runs every section from its from-station to its to-station', () => {
    for (const s of laid().filter((x) => x.kind === 'section')) {
      const a = station(s.from!);
      const b = station(s.to!);
      const start = pointAt(s.walked, 0);
      const end = pointAt(s.walked, s.walked.length);
      // Within its ring: a straight two-way run stands a LANE to the
      // right of its travel (the garage and the gates, exactly 45° apart
      // since the stations were spaced for the exit fans, 3c8b5653).
      expect([s.key, Math.hypot(start.x - a.x, start.y - a.y) <= LANE + 0.01]).toEqual([s.key, true]);
      expect([s.key, Math.hypot(end.x - b.x, end.y - b.y) <= LANE + 0.01]).toEqual([s.key, true]);
    }
  });

  it('draws at fixed angles only — every leg horizontal, vertical or 45°', () => {
    for (const s of laid()) {
      const pts = s.walked.points;
      for (let i = 1; i < pts.length; i++) {
        const dx = Math.abs(pts[i]!.x - pts[i - 1]!.x);
        const dy = Math.abs(pts[i]!.y - pts[i - 1]!.y);
        expect([s.key, dx === 0 || dy === 0 || Math.abs(dx - dy) < 0.01]).toEqual([s.key, true]);
      }
    }
  });

  it('runs straight to the next station on the line', () => {
    expect(find('receiving→marshalling').walked.points).toHaveLength(2);
  });

  it('lifts a forward run over the station it passes — the train departs from the gates OVER the dock to the track', () => {
    const pts = find('gates→track').walked.points;
    const dock = station('dock');
    expect(Math.min(...pts.map((p) => p.y))).toBe(dock.y - ARC_ABOVE);
    // It never touches the dock's ring.
    const nearest = Math.min(...pts.map((p) => Math.hypot(p.x - dock.x, p.y - dock.y)));
    expect(nearest).toBeGreaterThan(20);
  });

  it('drops a backward run UNDER the line — the train made up at the dock runs back to the gates', () => {
    const pts = find('dock→gates').walked.points;
    expect(Math.max(...pts.map((p) => p.y))).toBe(station('dock').y + ARC_BELOW);
    const back = find('shop-floor→marshalling').walked.points;
    expect(Math.max(...back.map((p) => p.y))).toBeGreaterThan(station('shop-floor').y);
  });

  it('keeps the two directions of a two-way run apart — the garage and the dock, both ways', () => {
    const out = find('dock→garage').walked;
    const back = find('garage→dock').walked;
    const mid = (w: typeof out) => pointAt(w, w.length / 2);
    expect(Math.hypot(mid(out).x - mid(back).x, mid(out).y - mid(back).y)).toBeGreaterThan(8);
  });

  it('moves a straight two-way run a lane apart', () => {
    const a = { name: 'a', x: 100, y: 100, below: false, line: 'delivery' as const };
    const b = { name: 'b', x: 100, y: 300, below: true, line: 'siding' as const };
    const there = octilinear(a, b, [a, b], true);
    const back = octilinear(b, a, [a, b], true);
    expect(there[0]!.x).not.toBe(back[1]!.x);
    expect(octilinear(a, b, [a, b], false)[0]).toEqual({ x: 100, y: 100 });
  });

  it('draws an exit away from the line, and an entry into its station', () => {
    const exit = find('dock→').walked.points;
    expect(exit[0]!.y).toBeGreaterThan(station('dock').y);
    expect(exit[exit.length - 1]!.y).toBeGreaterThan(exit[0]!.y);
    const entry = find('→receiving').walked.points;
    expect(entry[0]!.y).toBeLessThan(entry[entry.length - 1]!.y);
    expect(entry[entry.length - 1]!.y).toBeLessThan(station('receiving').y);
  });
});

// THE EXITS BAND (backlog 3c8b5653; design f0313eda, David chose E2 on
// 2026-09-26): every station's exits drop into one band under the line,
// a fan per station, a branch per terminal, every buffer on one rail.
describe('the exits band — one fan per station, one branch per terminal', () => {
  const out = sectionsOf(served());
  const fanOf = (name: string) => out.sections.find((s) => s.key === `${name}→`)!.fan!;
  const legsOctilinear = (pts: ReadonlyArray<{ x: number; y: number }>) =>
    pts.every((p, i) => {
      if (i === 0) return true;
      const dx = Math.abs(p.x - pts[i - 1]!.x);
      const dy = Math.abs(p.y - pts[i - 1]!.y);
      return dx < 0.01 || dy < 0.01 || Math.abs(dx - dy) < 0.01;
    });

  it('lays one band under every station, and a fan in it per exit', () => {
    expect(out.band).not.toBeNull();
    const lowest = Math.max(...STATIONS.map((s) => s.y));
    expect(out.band!.top).toBeGreaterThan(lowest + 40);
    const exits = ROUTES.filter((r) => r.to === null);
    expect(out.sections.filter((s) => s.fan !== null).map((s) => s.key)).toEqual(exits.map((r) => sectionKey(r.from, r.to)));
  });

  it('splits the fixture station into three branches in name order, each with its count', () => {
    const fan = fanOf('marshalling');
    expect(fan.branches.map((b) => [b.name, b.count])).toEqual([
      ['closed', 42],
      ['declined', 0],
      ['duplicate', 3],
    ]);
    expect(fan.total).toBe(45);
  });

  it('stands every buffer stop on one rail, below the band top', () => {
    const rails = new Set(out.sections.flatMap((s) => s.fan?.branches.map((b) => b.end.y) ?? []));
    expect([...rails]).toEqual([out.band!.rail]);
  });

  it('draws every stub and branch at fixed angles, from the station ring to its buffer', () => {
    for (const s of out.sections.filter((x) => x.fan !== null)) {
      const st = station(s.from!);
      for (const b of s.fan!.branches) {
        const pts = b.walked.points;
        expect([s.key, b.name, legsOctilinear(pts)]).toEqual([s.key, b.name, true]);
        expect(pts[0]).toEqual({ x: st.x, y: st.y + 12 });
        expect(pts[pts.length - 1]).toEqual(b.end);
      }
    }
  });

  it('keeps a branch\'s place when the counts change — order is by name, not by count', () => {
    const recount = ROUTES.map((r) =>
      r.from === 'marshalling' && r.to === null
        ? { ...r, sources: [...r.sources.filter((s) => s.source !== 'observed'), { source: 'observed' as const, moves: 900, last_at: '', kind: 'backlog-item', terminal: 'declined' }] }
        : r,
    );
    const fan = sectionsOf(served(recount)).sections.find((s) => s.key === 'marshalling→')!.fan!;
    expect(fan.branches.map((b) => b.name)).toEqual(['closed', 'declined', 'duplicate']);
    expect(fan.branches.map((b) => b.end.x)).toEqual(fanOf('marshalling').branches.map((b) => b.end.x));
  });

  it('a wide fan pushes the next one aside: its feeder jogs at 45° inside the band, and no two fans overlap', () => {
    const many = Array.from({ length: 14 }, (_, i) => `a-rather-long-terminal-name-${String(i).padStart(2, '0')}`);
    const wide = ROUTES.map((r) =>
      r.from === 'marshalling' && r.to === null
        ? { ...r, sources: many.map((t) => ({ source: 'workflow' as const, workflow: 'backlog-item', version: 1, step: t, via: 'closed' })) }
        : r,
    );
    const laidWide = sectionsOf(served(wide));
    const shop = laidWide.sections.find((s) => s.key === 'shop-floor→')!;
    expect(shop.fan!.jog).toBeGreaterThan(0);
    expect(legsOctilinear(shop.walked.points)).toBe(true);
    // The jog is a 45° leg: it drops as far as it moves across.
    const pts = shop.walked.points;
    expect(pts.some((p, i) => i > 0 && Math.abs(p.x - pts[i - 1]!.x) > 0 && Math.abs(Math.abs(p.x - pts[i - 1]!.x) - Math.abs(p.y - pts[i - 1]!.y)) < 0.01)).toBe(true);
    const span = (f: NonNullable<Section['fan']>) => [Math.min(...f.branches.map((b) => b.end.x)), Math.max(...f.branches.map((b) => b.end.x))];
    const fans = laidWide.sections.flatMap((s) => (s.fan === null ? [] : [s.fan])).sort((a, b) => a.junction.x - b.junction.x);
    for (let i = 1; i < fans.length; i++) expect(span(fans[i]!)[0]!).toBeGreaterThan(span(fans[i - 1]!)[1]!);
    // The view grows to hold it.
    expect(mapView(fans, laidWide.band).width).toBeGreaterThanOrEqual(Math.max(...fans.flatMap((f) => f.branches.map((b) => b.end.x))));
  });

  it('with no exit served there is no band, and the view is the stations\' alone', () => {
    const none = sectionsOf(served(ROUTES.filter((r) => r.to !== null)));
    expect(none.band).toBeNull();
    expect(mapView([], null).height).toBeLessThan(mapView(out.sections.flatMap((s) => (s.fan ? [s.fan] : [])), out.band).height);
  });

  it('writes a station\'s words off its lines: on the side no section leaves downward from', () => {
    // The garage's feeder drops down and right from the gates, so the
    // gates write their count on the left.
    expect(wordsSide('gates', out.sections)).toBe('left');
    expect(wordsSide('receiving', out.sections)).toBe('right');
  });

  it('reads each station\'s in / on / off off the served counts, and off is the sum of its branches', () => {
    const routes = served();
    const l = ledgerOf(routes, 'marshalling')!;
    expect(l).toEqual({ in: 20, on: 24, off: 45 });
    expect(l.off).toBe(fanOf('marshalling').total);
    expect(ledgerOf({ ...routes, observed: false }, 'marshalling')).toBeNull();
  });
});

// THE ONE LOOKUP THE MOVES TRAVEL BY (car M2 builds on it): the path of
// the move (from, to) is the path the layer draws, or null where no
// route is served — a dot never runs on a line the map does not draw.
describe('routePath — the path a move (from, to) travels', () => {
  const routes = served();

  it('answers the drawn section, exit or entry, the same path the layer draws', () => {
    const train = routePath(routes, 'gates', 'track')!;
    expect(train.kind).toBe('section');
    expect(train.d).toBe(find('gates→track').d);
    expect(pointAt(train.walked, 0)).toEqual({ x: station('gates').x, y: station('gates').y });
    expect(routePath(routes, 'dock', null)?.kind).toBe('exit');
    expect(routePath(routes, null, 'receiving')?.kind).toBe('entry');
  });

  it('answers an exit by its terminal as the stub and that branch, to its buffer stop', () => {
    const closed = routePath(routes, 'marshalling', null, 'closed')!;
    const fan = sectionsOf(routes).sections.find((s) => s.key === 'marshalling→')!.fan!;
    expect(closed.kind).toBe('exit');
    expect(pointAt(closed.walked, closed.walked.length)).toEqual(fan.branches.find((b) => b.name === 'closed')!.end);
    // A terminal the fan does not draw, or none: the stub alone.
    expect(routePath(routes, 'marshalling', null, 'no-such')!.d).toBe(routePath(routes, 'marshalling', null)!.d);
  });

  it('answers null for a move on a route the server does not serve', () => {
    expect(routePath(routes, 'dock', 'track')).toBeNull();
    expect(routePath(routes, 'arrivals', null)).toBeNull();
    expect(routePath(served([]), 'gates', 'track')).toBeNull();
  });
});
