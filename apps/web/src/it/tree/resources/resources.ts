// THE RESOURCES VIEW — a station's pools as a rail of units that fly to
// the stations that claim them and back (backlog 87575d69, car C of
// David's station visuals, 2026-11-19 ~17:30Z: "I think they will help
// anchor our tech debt payoff as we continue converging to the simpler
// system"; chosen on the prototype canvas, artboard Resources).
//
// What the view draws, each from a real, current read:
//
//   The pools   every pool the station allocates: its own allocation and
//               its children's (the root, holding no allocation, draws
//               what its children hold, sized by the registry), plus the
//               robots' rate. A pool counted in executors (unit `slot`,
//               `people`) is a rail of units; any other measure (vCPU) a
//               bar; a rate pool says its rate, and says PLAINLY when no
//               reader has stated one (budget:robots on 2026-11-19).
//   A unit      drawn where GET /api/claims says it is held: inside the
//               child of the station whose subtree holds the claim, or the
//               station itself, or back in the rail when nothing holds
//               it. A unit keeps its identity while its claim holds it
//               (`seatUnits`), so a claim or a return moves exactly the
//               units that moved and nothing else.
//   Equipment   a child with machines draws its declared capacity as
//               seats inside its box, never moving (the gates' three); a
//               seat is the station's own, not a pool's unit. Written in
//               the station tree's words (factory-taxonomy-v2): a station
//               with machines and a capacity, never an equipment kind.
//   Pressure    each station says one of: working (holds units or seats),
//               waiting in the parent's claim line (GET /api/claims/
//               waiting), starved (work waits while units sit idle), not
//               asking (work waits, nothing idle, no request in the line)
//               or quiet. An unread load is said, never read as quiet.
//   The record  the units that moved since the page opened, each stamped
//               with the event that moved it. Current activity only
//               (David, 2026-11-08 00:50Z): no replay.
//
// RESERVED VS BORROWED arrives with car D (30c1efe8). Until a read records
// a reservation every unit is lendable: `Unit.reserved` is null on every
// unit, the shape car D fills.
//
// Pure functions of the reads, pinned in resources.test.ts;
// ResourcesPage.svelte owns the reads, the stream and the strokes.

import type { Remote } from '../../../data/remote';
import type { StationLoadRow } from '../../marshalling/marshalling';
import { parseHeldClaims, stationHref } from '../station-page';
import { parseClaims, quantity, subtree, type StationTree } from '../station-tree';

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj | null => (v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Obj) : null);
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const plural = (n: number, one: string, many = `${one}s`): string => `${n} ${n === 1 ? one : many}`;

// ---------------------------------------------------------------------
// The address.
// ---------------------------------------------------------------------

const SUFFIX = '/resources';

/** A station's Resources view: its page's address plus `/resources`
 *  (router.ts HOSTED reads it back). */
export const resourcesHref = (name: string): string => `${stationHref(name)}${SUFFIX}`;

// ---------------------------------------------------------------------
// The reads.
// ---------------------------------------------------------------------

/** One pool row of GET /api/pools (crates/core/boss-jobs/src/http/pools.rs). */
export type PoolRow = Readonly<{
  name: string;
  unit: string;
  kind: string | null;
  shape: string | null;
  /** null: the registry declares it unknown — never 0. */
  capacity: number | null;
  /** The registry's own sentence on where an unknown capacity comes from. */
  capacityFrom: string | null;
}>;

/** GET /api/pools. A wrong shape throws: no pool would draw an empty floor. */
export function parsePools(raw: unknown): ReadonlyArray<PoolRow> {
  if (!Array.isArray(raw)) throw new Error('pools read: expected [...]');
  return raw.flatMap((r): PoolRow[] => {
    const o = obj(r);
    const name = str(o?.name);
    if (o === null || name === null) return [];
    const cap = obj(o.capacity);
    return [{
      name,
      unit: str(o.unit) ?? '',
      kind: str(o.kind),
      shape: str(o.shape),
      capacity: cap?.state === 'configured' ? quantity(cap.amount) : null,
      capacityFrom: str(o.capacity_from),
    }];
  });
}

/** One open claim, as much as the view reads: who, what it draws, where. */
export type UnitClaim = Readonly<{
  /** A claim is per step; the job and the index stand in only where the
   *  read names no step. */
  key: string;
  jobId: string | null;
  holder: string | null;
  workflowKind: string | null;
  since: string | null;
  /** The tree station the claim's step stands at; null where the record
   *  cannot say. */
  station: string | null;
  /** Each draw with the station it is counted at (station-tree.ts
   *  `parseClaims`: a run's host draw counts in the share its bill names). */
  draws: ReadonlyArray<Readonly<{ pool: string; amount: number | null; station: string | null }>>;
}>;

/** GET /api/claims, as the view reads it: the holders (station-page.ts)
 *  zipped with the counted draws (station-tree.ts). Both parsers walk the
 *  one list and skip the same non-object rows, so the rows line up. */
export function parseUnitClaims(raw: unknown): ReadonlyArray<UnitClaim> {
  const held = parseHeldClaims(raw);
  const rows = parseClaims(raw).claims;
  return held.map((h, i) => ({
    key: h.stepId ?? `${h.jobId ?? 'claim'}#${i}`,
    jobId: h.jobId,
    holder: h.holder,
    workflowKind: h.workflowKind,
    since: h.since,
    station: h.station,
    draws: rows[i]?.allocations ?? [],
  }));
}

/** One step standing in a pool's line. */
export type LineEntry = Readonly<{
  position: number;
  jobId: string;
  stepId: string;
  assignee: string | null;
  since: string | null;
  /** The station the step declares (`lineStationOf`); null until read,
   *  or where it declares none. */
  station: string | null;
}>;
export type WaitingLine = Readonly<{ pool: string; entries: ReadonlyArray<LineEntry> }>;
export type LineRead = Remote<WaitingLine>;

/** GET /api/claims/waiting?pool= (claims_console.rs `waiting`): the pool's
 *  line, oldest wait first. A wrong shape throws: no line reads as
 *  nothing waits. */
export function parseWaitingLine(raw: unknown): WaitingLine {
  const o = obj(raw);
  const pool = str(o?.pool);
  if (o === null || pool === null || !Array.isArray(o.line)) throw new Error('waiting line read: expected {pool, line: [...]}');
  const entries = o.line.flatMap((r): LineEntry[] => {
    const e = obj(r);
    const jobId = str(e?.job_id);
    const stepId = str(e?.step_id);
    if (e === null || jobId === null || stepId === null || typeof e.position !== 'number') return [];
    return [{ position: e.position, jobId, stepId, assignee: str(e.assignee_id), since: str(obj(e.waiting_on)?.since), station: null }];
  });
  return { pool, entries: [...entries].sort((a, b) => a.position - b.position) };
}

/** Where a waiting step stands: the station its own metadata declares
 *  (`metadata.at`, a station id — the claim door's `declared-at`), read
 *  off GET /api/jobs/{id}. A step that declares none, or a station the
 *  tree does not hold, is unplaced: never guessed. */
export function lineStationOf(tree: StationTree, job: unknown, stepId: string): string | null {
  const steps = obj(job)?.steps;
  if (!Array.isArray(steps)) return null;
  const step = steps.map(obj).find((s) => s !== null && s.id === stepId);
  const at = str(obj(step?.metadata)?.at);
  if (at === null) return null;
  return tree.nodes.find((n) => n.id === at)?.name ?? null;
}

// ---------------------------------------------------------------------
// The pools a station allocates.
// ---------------------------------------------------------------------

/** Counted in executors: a unit is one slot or one person. */
const COUNTED = /^(slots?|people|persons?)$/;
/** Past this many a counted pool is a bar: a rail is for units you can count at a glance. */
export const TOKENS_MAX = 16;

export type DrawnPool =
  | Readonly<{ name: string; unit: string; draw: 'tokens'; size: number; sizeFrom: 'allocation' | 'registry' }>
  | Readonly<{
    name: string; unit: string; draw: 'bar'; size: number | null; sizeFrom: 'allocation' | 'registry' | 'unknown';
    /** What the subtree's claims hold; 0 until `resourceFloor` sums it. */
    held: number;
    /** The children that hold a share of it as their own allocation. */
    shares: ReadonlyArray<Readonly<{ station: string; amount: number }>>;
  }>
  | Readonly<{ name: string; unit: string; draw: 'rate'; rate: number | null; words: string }>
  | Readonly<{ name: string; unit: string; draw: 'unknown'; why: string }>;

const DRAW_ORDER: Readonly<Record<DrawnPool['draw'], number>> = { tokens: 0, bar: 1, rate: 2, unknown: 3 };

/** The pools `station` allocates, units first, then bars, then rates. */
export function drawnPools(tree: StationTree, station: string, pools: ReadonlyArray<PoolRow>): ReadonlyArray<DrawnPool> {
  const node = tree.byName.get(station);
  if (node === undefined) return [];
  const kids = node.children.flatMap((c) => tree.byName.get(c) ?? []);
  const own = node.allocation;
  const names = new Set([...Object.keys(own), ...kids.flatMap((k) => Object.keys(k.allocation))]);
  // The robots' rate is the company's, drawn by every robot wherever it
  // works: shown at every station, and said plainly while unread.
  for (const p of pools) if (p.shape === 'rate') names.add(p.name);
  const byName = new Map(pools.map((p) => [p.name, p] as const));
  return [...names]
    .map((name): DrawnPool => {
      const row = byName.get(name);
      if (row === undefined) return { name, unit: '', draw: 'unknown', why: 'no pool row in the registry names it' };
      if (row.shape === 'rate') {
        return { name, unit: row.unit, draw: 'rate', rate: row.capacity,
          words: row.capacity === null ? `rate not read: ${row.capacityFrom ?? 'the registry states no rate for it'}` : `${row.capacity} ${row.unit}` };
      }
      const mine = name in own ? own[name] ?? null : undefined;
      const size = mine === undefined ? row.capacity : mine;
      const sizeFrom = mine === undefined ? (row.capacity === null ? 'unknown' : 'registry') : mine === null ? 'unknown' : 'allocation';
      if (COUNTED.test(row.unit) && size !== null && Number.isInteger(size) && size <= TOKENS_MAX && sizeFrom !== 'unknown') {
        return { name, unit: row.unit, draw: 'tokens', size, sizeFrom };
      }
      const shares = kids.flatMap((k) => {
        const a = k.allocation[name];
        return a === undefined || a === null ? [] : [{ station: k.name, amount: a }];
      });
      return { name, unit: row.unit, draw: 'bar', size, sizeFrom, held: 0, shares };
    })
    .sort((a, b) => DRAW_ORDER[a.draw] - DRAW_ORDER[b.draw] || a.name.localeCompare(b.name));
}

// ---------------------------------------------------------------------
// Where a unit stands.
// ---------------------------------------------------------------------

/** The box a claim at `at` stands in on `station`'s floor: the child of
 *  `station` whose subtree holds `at`, or `station` itself; null where
 *  the record names no station or one outside `station`. */
export function boxOf(tree: StationTree, station: string, at: string | null): string | null {
  if (at === null) return null;
  const nameOf = new Map(tree.nodes.flatMap((n) => (n.id === null ? [] : [[n.id, n.name] as const])));
  let here = tree.byName.get(at);
  for (let hops = 0; here !== undefined && hops <= tree.nodes.length; hops += 1) {
    if (here.name === station) return station;
    const up = here.parent === null ? undefined : nameOf.get(here.parent);
    if (up === station) return here.name;
    here = up === undefined ? undefined : tree.byName.get(up);
  }
  return null;
}

/** Which units each claim holds, by unit index, for one pool. */
export type Seat = ReadonlyMap<string, ReadonlyArray<number>>;
/** Every token pool's seats, carried from one read to the next. */
export type Seats = ReadonlyMap<string, Seat>;

/** The units each holder holds: a claim still holding keeps the units it
 *  had; a new one takes the lowest free. More held than the pool has is
 *  counted `over`, never drawn as a unit that does not exist. */
export function seatUnits(prev: Seat, holders: ReadonlyArray<Readonly<{ key: string; n: number }>>, size: number): Readonly<{ seats: Seat; over: number }> {
  const taken = new Set<number>();
  const kept = new Map<string, number[]>();
  for (const h of holders) {
    const had = (prev.get(h.key) ?? []).filter((i) => i < size && !taken.has(i)).slice(0, h.n);
    had.forEach((i) => taken.add(i));
    kept.set(h.key, had);
  }
  let over = 0;
  let next = 0;
  const seats = new Map<string, ReadonlyArray<number>>();
  for (const h of holders) {
    const mine = [...(kept.get(h.key) ?? [])];
    while (mine.length < h.n) {
      while (next < size && taken.has(next)) next += 1;
      if (next >= size) { over += h.n - mine.length; break; }
      taken.add(next);
      mine.push(next);
    }
    if (mine.length > 0) seats.set(h.key, [...mine].sort((a, b) => a - b));
  }
  return { seats, over };
}

// ---------------------------------------------------------------------
// The floor.
// ---------------------------------------------------------------------

/** One unit of a counted pool. */
export type Unit = Readonly<{
  pool: string;
  index: number;
  /** The claim holding it; null: idle in the rail. */
  held: UnitClaim | null;
  /** The box it stands in; null in the rail, or held where the record
   *  cannot place it (`held` says which). */
  box: string | null;
  /** The station that reserved it as its own; null: lendable. Car D
   *  (30c1efe8) records reservations; until then every unit is lendable. */
  reserved: string | null;
}>;

export type TokenPool = Readonly<{ pool: string; unit: string; size: number; units: ReadonlyArray<Unit>; free: number; over: number; seats: Seat }>;

export type Pressure = Readonly<{
  state: 'working' | 'waiting' | 'starved' | 'not-asking' | 'quiet' | 'unread';
  words: string;
}>;

export type Box = Readonly<{
  name: string;
  /** The station itself, holding work at its own level. */
  here: boolean;
  /** A child's own seats — its declared capacity, worked by its machines (factory-taxonomy-v2) — and how many claims hold one. */
  equipment: Readonly<{ count: number; claimed: number }> | null;
  /** Units of the counted pools it holds now. */
  held: number;
  pressure: Pressure;
}>;

export type LineRow =
  | Readonly<{ pool: string; state: 'ready'; entries: ReadonlyArray<LineEntry> }>
  | Readonly<{ pool: string; state: 'loading' }>
  | Readonly<{ pool: string; state: 'failed'; why: string }>;

export type ResourceFloor = Readonly<{
  station: string;
  pools: ReadonlyArray<DrawnPool>;
  tokens: ReadonlyArray<TokenPool>;
  boxes: ReadonlyArray<Box>;
  /** Units held where the record cannot place them on this floor. */
  unplaced: number;
  line: ReadonlyArray<LineRow>;
}>;

export type FloorReads = Readonly<{
  tree: StationTree;
  station: string;
  pools: ReadonlyArray<PoolRow>;
  claims: ReadonlyArray<UnitClaim>;
  /** The last floor's seats, so a unit keeps its identity across reads. */
  seats: Seats;
  load: Remote<ReadonlyMap<string, StationLoadRow>>;
  /** Each counted pool's line, by pool. */
  lines: ReadonlyMap<string, LineRead>;
}>;

/** The largest open count at any station of `name`'s subtree (`only`:
 *  `name`'s own row): stations overlap, so a sum would overcount. */
function openAt(tree: StationTree, name: string, only: boolean, load: ReadonlyMap<string, StationLoadRow>): number {
  const at = only ? new Set([name]) : subtree(tree, name);
  return Math.max(0, ...[...at].map((s) => load.get(s)?.depth ?? 0));
}

/** The whole floor of `station`, from the reads. */
export function resourceFloor(r: FloorReads): ResourceFloor {
  const { tree, station } = r;
  const node = tree.byName.get(station);
  const inside = subtree(tree, station);
  const drawn = drawnPools(tree, station, r.pools);
  // The claims read is the station's subtree's already (claimsPath, backlog
  // d05e29a7); one the record cannot place stays in it, drawn unplaced.
  const claims = r.claims;

  const tokens = drawn.flatMap((p): TokenPool[] => {
    if (p.draw !== 'tokens') return [];
    // Oldest claim first, so a fresh read seats units in a fixed order.
    const holding = claims
      .flatMap((c) => {
        const n = c.draws.filter((d) => d.pool === p.name).reduce((s, d) => s + Math.max(1, Math.round(d.amount ?? 1)), 0);
        return n === 0 ? [] : [{ key: c.key, n, claim: c }];
      })
      .sort((a, b) => (a.claim.since ?? '').localeCompare(b.claim.since ?? '') || a.key.localeCompare(b.key));
    const { seats, over } = seatUnits(r.seats.get(p.name) ?? new Map(), holding, p.size);
    const holderOf = new Map(holding.flatMap((h) => (seats.get(h.key) ?? []).map((i) => [i, h.claim] as const)));
    const units = Array.from({ length: p.size }, (_, index): Unit => {
      const held = holderOf.get(index) ?? null;
      return { pool: p.name, index, held, box: held === null ? null : boxOf(tree, station, held.station), reserved: null };
    });
    return [{ pool: p.name, unit: p.unit, size: p.size, units, free: units.filter((u) => u.held === null).length, over, seats }];
  });

  const pools = drawn.map((p): DrawnPool => {
    if (p.draw !== 'bar') return p;
    const held = claims.flatMap((c) => c.draws).filter((d) => d.pool === p.name && d.station !== null && inside.has(d.station)).reduce((s, d) => s + (d.amount ?? 0), 0);
    return { ...p, held };
  });

  const idle = tokens.reduce((s, t) => s + t.free, 0);
  const line: ReadonlyArray<LineRow> = tokens.map((t): LineRow => {
    const l = r.lines.get(t.pool);
    if (l === undefined || l.kind === 'loading') return { pool: t.pool, state: 'loading' };
    if (l.kind === 'failed') return { pool: t.pool, state: 'failed', why: l.error };
    return { pool: t.pool, state: 'ready', entries: l.data.entries };
  });

  const names = node === undefined ? [] : [station, ...[...node.children].sort()];
  const boxes = names.map((name): Box => {
    const here = name === station;
    const kid = tree.byName.get(name);
    const held = tokens.reduce((s, t) => s + t.units.filter((u) => u.box === name).length, 0);
    const equipment = !here && kid !== undefined && kid.machines.length > 0 && kid.capacity !== null
      ? { count: kid.capacity, claimed: claims.filter((c) => c.station !== null && boxOf(tree, station, c.station) === name).length }
      : null;
    const queued = line.flatMap((l) => (l.state === 'ready' ? l.entries.map((e) => ({ pool: l.pool, e })) : []))
      .find(({ e }) => e.station !== null && (here ? e.station === station : boxOf(tree, station, e.station) === name));
    return { name, here, equipment, held, pressure: pressureOf({ station, held, equipment, queued, idle, load: r.load, open: (m) => openAt(tree, name, here, m) }) };
  });

  const unplaced = tokens.reduce((s, t) => s + t.units.filter((u) => u.held !== null && u.box === null).length, 0);
  return { station, pools, tokens, boxes, unplaced, line };
}

function pressureOf(i: Readonly<{
  station: string;
  held: number;
  equipment: Box['equipment'];
  queued: Readonly<{ pool: string; e: LineEntry }> | undefined;
  idle: number;
  load: Remote<ReadonlyMap<string, StationLoadRow>>;
  open: (load: ReadonlyMap<string, StationLoadRow>) => number;
}>): Pressure {
  if (i.queued !== undefined) return { state: 'waiting', words: `waiting in ${i.station}'s line: #${i.queued.e.position} for ${i.queued.pool}` };
  const seated = i.equipment !== null && i.equipment.claimed > 0 ? `${i.equipment.claimed} of ${plural(i.equipment.count, 'seat')} claimed` : null;
  if (i.held > 0 || seated !== null) {
    return { state: 'working', words: `working: ${[i.held > 0 ? `holds ${plural(i.held, 'unit')}` : null, seated].filter((x) => x !== null).join(', ')}` };
  }
  if (i.load.kind === 'loading') return { state: 'unread', words: 'reading the load…' };
  if (i.load.kind === 'failed') return { state: 'unread', words: `the load read failed, so whether work waits here is unread: ${i.load.error}` };
  const open = i.open(i.load.data);
  if (open === 0) return { state: 'quiet', words: 'quiet: nothing open here' };
  if (i.idle > 0) return { state: 'starved', words: `starved: ${open} open here while ${plural(i.idle, 'unit')} sit idle` };
  return { state: 'not-asking', words: `not asking: ${open} open here, no unit idle, no request in the line` };
}

// ---------------------------------------------------------------------
// Where everything is drawn.
// ---------------------------------------------------------------------
//
// FIXED PLACES, so operators learn the floor (David, on the canvas): a
// box's place depends on the station's children and its pools' sizes,
// never on what is held, and unit i of a pool has column i in the rail
// AND in every box, so a claim moves exactly one token along one path
// and nothing else on the floor shifts.

export const GEO = { pitch: 30, token: 24, label: 160, railRow: 36, gap: 16, head: 78, pad: 12, minBox: 220 } as const;

export type Placed = Readonly<{ x: number; y: number }>;
export type FloorGeometry = Readonly<{
  width: number;
  height: number;
  boxW: number;
  boxH: number;
  /** Each box's origin, by box name; `unplaced` last when drawn. */
  boxes: ReadonlyMap<string, Placed>;
  /** Each unit's place, by `<pool>#<index>`. */
  units: ReadonlyMap<string, Placed>;
  /** Each rail socket, by `<pool>#<index>`. */
  sockets: ReadonlyMap<string, Placed>;
  /** Each pool's rail row label, by pool. */
  rows: ReadonlyMap<string, Placed>;
  /** Each seat's place, by `<box>#<index>`. */
  equipment: ReadonlyMap<string, Placed>;
}>;

/** The name the unplaced box is drawn under; no station row is named so,
 *  because a station name never carries a space. */
export const UNPLACED = 'held, unplaced';

export function floorGeometry(f: ResourceFloor, width: number): FloorGeometry {
  const { pitch, label, railRow, gap, head, pad, minBox } = GEO;
  const maxSize = Math.max(0, ...f.tokens.map((t) => t.size), ...f.boxes.map((b) => b.equipment?.count ?? 0));
  const boxW = Math.max(minBox, pad * 2 + maxSize * pitch);
  const anyFixed = f.boxes.some((b) => b.equipment !== null);
  const boxH = head + f.tokens.length * pitch + (anyFixed ? pitch + 6 : 0) + pad;
  const railH = f.tokens.length * railRow;
  const top = railH + (f.tokens.length > 0 ? 28 : 0);
  const cols = Math.max(1, Math.floor((width + gap) / (boxW + gap)));
  const names = [...f.boxes.map((b) => b.name), ...(f.unplaced > 0 ? [UNPLACED] : [])];
  const boxes = new Map(names.map((n, k) => [n, { x: (k % cols) * (boxW + gap), y: top + Math.floor(k / cols) * (boxH + gap) }] as const));
  const rows = new Map(f.tokens.map((t, r) => [t.pool, { x: 0, y: r * railRow }] as const));
  const sockets = new Map<string, Placed>(f.tokens.flatMap((t, r) => t.units.map((u) => [`${u.pool}#${u.index}`, { x: label + u.index * pitch, y: r * railRow + (railRow - GEO.token) / 2 }] as const)));
  const units = new Map(f.tokens.flatMap((t, r) => t.units.map((u): readonly [string, Placed] => {
    const key = `${u.pool}#${u.index}`;
    const at = u.held === null ? undefined : boxes.get(u.box ?? UNPLACED);
    return [key, at === undefined ? sockets.get(key)! : { x: at.x + pad + u.index * pitch, y: at.y + head + r * pitch }];
  })));
  const equipment = new Map<string, Placed>(f.boxes.flatMap((b) => {
    const at = boxes.get(b.name);
    if (b.equipment === null || at === undefined) return [];
    return Array.from({ length: b.equipment.count }, (_, i) => [`${b.name}#${i}`, { x: at.x + pad + i * pitch, y: at.y + head + f.tokens.length * pitch + 4 }] as const);
  }));
  const rowsOfBoxes = Math.ceil(names.length / cols);
  return { width, height: top + rowsOfBoxes * (boxH + gap), boxW, boxH, boxes, units, sockets, rows, equipment };
}

// ---------------------------------------------------------------------
// The record of moves.
// ---------------------------------------------------------------------

export type ResourceMove = Readonly<{ at: string; text: string; jobId: string | null }>;
/** How many moves the record keeps. */
export const MOVES_MAX = 12;

const who = (c: UnitClaim): string => [c.holder, c.workflowKind].filter((x) => x !== null).join(', ') || 'a holder the record does not name';

/** The units whose holder changed between two floors of one station:
 *  each return, then each take, as one line stamped `at`. */
export function unitMoves(before: ResourceFloor, after: ResourceFloor, at: string): ReadonlyArray<ResourceMove> {
  const was = new Map(before.tokens.flatMap((t) => t.units.map((u) => [`${u.pool}#${u.index}`, u] as const)));
  const returns: ResourceMove[] = [];
  const takes: ResourceMove[] = [];
  const where = (u: Unit): string => u.box ?? 'a station the record cannot place';
  for (const t of after.tokens) {
    for (const u of t.units) {
      const b = was.get(`${u.pool}#${u.index}`);
      const from = b?.held ?? null;
      const to = u.held;
      if (from?.key === to?.key) continue;
      const unit = `${u.pool} unit ${u.index + 1}`;
      if (from !== null && b !== undefined) returns.push({ at, jobId: from.jobId, text: `${where(b)} returns ${unit} to ${after.station} (${who(from)})` });
      if (to !== null) takes.push({ at, jobId: to.jobId, text: `${where(u)} takes ${unit} from ${after.station} (${who(to)})` });
    }
  }
  return [...returns, ...takes];
}
