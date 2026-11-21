// THE STATION PAGE — what opening a station shows above its floor (design
// c4854fd4 E2, decided by David 2026-11-07 23:38Z; backlog 72d58e00).
//
// The header: the breadcrumb, the walk upstream, and the reads the answer
// band takes (station-answer.ts). The BODY is the live floor
// (station-floor.ts, backlog dd0eb058, decided by David 2026-11-19 on the
// prototype canvas), which replaced what stood here: each sub-station as
// its own card (the parts grid), the pools as strips segmented by holder
// with their caveat said once, and the Working list by part. The floor
// draws the same claims as tokens in each station's working slots.
//
// The breadcrumb is the parent chain back to the root, and trouble climbs
// it: every ancestor names the stations below it whose plate is troubled
// (the algedonic signal, design c4854fd4). A plate is troubled only on what a read
// judges — a station over its limit; no read carries machine health yet.
//
// Pure functions of the reads, pinned in station-page.test.ts;
// StationPage.svelte owns the strokes.

import type { Remote } from '../../data/remote';
import type { StationLoadRow } from '../marshalling/marshalling';
import {
  parseClaims, quantity, stationCard, subtree, treeHref,
  type CardReads, type ClaimsRead, type StationCardModel, type StationTree, type TreeNode,
} from './station-tree';

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj | null => (v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Obj) : null);
const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);

// ---------------------------------------------------------------------
// The address.
// ---------------------------------------------------------------------

/** A station's page: `/tree/<name>`, below the floor the company root
 *  hosts (`treeHref`; it was `/it/tree/<name>` until backlog 92bfcb04).
 *  Opening a part swaps the page, never zooms. The router reads the name
 *  back as its own segment (router.ts HOSTED), so no parser lives here. */
export function stationHref(name: string): string {
  return `${treeHref(null)}/${encodeURIComponent(name)}`;
}

// ---------------------------------------------------------------------
// The reads the page adds to the card's.
// ---------------------------------------------------------------------

/** The taxonomy's three executors (docs/design/factory-taxonomy-v2.md). */
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
// The page.
// ---------------------------------------------------------------------

export type StationPageModel = Readonly<{
  name: string;
  title: string;
  card: StationCardModel;
  crumbs: ReadonlyArray<Crumb>;
  upstream: Upstream | null;
}>;

/** What opening `name` shows above its floor, from the reads; null for a
 *  station the tree read does not hold. The body is the floor's
 *  (station-floor.ts). */
export function stationPage(tree: StationTree, name: string, reads: PageReads): StationPageModel | null {
  const node = tree.byName.get(name);
  const card = cardReads(reads);
  const model = stationCard(tree, name, card);
  if (node === undefined || model === null) return null;
  return {
    name,
    title: node.title,
    card: model,
    crumbs: breadcrumb(tree, name, card),
    upstream: upstreamOf(tree, name),
  };
}
