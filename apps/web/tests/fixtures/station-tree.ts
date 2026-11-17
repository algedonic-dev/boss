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
];

export const idOf = (name: string): string => `id-${name}`;

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
type QueueMember = Readonly<{ id: string; title: string; kind?: string; priority?: string }>;

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
    opened_on: '2026-11-08',
    opened_at: '2026-11-08T05:00:00Z',
    metadata: {},
  })),
});

// MOTION ON THE STATION PAGE (design c4854fd4 E3, backlog 87c5293f).

/** One protocol step as a workflow row serves it: `title` is its slug.
 *  Placement is T6's — `metadata_defaults.at` a station's stable id, or
 *  `fulfilled_by` a hand-off to another protocol — else the step's
 *  station spec (`executor = "equipment:<station>"`) or its audience. */
type WfStep = Readonly<{
  slug: string;
  kind?: string;
  at?: string;
  fulfilledBy?: string;
  executor?: string;
  audienceStation?: string;
  terminal?: boolean;
}>;

export const workflowRow = (kind: string, owner: string | null, steps: ReadonlyArray<WfStep>, version = 1) => ({
  kind,
  version,
  status: 'active',
  label: `${kind} — a protocol`,
  metadata: { department: 'it', owner_role: 'platform-admin', ...(owner === null ? {} : { owner: idOf(owner) }) },
  steps: steps.map((s) => ({
    title: s.slug,
    kind: s.kind ?? 'task',
    ready_when: 'true',
    title_template: `The ${s.slug} step`,
    ...(s.executor === undefined ? {} : { executor: s.executor }),
    ...(s.terminal === true ? { terminal: { outcome: s.slug } } : {}),
    metadata_defaults: {
      ...(s.kind === 'trigger' ? { trigger_kind: 'operator' } : {}),
      ...(s.at === undefined ? {} : { at: s.at }),
      ...(s.fulfilledBy === undefined ? {} : { fulfilled_by: s.fulfilledBy }),
      ...(s.audienceStation === undefined ? {} : { audience: { station: s.audienceStation } }),
    },
  })),
});

/** `GET /api/workflows`: ship-a-change and pr-train owned by engineering,
 *  gate-run by gates (a descendant), roll-a-train by operations, and one
 *  row from before T6 car A that names no owner. */
export const workflowsRead = () => [
  workflowRow('ship-a-change', 'engineering', [
    { slug: 'opened', kind: 'trigger' },
    { slug: 'scope' },
    { slug: 'build' },
    { slug: 'gate', executor: 'equipment:gates' },
    { slug: 'review', at: idOf('loading-dock') },
    { slug: 'proven', executor: 'equipment:probe-shed' },
    { slug: 'landed-review', audienceStation: 'forge-ci' },
    { slug: 'merged', kind: 'outcome', terminal: true },
  ]),
  workflowRow('pr-train', 'engineering', [
    { slug: 'opened', kind: 'trigger' },
    { slug: 'ci', at: idOf('forge-ci') },
    { slug: 'deploy', fulfilledBy: 'roll-a-train' },
    { slug: 'done', kind: 'outcome', terminal: true },
  ]),
  workflowRow('gate-run', 'gates', [{ slug: 'launched' }, { slug: 'record-verdict', at: idOf('gates') }]),
  workflowRow('roll-a-train', 'operations', [{ slug: 'roll', at: idOf('deploy') }]),
  workflowRow('backlog-item', null, [{ slug: 'triage' }]),
];

/** One open packet as `GET /api/jobs/{id}` serves it, standing at `at`:
 *  every line step before it completed, `at` active, the rest pending. */
export const openPacket = (id: string, title: string, at: string | null, kind = 'ship-a-change', status = 'open') => {
  const slugs = workflowsRead().find((w) => w.kind === kind)?.steps.map((s) => s.title) ?? [];
  const here = at === null ? -1 : slugs.indexOf(at);
  return {
    id,
    kind,
    title,
    status,
    opened_at: '2026-11-08T05:00:00Z',
    metadata: {},
    steps: slugs.map((slug, i) => ({
      id: `${id}-${slug}`,
      spec_slug: slug,
      title: `The ${slug} step`,
      status: here < 0 ? (i === 0 ? 'completed' : 'pending') : i < here ? 'completed' : i === here ? 'active' : 'pending',
    })),
  };
};

/** `GET /api/jobs?kind=<kind>&status=open`: a page of packets and the total. */
export const packetsRead = (rows: ReadonlyArray<ReturnType<typeof openPacket>>, total: number = rows.length) => ({
  data: rows,
  total,
  limit: 200,
  offset: 0,
  full: false,
});

// THE STATION LINE (design 08d1530e car B, backlog 9d0b0944).

/** `GET /api/jobs/kinds`: per protocol, its open packets and how many
 *  stand on each pinned version. */
export const kindsRead = (rows: ReadonlyArray<Readonly<{ kind: string; open: number; byVersion?: Readonly<Record<string, number>> }>>) => ({
  kinds: rows.map((r) => ({ kind: r.kind, open: r.open, open_by_version: r.byVersion ?? { '1': r.open }, packets: r.open, newest_terminal: null })),
});

/** One open gate-run as the server serves it: its record-verdict step
 *  stays READY and unclaimed whether it waits for a bay or holds one; only
 *  its metadata says which (`gate_reservation`, `gate_bay`). `bay` null
 *  is a packet with neither key, 'queued' one waiting for a bay. */
export const gateRunPacket = (id: string, title: string, bay: string | 'queued' | null, queuedAt = '2026-11-08T11:40:00Z') => {
  const p = openPacket(id, title, 'record-verdict', 'gate-run');
  const metadata =
    bay === null ? {}
    : bay === 'queued' ? { gate_reservation: { consumed: false, contract_version: 1, queued_at: queuedAt } }
    : {
      gate_reservation: { consumed: true, contract_version: 1, queued_at: queuedAt },
      gate_bay: { bay, bound: 2, occupied: 1, station: { name: 'gates', version: 1 }, station_bays: 2 },
    };
  return {
    ...p,
    metadata,
    steps: p.steps.map((s) => (s.spec_slug === 'record-verdict' ? { ...s, status: 'ready' } : { ...s, completed_at: '2026-11-08T11:40:00Z' })),
  };
};

/** One `move` frame of GET /api/yard/moves/stream (moves.rs `Recorded`). */
export const moveFrame = (seq: number, packet: string, label: string, from: string | null, to: string | null, kind = 'ship-a-change', at = '2026-11-08T12:11:48Z') => ({
  seq,
  at,
  packet,
  kind,
  label,
  from,
  to,
  declared: true,
  cause_event_id: `e-${seq}`,
  cause_seq: 4_500_000 + seq,
  cause_kind: 'jobs.step.completed',
  handoff_from: null,
  lineage: null,
  aboard: [],
  terminal: null,
});
