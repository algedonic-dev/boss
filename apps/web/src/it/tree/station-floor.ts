// THE STATION FLOOR — what opening a station shows under its header
// (backlog dd0eb058, car B of David's station-floor visuals, decided with
// him 2026-11-19 on the prototype canvas, artboards Plan and BuildFloor).
// It replaced the page body that stood here: the parts card grid, the
// waits ladder, the pool strips and the station line.
//
// DAVID'S DECISIONS, each one a rule below:
//
//   One edge in    every station is a system with ONE admission edge: the
//                  station's own Admission machine at its boundary. Each
//                  child's waiting area is its own admission queue.
//   No Out         a job that leaves goes straight into the next
//                  station's admission. OUTBOUND is a RECORD of what left
//                  and where it went, never a place to wait, and it names
//                  the next station at the SAME TIER (Engineering ->
//                  Operations), never a part inside it (`peerOf`).
//   Fixed places   stations sit in 2D at fixed positions so operators
//                  learn the floor: a row's own `floor` placement when it
//                  declares one (backlog e3a927c3), else the fallback
//                  grid, laid along the declared routes (`layoutFloor`).
//   Tokens         a job is a token that moves only when its record moves:
//                  the head of each queue (the first few in the station's
//                  own order, priority then age) as tokens, the rest as a
//                  pile in four age bands whose thickness grows with
//                  depth, and the open claims in the working slots.
//   Stillness      every station and token says how long it has been
//                  still, and turns orange past its bound (`STILL_BOUND`).
//   Now only       current activity, no replay: the moves are the live
//                  events stream's, this page session only.
//
// THE READS, EACH SAID WHEN IT FAILS: the tree (GET /api/stations?under=),
// the load, each drawn part's queue (GET /api/stations/{name}/queue), the
// open claims, the flow (GET /api/stations/flow) and the live events
// stream (GET /api/events/stream).
//
// WHAT NO READ STATES IS NOT DRAWN AS A NUMBER. The flow read counts
// arrivals and departures PER STATION over its window; no read counts a
// crossing per ROUTE between two stations (GET /api/yard/routes counts
// them, keyed by the retired map's regions, which no station row maps
// onto). So a route carries no count of its own: it is drawn from the
// rows' declared `edges`, DASHED when its source recorded no departure
// in the window — nothing can have crossed it — and each station says its
// own in and out per day. A bound is no row's yet (no read carries a
// step's budget), so the page's own bound is stated in the legend.
//
// Pure functions of the reads, pinned in station-floor.test.ts;
// StationFloor.svelte owns the strokes, the stream and the timing.

import { readFailed, type Remote } from '../../data/remote';
import type { StationFlowEnvelope, StationLoadRow } from '../marshalling/marshalling';
import { ageWords } from './station-answer';
import { HOLDER_MARK, stationHref, type HeldClaim, type PageClaims } from './station-page';
import { jobHref, seatsOf, subtree, type StationTree, type TreeNode } from './station-tree';

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj | null => (v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Obj) : null);
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const DAY = 86_400_000;

// ---------------------------------------------------------------------
// The queue read.
// ---------------------------------------------------------------------

/** One member of a station's queue, as much as a token reads of it. */
export type Member = Readonly<{
  id: string;
  title: string;
  priority: string | null;
  /** When the packet opened: its age, not its time in this queue. */
  openedAt: string | null;
  /** The packet's last recorded move: its newest step completion when
   *  the read carries its steps, else when it opened. */
  movedAt: string | null;
}>;

export type QueueRead = Readonly<{ total: number; members: ReadonlyArray<Member> }>;

const at = (s: string | null): number | null => {
  if (s === null) return null;
  const t = Date.parse(s);
  return Number.isFinite(t) ? t : null;
};
const latest = (xs: ReadonlyArray<string | null>): string | null =>
  xs.reduce<string | null>((a, b) => ((at(b) ?? -Infinity) > (at(a) ?? -Infinity) ? b : a), null);

/** GET /api/stations/{name}/queue (station_queue.rs `StationQueue`): the
 *  members in the station's own discipline order. A wrong shape throws:
 *  an empty list here would draw a quiet station. */
export function parseQueue(raw: unknown): QueueRead {
  const o = obj(raw);
  if (o === null || !Array.isArray(o.data)) throw new Error('queue read: expected {data: [...]}');
  const steps = obj(o.steps) ?? {};
  const members = o.data.flatMap((j): Member[] => {
    const row = obj(j);
    const id = str(row?.id);
    if (row === null || id === null) return [];
    const openedAt = str(row.opened_at) ?? (str(row.opened_on) === null ? null : `${str(row.opened_on)!.slice(0, 10)}T00:00:00Z`);
    const done = (Array.isArray(steps[id]) ? (steps[id] as unknown[]) : []).map((s) => str(obj(s)?.completed_at));
    return [{ id, title: str(row.title) ?? id, priority: str(row.priority), openedAt, movedAt: latest([openedAt, ...done]) }];
  });
  return { total: typeof o.total === 'number' ? o.total : members.length, members };
}

// ---------------------------------------------------------------------
// Head, pile and stillness.
// ---------------------------------------------------------------------

/** How many of a queue's first members are drawn as tokens. */
export const HEAD_MAX = 4;
/** How many working slots a station draws before it counts the rest. */
export const SLOTS_MAX = 6;

/** The pile's four age bands (David, on the canvas): under a day, one to
 *  seven days, eight to thirty, over thirty. Ages are the packet's. */
export const AGE_BANDS = ['under a day', '1 to 7 days', '8 to 30 days', 'over 30 days'] as const;

export type Pile = Readonly<{
  /** Members per band, in AGE_BANDS order. */
  bands: readonly [number, number, number, number];
  /** Members whose age the read does not carry. */
  unaged: number;
  total: number;
  /** The pile's thickness in px: grows with depth, bounded. */
  thickness: number;
  words: string;
}>;

/** Thicker with depth, on a log scale so a pile of 900 still fits. */
export const pileThickness = (n: number): number => (n <= 0 ? 0 : Math.max(8, Math.min(40, Math.round(6 * Math.log2(n + 1)))));

export function pileOf(rest: ReadonlyArray<Member>, now: number): Pile | null {
  if (rest.length === 0) return null;
  const bands: [number, number, number, number] = [0, 0, 0, 0];
  let unaged = 0;
  for (const m of rest) {
    const t = at(m.openedAt);
    if (t === null) { unaged += 1; continue; }
    const days = (now - t) / DAY;
    bands[days < 1 ? 0 : days < 8 ? 1 : days <= 30 ? 2 : 3] += 1;
  }
  const said = [bands[3] > 0 ? `${bands[3]} over 30 days` : null, bands[2] > 0 ? `${bands[2]} at 8 to 30 days` : null, unaged > 0 ? `${unaged} of unread age` : null]
    .filter((s): s is string => s !== null);
  return {
    bands,
    unaged,
    total: rest.length,
    thickness: pileThickness(rest.length),
    words: `${rest.length} behind the head${said.length === 0 ? '' : `: ${said.join(', ')}`}`,
  };
}

/** Past this long still, a token or a station turns orange. No station
 *  row declares a bound yet, so these are the page's, stated in its
 *  legend: a packet waiting past a week has reached the pile's orange
 *  bands; a claim held, or a station holding work, unmoved past a day. */
export const STILL_BOUND = { waiting: 7 * DAY, working: DAY, station: DAY } as const;

export type Still = Readonly<{ ms: number | null; words: string; past: boolean }>;

export function stillOf(since: string | null, now: number, bound: number): Still {
  const t = at(since);
  if (t === null) return { ms: null, words: 'still: not recorded', past: false };
  const ms = Math.max(0, now - t);
  return { ms, words: ms < 60_000 ? 'just moved' : `still ${ageWords(ms / DAY).replace(' ', '')}`, past: ms > bound };
}

// ---------------------------------------------------------------------
// The live events stream.
// ---------------------------------------------------------------------

/** The kinds that move a token: a claim, a completion, a release, a
 *  status change. Every other audit row is not motion on this floor. */
const MOTION = ['jobs.station.claimed', 'jobs.step.completed', 'jobs.step.released', 'jobs.job.status_changed'] as const;

export type FloorEvent = Readonly<{
  id: string;
  kind: string;
  at: string;
  job: string | null;
  /** The station the record names, when it names one. */
  station: string | null;
  /** A status change that closed the packet. */
  closed: boolean;
}>;

function receiptOf(p: Obj): Obj | null {
  const r = p.receipt_json;
  if (typeof r === 'string') {
    try { return obj(JSON.parse(r)); } catch { return null; }
  }
  return obj(r);
}

/** One frame of GET /api/events/stream (tail_http.rs `AuditEntry`), or
 *  null for one that is not a whole motion event — half an event never
 *  moves a token. A claim through the per-step door is a step update
 *  carrying `_station` (events.rs CLAIM_STATION_KEY). */
export function parseFloorEvent(data: string): FloorEvent | null {
  let o: Obj | null;
  try { o = obj(JSON.parse(data)); } catch { return null; }
  const kind = str(o?.kind);
  const id = str(o?.event_id);
  const when = str(o?.timestamp);
  const p = obj(o?.payload);
  if (o === null || kind === null || id === null || when === null || p === null) return null;
  const stepClaim = kind === 'jobs.step.updated' && str(p._station) !== null;
  if (!MOTION.some((k) => k === kind) && !stepClaim) return null;
  const receipt = receiptOf(p);
  const job = str(p.job_id) ?? str(obj(receipt?.selected)?.job_id) ?? (kind === 'jobs.job.status_changed' ? str(p.id) : null);
  const station = str(p._station) ?? str(p.station) ?? str(obj(p.station)?.name) ?? str(obj(receipt?.station)?.name);
  return { id, kind, at: when, job, station, closed: kind === 'jobs.job.status_changed' && p.new_status === 'closed' };
}

export type Feed = 'connecting' | 'live' | 'down';

// ---------------------------------------------------------------------
// Peers and the Outbound record.
// ---------------------------------------------------------------------

const parentOf = (tree: StationTree, n: TreeNode): TreeNode | null =>
  n.parent === null ? null : (tree.nodes.find((x) => x.id === n.parent) ?? null);

/** The station `other` stands in at `name`'s own tier: the child of the
 *  nearest common ancestor that holds `other` — Engineering -> Operations,
 *  never Converge inside it. Null when `other` is inside `name`, or the
 *  read does not hold it. */
export function peerOf(tree: StationTree, name: string, other: string): string | null {
  if (!tree.byName.has(other) || subtree(tree, name).has(other)) return null;
  let at: TreeNode | null = tree.byName.get(name) ?? null;
  for (let i = 0; at !== null && i <= tree.nodes.length; i += 1) {
    const up = parentOf(tree, at);
    if (up === null) return null;
    if (subtree(tree, up.name).has(other)) {
      return up.children.find((c) => subtree(tree, c).has(other)) ?? null;
    }
    at = up;
  }
  return null;
}

/** How many rows the Outbound record keeps. */
export const OUTBOUND_MAX = 8;

export type OutboundRow = Readonly<{ key: string; at: string; title: string; href: string; to: string | null; words: string }>;

/** The record with `e` admitted when it says a packet on this floor left:
 *  claimed at a station outside it (named by its peer), or closed. Newest
 *  first, each event once, bounded. Nothing else is a departure. */
export function admitOutbound(
  list: ReadonlyArray<OutboundRow>,
  e: FloorEvent,
  tree: StationTree,
  name: string,
  onFloor: ReadonlyMap<string, string>,
): ReadonlyArray<OutboundRow> {
  if (e.job === null || !onFloor.has(e.job) || list.some((r) => r.key === e.id)) return list;
  const title = onFloor.get(e.job)!;
  const peer = e.station === null ? null : peerOf(tree, name, e.station);
  if (peer === null && !e.closed) return list;
  const row: OutboundRow = {
    key: e.id,
    at: e.at,
    title,
    href: jobHref(e.job),
    to: peer,
    words: peer === null ? 'closed: left the floor' : `sent to ${peer}`,
  };
  return [row, ...list].slice(0, OUTBOUND_MAX);
}

// ---------------------------------------------------------------------
// The layout.
// ---------------------------------------------------------------------

export type Box = Readonly<{ x: number; y: number; w: number; h: number }>;

/** The fallback grid: one box per part, laid in columns along the
 *  declared routes. */
export const GRID = { w: 232, h: 268, gapX: 72, gapY: 32 } as const;
const PAD = 24;
const ADMISSION_W = 150;
const OUTBOUND_W = 200;
const PEER = { w: 150, h: 84 } as const;

/** The parts a floor draws: the station's children, or the station itself
 *  when it has none — a leaf's floor is its own admission, its working
 *  slots and its Outbound record. */
export function partsOf(tree: StationTree, name: string): ReadonlyArray<string> {
  const kids = (tree.byName.get(name)?.children ?? []).filter((c) => tree.byName.has(c));
  return kids.length === 0 ? [name] : kids;
}

/** The drawn part a station stands in, or null outside this floor. */
function partOfFn(tree: StationTree, parts: ReadonlyArray<string>): (s: string) => string | null {
  const trees = parts.map((p) => [p, subtree(tree, p)] as const);
  return (s) => trees.find(([, t]) => t.has(s))?.[0] ?? null;
}

/** Every declared route on this floor, between drawn parts or out of it,
 *  each once: a row's `edges`, read off every station inside. */
export function declaredRoutes(tree: StationTree, name: string): ReadonlyArray<Readonly<{ from: string; to: string | null; target: string }>> {
  const parts = partsOf(tree, name);
  const partOf = partOfFn(tree, parts);
  const seen = new Set<string>();
  return tree.nodes
    .filter((n) => subtree(tree, name).has(n.name))
    .flatMap((n) => n.edges.map((e) => [partOf(n.name), e] as const))
    .flatMap(([from, target]) => {
      if (from === null) return [];
      const inside = partOf(target);
      const to = inside ?? peerOf(tree, name, target);
      if (to === from || (inside === null && to === null)) return [];
      const key = `${from}>${inside === null ? `out:${to}` : to}`;
      if (seen.has(key)) return [];
      seen.add(key);
      return [{ from, to: inside, target: inside ?? to! }];
    });
}

/** Column of each part: the longest declared path to it from a part no
 *  sibling routes into, bounded so a cycle cannot loop. */
function columns(parts: ReadonlyArray<string>, routes: ReadonlyArray<Readonly<{ from: string; to: string | null }>>): ReadonlyMap<string, number> {
  const col = new Map<string, number>(parts.map((p) => [p, 0]));
  for (let i = 0; i < parts.length; i += 1) {
    for (const r of routes) {
      if (r.to === null) continue;
      const next = (col.get(r.from) ?? 0) + 1;
      if (next > (col.get(r.to) ?? 0) && next < parts.length) col.set(r.to, next);
    }
  }
  return col;
}

export type Layout = Readonly<{
  width: number;
  height: number;
  admission: Box;
  outbound: Box;
  parts: ReadonlyMap<string, Box & Readonly<{ placed: 'declared' | 'grid' }>>;
  peers: ReadonlyMap<string, Box>;
}>;

/** Where everything stands. A part's own `placement` box is honoured as
 *  declared (in its parent floor's units, backlog e3a927c3); every other
 *  part takes the fallback grid below them.
 *  The admission machine stands left of the parts, the Outbound record
 *  right of them, and the peers it names right of that. */
export function layoutFloor(tree: StationTree, name: string): Layout {
  const parts = partsOf(tree, name);
  const routes = declaredRoutes(tree, name);
  const col = columns(parts, routes);
  const cell = (c: number, r: number): Box => ({ x: c * (GRID.w + GRID.gapX), y: r * (GRID.h + GRID.gapY), w: GRID.w, h: GRID.h });
  const declared = parts.flatMap((p) => {
    const f = tree.byName.get(p)?.placement;
    return f === undefined || f === null ? [] : [[p, { x: f.x, y: f.y, w: f.w, h: f.h }] as const];
  });
  const taken = new Set(declared.map(([p]) => p));
  const below = declared.length === 0 ? 0 : Math.max(...declared.map(([, b]) => b.y + b.h)) + GRID.gapY;
  const rows = new Map<number, number>();
  const grid = parts.filter((p) => !taken.has(p)).map((p) => {
    const c = col.get(p) ?? 0;
    const r = rows.get(c) ?? 0;
    rows.set(c, r + 1);
    const b = cell(c, r);
    return [p, { ...b, y: b.y + below }] as const;
  });
  const all = [...declared.map(([p, b]) => [p, { ...b, placed: 'declared' as const }] as const), ...grid.map(([p, b]) => [p, { ...b, placed: 'grid' as const }] as const)];
  const minX = Math.min(...all.map(([, b]) => b.x));
  const minY = Math.min(...all.map(([, b]) => b.y));
  const left = PAD + ADMISSION_W + GRID.gapX;
  const top = PAD + 28;
  const placed = new Map(all.map(([p, b]) => [p, { ...b, x: b.x - minX + left, y: b.y - minY + top }] as const));
  const right = Math.max(...[...placed.values()].map((b) => b.x + b.w));
  const bottom = Math.max(...[...placed.values()].map((b) => b.y + b.h));
  const innerH = bottom - top;
  const admission: Box = { x: PAD, y: top, w: ADMISSION_W, h: Math.max(innerH, GRID.h) };
  const outbound: Box = { x: right + GRID.gapX, y: top, w: OUTBOUND_W, h: Math.max(innerH, GRID.h) };
  const peerNames = [...new Set(routes.filter((r) => r.to === null).map((r) => r.target))];
  const peers = new Map(peerNames.map((p, i) => [p, { x: outbound.x + outbound.w + GRID.gapX, y: top + i * (PEER.h + GRID.gapY), w: PEER.w, h: PEER.h }] as const));
  const peerBottom = peerNames.length === 0 ? 0 : top + peerNames.length * (PEER.h + GRID.gapY);
  return {
    width: (peerNames.length === 0 ? outbound.x + outbound.w : outbound.x + outbound.w + GRID.gapX + PEER.w) + PAD,
    height: Math.max(top + admission.h, peerBottom) + PAD,
    admission,
    outbound,
    parts: placed,
    peers,
  };
}

// ---------------------------------------------------------------------
// The routes.
// ---------------------------------------------------------------------

export type RouteLine = Readonly<{
  key: string;
  d: string;
  /** Nothing can have crossed it in the window: its source recorded no
   *  departure. */
  dashed: boolean;
  /** The flow read did not answer for its source, so it is not judged. */
  unread: boolean;
  /** The stream moved a packet at its source just now, this session. */
  hot: boolean;
  title: string;
  label: string | null;
  lx: number;
  ly: number;
}>;

const ctr = (b: Box) => ({ x: b.x + b.w / 2, y: b.y + b.h / 2 });
/** Where the line from `from` toward `b`'s centre meets `b`'s edge. */
function clip(b: Box, toward: Readonly<{ x: number; y: number }>): Readonly<{ x: number; y: number }> {
  const c = ctr(b);
  const dx = toward.x - c.x;
  const dy = toward.y - c.y;
  const kx = dx === 0 ? Infinity : (b.w / 2 + 6) / Math.abs(dx);
  const ky = dy === 0 ? Infinity : (b.h / 2 + 6) / Math.abs(dy);
  const k = Math.min(kx, ky, 1);
  return { x: c.x + dx * k, y: c.y + dy * k };
}
const r1 = (n: number): number => Math.round(n * 10) / 10;

function curve(a: Box, b: Box, bend: number) {
  const p1 = clip(a, ctr(b));
  const p2 = clip(b, ctr(a));
  const mx = (p1.x + p2.x) / 2;
  const my = (p1.y + p2.y) / 2;
  const len = Math.hypot(p2.x - p1.x, p2.y - p1.y) || 1;
  const cx = mx - ((p2.y - p1.y) / len) * bend;
  const cy = my + ((p2.x - p1.x) / len) * bend;
  return { d: `M${r1(p1.x)} ${r1(p1.y)} Q${r1(cx)} ${r1(cy)} ${r1(p2.x)} ${r1(p2.y)}`, lx: r1((mx + cx) / 2), ly: r1((my + cy) / 2) };
}

export type FlowOf = (station: string) => Readonly<{ arrived: number; departed: number }> | Readonly<{ unread: string }>;

/** The flow read as the routes judge it: a station's arrivals and
 *  departures in the window, or why they are unread. */
export function flowJudge(flow: Remote<StationFlowEnvelope>): FlowOf {
  return (station) => {
    if (flow.kind !== 'ready') return { unread: flow.kind === 'loading' ? 'reading the flow' : `the flow read ${readFailed(flow)}: ${flow.error}` };
    const row = flow.data.rows.find((r) => r.station === station);
    if (row === undefined) return { unread: `the flow read holds no row for ${station}` };
    return row.departed === null || row.arrived === null
      ? { unread: row.unavailableReason ?? `${station}'s flow is not counted` }
      : { arrived: row.arrived, departed: row.departed };
  };
}

/** Every route drawn: the admission machine into each part no sibling
 *  routes into, each declared route between parts, each route out to the
 *  Outbound record, and the record to the peer it names. */
export function routeLines(
  tree: StationTree,
  name: string,
  layout: Layout,
  flowOf: FlowOf,
  windowHours: number | null,
  hot: ReadonlySet<string>,
): ReadonlyArray<RouteLine> {
  const parts = partsOf(tree, name);
  const routes = declaredRoutes(tree, name);
  const routed = new Set(routes.flatMap((r) => (r.to === null ? [] : [r.to])));
  const entries = parts.filter((p) => !routed.has(p));
  const window = windowHours === null ? 'the window' : windowHours === 24 ? 'the last day' : `the last ${windowHours} h`;
  // A route is judged by its source's departures — or, out of the
  // admission machine, by the station's own arrivals: zero in the window
  // means nothing can have crossed it.
  const judged = (from: string, side: 'arrived' | 'departed', bend: number, a: Box, b: Box, key: string, hotKey: string): RouteLine => {
    const f = flowOf(from);
    const c = curve(a, b, bend);
    const unread = 'unread' in f;
    const n = unread ? null : f[side];
    const what = side === 'arrived' ? 'arrival' : 'departure';
    return {
      key, d: c.d, dashed: n === 0, unread, hot: hot.has(hotKey), lx: c.lx, ly: c.ly, label: null,
      title: unread ? `not judged: ${f.unread}` : n === 0 ? `${from} recorded no ${what} in ${window}, so nothing crossed here` : `${from} recorded ${n} ${what}${n === 1 ? '' : 's'} in ${window}`,
    };
  };
  const pairs = new Set(routes.flatMap((r) => (r.to === null ? [] : [`${r.from}>${r.to}`])));
  const own = layout.admission;
  const intoParts = (entries.length === 0 ? parts.slice(0, 1) : entries).flatMap((p) => {
    const b = layout.parts.get(p);
    return b === undefined ? [] : [judged(name, 'arrived', 0, own, b, `admission>${p}`, p)];
  });
  const between = routes.flatMap((r) => {
    const a = layout.parts.get(r.from);
    if (a === undefined) return [];
    if (r.to === null) return [judged(r.from, 'departed', 0, a, layout.outbound, `${r.from}>outbound:${r.target}`, r.from)];
    const b = layout.parts.get(r.to);
    // A pair routed both ways bends apart, so neither hides the other.
    return b === undefined ? [] : [judged(r.from, 'departed', pairs.has(`${r.to}>${r.from}`) ? 28 : 0, a, b, `${r.from}>${r.to}`, r.from)];
  });
  const out = [...layout.peers.entries()].map(([peer, b]): RouteLine => {
    const c = curve(layout.outbound, b, 0);
    return { key: `outbound>${peer}`, d: c.d, dashed: true, unread: false, hot: hot.has(`out:${peer}`), lx: c.lx, ly: c.ly, label: `to ${peer}`, title: `what leaves for ${peer} enters its admission` };
  });
  return [...intoParts, ...between, ...out];
}

// ---------------------------------------------------------------------
// The tokens and the stations.
// ---------------------------------------------------------------------

export type Token = Readonly<{
  /** The job: one token per packet, so a packet glides between places. */
  key: string;
  label: string;
  title: string;
  href: string;
  place: 'waiting' | 'working';
  meta: string;
  still: Still;
  moving: boolean;
  x: number;
  y: number;
}>;

export type QueueState =
  | Readonly<{ kind: 'loading' }>
  | Readonly<{ kind: 'unread'; why: string }>
  | Readonly<{ kind: 'empty' }>
  | Readonly<{ kind: 'ready'; total: number }>;

export type FloorPart = Readonly<{
  name: string;
  title: string;
  href: string;
  box: Box & Readonly<{ placed: 'declared' | 'grid' }>;
  /** The part is the station itself: a leaf's floor. */
  self: boolean;
  equipment: boolean;
  queue: QueueState;
  pile: Pile | null;
  pileTop: number;
  slots: ReadonlyArray<Readonly<{ x: number; y: number }>>;
  /** Working claims past the drawn slots. */
  moreWorking: number;
  /** In and out per day, from the flow read, or why not. */
  flow: string;
  still: Still;
  /** It holds work: waiting or working. A quiet empty station is not a
   *  stillness signal. */
  holding: boolean;
  moving: boolean;
}>;

export type FloorReads = Readonly<{
  load: Remote<ReadonlyMap<string, StationLoadRow>>;
  claims: Remote<PageClaims>;
  flow: Remote<StationFlowEnvelope>;
  /** Each drawn part's queue read; absent: not asked (its load is empty). */
  queues: ReadonlyMap<string, Remote<QueueRead>>;
  /** When the stream last moved each job and station, this session. */
  movedJobs: ReadonlyMap<string, number>;
  movedStations: ReadonlyMap<string, number>;
}>;

/** How long a moved token or station stays marked as moving. */
export const MOVING_MS = 4_000;
const TOKEN = { w: 104, h: 24, gap: 6 } as const;
const HEAD_TOP = 64;

export type FloorModel = Readonly<{
  layout: Layout;
  parts: ReadonlyArray<FloorPart>;
  tokens: ReadonlyArray<Token>;
  routes: ReadonlyArray<RouteLine>;
  admission: Readonly<{ words: string; still: Still }>;
  peers: ReadonlyArray<Readonly<{ name: string; title: string; href: string; box: Box }>>;
  /** Job id → title, for every packet on this floor: what the Outbound
   *  record names when one leaves. */
  onFloor: ReadonlyMap<string, string>;
  /** What the claims read cannot place: said beside the floor. */
  notes: ReadonlyArray<string>;
}>;

const shortHolder = (h: string | null): string => (h === null ? 'holder not recorded' : h.replace(/^(automation|agent):/, ''));

/** The parts' places per row of tokens: two across a box wide enough. */
const across = (b: Box): number => (b.w >= TOKEN.w * 2 + 3 * 8 ? 2 : 1);

/** What the floor draws, from the reads. Pure: `now` is passed in. */
export function floorModel(tree: StationTree, name: string, reads: FloorReads, now: number): FloorModel {
  const layout = layoutFloor(tree, name);
  const parts = partsOf(tree, name);
  const self = parts.length === 1 && parts[0] === name;
  const flowOf = flowJudge(reads.flow);
  const claims = reads.claims.kind === 'ready' && !reads.claims.data.card.sourceUnavailable ? reads.claims.data.held : null;
  const partOf = partOfFn(tree, parts);
  const recent = (t: number | undefined): boolean => t !== undefined && now - t < MOVING_MS;
  const titles = new Map<string, string>();
  for (const q of reads.queues.values()) if (q.kind === 'ready') for (const m of q.data.members) if (!titles.has(m.id)) titles.set(m.id, m.title);

  // Working first: a packet held anywhere is drawn where it is held, once.
  const drawn = new Set<string>();
  const tokens: Token[] = [];
  const working = new Map<string, HeldClaim[]>();
  for (const c of claims ?? []) {
    const p = c.station === null ? null : partOf(c.station);
    if (p === null || c.jobId === null) continue;
    working.set(p, [...(working.get(p) ?? []), c]);
  }
  const heldHere = new Set([...working.values()].flat().map((c) => c.jobId!));

  const floorParts = parts.flatMap((p): FloorPart[] => {
    const box = layout.parts.get(p);
    const node = tree.byName.get(p);
    if (box === undefined || node === undefined) return [];
    const inside = subtree(tree, p);
    const held = working.get(p) ?? [];
    const seats = seatsOf(node);
    const slotCount = Math.min(SLOTS_MAX, Math.max(held.length, seats ?? 0));
    const cols = across(box);
    const slotRows = Math.ceil(slotCount / cols);
    const slots = Array.from({ length: slotCount }, (_, i) => ({
      x: box.x + 8 + (i % cols) * (TOKEN.w + 8),
      y: box.y + box.h - 28 - (slotRows - 1 - Math.floor(i / cols)) * (TOKEN.h + TOKEN.gap) - 4,
    }));
    held.slice(0, slotCount).forEach((c, i) => {
      if (drawn.has(c.jobId!)) return;
      drawn.add(c.jobId!);
      const slot = slots[i]!;
      tokens.push({
        key: c.jobId!,
        label: titles.get(c.jobId!) ?? c.workflowKind ?? 'a packet',
        title: `${titles.get(c.jobId!) ?? c.workflowKind ?? 'a packet'} — held by ${c.holder ?? 'a holder not recorded'} at ${c.station}`,
        href: jobHref(c.jobId!),
        place: 'working',
        meta: `${c.holderKind === null ? '?' : HOLDER_MARK[c.holderKind]} ${shortHolder(c.holder)}`,
        still: stillOf(c.since, now, STILL_BOUND.working),
        moving: recent(reads.movedJobs.get(c.jobId!)),
        x: slot.x, y: slot.y,
      });
    });

    const load = reads.load.kind === 'ready' ? reads.load.data.get(p) : undefined;
    const q = reads.queues.get(p);
    const queue: QueueState =
      q === undefined ? (reads.load.kind === 'ready' ? { kind: 'empty' } : reads.load.kind === 'loading' ? { kind: 'loading' } : { kind: 'unread', why: `the load read ${readFailed(reads.load)}: ${reads.load.error}` })
      : q.kind === 'loading' ? { kind: 'loading' }
      : q.kind === 'failed' ? { kind: 'unread', why: `the queue read ${readFailed(q)}: ${q.error}` }
      : q.data.members.length === 0 ? { kind: 'empty' } : { kind: 'ready', total: q.data.total };
    // A packet held anywhere on this floor is drawn where it is held, so
    // it waits in no queue's head or pile.
    const members = q?.kind === 'ready' ? q.data.members.filter((m) => !drawn.has(m.id) && !heldHere.has(m.id)) : [];
    const head = members.slice(0, HEAD_MAX);
    head.forEach((m, i) => {
      drawn.add(m.id);
      const still = stillOf(m.movedAt, now, STILL_BOUND.waiting);
      tokens.push({
        key: m.id, label: m.title, title: `${m.title} — waiting at ${p}${m.priority === null ? '' : `, ${m.priority}`}`,
        href: jobHref(m.id), place: 'waiting',
        meta: still.ms === null ? '' : still.ms < 60_000 ? 'arrived' : ageWords(still.ms / DAY).replace(' ', ''),
        still, moving: recent(reads.movedJobs.get(m.id)),
        x: box.x + 8 + (i % cols) * (TOKEN.w + 8), y: box.y + HEAD_TOP + Math.floor(i / cols) * (TOKEN.h + TOKEN.gap),
      });
    });
    const pile = pileOf(members.slice(HEAD_MAX), now);
    const headRows = Math.ceil(head.length / cols);

    // Stillness: the newest recorded move of anything here — a claim
    // taken, a packet's step done or its opening, or the stream this
    // session.
    const streamAt = [...reads.movedStations.entries()].filter(([s]) => inside.has(s)).map(([, t]) => t);
    const lastAt = Math.max(
      ...held.map((c) => at(c.since) ?? -Infinity),
      ...(q?.kind === 'ready' ? q.data.members.map((m) => at(m.movedAt) ?? -Infinity) : []),
      ...streamAt,
      -Infinity,
    );
    const still = lastAt === -Infinity ? { ms: null, words: 'no move recorded', past: false } : stillOf(new Date(lastAt).toISOString(), now, STILL_BOUND.station);
    const holding = held.length > 0 || (load?.depth ?? 0) > 0 || members.length > 0;
    const f = flowOf(p);
    const fr = reads.flow.kind === 'ready' ? reads.flow.data.rows.find((r) => r.station === p) : undefined;
    const perDay = (n: number) => (reads.flow.kind === 'ready' && reads.flow.data.windowHours !== null && reads.flow.data.windowHours !== 24 ? `${n} in ${reads.flow.data.windowHours} h` : `${n}/d`);
    return [{
      name: p,
      title: node.title,
      href: stationHref(p),
      box,
      self,
      equipment: node.kind === 'equipment',
      queue,
      pile,
      pileTop: box.y + HEAD_TOP + headRows * (TOKEN.h + TOKEN.gap) + 4,
      slots,
      moreWorking: Math.max(0, held.length - slotCount),
      flow: 'unread' in f ? 'flow unread' : `in ${perDay(fr?.arrived ?? 0)} · out ${perDay(f.departed)}`,
      still,
      holding,
      moving: streamAt.some((t) => recent(t)),
    }];
  });

  const hot = new Set([...reads.movedStations.entries()].filter(([, t]) => recent(t)).flatMap(([s]) => {
    const p = partOf(s);
    return p === null ? [] : [p];
  }));
  const routes = routeLines(tree, name, layout, flowOf, reads.flow.kind === 'ready' ? reads.flow.data.windowHours : null, hot);

  const own = reads.load.kind === 'ready' ? reads.load.data.get(name) : undefined;
  const admissionWords = reads.load.kind === 'loading' ? 'reading the load'
    : reads.load.kind === 'failed' ? `the load read ${readFailed(reads.load)}: ${reads.load.error}`
    : own === undefined ? 'one edge in: every job enters here' : `${own.depth} admitted · one edge in`;
  const ownAt = [...reads.movedStations.entries()].filter(([s]) => s === name).map(([, t]) => t);
  const admissionStill: Still = ownAt.length === 0 ? { ms: null, words: 'no move seen this session', past: false } : stillOf(new Date(Math.max(...ownAt)).toISOString(), now, STILL_BOUND.station);

  const placeless = claims === null ? 0 : claims.filter((c) => c.station === null).length;
  // A claim held at the station itself stands in none of its parts, so it
  // fills no slot on this floor; it is counted here rather than placed.
  const atSelf = self || claims === null ? 0 : claims.filter((c) => c.station === name).length;
  const notes = [
    atSelf > 0 ? `${atSelf} open claim${atSelf === 1 ? '' : 's'} held at ${name} itself, in none of its parts.` : null,
    reads.claims.kind === 'failed' ? `The claims read ${readFailed(reads.claims)}, so no working slot is filled: ${reads.claims.error}` : null,
    reads.claims.kind === 'ready' && reads.claims.data.card.sourceUnavailable ? 'The claims read says its source could not be read, so no working slot is filled.' : null,
    placeless > 0 ? `${placeless} open claim${placeless === 1 ? '' : 's'} the record cannot place at any station.` : null,
    reads.flow.kind === 'failed' ? `The flow read ${readFailed(reads.flow)}, so no route is judged: ${reads.flow.error}` : null,
  ].filter((s): s is string => s !== null);

  const onFloor = new Map<string, string>();
  for (const t of tokens) onFloor.set(t.key, t.label);
  for (const [id, title] of titles) if (!onFloor.has(id)) onFloor.set(id, title);

  return {
    layout,
    parts: floorParts,
    tokens,
    routes,
    admission: { words: admissionWords, still: admissionStill },
    peers: [...layout.peers.entries()].map(([p, box]) => ({ name: p, title: tree.byName.get(p)?.title ?? p, href: stationHref(p), box })),
    onFloor,
    notes,
  };
}

/** The parts whose queue the floor reads: every drawn part the load says
 *  holds a packet, and every part with stations of its own — a tree
 *  station's queue holds the work addressed inside it, which its own load
 *  row does not count (station_queue `tree_addresses`). An empty leaf is
 *  drawn empty without a read; every part is read when the load did not
 *  answer. */
export function queuesToRead(tree: StationTree, name: string, load: Remote<ReadonlyMap<string, StationLoadRow>>): ReadonlyArray<string> {
  const parts = partsOf(tree, name);
  if (load.kind === 'loading') return [];
  if (load.kind === 'failed') return parts;
  return parts.filter((p) => (load.data.get(p)?.depth ?? 0) > 0 || (tree.byName.get(p)?.children.length ?? 0) > 0);
}
