// THE TREE READ, STANDING IN FOR THE SERVER — `GET /api/stations?under=`
// as crates/core/boss-jobs/src/http/stations.rs `station_tree_read`
// serves it (T2+T5 of design 4b27a65a): the subtree root first, then
// depth-first with siblings in name order, each row `{station, address,
// depth, children}`; beside it the pools the subtree holds from, each
// host pool joined to its estate node or carrying `host_unavailable`;
// and every gap named.
//
// The station NAMES are the platform bundle's (infra/platform/stations),
// because the drawing places the yard's regions on them. The company's
// own name is NOT the bundle's: the drawing must find the root by the
// tree's shape, never by a name, and a fixture that used the real one
// would let a page that names it pass.
//
// The IDS of the stations a catalog page is hosted at ARE the bundle's
// (nav-catalog.ts `STATION`), the root's included: the catalog names a
// host by id and the SPA places each page at its host's address in the
// tree (backlog 0bc2eb68), so a mocked spec opens a hosted page only if
// this tree holds its host under the same id. Every other id is
// `id-<name>`.

import { ROUTE_CATALOG, STATION, type NavItem } from '../../src/shell/nav-catalog';
import { addressesOf, fillPath, pagePath } from '../../src/shell/page-address';
import { parseTree } from '../../src/it/tree/station-tree';

type Row = Readonly<{
  name: string;
  parent?: string;
  capacity?: number;
  machines?: ReadonlyArray<string>;
  allocation?: Readonly<Record<string, string>>;
  edges?: ReadonlyArray<string>;
  mode?: 'standing' | 'ad-hoc';
  kind?: string;
  bays?: number;
  department?: string;
  /** The department line design c4854fd4 names on a top-level row. */
  line?: string;
  /** The admission predicate; a `station:<name>` tag when not given. */
  predicate?: Readonly<Record<string, unknown>>;
  /** Where the row stands on its parent's floor, as car A serves it
   *  (`placement`, backlog e3a927c3): a box in floor units. */
  placement?: Readonly<{ x: number; y: number; w: number; h: number }>;
}>;

const ROOT = 'company';

/** The tenant's tree as rows: name, the parent's NAME (turned into its
 *  id below), and the tree fields a card draws. */
export const TREE_ROWS: ReadonlyArray<Row> = [
  { name: ROOT, machines: ['automation:dispatcher'] },
  { name: 'engineering', parent: ROOT, allocation: { 'host:w-1': '24' }, line: 'it' },
  { name: 'executive', parent: ROOT },
  { name: 'operations', parent: ROOT, line: 'hosting' },
  { name: 'forge', parent: 'engineering' },
  { name: 'forge-ci', parent: 'forge' },
  { name: 'gates', parent: 'engineering', kind: 'equipment', capacity: 2, bays: 2, department: 'it', machines: ['automation:gate-runner'], allocation: { 'host:w-1': '8' }, edges: ['loading-dock'] },
  { name: 'loading-dock', parent: 'engineering', capacity: 1, machines: ['automation:train-conductor'], edges: ['probe-shed'] },
  { name: 'probe-shed', parent: 'engineering' },
  { name: 'design-review', parent: 'executive', edges: ['design-decided'] },
  { name: 'design-decided', parent: 'executive' },
  { name: 'converge', parent: 'operations' },
  { name: 'deploy', parent: 'operations', capacity: 1 },
  { name: 'edge', parent: 'operations' },
  { name: 'estate', parent: 'operations' },
  { name: 'repair', parent: 'operations', edges: ['loading-dock'] },
  // The three hosts of catalog pages the drawing needs nothing else of.
  { name: 'design', parent: 'engineering' },
  { name: 'vault', parent: 'operations' },
  { name: 'finance', parent: ROOT },
];

/** The bundle's id for the root and every named host, else `id-<name>`. */
const BUNDLE_IDS: Readonly<Record<string, string>> = { ...STATION, [ROOT]: STATION.algedonic };
export const idOf = (name: string): string => BUNDLE_IDS[name] ?? `id-${name}`;

/** The name of the row an `under` names — by its id, or by its name. */
export const nameOf = (under: string): string => TREE_ROWS.find((r) => idOf(r.name) === under)?.name ?? under;

const childrenOf = (rows: ReadonlyArray<Row>, name: string): ReadonlyArray<string> =>
  rows.filter((r) => r.parent === name).map((r) => r.name).sort();

const addressOf = (rows: ReadonlyArray<Row>, name: string): string => {
  const row = rows.find((r) => r.name === name);
  return row?.parent === undefined ? `/${name}` : `${addressOf(rows, row.parent)}/${name}`;
};

const depthOf = (rows: ReadonlyArray<Row>, name: string): number => {
  const row = rows.find((r) => r.name === name);
  return row?.parent === undefined ? 0 : depthOf(rows, row.parent) + 1;
};

/** One served row, its station spec as StationSpec serializes it. */
export const treeNode = (rows: ReadonlyArray<Row>, r: Row) => ({
  station: {
    name: r.name,
    version: 1,
    status: 'active',
    title: `${r.name} — a station`,
    kind: r.kind ?? 'group',
    discipline: ['age'],
    id: idOf(r.name),
    ...(r.parent === undefined ? {} : { parent: idOf(r.parent) }),
    mode: r.mode ?? 'standing',
    ...(r.capacity === undefined ? {} : { capacity: r.capacity }),
    ...(r.bays === undefined ? {} : { bays: r.bays }),
    ...(r.department === undefined ? {} : { department: r.department }),
    ...(r.line === undefined ? {} : { line: r.line }),
    ...(r.machines === undefined ? {} : { machines: r.machines }),
    ...(r.allocation === undefined ? {} : { allocation: r.allocation }),
    ...(r.edges === undefined ? {} : { edges: r.edges }),
    ...(r.placement === undefined ? {} : { placement: r.placement }),
    predicate: r.predicate ?? { tags_any: [`station:${r.name}`] },
    created_at: '2026-11-06T00:00:00Z',
  },
  address: addressOf(rows, r.name),
  depth: depthOf(rows, r.name),
  children: childrenOf(rows, r.name),
});

/** Depth-first from `top`, siblings in name order — the server's order. */
const ordered = (rows: ReadonlyArray<Row>, top: string): ReadonlyArray<Row> => {
  const row = rows.find((r) => r.name === top);
  return row === undefined ? [] : [row, ...childrenOf(rows, top).flatMap((c) => ordered(rows, c))];
};

export const POOLS = [
  {
    pool: { name: 'host:w-1', version: 1, status: 'active', created_at: '2026-11-06T00:00:00Z', unit: 'vCPU', kind: 'flow', shape: 'capacity', capacity: { state: 'configured', amount: '32' }, keeper: 'operations' },
    host: { id: 'w-1', label: 'w-1 · the build node', role: 'worker', retired: false },
  },
  {
    pool: { name: 'host:w-2', version: 1, status: 'active', created_at: '2026-11-06T00:00:00Z', unit: 'vCPU', kind: 'flow', shape: 'capacity', capacity: { state: 'configured', amount: '32' }, keeper: 'operations' },
    host_unavailable: 'no estate node `w-2` is declared',
  },
  {
    pool: { name: 'people:platform-admin', version: 1, status: 'active', created_at: '2026-11-06T00:00:00Z', unit: 'people', kind: 'flow', shape: 'capacity', capacity: { state: 'configured', amount: '1' } },
  },
  {
    pool: { name: 'robots:opus', version: 1, status: 'active', created_at: '2026-11-06T00:00:00Z', unit: 'slots', kind: 'flow', shape: 'capacity', capacity: { state: 'configured', amount: '3' } },
  },
  {
    pool: { name: 'disk:ci-workspaces', version: 1, status: 'active', created_at: '2026-11-06T00:00:00Z', unit: 'GB', kind: 'stock', shape: 'stock', capacity: { state: 'unknown' } },
  },
] as const;

/** The tree read under `top` (a name), as the server answers it. */
export function treeRead(top: string = ROOT, rows: ReadonlyArray<Row> = TREE_ROWS) {
  const data = ordered(rows, top).map((r) => treeNode(rows, r));
  const isRoot = rows.find((r) => r.name === top)?.parent === undefined;
  const held = new Set(data.flatMap((n) => Object.keys(n.station.allocation ?? {})));
  return {
    under: top,
    data,
    total: data.length,
    pools: POOLS.filter((p) => isRoot || held.has(p.pool.name)),
    pools_unavailable: null,
    hosts_unavailable: null,
    hosts_without_pool: isRoot ? ['forge'] : [],
    outside_tree: ['my-watchlist'],
  };
}

/** The tree above as the SPA places pages on it (backlog 0bc2eb68): each
 *  station's prefix, read from the same tree read the floor serves. */
export const ADDRESSES = addressesOf(parseTree(treeRead()));

/** A catalogued page's path in this tree, its parameters filled — what a
 *  spec opens, so no spec spells a page's prefix. Throws for a page the
 *  tree cannot place: a fixture that loses a host fails loudly. */
export function pageAt(id: keyof typeof ROUTE_CATALOG, params: Readonly<Record<string, string>> = {}): string {
  const base = pagePath(ROUTE_CATALOG[id] as NavItem, ADDRESSES);
  const path = base === null ? null : fillPath(base, params);
  if (path === null) throw new Error(`the fixture tree cannot place the catalog page \`${id}\``);
  return path;
}

/** `GET /api/stations` — every active row, the list the page finds the
 *  root in when no `under` is asked. One row outside the tree (no id). */
export const stationList = () => ({
  data: [
    ...TREE_ROWS.map((r) => treeNode(TREE_ROWS, r).station),
    { name: 'my-watchlist', version: 1, status: 'active', title: 'My watchlist', kind: 'group', discipline: ['age'], predicate: {}, created_at: '2026-11-06T00:00:00Z' },
  ],
  total: TREE_ROWS.length + 1,
});

/** `GET /api/stations/load`, in the wire shape marshalling.ts reads. */
export const stationLoad = () => ({
  data: [
    { station: 'gates', kind: 'group', workflow_kind: 'gate-run', depth: 3, oldest_age_days: 0.4, also_elsewhere: 0 },
    { station: 'loading-dock', kind: 'group', workflow_kind: 'ship-a-change', depth: 30, oldest_age_days: 2.5, also_elsewhere: 0 },
  ],
  distinct_packets: 33,
});

/** `GET /api/claims` — the open claims and the read's own projection
 *  marks, in the wire shape claims_console.rs serves (design c4854fd4's
 *  card reads `station` and `allocations` off each claim, and nothing
 *  else). As it stood on 2026-11-08: no claim carries an allocation yet
 *  (backlog 35e9babf), and a claim recorded before the history rebuild
 *  names no station. */
export const claimsRead = (claims: ReadonlyArray<Readonly<Record<string, unknown>>> = CLAIMS, sourceUnavailableAt: string | null = null) => ({
  projection: { generation: 0, through: 4464096, observed_audit_head: 4464096, sequence_distance: 0, source_unavailable_at: sourceUnavailableAt },
  claims,
  waiting_on: [],
  pools: [],
  binding: null,
});

export const CLAIMS: ReadonlyArray<Readonly<Record<string, unknown>>> = [
  { step_id: 's-1', job_id: 'j-1', holder: 'agent-claude', executor_kind: 'robot', station: 'gates', station_unavailable: null, allocations: [] },
  { step_id: 's-2', job_id: 'j-2', holder: 'agent-codex', executor_kind: 'robot', station: null, station_unavailable: 'history-not-rebuilt', allocations: [] },
];

/** A straight chain `n` stations deep under one root: the adversarial
 *  deep tree. */
export function chainRows(n: number): ReadonlyArray<Row> {
  return Array.from({ length: n }, (_, i): Row => (i === 0 ? { name: 's0' } : { name: `s${i}`, parent: `s${i - 1}` }));
}

// THE STATION PAGE'S OWN READS (design c4854fd4 E2, backlog 72d58e00).

/** A held claim as claims_console.rs serves it, with the fields the page's
 *  Working column reads: who holds it, what kind of executor, since when. */
export const heldClaim = (
  job: string,
  station: string | null,
  over: Readonly<Record<string, unknown>> = {},
): Readonly<Record<string, unknown>> => ({
  step_id: `s-${job}`,
  job_id: job,
  holder: 'agent-claude',
  executor_kind: 'robot',
  executor_kind_basis: 'recorded',
  station,
  station_unavailable: station === null ? 'history-not-rebuilt' : null,
  allocations: [],
  resource_acquisition: null,
  since: '2026-11-08T07:00:00Z',
  ...over,
});

/** One queue member, as `GET /api/stations/{name}/queue` serves a packet. */
type QueueMember = Readonly<{ id: string; title: string; kind?: string; priority?: string; openedAt?: string }>;

/** `GET /api/stations/{name}/queue` — the station's members in its
 *  `discipline` order (stations.rs `station_queue`). */
export const queueRead = (station: string, members: ReadonlyArray<QueueMember> = [], discipline: ReadonlyArray<string> = ['priority', 'age']) => ({
  station,
  kind: 'group',
  discipline,
  terminal_window_days: null,
  lens: null,
  total: members.length,
  data: members.map((m) => ({
    id: m.id,
    kind: m.kind ?? 'ship-a-change',
    title: m.title,
    priority: m.priority ?? 'standard',
    status: 'open',
    opened_on: (m.openedAt ?? '2026-11-08').slice(0, 10),
    opened_at: m.openedAt ?? '2026-11-08T05:00:00Z',
    metadata: {},
  })),
});
