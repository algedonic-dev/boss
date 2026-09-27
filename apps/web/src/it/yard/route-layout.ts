// THE ROUTE LAYOUT — where every route the server serves is drawn
// (design e765b3fc, car R3 on feedback 84cba7e2).
//
// ONE PLACE ANSWERS "WHAT PATH DOES THE MOVE (from, to) TRAVEL". The
// transit map draws its sections, exits and entries from here
// (RouteLayer.svelte), and the moves of car M2 — a ping at a station, a
// slow transit between two — travel the same paths, so a dot can never
// run along a line the map does not draw:
//
//     routePath(routes, 'gates', 'track')   the section a train departs by
//     routePath(routes, 'dock', null)       the dock's exit stub, to its fan
//     routePath(routes, 'dock', null, 't')  ... and down its branch for terminal t
//     routePath(routes, null, 'receiving')  receiving's entry stub
//
// each answering `{ d, walked }` — the SVG path, and the walked polyline
// `pointAt(walked, t · walked.length)` reads a point along — or null
// where the server serves no such route (a move on it is then one the
// map cannot draw, and says so rather than inventing a track). In the
// DOM every drawn element carries the same key, `data-section="<from>→<to>"`
// (an empty end for an exit or an entry), with `data-from` / `data-to`.
//
// NOTHING HERE IS AN EDGE. The station positions are transit.ts's data
// (a layout is not a route), the routes are the server's, and the lint
// `a-map-edge-is-served-not-drawn` refuses a station-pair literal in
// this directory. Pure functions, so `bun test` pins every rule.

import { exitBranches, type ExitBranch, type Route, type Routes } from './routes';
import { LINE_TOKEN, STATIONS, TRANSIT_VIEW, type Station, type TransitLine } from './transit';
import { type Point, type Walked, pathPoints, walk } from './world-motion';

// ---------------------------------------------------------------------
// The router: a served route onto the page, at fixed angles only.
// ---------------------------------------------------------------------

/** How far an arc over (or under) the main line stands off it: past the
 *  station names above, past the counts below, and a step further for
 *  every station it passes, so two arcs over one stretch never share a
 *  track. */
export const ARC_ABOVE = 62;
export const ARC_BELOW = 56;
export const ARC_STEP = 14;
/** How far apart the two directions of one straight run are drawn — 9,
 *  not 5, so a two-way pair reads as two tracks (design f0313eda E2). */
export const LANE = 9;
/** An entry's stub: from outside the ring to this far out. */
const RING = 12;
export const RAMP = 14;

const fmt = (n: number): string => String(Math.round(n * 100) / 100);
const pathOf = (pts: ReadonlyArray<Point>): string =>
  pts.map((p, i) => `${i === 0 ? 'M' : 'L'}${fmt(p.x)} ${fmt(p.y)}`).join(' ');

/** A polyline moved `by` units to the right of its own direction of
 *  travel — the lane a straight run takes when the run back is served
 *  too, as a double track keeps its directions apart. */
function shifted(pts: ReadonlyArray<Point>, by: number): ReadonlyArray<Point> {
  const a = pts[0]!;
  const b = pts[pts.length - 1]!;
  const len = Math.hypot(b.x - a.x, b.y - a.y) || 1;
  const nx = (-(b.y - a.y) / len) * by;
  const ny = ((b.x - a.x) / len) * by;
  return pts.map((p) => ({ x: p.x + nx, y: p.y + ny }));
}

/** THE OCTILINEAR ROUTER (design e765b3fc §2c: "each section's path is
 *  computed by a small octilinear router between two station positions,
 *  with parallel lines offset"). Every leg is horizontal, vertical or at
 *  45°, and the path starts at `a`'s centre and ends at `b`'s:
 *
 *  - ON THE MAIN LINE, forward to the next station: straight along it.
 *    Forward past a station, it rises over the line in an arc — 45° up,
 *    along, 45° down — so it never runs through the station it skips.
 *    Backward, it drops under the line the same way, so the two
 *    directions between two stations never share a track (the train
 *    made up at the dock runs BACK to the gates under the line, then
 *    over the dock to the track).
 *  - BETWEEN ROWS, a straight leg first and then the 45° into `b`, so
 *    the run back — straight first from ITS end — draws the other half
 *    of a parallelogram rather than over this one.
 *  - A run that is one straight leg (vertical, or exactly 45°) is moved
 *    LANE to the right of its travel when the run back is served too.
 *
 *  `stations` is the layout the arcs clear; `twoWay` whether `b → a` is
 *  served as well. */
export function octilinear(a: Station, b: Station, stations: ReadonlyArray<Station>, twoWay: boolean): ReadonlyArray<Point> {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  if (dy === 0) {
    const lo = Math.min(a.x, b.x);
    const hi = Math.max(a.x, b.x);
    const passed = stations.filter((s) => s.y === a.y && s.x > lo && s.x < hi).length;
    const forward = dx > 0;
    if (forward && passed === 0) return [{ x: a.x, y: a.y }, { x: b.x, y: b.y }];
    const steps = forward ? passed - 1 : passed;
    const want = (forward ? ARC_ABOVE : ARC_BELOW) + ARC_STEP * steps;
    const lift = Math.min(want, Math.abs(dx) / 2);
    const y = a.y + (forward ? -lift : lift);
    const dir = Math.sign(dx);
    return [
      { x: a.x, y: a.y },
      { x: a.x + dir * lift, y },
      { x: b.x - dir * lift, y },
      { x: b.x, y: b.y },
    ];
  }
  const ax = Math.abs(dx);
  const ay = Math.abs(dy);
  if (dx === 0 || ax === ay) {
    const pts = [{ x: a.x, y: a.y }, { x: b.x, y: b.y }];
    return twoWay ? shifted(pts, LANE) : pts;
  }
  const elbow =
    ax > ay
      ? { x: a.x + Math.sign(dx) * (ax - ay), y: a.y }
      : { x: a.x, y: a.y + Math.sign(dy) * (ay - ax) };
  return [{ x: a.x, y: a.y }, elbow, { x: b.x, y: b.y }];
}

// ---------------------------------------------------------------------
// The exits band: every station's exits, one fan each (backlog 3c8b5653,
// design f0313eda — David chose E2, the exits band, 2026-09-26).
// ---------------------------------------------------------------------
//
// David, 2026-09-26: "I am okay using a bit more space to make things
// look nicer or to include the different exits from the stations in a
// coherent way." Each exit was one 14-unit off-ramp and a buffer stop;
// the terminals it stood for lived only in its hover, and the count in
// no drawing at all — marshalling's one ramp stood for 283 exits by
// twenty terminal names. Now every station's exit is ONE STUB dropping
// straight down into a dedicated EXITS band under the line, splaying at
// 45° into one branch per terminal (routes.ts `exitBranches`, in name
// order), each ending in a buffer stop on ONE RAIL shared by every fan,
// labelled at 45° under it with its count. Each fan is headed in the
// band by its station and its total. A wide fan pushes the next one
// aside: that feeder jogs right at 45° inside the band, so fans never
// overlap and never cross a route.

/** Branch spacing along the rail. */
export const SPREAD = 20;
/** How far below the lowest station the band begins: past the words
 *  written beside it. */
const BAND_BELOW = 80;
/** Where, below the band's top, a feeder may start its jog — under the
 *  fan's heading. */
const FEED = 36;
/** The straight drop from the fan's widest 45° to the rail. */
const DROP = 22;
/** A branch label's run at 45°: characters of `count name` × this. */
const LABEL_CHAR = 5.2 * Math.SQRT1_2;

/** A fan's branch: its terminal, count and declaration, and the path an
 *  exit by it travels — the stub, then this branch, to the buffer stop. */
export type FanBranch = Readonly<
  ExitBranch & {
    /** The branch alone, junction to buffer — the stroke. */
    d: string;
    /** The whole way off the map by this terminal — ring, stub, branch. */
    walked: Walked;
    way: string;
    /** The buffer stop's centre, on the rail. */
    end: Point;
  }
>;

/** One station's exit fan, as the band lays it. */
export type Fan = Readonly<{
  station: string;
  total: number;
  /** Where the stub leaves the ring, and where the band places the fan's
   *  junction: `jog` is how far right the feeder jogged at 45°. */
  jog: number;
  junction: Point;
  /** The stub, ring to junction — the exit's own stroke, and the way a
   *  packet whose terminal is unknown travels. */
  stub: string;
  /** The fan's heading in the band: the station, its total. */
  head: Point;
  rail: number;
  branches: ReadonlyArray<FanBranch>;
}>;

/** The band itself: its top edge and the rail every buffer stands on. */
export type Band = Readonly<{ top: number; rail: number }>;

const labelRun = (b: ExitBranch): number => (`${b.count} ${b.name}`.length) * LABEL_CHAR + 6;

/** LAY OUT EVERY EXIT FAN IN THE BAND (design f0313eda E2). Fans in
 *  station order; each sits under its station unless the fan before it,
 *  labels and all, needs the room — then its feeder jogs right at 45°.
 *  The rail sits low enough for the widest fan and the longest jog, and
 *  every buffer stands on it. */
export function layFans(
  exits: ReadonlyArray<Readonly<{ station: Station; branches: ReadonlyArray<ExitBranch> }>>,
  stations: ReadonlyArray<Station> = STATIONS,
): Readonly<{ fans: ReadonlyArray<Fan>; band: Band | null }> {
  if (exits.length === 0) return { fans: [], band: null };
  const top = Math.max(...stations.map((s) => s.y)) + BAND_BELOW;
  const ordered = [...exits].sort((a, b) => a.station.x - b.station.x);
  const halfOf = (n: number): number => (Math.max(n, 1) - 1) / 2 * SPREAD;
  const placed = ordered.reduce<ReadonlyArray<Readonly<{ cx: number; right: number }>>>((acc, e) => {
    const half = halfOf(e.branches.length);
    const right = acc.length === 0 ? -Infinity : acc[acc.length - 1]!.right;
    const cx = Math.max(e.station.x, right + 18 + half);
    const over = Math.max(0, ...e.branches.map(labelRun));
    return [...acc, { cx, right: cx + half + over }];
  }, []);
  const maxHalf = Math.max(...ordered.map((e) => halfOf(e.branches.length)));
  const rail = Math.max(
    top + FEED + maxHalf + DROP,
    ...ordered.map((e, i) => top + FEED + (placed[i]!.cx - e.station.x) + halfOf(e.branches.length) + DROP + 14),
  );
  const fans = ordered.map((e, i): Fan => {
    const { station: s, branches } = e;
    const cx = placed[i]!.cx;
    const jog = cx - s.x;
    const half = halfOf(branches.length);
    const jy = rail - half - DROP;
    const ring = { x: s.x, y: s.y + RING };
    const feed = top + FEED;
    const stub: ReadonlyArray<Point> =
      jog > 0
        ? [ring, { x: s.x, y: feed }, { x: cx, y: feed + jog }, { x: cx, y: jy }]
        : [ring, { x: s.x, y: jy }];
    const n = branches.length;
    return {
      station: s.name,
      total: branches.reduce((t, b) => t + b.count, 0),
      jog,
      junction: { x: cx, y: jy },
      stub: pathOf(stub),
      head: { x: s.x + 8, y: top + 24 },
      rail,
      branches: branches.map((b, k): FanBranch => {
        const bx = cx + (k - (n - 1) / 2) * SPREAD;
        const dd = Math.abs(bx - cx);
        const leg: ReadonlyArray<Point> =
          dd > 0 ? [{ x: cx, y: jy }, { x: bx, y: jy + dd }, { x: bx, y: rail }] : [{ x: cx, y: jy }, { x: cx, y: rail }];
        const way = pathOf([...stub, ...leg.slice(1)]);
        return { ...b, d: pathOf(leg), way, walked: walk(pathPoints(way)), end: { x: bx, y: rail } };
      }),
    };
  });
  return { fans, band: { top, rail } };
}

/** THE DRAWING'S SIZE: the stations, and — when any exit is served — the
 *  band under them, wide enough for every fan and its 45° labels, deep
 *  enough for the labels under the rail. */
export function mapView(
  fans: ReadonlyArray<Fan>,
  band: Band | null,
  stations: ReadonlyArray<Station> = STATIONS,
): Readonly<{ width: number; height: number }> {
  const stationsRight = Math.max(...stations.map((s) => s.x)) + 80;
  if (band === null) return { width: Math.max(TRANSIT_VIEW.width, stationsRight), height: TRANSIT_VIEW.height };
  const branches = fans.flatMap((f) => f.branches);
  const right = Math.max(stationsRight, ...branches.map((b) => b.end.x + labelRun(b) + 24));
  const under = Math.max(0, ...branches.map(labelRun));
  return { width: Math.ceil(Math.max(TRANSIT_VIEW.width, right)), height: Math.ceil(band.rail + under + 64) };
}

/** WHICH SIDE OF ITS RING A STATION WRITES ITS COUNT ON — off the lines
 *  (design f0313eda E2): the stub drops straight down through where the
 *  words stood, so they go beside the ring, on the side no section
 *  leaves downward from. */
export function wordsSide(name: string, sections: ReadonlyArray<Section>): 'left' | 'right' {
  const downRight = sections.some((s) => {
    if (s.kind !== 'section') return false;
    const pts = s.walked.points;
    const ends: ReadonlyArray<readonly [Point, Point]> =
      s.from === name && pts.length > 1 ? [[pts[0]!, pts[1]!]] : s.to === name && pts.length > 1 ? [[pts[pts.length - 1]!, pts[pts.length - 2]!]] : [];
    return ends.some(([end, next]) => next.y > end.y + 0.1 && next.x > end.x);
  });
  return downRight ? 'left' : 'right';
}

/** A station's ledger off the served counts: moves IN to it, moves ON to
 *  another station, moves OFF the map — the conservation its fan splits
 *  (`off` is the sum of its branches). Null where the moves record was
 *  not read: never zeroes. */
export function ledgerOf(routes: Routes, name: string): Readonly<{ in: number; on: number; off: number }> | null {
  if (!routes.observed) return null;
  const moves = (r: Route): number => r.sources.reduce((n, s) => n + (s.source === 'observed' ? s.moves : 0), 0);
  const sum = (f: (r: Route) => boolean): number => routes.routes.filter(f).reduce((n, r) => n + moves(r), 0);
  return {
    in: sum((r) => r.to === name),
    on: sum((r) => r.from === name && r.to !== null),
    off: sum((r) => r.from === name && r.to === null),
  };
}

/** An entry's stub: into the ring from above a main-line station, from
 *  the left of one under the line. */
export function entryPath(s: Station): ReadonlyArray<Point> {
  return s.below
    ? [{ x: s.x - RING - RAMP, y: s.y }, { x: s.x - RING, y: s.y }]
    : [{ x: s.x, y: s.y - RING - RAMP }, { x: s.x, y: s.y - RING }];
}

// ---------------------------------------------------------------------
// The sections: every served route, laid out.
// ---------------------------------------------------------------------

/** A border's key, in the arrow the world map and its specs use — and
 *  the key a drawn section answers to: `gates→track` for a section,
 *  `dock→` for the dock's exit, `→receiving` for receiving's entry. The
 *  moves feed (car M2) finds the path a move `(from, to)` travels by
 *  this key. */
export const sectionKey = (from: string | null, to: string | null): string => `${from ?? ''}→${to ?? ''}`;

/** A section of track, an exit or an entry: the route it draws, its
 *  line, and its path — `d` for the stroke, `walked` for anything that
 *  travels it (the waiting blocks, the moving blocks, and the moves of
 *  car M2, which call `pointAt(walked, t · walked.length)`). An exit's
 *  `d` is its fan's stub, ring to junction, and its `fan` the branches
 *  under it in the exits band (design f0313eda E2); null on the rest. */
export type Section = Readonly<{
  key: string;
  kind: 'section' | 'exit' | 'entry';
  from: string | null;
  to: string | null;
  line: TransitLine;
  /** False: only the moves record supports it — drawn dashed red. */
  declared: boolean;
  route: Route;
  d: string;
  walked: Walked;
  fan: Fan | null;
}>;

const lineOf = (a: Station | undefined, b: Station | undefined): TransitLine =>
  [a, b].find((s) => s !== undefined && s.line !== 'delivery')?.line ?? 'delivery';

/** EVERY SERVED ROUTE, LAID OUT — and the ones this layout cannot place,
 *  said rather than dropped: a route to a region the map has no station
 *  for is named at the foot of the map. The exits are laid together, in
 *  one band (`layFans`), so `band` is where it stands — null when no
 *  exit is served. */
export function sectionsOf(
  routes: Routes,
  stations: ReadonlyArray<Station> = STATIONS,
): Readonly<{ sections: ReadonlyArray<Section>; unplaced: ReadonlyArray<string>; band: Band | null }> {
  const at = (name: string | null): Station | undefined => (name === null ? undefined : stations.find((s) => s.name === name));
  const served = new Set(routes.routes.map((r) => sectionKey(r.from, r.to)));
  const exits = routes.routes.flatMap((r) => {
    const a = at(r.from);
    return r.to === null && a !== undefined ? [{ station: a, branches: exitBranches(r) }] : [];
  });
  const { fans, band } = layFans(exits, stations);
  const fanAt = new Map(fans.map((f) => [f.station, f] as const));
  const placed = routes.routes.map((route): Section | string => {
    const key = sectionKey(route.from, route.to);
    const a = at(route.from);
    const b = at(route.to);
    const unplacedEnd = (route.from !== null && a === undefined) || (route.to !== null && b === undefined);
    if (unplacedEnd) return key;
    const fan = a !== undefined && b === undefined ? (fanAt.get(a.name) ?? null) : null;
    const d =
      a !== undefined && b !== undefined
        ? pathOf(octilinear(a, b, stations, served.has(sectionKey(route.to, route.from))))
        : fan !== null
          ? fan.stub
          : pathOf(entryPath(b!));
    return {
      key,
      kind: a !== undefined && b !== undefined ? 'section' : a !== undefined ? 'exit' : 'entry',
      from: route.from,
      to: route.to,
      line: lineOf(a, b),
      declared: route.declared,
      route,
      d,
      walked: walk(pathPoints(d)),
      fan,
    };
  });
  return {
    sections: placed.filter((p): p is Section => typeof p !== 'string'),
    unplaced: placed.filter((p): p is string => typeof p === 'string'),
    band,
  };
}

/** The lines the served routes draw, in the key's order — the key names
 *  only what is on the map. */
export function linesOf(sections: ReadonlyArray<Section>): ReadonlyArray<TransitLine> {
  const drawn = new Set(sections.map((s) => s.line));
  return (Object.keys(LINE_TOKEN) as TransitLine[]).filter((l) => drawn.has(l));
}

// ---------------------------------------------------------------------
// The one lookup the moves (car M2) travel by.
// ---------------------------------------------------------------------

/** A drawn route's path: `d` for a stroke, `walked` for anything that
 *  travels it. */
export type DrawnPath = Readonly<{ key: string; kind: Section['kind']; d: string; walked: Walked }>;

/** THE PATH THE MOVE (from, to) TRAVELS ON THE MAP — a section between
 *  two stations, an exit (`to` null), an entry (`from` null) — or null
 *  where the server serves no such route, or serves it to a region this
 *  layout has no station for. Laid out exactly as RouteLayer draws it:
 *  the same routes give the same path, lanes and arcs included.
 *
 *  AN EXIT TRAVELS ITS TERMINAL'S BRANCH (design f0313eda E2): with the
 *  terminal the move left by (and its kind, which finds the branch an
 *  undeclared terminal stands on apart), the path is the stub and that
 *  branch down to its buffer stop. With none — a move whose terminal
 *  was not read, or one the fan does not draw — it is the stub alone,
 *  to the junction: never a branch the packet did not take. */
export function routePath(
  routes: Routes,
  from: string | null,
  to: string | null,
  terminal: string | null = null,
  kind: string | null = null,
): DrawnPath | null {
  const key = sectionKey(from, to);
  const s = sectionsOf(routes).sections.find((x) => x.key === key);
  if (s === undefined) return null;
  const branch = branchOf(s, terminal, kind);
  return branch === null
    ? { key: s.key, kind: s.kind, d: s.d, walked: s.walked }
    : { key: s.key, kind: s.kind, d: branch.way, walked: branch.walked };
}

/** The fan branch an exit by `terminal` (of `kind`) runs down, or null. */
export function branchOf(s: Section, terminal: string | null, kind: string | null = null): FanBranch | null {
  if (s.fan === null || terminal === null) return null;
  const by = (name: string): FanBranch | undefined => s.fan!.branches.find((b) => b.name === name);
  return (kind !== null ? by(`${terminal} · ${kind}`) : undefined) ?? by(terminal) ?? null;
}
