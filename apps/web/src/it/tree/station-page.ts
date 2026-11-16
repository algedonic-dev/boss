// THE STATION PAGE — what opening a station shows (design c4854fd4 E2,
// decided by David 2026-11-07 23:38Z; backlog 72d58e00, car 2 of 3).
//
// David, 22:52Z: "a strong default 'page view' and 'card view' for a
// Station that shows the resource constraints, allocations, and dynamic
// space." The card is car 1 (station-tree.ts `stationCard`); this is the
// page:
//
//   1 Constraints  every pool the station holds or draws, segmented by
//                  holder: each part's share, the station's own floating
//                  rest, and the root's share that is not its own,
//                  hatched. A shared root pool it only draws on is its
//                  draw, the other stations' draw hatched, and what is
//                  free at the root. Since design 08d1530e (car A,
//                  backlog 950ad972) one strip each on the right rail,
//                  under the waits ladder, with the caveat said ONCE
//                  (`poolsCaveat`): the per-pool notes — "room not
//                  stated", the keeper, the row version, said on every
//                  pool — went (David: "put the station's resource pools
//                  to the right so the active lines between stations is
//                  the focus").
//   2 Parts        each sub-station as its own E1 card, and the
//                  station's own machines with the claims they hold.
//   3 Working      the claims inside it, grouped by the part holding
//                  them, each with its holder mark. The In and Out
//                  columns went with design 08d1530e item 8: Out was
//                  always "no departures read yet", and In read the
//                  station's whole queue — 1.46 MB for engineering — to
//                  show five titles. The page takes no queue read now.
//
// The answer band and the rail's waits, stuck, needs-you and moves are
// station-answer.ts.
//
// REAL, CURRENT READS ONLY (David, 2026-11-08 00:50Z): the tree, the
// load and the open claims — never a
// membership generation, a flow read, a border rate or an "as of" cut.
// "Room for N" is the server's sentence and no read states one on
// 2026-11-08, so every band says "room not stated" and adds nothing up
// of its own. Who keeps a pool is the row's `keeper` (station-tree.md §3:
// operations keeps every box; backlog 42cd9a83), read off the tree read
// and never a station's name written into this file — a row naming none
// is said so.
//
// The breadcrumb is the parent chain back to the root, and trouble climbs
// it: every ancestor names the stations below it whose plate is troubled
// (the algedonic signal, design c4854fd4). A plate is troubled only on what a read
// judges — a station over its limit; no read carries machine health yet.
//
// The four old region views that filtered the columns retired with the
// table that stood regions on stations (R3 of the transit-map
// retirement, backlog 23a4b4d7).
//
// Pure functions of the reads, pinned in station-page.test.ts;
// StationPage.svelte owns the strokes. Motion is StationLine.svelte's,
// reading station-motion.ts.

import type { Remote } from '../../data/remote';
import type { StationLoadRow } from '../marshalling/marshalling';
import {
  GROUPS, groupOf, parseClaims, poolUse, quantity, stationCard, subtree,
  type CardPool, type CardReads, type ClaimsRead, type PoolGroupName, type StationCardModel, type StationTree, type TreeNode,
} from './station-tree';

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj | null => (v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Obj) : null);
const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);
const sum = (xs: ReadonlyArray<number>): number => xs.reduce((a, b) => a + b, 0);

// ---------------------------------------------------------------------
// The address.
// ---------------------------------------------------------------------

/** A station's page: `/it/tree/<name>`. The floor keeps `/it/tree`;
 *  opening a part swaps the page, never zooms. */
export function stationHref(name: string): string {
  return `/it/tree/${encodeURIComponent(name)}`;
}

/** The station a path under `/it/tree/` names, or null for none: an
 *  empty name, a deeper path, or one that does not decode. */
export function stationOfPath(rest: string): string | null {
  if (rest === '' || rest.includes('/')) return null;
  try {
    const name = decodeURIComponent(rest);
    return name === '' ? null : name;
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------
// The reads the page adds to the card's.
// ---------------------------------------------------------------------

/** The taxonomy's three executors (docs/design/factory-taxonomy.md). */
export type HolderKind = 'person' | 'robot' | 'automaton';
export const HOLDER_MARK: Readonly<Record<HolderKind, string>> = { person: '●', robot: '◆', automaton: '■' };
const HOLDER_KINDS: ReadonlyArray<HolderKind> = ['person', 'robot', 'automaton'];

/** One open claim, as much as the Working column reads of it. */
export type HeldClaim = Readonly<{
  /** The claim's identity: a claim is per step (GET /api/claims
   *  `step_id`), so two steps of one job are two claims. */
  stepId: string | null;
  jobId: string | null;
  holder: string | null;
  /** null for a kind the taxonomy does not hold: never guessed. */
  holderKind: HolderKind | null;
  station: string | null;
  since: string | null;
  workflowKind: string | null;
  /** What the claim holds, per pool; the motion line reads the change in
   *  it when a packet moves (station-motion.ts `resourceDelta`). */
  allocations: ReadonlyArray<Readonly<{ pool: string; amount: number | null }>>;
}>;

/** GET /api/claims (claims_console.rs), the holders. A wrong shape
 *  throws, as `parseClaims` does: an empty list would draw an idle floor. */
export function parseHeldClaims(raw: unknown): ReadonlyArray<HeldClaim> {
  const o = obj(raw);
  if (o === null || !Array.isArray(o.claims)) throw new Error('claims read: expected {claims: [...]}');
  return o.claims.flatMap((c): HeldClaim[] => {
    const row = obj(c);
    if (row === null) return [];
    const kind = HOLDER_KINDS.find((k) => k === row.executor_kind) ?? null;
    return [{
      stepId: str(row.step_id),
      jobId: str(row.job_id),
      holder: str(row.holder),
      holderKind: kind,
      station: str(row.station),
      since: str(row.since),
      workflowKind: str(obj(row.resource_acquisition)?.workflow_kind),
      allocations: (Array.isArray(row.allocations) ? row.allocations : []).flatMap((a) => {
        const pool = str(obj(a)?.pool);
        return pool === null ? [] : [{ pool, amount: quantity(obj(a)?.amount) }];
      }),
    }];
  });
}

/** The claims read's binding constraint (claims_console.rs `binding`:
 *  the pool with the most refused reservations in the window), with the
 *  read's own reading of that pool. `unread` is a server that predates
 *  the key — never "none", which is the server saying nothing was
 *  refused. */
export type BindingRead =
  | Readonly<{ kind: 'unread' }>
  | Readonly<{ kind: 'none'; windowHours: number | null }>
  | Readonly<{
    kind: 'pool';
    pool: string;
    windowHours: number | null;
    /** null when the read carries no reading for the pool it names. */
    reading: Readonly<{ unit: string; reserved: number | null; capacity: number | null; refused: number }> | null;
  }>;

export function parseBinding(raw: unknown): BindingRead {
  const o = obj(raw);
  if (o === null || !('binding' in o)) return { kind: 'unread' };
  const windowHours = typeof o.window_hours === 'number' ? o.window_hours : null;
  const pool = str(o.binding);
  if (pool === null) return { kind: 'none', windowHours };
  const p = (Array.isArray(o.pools) ? o.pools : []).map(obj).find((x) => x !== null && x.pool_key === pool) ?? null;
  const spec = obj(p?.pool);
  const cap = obj(spec?.capacity);
  return {
    kind: 'pool',
    pool,
    windowHours,
    reading: p === null ? null : {
      unit: str(spec?.unit) ?? '',
      reserved: quantity(p.reserved),
      capacity: cap?.state === 'configured' ? quantity(cap.amount) : null,
      refused: typeof p.failed_reservations === 'number' ? p.failed_reservations : 0,
    },
  };
}

/** The one claims read, as the card and the page each read it. */
export type PageClaims = Readonly<{ card: ClaimsRead; held: ReadonlyArray<HeldClaim>; binding: BindingRead }>;
export const parsePageClaims = (raw: unknown): PageClaims => ({ card: parseClaims(raw), held: parseHeldClaims(raw), binding: parseBinding(raw) });

/** The page's reads, as the page holds them. */
export type PageReads = Readonly<{
  load: Remote<ReadonlyMap<string, StationLoadRow>>;
  claims: Remote<PageClaims>;
}>;

/** The two the card reads, out of the page's. */
export const cardReads = (r: Pick<PageReads, 'load' | 'claims'>): CardReads => ({
  load: r.load,
  claims: r.claims.kind === 'ready' ? { kind: 'ready', data: r.claims.data.card } : r.claims,
});

// ---------------------------------------------------------------------
// The breadcrumb.
// ---------------------------------------------------------------------

export type Crumb = Readonly<{
  name: string;
  href: string;
  current: boolean;
  /** The stations at or below this one whose plate is troubled. */
  troubled: ReadonlyArray<string>;
}>;

/** The station `node` stands in, in this read. */
const parentOf = (tree: StationTree, node: TreeNode): TreeNode | null =>
  node.parent === null ? null : (tree.nodes.find((n) => n.id === node.parent) ?? null);

/** The parent chain, root first, `name` last; bounded by the read's
 *  size, so a malformed cycle cannot hang it. */
function chainOf(tree: StationTree, name: string): ReadonlyArray<TreeNode> {
  const chain: TreeNode[] = [];
  let at: TreeNode | null = tree.byName.get(name) ?? null;
  for (let i = 0; at !== null && i <= tree.nodes.length; i += 1) {
    chain.unshift(at);
    at = parentOf(tree, at);
  }
  return chain;
}

export function breadcrumb(tree: StationTree, name: string, reads: CardReads): ReadonlyArray<Crumb> {
  const troubled = tree.nodes
    .map((n) => n.name)
    .filter((n) => stationCard(tree, n, reads)?.plate.state === 'troubled');
  return chainOf(tree, name).map((n) => {
    const below = subtree(tree, n.name);
    return { name: n.name, href: stationHref(n.name), current: n.name === name, troubled: troubled.filter((t) => below.has(t)) };
  });
}

/** The walk upstream: the station `name` stands in, at its page. Derived
 *  from the tree read and never declared on a row (backlog a1c0a0ff,
 *  design deba3000): seven rows hand-typed `/system/yard` here, a route
 *  retired on 2026-08-31, and a re-parented station would have kept
 *  pointing at its old home. Null on the root, which stands in nothing. */
export type Upstream = Readonly<{ name: string; title: string; href: string }>;

export function upstreamOf(tree: StationTree, name: string): Upstream | null {
  const node = tree.byName.get(name);
  const parent = node === undefined ? null : parentOf(tree, node);
  return parent === null ? null : { name: parent.name, title: parent.title, href: stationHref(parent.name) };
}

// ---------------------------------------------------------------------
// 1 The constraint band.
// ---------------------------------------------------------------------

export type Segment = Readonly<{
  key: string;
  /** A part's name, `floating`, `root slack`, `not <station>'s`, the
   *  station's own name for its draw, `other stations`, `free at the root`. */
  label: string;
  amount: number | null;
  /** What the claims record in use inside the segment; null with the
   *  band's note when the record cannot say. */
  inUse: number | null;
  free: number | null;
  /** Someone else's share of a pool this station stands in. */
  hatched: boolean;
}>;

export type PoolBand = Readonly<{
  pool: string;
  unit: string;
  /** The estate node's label, for a `host:` pool. */
  host: string | null;
  group: PoolGroupName;
  /** The root pool's capacity; null unknown, never 0. */
  capacity: number | null;
  segments: ReadonlyArray<Segment>;
  inUseNote: CardPool['inUseNote'];
  /** What the band says beside its figures, each a sentence. */
  notes: ReadonlyArray<string>;
}>;

export function constraintBand(tree: StationTree, name: string, reads: CardReads): ReadonlyArray<PoolBand> {
  const node = tree.byName.get(name);
  if (node === undefined) return [];
  const isRoot = node.parent === null;
  const inside = subtree(tree, name);
  const { claims } = reads;
  const ready = claims.kind === 'ready' && !claims.data.sourceUnavailable ? claims.data : null;
  // A read narrowed to a subtree (backlog d05e29a7) holds no claim
  // standing outside it, so what is used there is unread, never zero.
  const narrowed = ready !== null && ready.within !== null;

  // In use, by the card's own rule (`poolUse`): nothing recorded against
  // the pool is not nothing in use, one claim the record cannot place
  // could be in any segment, and one claim recording no allocation could
  // be drawing on any segment — the band spans the whole pool, so every
  // claim is counted against it (backlog f23ce786).
  const usage = (pool: string): Readonly<{ note: CardPool['inUseNote']; counts: string | null; used: ((at: (s: string) => boolean) => number) | null }> => {
    if (claims.kind === 'loading') return { note: 'reading', counts: null, used: null };
    if (claims.kind === 'failed') return { note: 'in use unread', counts: null, used: null };
    if (ready === null) return { note: 'in use stale', counts: null, used: null };
    const use = poolUse(ready.claims, pool, () => true, true);
    if (!use.known) return { note: 'in use not recorded', counts: use.counts, used: null };
    return { note: null, counts: null, used: (at) => sum(use.on.filter((a) => a.station !== null && at(a.station)).map((a) => a.amount)) };
  };

  // The pools: the root holds every one; a station what its row
  // allocates, then any its claims draw on without holding.
  const drawn = (ready?.claims ?? [])
    .flatMap((c) => c.allocations.filter((a) => a.station !== null && inside.has(a.station)).map((a) => a.pool));
  const names = isRoot ? tree.pools.map((p) => p.name) : [...new Set([...Object.keys(node.allocation), ...drawn])];
  const declaredOf = (p: string) => tree.pools.find((x) => x.name === p);
  const ordered = GROUPS.flatMap((g) => names.filter((p) => groupOf(p, declaredOf(p)) === g));
  const kids = node.children.map((c) => tree.byName.get(c)).filter((k): k is TreeNode => k !== undefined);

  return ordered.map((pool): PoolBand => {
    const declared = declaredOf(pool);
    const capacity = declared?.capacity ?? null;
    const { note, counts, used } = usage(pool);
    const notes: string[] = counts === null ? [] : [counts];
    const seg = (key: string, label: string, amount: number | null, at: ((s: string) => boolean) | null, hatched = false): Segment => {
      const inUse = used === null || at === null ? null : used(at);
      return { key, label, amount, inUse, free: inUse === null || amount === null ? null : amount - inUse, hatched };
    };

    const holds = isRoot || node.allocation[pool] !== undefined;
    const segments = ((): ReadonlyArray<Segment> => {
      if (holds) {
        const held = isRoot ? capacity : (node.allocation[pool] ?? null);
        const parts = kids.filter((k) => k.allocation[pool] !== undefined);
        const partTrees = parts.map((k) => subtree(tree, k.name));
        const given = parts.map((k) => k.allocation[pool] ?? null);
        const rest = held === null || given.some((g) => g === null) ? null : held - sum(given.map((g) => g ?? 0));
        if (rest !== null && rest < 0) notes.push(`the parts hold ${-rest} more than ${name} holds`);
        const own = (s: string) => inside.has(s) && !partTrees.some((t) => t.has(s));
        return [
          ...parts.map((k, i) => seg(`part:${k.name}`, k.name, given[i] ?? null, (s) => partTrees[i]!.has(s))),
          ...(rest === null || rest < 0 ? [] : [seg(isRoot ? 'slack' : 'floating', isRoot ? 'root slack' : 'floating', rest, own)]),
          ...(isRoot || capacity === null || held === null ? [] : [seg('outside', `not ${name}'s`, capacity - held, narrowed ? null : (s) => !inside.has(s), true)]),
        ];
      }
      // A shared root pool this station draws on without holding a share.
      if (used === null) return [];
      const mine = used((s) => inside.has(s));
      const others = narrowed ? null : used((s) => !inside.has(s));
      const free = capacity === null || others === null ? null : capacity - mine - others;
      return [
        { key: 'draw', label: name, amount: mine, inUse: mine, free: 0, hatched: false },
        { key: 'others', label: 'other stations', amount: others, inUse: others, free: others === null ? null : 0, hatched: true },
        ...(capacity === null ? [] : [{ key: 'free', label: 'free at the root', amount: free, inUse: free === null ? null : 0, free, hatched: false }]),
      ];
    })();

    // What used to follow here on EVERY pool — the narrowed read, a pool
    // the tree serves no row for, capacity unknown, the host gap, "room
    // not stated", the keeper and the row version — is said once for
    // the strip by `poolsCaveat`, or by the figure itself (design
    // 08d1530e item 7). A note left here is an anomaly in THIS pool.
    if (declared?.hostGap) notes.push(declared.hostGap);
    return {
      pool,
      unit: declared?.unit ?? '',
      host: declared?.host ?? null,
      group: groupOf(pool, declared),
      capacity,
      segments,
      inUseNote: note,
      notes,
    };
  });
}

/** What the pool strip says ONCE, under every pool (design 08d1530e item
 *  7): why in use is not drawn, naming the pools only when some are and
 *  some are not, and that use outside a narrowed read is not read. Null
 *  when every figure on the strip speaks for itself. */
export function poolsCaveat(bands: ReadonlyArray<PoolBand>, name: string, reads: CardReads): string | null {
  const said: string[] = [];
  const noted = bands.filter((b) => b.inUseNote !== null);
  const which = (bs: ReadonlyArray<PoolBand>) => (bs.length === noted.length && noted.length === bands.length ? 'these pools' : bs.map((b) => b.pool).join(', '));
  const by = (note: PoolBand['inUseNote']) => noted.filter((b) => b.inUseNote === note);
  if (by('in use not recorded').length > 0) said.push(`In use is not recorded for ${which(by('in use not recorded'))} yet, so the strips show shares, not use.`);
  if (by('in use unread').length > 0) said.push('In use is unread: the claims read failed.');
  if (by('in use stale').length > 0) said.push('In use is stale: the claims read says its source could not be read.');
  if (by('reading').length > 0) said.push('Reading the claims.');
  const narrowed = reads.claims.kind === 'ready' && !reads.claims.data.sourceUnavailable && reads.claims.data.within !== null;
  if (narrowed && bands.some((b) => b.segments.some((s) => s.hatched))) said.push(`Use outside ${name} is not read on this page.`);
  return said.length === 0 ? null : said.join(' ');
}

// ---------------------------------------------------------------------
// The page.
// ---------------------------------------------------------------------

/** Past this many cars a column says "+n more". */
export const COLUMN_MAX = 5;

export type WorkRow = Readonly<{ key: string; mark: string | null; holder: string; what: string; since: string | null; href: string | null }>;
export type WorkGroup = Readonly<{ part: string; here: boolean; rows: ReadonlyArray<WorkRow>; more: number }>;

export type WorkingColumn =
  | Readonly<{ state: 'loading' }>
  | Readonly<{ state: 'failed'; error: string }>
  | Readonly<{ state: 'ready'; groups: ReadonlyArray<WorkGroup>; unplaced: number }>;

export type StationPageModel = Readonly<{
  name: string;
  title: string;
  card: StationCardModel;
  crumbs: ReadonlyArray<Crumb>;
  upstream: Upstream | null;
  bands: ReadonlyArray<PoolBand>;
  /** The strip's caveat, said once (`poolsCaveat`). */
  poolsCaveat: string | null;
  parts: Readonly<{ stations: ReadonlyArray<string>; machines: ReadonlyArray<Readonly<{ name: string; holding: number | null }>> }>;
  working: WorkingColumn;
}>;

const jobHref = (id: string): string => `/ux/jobs/${encodeURIComponent(id)}`;
const clipped = <T>(rows: ReadonlyArray<T>) => ({ shown: rows.slice(0, COLUMN_MAX), more: Math.max(0, rows.length - COLUMN_MAX) });

/** What opening `name` shows, from the reads; null for a station the
 *  tree read does not hold. */
export function stationPage(tree: StationTree, name: string, reads: PageReads): StationPageModel | null {
  const node = tree.byName.get(name);
  const card = cardReads(reads);
  const model = stationCard(tree, name, card);
  if (node === undefined || model === null) return null;
  const inside = subtree(tree, name);
  const kids = node.children.filter((c) => tree.byName.has(c));
  const kidTrees = kids.map((k) => [k, subtree(tree, k)] as const);

  const held = reads.claims.kind === 'ready' ? reads.claims.data.held : null;
  const working = (held ?? []).filter((c) => c.station !== null && inside.has(c.station));

  const workingColumn = ((): WorkingColumn => {
    if (reads.claims.kind !== 'ready') return reads.claims.kind === 'failed' ? { state: 'failed', error: reads.claims.error } : { state: 'loading' };
    const partOf = (station: string): string => kidTrees.find(([, t]) => t.has(station))?.[0] ?? name;
    const rows = working;
    const groups = [name, ...kids]
      .map((part): WorkGroup => {
        const mine = rows
          .filter((c) => partOf(c.station!) === part)
          .map((c, i): WorkRow => ({
            key: c.stepId === null ? `${c.jobId ?? part}-${i}` : `${c.jobId ?? ''}:${c.stepId}`,
            mark: c.holderKind === null ? null : HOLDER_MARK[c.holderKind],
            holder: c.holder ?? 'holder not recorded',
            // The claim carries no title, and the queue read that lent
            // one went with In (1.46 MB for five titles): its protocol.
            what: c.workflowKind ?? 'a packet',
            // The instant itself: the page draws it local and UTC side by
            // side (user-feedback 0b1fd59f, design b03ef2d4).
            since: c.since,
            href: c.jobId === null ? null : jobHref(c.jobId),
          }));
        const { shown, more } = clipped(mine);
        return { part, here: part === name, rows: shown, more };
      })
      .filter((g) => g.rows.length > 0);
    return { state: 'ready', groups, unplaced: (held ?? []).filter((c) => c.station === null).length };
  })();

  const bands = constraintBand(tree, name, card);
  return {
    name,
    title: node.title,
    card: model,
    crumbs: breadcrumb(tree, name, card),
    upstream: upstreamOf(tree, name),
    bands,
    poolsCaveat: poolsCaveat(bands, name, card),
    parts: {
      stations: kids,
      machines: node.machines.map((m) => ({ name: m, holding: held === null ? null : held.filter((c) => c.holder === m).length })),
    },
    working: workingColumn,
  };
}
