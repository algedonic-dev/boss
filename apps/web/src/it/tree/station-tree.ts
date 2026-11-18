// THE FLOOR SPEAKS THE TREE — the read and the card (T7 of design
// 4b27a65a, backlog 94a9dad4; the bundle David approved 2026-11-06,
// `bundle_decision_20261106_1944Z`).
//
// ONE CARD, DRAWN RECURSIVELY. The company is a station, a department is
// a station, a room in it is a station; each is drawn by the same card —
// the five-row card of design c4854fd4 E1 (`stationCard` below) — from
// GET /api/stations?under= (crates/core/boss-jobs/src/http/stations.rs
// `station_tree_read`), the open-packet load and the open claims.
//
// EVERY GAP IS NAMED, NEVER FILLED. The server names what it could not
// read (`pools_unavailable`, `hosts_unavailable`, a pool's
// `host_unavailable`, a live node with no pool, rows outside the tree);
// each becomes a sentence here, and an unknown capacity stays unknown —
// never 0, never plenty.
//
// CODE NEVER NAMES AN ADDRESS (handoff §3 rule 9). The root is found by
// the tree's shape — the one tree row with no parent — and an address is
// whatever the read derived. The table that stood the old map's regions
// on stations (`REGION_AT`) and the four region views it fed retired with
// the transit map (R3, backlog 23a4b4d7): nothing here speaks regions.
//
// Pure functions of the payload, so `bun test` pins every rule without a
// DOM; StationCard.svelte and TreeFloor.svelte own the strokes.

import type { Remote } from '../../data/remote';
import type { StationLoadRow } from '../marshalling/marshalling';

/** One station of the subtree as the read serves it. */
export type TreeNode = Readonly<{
  name: string;
  title: string;
  /** The stable id `parent` names; null on a row the read should never
   *  serve without one, kept rather than refused. */
  id: string | null;
  /** The parent's id; null on the tree's root. */
  parent: string | null;
  /** Derived by the server from the tree as it stands; never stored. */
  address: string;
  depth: number;
  children: ReadonlyArray<string>;
  mode: 'standing' | 'ad-hoc' | null;
  capacity: number | null;
  /** An equipment station's seats; null where the row declares none. */
  bays: number | null;
  /** The department's line code (design c4854fd4: a `line` field on the
   *  top-level row); null when the row names none. */
  line: string | null;
  machines: ReadonlyArray<string>;
  /** What this station holds from its parent, per pool; null where the
   *  served quantity is not a number this page can draw. */
  allocation: Readonly<Record<string, number | null>>;
  /** Outbound routes by station name (pass 4, design ccfe87ff). */
  edges: ReadonlyArray<string>;
  /** The row's `kind`: `equipment` or `group`; null where it names none. */
  kind: string | null;
  /** The protocol step the row's admission predicate names, when it names
   *  one (`predicate.kind`, `predicate.step.slug`); null for a tag or
   *  empty predicate. The station line places a step that declares no
   *  station by this (design 08d1530e car B). */
  admits: Admits | null;
}>;

export type Admits = Readonly<{
  kind: string;
  /** null: the predicate names the protocol and no step of it. */
  step: string | null;
  /** The predicate carries a condition beyond kind, status, step slug and
   *  step status, so it admits only SOME packets at the step. */
  narrower: boolean;
}>;

const ADMIT_KEYS = new Set(['kind', 'status', 'step']);
const ADMIT_STEP_KEYS = new Set(['slug', 'status_in']);

/** What a station's admission predicate names, read off the row. */
export function admitsOf(predicate: unknown): Admits | null {
  const p = obj(predicate);
  const kind = str(p?.kind);
  if (p === null || kind === null) return null;
  const step = obj(p.step);
  const narrower = Object.keys(p).some((k) => !ADMIT_KEYS.has(k)) || Object.keys(step ?? {}).some((k) => !ADMIT_STEP_KEYS.has(k));
  return { kind, step: str(step?.slug), narrower };
}

export type TreePool = Readonly<{
  name: string;
  unit: string;
  /** null: the registry declares the capacity unknown — not zero. */
  capacity: number | null;
  /** The registry's `kind` (flow or stock) and `shape` (capacity or
   *  rate): where the card's fixed pool order puts the pool. */
  kind: string | null;
  shape: string | null;
  /** The estate node's label for a `host:` pool the estate answered. */
  host: string | null;
  /** Why a `host:` pool's node could not be read, in the server's words. */
  hostGap: string | null;
  /** The pool row's version and when it was published: the last
   *  capacity event the tree read carries (the station page names it,
   *  design c4854fd4 E2). null where the row does not say. */
  version: number | null;
  since: string | null;
  /** The station the row names as keeping the pool — the mandate to keep
   *  the box, not a share of it (backlog 42cd9a83). null where the row
   *  names none. */
  keeper: string | null;
}>;

export type StationTree = Readonly<{
  /** The subtree's top station. */
  top: string;
  /** Root first, then depth-first — the server's order. */
  nodes: ReadonlyArray<TreeNode>;
  byName: ReadonlyMap<string, TreeNode>;
  pools: ReadonlyArray<TreePool>;
  /** What the server said it could not read, each as a sentence. */
  gaps: ReadonlyArray<string>;
}>;

// ---------------------------------------------------------------------
// The wire.
// ---------------------------------------------------------------------

type Obj = Record<string, unknown>;
const WHERE = 'station tree';

const obj = (v: unknown): Obj | null => (v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Obj) : null);
const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);
const strs = (v: unknown): ReadonlyArray<string> => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []);
/** A Quantity is one exact decimal string on the wire (resources.rs);
 *  a number is read too, so an older fixture or server still draws. */
export const quantity = (v: unknown): number | null => {
  const n = typeof v === 'number' ? v : typeof v === 'string' && v.trim() !== '' ? Number(v) : Number.NaN;
  return Number.isFinite(n) ? n : null;
};

function parseNode(raw: unknown): TreeNode {
  const o = obj(raw);
  const s = obj(o?.station);
  const name = str(s?.name);
  const address = str(o?.address);
  if (o === null || s === null || name === null || address === null || !Array.isArray(o.children)) {
    throw new Error(`${WHERE}: a row without a station name, an address or its children`);
  }
  const mode = s.mode === 'standing' || s.mode === 'ad-hoc' ? s.mode : null;
  const alloc = obj(s.allocation) ?? {};
  return {
    name,
    title: str(s.title) ?? name,
    id: str(s.id),
    parent: str(s.parent),
    address,
    depth: typeof o.depth === 'number' ? o.depth : 0,
    children: strs(o.children),
    mode,
    capacity: quantity(s.capacity),
    bays: quantity(s.bays),
    line: str(s.line),
    machines: strs(s.machines),
    allocation: Object.fromEntries(Object.entries(alloc).map(([k, v]) => [k, quantity(v)])),
    edges: strs(s.edges),
    kind: str(s.kind),
    admits: admitsOf(s.predicate),
  };
}

function parsePool(raw: unknown): TreePool | null {
  const o = obj(raw);
  const p = obj(o?.pool);
  const name = str(p?.name);
  if (o === null || p === null || name === null) return null;
  const cap = obj(p.capacity);
  const host = obj(o.host);
  return {
    name,
    unit: str(p.unit) ?? '',
    capacity: cap?.state === 'configured' ? quantity(cap.amount) : null,
    kind: str(p.kind),
    shape: str(p.shape),
    host: host === null ? null : (str(host.label) ?? str(host.id)),
    hostGap: str(o.host_unavailable),
    version: typeof p.version === 'number' ? p.version : null,
    since: str(p.created_at),
    keeper: str(p.keeper),
  };
}

/** The tree read whole, or a throw: a payload with no station in it is a
 *  wrong server or a wrong `under`, never an empty company. */
export function parseTree(raw: unknown): StationTree {
  const o = obj(raw);
  if (o === null || !Array.isArray(o.data)) throw new Error(`${WHERE}: expected {data: [...]}`);
  if (o.data.length === 0) throw new Error(`${WHERE}: the read held no station`);
  const nodes = o.data.map(parseNode);
  const rawPools = Array.isArray(o.pools) ? o.pools : [];
  const pools = rawPools.map(parsePool).filter((p): p is TreePool => p !== null);
  const unreadPools = rawPools.length - pools.length;
  const without = strs(o.hosts_without_pool);
  const outside = strs(o.outside_tree);
  const gaps = [
    str(o.pools_unavailable),
    str(o.hosts_unavailable),
    unreadPools > 0 ? `${unreadPools} pool row(s) this page could not read` : null,
    // A host the estate could not answer for: said here, beside the floor,
    // since the card carries nothing but its five rows.
    ...pools.map((p) => (p.hostGap === null ? null : `${p.name}: ${p.hostGap}`)),
    ...without.map((n) => `live node \`${n}\` has no host pool`),
    outside.length > 0 ? `${outside.length} station row(s) stand outside the tree: ${outside.join(', ')}` : null,
  ].filter((g): g is string => g !== null && g !== '');
  return {
    top: nodes[0]!.name,
    nodes,
    byName: new Map(nodes.map((n) => [n.name, n] as const)),
    pools,
    gaps,
  };
}

/** The tree's root id, from GET /api/stations: the ONE active row that
 *  declares an id and names no parent. Null when there is none or more
 *  than one — the page says so rather than pick. */
export function rootIdOf(raw: unknown): string | null {
  const data = obj(raw)?.data;
  if (!Array.isArray(data)) return null;
  const roots = data
    .map(obj)
    .filter((s): s is Obj => s !== null && (s.status === undefined || s.status === 'active'))
    .filter((s) => str(s.id) !== null && str(s.parent) === null)
    .map((s) => str(s.id)!);
  return roots.length === 1 ? roots[0]! : null;
}

// ---------------------------------------------------------------------
// The station card (design c4854fd4 E1, decided by David 2026-11-07
// 23:38Z; backlog e1b13a15).
// ---------------------------------------------------------------------
//
// THE UNIT A PARENT FLOOR TILES, with the same five rows on every station
// kind and an empty row saying so, so a floor of cards lines up and reads
// as one resource picture (decision card-rows):
//
//   1 Rail   the department's line: identity, never state, inherited by
//            every station inside it. A failed read turns it red; a read
//            whose own projection says its source is gone, hollow.
//   2 Band   the name, the mode as the frame (solid standing, dashed ad
//            hoc), and ONE plate that always carries its word.
//   3 Pools  one row per pool held, one cell per unit, filled in use and
//            open free; past ~32 units a bar with its figure (decision
//            card-free-space).
//   4 Parts  sub-stations, ad hoc stations open now, seats busy of bays,
//            machines, packets standing.
//   5 Nothing else: no packets, no movement, no connections.
//
// REAL, CURRENT READS ONLY (David, 2026-11-08 00:50Z): the station rows,
// the open-packet load and the open claims — never a membership
// generation, a flow read, a border rate or an "as of" cut. A figure
// those reads do not carry is SAID to be unrecorded, never drawn as 0: on
// 2026-11-08 no open claim carried an allocation (backlog 35e9babf), so
// every pool draws what it holds open and says "in use not recorded"
// rather than vouch for free space nobody measured. "Room for N" is the
// server's sentence (card-free-space); no read states one yet, so the
// card adds none of its own.

/** One open claim, as much of it as the card reads. */
export type ClaimRow = Readonly<{
  /** The tree station its step stands at; null where the record cannot
   *  say (a claim recorded before the history rebuild). */
  station: string | null;
  /** Each with the station it is counted at: the claim's own, except a
   *  run's host draw, which counts in the share its bill names
   *  (`bill.host.station`, backlog 5608c1a2). */
  allocations: ReadonlyArray<Readonly<{ pool: string; amount: number | null; station: string | null }>>;
}>;

export type ClaimsRead = Readonly<{
  claims: ReadonlyArray<ClaimRow>;
  /** The read's own projection says its source could not be read. */
  sourceUnavailable: boolean;
  /** The station whose subtree the server narrowed `claims` to; null
   *  when they are every claim (backlog d05e29a7). Read off the answer,
   *  never off what was asked: the root narrows nothing. */
  within: string | null;
}>;

/** The claims read for one station's subtree, by its id or name — the
 *  floor and the page each read their own, never every claim on every
 *  load (backlog d05e29a7); null asks for every claim. */
export const claimsPath = (station: string | null): string =>
  station === null ? '/api/claims' : `/api/claims?station=${encodeURIComponent(station)}`;

/** GET /api/claims (claims_console.rs). A wrong shape throws: an empty
 *  list here would draw every pool free. */
export function parseClaims(raw: unknown): ClaimsRead {
  const o = obj(raw);
  if (o === null || !Array.isArray(o.claims)) throw new Error('claims read: expected {claims: [...]}');
  const claims = o.claims.flatMap((c): ClaimRow[] => {
    const row = obj(c);
    if (row === null) return [];
    const station = str(row.station);
    // A run's vCPU is drawn from a station's floating share of a box,
    // which the claim's bill names; the claim itself is taken from the
    // run's queue station, often outside the tree (backlog 5608c1a2).
    const host = obj(obj(obj(row.resource_acquisition)?.bill)?.host);
    const hostPool = str(host?.node) === null ? null : `host:${str(host?.node)}`;
    const drawnAt = str(host?.station);
    const allocations = (Array.isArray(row.allocations) ? row.allocations : []).flatMap((a) => {
      const pool = str(obj(a)?.pool);
      if (pool === null) return [];
      return [{ pool, amount: quantity(obj(a)?.amount), station: pool === hostPool && drawnAt !== null ? drawnAt : station }];
    });
    return [{ station, allocations }];
  });
  return { claims, sourceUnavailable: str(obj(o.projection)?.source_unavailable_at) !== null, within: str(o.within) };
}

/** The two reads a card draws beside the tree, as the floor holds them. */
export type CardReads = Readonly<{
  load: Remote<ReadonlyMap<string, StationLoadRow>>;
  claims: Remote<ClaimsRead>;
}>;

/** The pool rows, in the card's fixed order. The first three are drawn
 *  on every card, "none held" when empty; the rest only when held. */
export type PoolGroupName = 'hosts' | 'people' | 'robots' | 'stock' | 'rate' | 'other';
export const GROUPS: ReadonlyArray<PoolGroupName> = ['hosts', 'people', 'robots', 'stock', 'rate', 'other'];
const ALWAYS: ReadonlySet<PoolGroupName> = new Set(['hosts', 'people', 'robots']);
/** A pool past this many units is drawn as a bar with its figure. */
export const CELLS_MAX = 32;

/** A station's seats, as the card counts them (`stationCard` seats): the
 *  row's declared bays, else its capacity; null where it declares neither.
 *  The station page's waits ladder reads the same rule (station-answer.ts,
 *  backlog 950ad972). */
export const seatsOf = (node: TreeNode): number | null => node.bays ?? node.capacity;
/** Sub-stations, ad hoc open, seats, machines, standing: one line each,
 *  so "busy not recorded" has the width to be read. */
export const PARTS_LINES = 5;

export type CardPool = Readonly<{
  pool: string;
  unit: string;
  /** The estate node's label for a `host:` pool. */
  host: string | null;
  /** Units held: the root holds the registry's capacity, any other
   *  station its row's allocation. null: unknown, never 0. */
  held: number | null;
  /** Units the open claims record in use here; null with its note when
   *  the record cannot say. */
  inUse: number | null;
  inUseNote: 'in use not recorded' | 'in use unread' | 'in use stale' | 'reading' | null;
  /** Beside "in use not recorded" when some counted claims record an
   *  allocation on the pool and some record none: the two claim counts,
   *  "1 recorded, 1 unrecorded" (backlog f23ce786). null otherwise. */
  inUseCounts: string | null;
  draw: 'cells' | 'bar' | 'none';
}>;

/** In use of one pool, by the one rule the card and the page share
 *  (backlog f23ce786, Codex's landed review of car 1): the figure is
 *  DEFINITE only when every claim counted against the pool records an
 *  allocation. A claim recording none could be using it — the wire does
 *  not yet tell a measured "none" from an unrecorded one — so beside a
 *  claim recording 3 it is not "3 in use": flattening the allocations
 *  first made it vanish and drew "5 free of 8" that nothing measured.
 *  `counted` says which claims could draw on the pool where it is drawn;
 *  `placed` refuses an allocation whose claim names no station. */
export type PoolUse =
  | Readonly<{ known: true; on: ReadonlyArray<Readonly<{ station: string | null; amount: number }>> }>
  | Readonly<{ known: false; counts: string | null }>;

export function poolUse(claims: ReadonlyArray<ClaimRow>, pool: string, counted: (c: ClaimRow) => boolean, placed: boolean): PoolUse {
  // Each allocation at the station it is counted at — the claim's own,
  // except a run's host draw, counted in the share its bill names
  // (`parseClaims`, backlog 5608c1a2).
  const on = claims.flatMap((c) => c.allocations.filter((a) => a.pool === pool).map((a) => ({ station: a.station, amount: a.amount })));
  const mine = claims.filter(counted);
  const recorded = mine.filter((c) => c.allocations.some((a) => a.pool === pool)).length;
  const unrecorded = mine.filter((c) => c.allocations.length === 0).length;
  // Nothing recorded against the pool is not the same as nothing in use.
  if (on.length === 0 || unrecorded > 0 || (placed && on.some((a) => a.station === null))) {
    return { known: false, counts: recorded > 0 && unrecorded > 0 ? `${recorded} recorded, ${unrecorded} unrecorded` : null };
  }
  const amounts = on.flatMap((a) => (a.amount === null ? [] : [{ station: a.station, amount: a.amount }]));
  return amounts.length < on.length ? { known: false, counts: null } : { known: true, on: amounts };
}

export type PoolGroup = Readonly<{ group: PoolGroupName; pools: ReadonlyArray<CardPool> }>;

export type CardPlate = Readonly<{
  state: 'clear' | 'troubled' | 'stale' | 'failed' | 'reading' | 'unjudged';
  word: string;
  /** The reason, in the reads' own figures, when the plate is not clear. */
  why: string | null;
}>;

export type StationCardModel = Readonly<{
  name: string;
  mode: TreeNode['mode'];
  /** The line code (a `--map-line-<code>` token); null draws night ink. */
  rail: Readonly<{ line: string | null; mark: 'solid' | 'hollow' | 'failed' }>;
  plate: CardPlate;
  pools: ReadonlyArray<PoolGroup>;
  /** The pools row's label when in use cannot be drawn: one note for the
   *  row, naming the pools only when some are recorded and some not. */
  poolsNote: string | null;
  parts: Readonly<{
    subStations: number;
    adHocOpen: number;
    seats: Readonly<{ bays: number | null; busy: number | null; note: string | null }>;
    machines: number;
    standing: Readonly<{ value: number | null; note: string | null }>;
  }>;
  /** How many CARD.line units the body is: the layout reads this, the
   *  card draws exactly it, so neither can disagree about a card's height. */
  lines: number;
}>;

const sum = (xs: ReadonlyArray<number>): number => xs.reduce((a, b) => a + b, 0);

/** The station `node` stands in, in this read. */
const parentOf = (tree: StationTree, node: TreeNode): TreeNode | null =>
  node.parent === null ? null : (tree.nodes.find((n) => n.id === node.parent) ?? null);

/** `name` and every station beneath it in the read, each once — a
 *  malformed read naming a cycle cannot loop the walk. */
export function subtree(tree: StationTree, name: string): ReadonlySet<string> {
  const seen = new Set<string>([name]);
  const stack = [...(tree.byName.get(name)?.children ?? [])];
  while (stack.length > 0) {
    const c = stack.pop()!;
    const kid = tree.byName.get(c);
    if (kid === undefined || seen.has(c)) continue;
    seen.add(c);
    stack.push(...kid.children);
  }
  return seen;
}

const LINE_CODE = /^[a-z][a-z0-9-]*$/;
/** The nearest declared line up the tree.
 *  Bounded by the read's size, so a cycle cannot hang it. */
function lineOf(tree: StationTree, node: TreeNode): string | null {
  let at: TreeNode | null = node;
  for (let i = 0; at !== null && i <= tree.nodes.length; i += 1) {
    const code = at.line;
    if (code !== null && LINE_CODE.test(code)) return code;
    at = parentOf(tree, at);
  }
  return null;
}

export function groupOf(name: string, declared: TreePool | undefined): PoolGroupName {
  if (declared?.kind === 'stock') return 'stock';
  if (declared?.shape === 'rate') return 'rate';
  if (name.startsWith('host:')) return 'hosts';
  if (name.startsWith('people:')) return 'people';
  if (name.startsWith('robots:')) return 'robots';
  return 'other';
}

const drawOf = (held: number | null): CardPool['draw'] =>
  held === null || held <= 0 ? 'none' : Number.isInteger(held) && held <= CELLS_MAX ? 'cells' : 'bar';

/** What one card says about one station, from the three reads; null for
 *  a station the tree read does not hold. */
export function stationCard(tree: StationTree, name: string, reads: CardReads): StationCardModel | null {
  const node = tree.byName.get(name);
  if (node === undefined) return null;
  const isRoot = node.parent === null;
  const { load, claims } = reads;
  const inside = subtree(tree, name);
  const stale = claims.kind === 'ready' && claims.data.sourceUnavailable;
  const failed = [load, claims].flatMap((r) => (r.kind === 'failed' ? [r.error] : []));

  // 3 Pools: the root holds every pool whole; any other station what its
  // row allocates. In use is what the open claims record, or a note.
  const declaredOf = (p: string): TreePool | undefined => tree.pools.find((x) => x.name === p);
  const heldNames = isRoot ? tree.pools.map((p) => p.name) : Object.keys(node.allocation);
  const inUseOf = (p: string): Pick<CardPool, 'inUse' | 'inUseNote' | 'inUseCounts'> => {
    if (claims.kind === 'loading') return { inUse: null, inUseNote: 'reading', inUseCounts: null };
    if (claims.kind === 'failed') return { inUse: null, inUseNote: 'in use unread', inUseCounts: null };
    if (stale) return { inUse: null, inUseNote: 'in use stale', inUseCounts: null };
    // The root counts every claim; below it, a claim inside the station,
    // one the record cannot place, or one whose draw is counted inside it
    // (a run's host draw, at the share its bill names: backlog 5608c1a2),
    // and an allocation with no station cannot be placed below the root.
    const counted = (c: ClaimRow): boolean =>
      isRoot || c.station === null || inside.has(c.station) || c.allocations.some((a) => a.station !== null && inside.has(a.station));
    const use = poolUse(claims.data.claims, p, counted, !isRoot);
    if (!use.known) return { inUse: null, inUseNote: 'in use not recorded', inUseCounts: use.counts };
    const here = isRoot ? use.on : use.on.filter((a) => a.station !== null && inside.has(a.station));
    return { inUse: sum(here.map((a) => a.amount)), inUseNote: null, inUseCounts: null };
  };
  const cardPools = heldNames.map((p): CardPool => {
    const declared = declaredOf(p);
    const held = isRoot ? (declared?.capacity ?? null) : (node.allocation[p] ?? null);
    return { pool: p, unit: declared?.unit ?? '', host: declared?.host ?? null, held, ...inUseOf(p), draw: drawOf(held) };
  });
  const pools = GROUPS.map((group) => ({ group, pools: cardPools.filter((p) => groupOf(p.pool, declaredOf(p.pool)) === group) }))
    .filter((g) => ALWAYS.has(g.group) || g.pools.length > 0);

  // 4 Parts.
  const kids = node.children.map((c) => tree.byName.get(c)).filter((k): k is TreeNode => k !== undefined);
  const row = load.kind === 'ready' ? load.data.get(name) : undefined;
  const bays = node.bays ?? node.capacity;
  const seats = ((): StationCardModel['parts']['seats'] => {
    if (bays === null || bays <= 0) return { bays: null, busy: null, note: null };
    if (claims.kind === 'loading') return { bays, busy: null, note: 'reading' };
    if (claims.kind === 'failed') return { bays, busy: null, note: 'busy unread' };
    if (stale) return { bays, busy: null, note: 'busy stale' };
    // One claim the record cannot place could be in any of these bays.
    if (claims.data.claims.some((c) => c.station === null)) return { bays, busy: null, note: 'busy not recorded' };
    return { bays, busy: claims.data.claims.filter((c) => c.station === name).length, note: null };
  })();
  const standing =
    load.kind === 'loading' ? { value: null, note: 'reading' }
    : load.kind === 'failed' ? { value: null, note: 'unread' }
    : row === undefined ? { value: null, note: 'no load row' }
    : { value: row.depth, note: null };

  // 2 Band: one plate. Over its limit is the one trouble these reads can
  // state (no read carries machine health or a server-judged state yet).
  const plate = ((): CardPlate => {
    if (failed.length > 0) return { state: 'failed', word: 'read failed', why: failed.join('; ') };
    if (stale) return { state: 'stale', word: 'stale', why: 'the claims read says its source could not be read' };
    if (load.kind === 'loading' || claims.kind === 'loading') return { state: 'reading', word: 'reading', why: null };
    return row === undefined ? { state: 'unjudged', word: 'unjudged', why: null } : { state: 'clear', word: 'clear', why: null };
  })();

  const drawn = cardPools.filter((p) => p.draw !== 'none');
  const noted = drawn.filter((p) => p.inUseNote !== null);
  const notes = [...new Set(noted.map((p) => (p.inUseCounts === null ? p.inUseNote : `${p.inUseNote} (${p.inUseCounts})`)))].join(', ');
  const poolsNote = noted.length === 0 ? null : noted.length === drawn.length ? notes : `${notes}: ${noted.map((p) => p.pool).join(', ')}`;
  const poolLines =
    sum(pools.map((g) => (g.pools.length === 0 ? 1 : sum(g.pools.map((p) => (p.draw === 'none' ? 1 : 2)))))) + (poolsNote === null ? 0 : 1);
  return {
    name,
    mode: node.mode,
    rail: { line: lineOf(tree, node), mark: failed.length > 0 ? 'failed' : stale ? 'hollow' : 'solid' },
    plate,
    pools,
    poolsNote,
    parts: {
      subStations: kids.filter((k) => k.mode !== 'ad-hoc').length,
      adHocOpen: kids.filter((k) => k.mode === 'ad-hoc').length,
      seats,
      machines: node.machines.length,
      standing,
    },
    // Each row a half line above and below; the parts a line each.
    lines: 1 + poolLines + 1 + PARTS_LINES,
  };
}

// ---------------------------------------------------------------------
// The page's address.
// ---------------------------------------------------------------------

/** How many levels below the top draw their children — the prototype's
 *  three choices; the default draws every level the platform tree has. */
export const DEPTH_CHOICES = [1, 2, 3] as const;
export const DEFAULT_DEPTH = 3;

/** The floor under one station (its id, or a name the read accepts), at
 *  a depth; the default depth is left out so the plain link stays plain. */
export function treeHref(under: string | null, depth: number = DEFAULT_DEPTH): string {
  const q = new URLSearchParams();
  if (under !== null) q.set('under', under);
  if (depth !== DEFAULT_DEPTH) q.set('depth', String(depth));
  const s = q.toString();
  return s === '' ? '/it/tree' : `/it/tree?${s}`;
}

/** `?depth=` as a depth: a whole number from 1 to 9, else the default. */
export function depthOf(raw: string | null): number {
  const n = raw !== null && /^[1-9]$/.test(raw) ? Number(raw) : Number.NaN;
  return Number.isFinite(n) ? n : DEFAULT_DEPTH;
}

/** The floor's query as the router carries it: a station asked for, and
 *  a depth only when one was asked for and is one. */
export function treeQuery(search: string): Readonly<{ under?: string; depth?: number }> {
  const q = new URLSearchParams(search);
  const under = q.get('under');
  const raw = q.get('depth');
  const depth = raw !== null && depthOf(raw) === Number(raw) ? depthOf(raw) : undefined;
  return { ...(under ? { under } : {}), ...(depth === undefined ? {} : { depth }) };
}

// ---------------------------------------------------------------------
// Words.
// ---------------------------------------------------------------------

const qty = (n: number): string => String(Math.round(n * 100) / 100);

/** A pool row's figure: "3 free of 8 vCPU" when in use is recorded,
 *  "8 vCPU held" when it is not, "capacity unknown" when nothing is. */
export function poolFigure(p: CardPool): string {
  const unit = p.unit === '' ? '' : ` ${p.unit}`;
  if (p.held === null) return 'capacity unknown';
  if (p.inUse === null) return `${qty(p.held)}${unit} held`;
  return `${qty(Math.max(0, p.held - p.inUse))} free of ${qty(p.held)}${unit}`;
}

/** How much of a bar is in use; 0 where in use is not recorded, which
 *  the row's note says. */
export const poolFill = (p: CardPool): number =>
  p.held === null || p.held <= 0 || p.inUse === null ? 0 : Math.min(1, p.inUse / p.held);

/** The parts row, one labelled figure a line, in the card's fixed order. */
export function partWords(parts: StationCardModel['parts']): ReadonlyArray<Readonly<{ key: string; label: string; value: string }>> {
  const s = parts.seats;
  const bays = s.bays === null ? '' : `${qty(s.bays)} bay${s.bays === 1 ? '' : 's'}`;
  const seats = s.bays === null ? 'none held' : s.busy === null ? `${s.note ?? 'not recorded'} · ${bays}` : `${s.busy} of ${qty(s.bays)}`;
  return [
    { key: 'sub-stations', label: 'Sub-stations', value: String(parts.subStations) },
    { key: 'ad-hoc', label: 'Ad hoc open', value: String(parts.adHocOpen) },
    { key: 'seats', label: 'Seats busy', value: seats },
    { key: 'machines', label: 'Machines', value: parts.machines === 0 ? 'none' : String(parts.machines) },
    { key: 'standing', label: 'Standing', value: parts.standing.value === null ? (parts.standing.note ?? 'unread') : String(parts.standing.value) },
  ];
}
