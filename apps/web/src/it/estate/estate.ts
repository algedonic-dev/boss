// The estate page's data — the hardware registry rendered instead of
// prose (59ef456a; on 2026-08-30 three separate hand-written accounts
// of the machines were wrong in the same direction, because none was
// connected to the machines — this lens is).
//
// Three readers, all guest-safe:
//   GET /api/estate/nodes         — what we MEANT to have (declared)
//   GET /api/estate/observations  — what a look FOUND (events), read
//                                   one series at a time (75027a93)
//   GET /api/estate/comparisons   — the difference, computed on event
//
// Every fetch lands in a Remote<T>: this page's whole subject is
// absence lying, so an outage must render as failure, never as an
// empty estate (the false-empty family).

import { fetchRemote, type Remote } from '../../data/remote';
import { sinceText } from '../yard/yard-floor';
import { journeyText } from '../yard/yard-status';

export type EstateNode = Readonly<{
  id: string;
  label: string;
  address: string | null;
  role: string;
  /** Every role the node DECLARES (Classes of `node`, 202609120300) —
   *  the set a managed host derives its unit roster from. `role` above
   *  is the primary one the page keys on; this is the full set. Empty
   *  when the node declares nothing. */
  roles: readonly string[];
  cpu: number | null;
  memory_gb: number | null;
  disk_gb: number | null;
  notes: string | null;
  retired: boolean;
}>;

/** One unit on a `host-units` reading, as observe-units.sh records it —
 *  only the keys the page reads. */
export type ObservedUnit = Readonly<{ unit?: string; healthy?: boolean }>;

export type ObservedNode = Readonly<{
  id: string;
  address?: string | null;
  cpu?: number | null;
  memory_gb?: number | null;
  disk_gb?: number | null;
  disk_free_gb?: number | null;
  ready?: boolean;
  /** The `host-units` scope's one node carries the units it watched. */
  units?: readonly ObservedUnit[];
}>;

export type Observation = Readonly<{
  observed_at: string;
  observer: string;
  scope: string;
  nodes: readonly ObservedNode[];
}>;

export type ComparisonCounts = Readonly<{
  observed: number;
  participating_declared: number;
  observed_not_declared: number;
  declared_not_observed: number;
  drift: number;
  /** Machines below the disk floor — free under 16 GiB or under 35% of
   *  capacity. Optional because every row recorded before a520737f
   *  predates the key on the cluster scope. */
  disk_tight?: number;
  /** Machines whose free-space reading could not be taken. The kubelet
   *  read is best-effort so the rest of the observation survives losing
   *  it; this is the count that keeps going blind distinguishable from
   *  having room. */
  disk_unmeasured?: number;
  /** Observed machines reporting NotReady (ea5e0e8b). The server counts
   *  them since that car; an older row carries only the finding, and
   *  the parser counts that instead. */
  not_ready?: number;
  /** Recent dispatcher dead letters with no record (c2373cc4) — a HARD
   *  finding estate.alarm raises; cluster scope only. */
  dead_letters_unrecorded?: number;
}>;

export type Comparison = Readonly<{
  observed_at: string;
  scope: string;
  /** The host a SELF-SCOPED comparison is about — compare_host stamps
   *  it (estate_compare.rs, the raiser's series key); the cluster and
   *  door comparisons carry none, and neither does a literal built
   *  before this key was read, hence optional. */
  host?: string | null;
  counts: ComparisonCounts;
  /** The ids behind `counts.not_ready`, so the verdict names the sick
   *  machine rather than only counting it. */
  not_ready?: readonly string[];
  /** Why the dispatcher's dead-letter counters could not be read, or
   *  null when they were (estate_compare.rs dead_letter_finding: UNREAD
   *  is never reported as zero). Absent on host rows. */
  dispatcher_unread?: string | null;
}>;

// THE HOST COMPARISON (backlog 2d8d983b; page audit 2cff1d6e, GAP 1).
// Until this read the page rendered the cluster's verdict only, while
// every host row carried a finding: measured 2026-09-23, each forge
// row 15:17Z-16:17Z drift 1 (memory declared 30, observed 31), and
// boss-gcp's 10:25Z row disk_tight 1 (13 G free against a 17 G floor)
// plus drift 1. None of it reached the estate surface.
//
// SCOPED, not taken from the unscoped page of 20 the rest of the
// section reads (75027a93): forge compares every 15 minutes and
// boss-gcp once a day, so a mixed page spent by the five-minute series
// held boss-gcp's row about one hour in twenty-four.
//
// AND GROUPED PER HOST ON THE SERVER (backlog 725532ab). Scoped alone,
// a daily host still fell off: measured 2026-09-25 07:50Z, the page of
// 50 held 50 of 768 host rows, all forge's, and boss-gcp's 10:25Z row
// was gone — so the page rendered a coverage line saying how far back
// it reached, a truthful statement of a missing answer. `latest_per=
// host` asks the question the section renders — each host's newest
// word — and its `total` counts HOSTS, so one read is the whole answer
// whenever rows == total, and the page owes every declared host a line.
export const HOST_COMPARISONS_READ = '/api/estate/comparisons?scope=host&latest_per=host&limit=50';

/** The host series read grouped: each host's newest row, and how many
 *  hosts the server counted (null from a reader that did not say). */
export type HostComparisonPage = Readonly<{
  rows: readonly Comparison[];
  total: number | null;
}>;

/** One line of the host comparison: a host and its newest comparison,
 *  or null when the read holds none for a host the registry declares. */
export type HostLine = Readonly<{ host: string | null; cmp: Comparison | null }>;

// THE LOOPS (backlog 0d9b2960; page audit 2cff1d6e, GAP 10). The
// estate is kept by loops — the hosts converge themselves, observe
// their units and the other hosts, the forge watches the cluster from
// outside, the runners answer ops-requests — and every run leaves a
// packet. This page showed none of them, so "did the loop run" had no
// answer here while ~2,000 packets of each kind sat in the log. The
// newest TERMINAL of each loop is that answer (outcome and age); an
// open packet is a run in flight, or one that never finished.
//
// PER HOST where the packet names one, and only there. Measured
// 2026-09-24: an ops-request carries `metadata.host`; the forge and
// boss-gcp converges stamp `node_id` on their run step; the watchdog,
// the cluster converge and both observers name no host (observe-host
// runs on BOTH the forge and boss-gcp under one kind, and neither
// packet says which). The page says "not named on the packet" rather
// than guess one from a unit file it cannot read.

/** A loop the page reads by kind. Pinned to infra/platform/workflows by
 *  estate.test.ts, so a renamed kind is a red test rather than a row
 *  that reads "never finished" forever. */
export type EstateLoop = Readonly<{ kind: string; label: string }>;

export const ESTATE_LOOPS: readonly EstateLoop[] = [
  { kind: 'maintenance-forge-converge', label: 'forge converge' },
  { kind: 'maintenance-boss-gcp-converge', label: 'boss-gcp converge' },
  { kind: 'maintenance-cluster-converge', label: 'cluster converge' },
  { kind: 'maintenance-cluster-watchdog', label: 'cluster watchdog' },
  { kind: 'maintenance-estate-observe-host', label: 'observe hosts' },
  { kind: 'maintenance-estate-observe-units', label: 'observe units' },
];

/** The ops-request loop is read once per host that DECLARES it answers
 *  them — the `ops-runner` role (infra/estate/roles.toml: "which hosts
 *  should be answering ... so silence has something to be silence
 *  from"). Its packets carry `metadata.host`, so the split is the
 *  server's filter, not this page's. */
export const OPS_REQUEST_KIND = 'ops-request';
export const OPS_RUNNER_ROLE = 'ops-runner';

/** The success terminals of the loops above; every other terminal they
 *  declare (failed, refused) renders as trouble. Held to the workflow
 *  files by estate.test.ts. */
export const LOOP_OK_OUTCOMES: ReadonlySet<string> = new Set(['completed', 'answered']);

/** One packet of a loop, reduced to what "did it run" needs. */
export type LoopPacket = Readonly<{
  id: string;
  status: string;
  /** `metadata.outcome`, stamped on close; null while open. */
  outcome: string | null;
  /** When it closed (a terminal) or opened (an open packet). */
  at: string | null;
  /** The host the packet names, or null when it names none. */
  host: string | null;
}>;

export type LoopPlan = Readonly<{ kind: string; label: string; host: string | null }>;

export type LoopRow = LoopPlan & Readonly<{
  /** Ready-and-null is a kind with no closed packet at all. */
  latest: Remote<LoopPacket | null>;
  open: Remote<readonly LoopPacket[]>;
}>;

// ONE SCOPED READ PER RENDERED SERIES (backlog 75027a93; page audit
// 2cff1d6e, GAP 4). The observations were one unscoped page of 20 rows
// across every scope, which the fastest series spends: measured
// 2026-09-27 17:14Z, that page held 11 host-units rows, 6 door, 2 host
// (both forge's) and 1 kubernetes-nodes of 6106, and boss-gcp's daily
// host reading (10:25Z) was not on it — so boss-gcp had no host line,
// and a slower series would have read "no observation recorded yet"
// while the log held it. The reader takes ?scope= and ?host= for
// exactly this (jobs.rs, list_estate_observations), so each series the
// page draws is its own read: the cluster's, and per host its `host`
// and `host-units` series. Observations carry no `host` stamp
// (4579f9b5), so ?latest_per=host cannot group them; ?host= reads a
// series by its first node's id, the way the estate alarm reads it.
//
// TEN rows each, not one: the verdict below needs the series' own
// cadence, which is measured from its gaps (the alarm reads 50; ten
// gives nine gaps, and a status page re-reads every minute).
export const SERIES_SAMPLE = 10;
export const CLUSTER_SCOPE = 'kubernetes-nodes';
export const HOST_SCOPE = 'host';
export const UNITS_SCOPE = 'host-units';
export const CLUSTER_OBSERVATIONS_READ = `/api/estate/observations?scope=${CLUSTER_SCOPE}&limit=${SERIES_SAMPLE}`;
/** The cluster verdict is the newest comparison of ITS scope — the
 *  unscoped page of 20 it came from was spent the same way. */
export const CLUSTER_COMPARISON_READ = `/api/estate/comparisons?scope=${CLUSTER_SCOPE}&limit=1`;

export function hostSeriesRead(scope: string, host: string): string {
  return `/api/estate/observations?scope=${scope}&host=${encodeURIComponent(host)}&limit=${SERIES_SAMPLE}`;
}

/** One series' read: its own rows, newest first, how many rows the
 *  server returned before they were kept to the series, and the
 *  server's count of the series (null from a reader that did not say). */
export type SeriesPage = Readonly<{
  rows: readonly Observation[];
  returned: number;
  total: number | null;
}>;

/** A series read kept to its series: the scope, and for a host series
 *  the host its first node names. A reader that predates ?host= answers
 *  the whole scope to every host's read, and a neighbour's rows counted
 *  as this host's would measure a cadence neither has (the alarm's own
 *  guard, estate_alarm.rs per_host_observations). */
export function parseSeriesPage(raw: unknown, scope: string, host: string | null): SeriesPage {
  const all = parseObservations(raw);
  const total = (raw as { total?: unknown } | null)?.total;
  return {
    rows: all.filter((o) => o.scope === scope && (host === null || o.nodes[0]?.id === host)),
    returned: all.length,
    total: typeof total === 'number' ? total : null,
  };
}

/** What a series with no row reads. "Never recorded" is the SERIES'
 *  read counting zero, and only that; any other empty answer is the
 *  read's, and says what it held. */
export function seriesAbsentText(page: SeriesPage): string {
  if (page.total === 0) return 'no observation recorded yet';
  const held = `none of the ${page.returned} row${page.returned === 1 ? '' : 's'} this read returned is this series'`;
  return page.total === null
    ? `${held}, and the read did not say how many there are`
    : `${held} (the read counted ${page.total})`;
}

/** The hosts whose series the page reads, and whether any source could
 *  say. Every live declared host outside the cluster (declaredHosts —
 *  the page owes each a line, 725532ab) plus every host that has
 *  compared (the alarm's own source of hosts). Neither answering is
 *  `known: false` — a host list nobody could read is not an empty one. */
export type HostPlan = Readonly<{ known: boolean; hosts: readonly string[] }>;

export function hostPlan(
  nodes: Remote<readonly EstateNode[]>,
  compared: Remote<HostComparisonPage>,
): HostPlan {
  const declared = nodes.kind === 'ready' ? declaredHosts(nodes.data) : [];
  const seen = compared.kind === 'ready'
    ? compared.data.rows.flatMap((c) => (c.host ? [c.host] : []))
    : [];
  return {
    known: nodes.kind === 'ready' || compared.kind === 'ready',
    hosts: [...new Set([...declared, ...seen])].sort((a, b) => a.localeCompare(b)),
  };
}

/** One host's two observation series, each its own read. */
export type HostSeries = Readonly<{
  host: string;
  readings: Remote<SeriesPage>;
  units: Remote<SeriesPage>;
}>;

// A SERIES IS JUDGED BY ITS OWN CADENCE (backlog e1eb34bc; page audit
// 2cff1d6e, GAP 8). An observation's age was grey relative text —
// "today" for a three-minute-old row and a three-hour-old one — while
// the estate alarm files `unobserved:<series>` once a series is quiet
// past STALE_MULTIPLIER of its own measured cadence (estate_alarm.rs,
// stale_series). CLAUDE.md §Diagnosis: a state past its own alarm
// threshold must look troubled on the surface that shows it. Measured
// by the alarm's method, not from a timer file the page cannot read:
// boss-gcp's host observer is daily (10:25Z) and forge's is fifteen
// minutes, and one global age would read boss-gcp stale every
// afternoon. The three numbers are the alarm's, pinned to its source
// by estate.test.ts (CLAUDE.md §9a).
export const STALE_MULTIPLIER = 3;
export const STALE_MIN_OBSERVATIONS = 3;
export const STALE_MIN_CADENCE_S = 60;

export type Freshness = Readonly<{
  state: 'fresh' | 'stale' | 'unmeasured';
  ageS: number;
  /** The median gap between readings, floored; null with too few. */
  cadenceS: number | null;
}>;

/** The newest reading's age against the series' own cadence — the
 *  alarm's test, pure: cadence is the (upper) median gap between
 *  consecutive readings, floored at a minute; stale is strictly past
 *  three of them. Fewer than three readings measure no cadence, and no
 *  verdict is invented. Null for a series with no reading. */
export function seriesFreshness(rows: readonly Observation[], now: Date): Freshness | null {
  const times = rows
    .map((r) => Date.parse(r.observed_at))
    .filter((t) => !Number.isNaN(t))
    .sort((a, b) => b - a);
  const newest = times[0];
  if (newest === undefined) return null;
  const ageS = Math.max(0, Math.round((now.getTime() - newest) / 1000));
  if (times.length < STALE_MIN_OBSERVATIONS) return { state: 'unmeasured', ageS, cadenceS: null };
  const gaps = times.slice(1).map((t, i) => Math.round((times[i]! - t) / 1000)).sort((a, b) => a - b);
  const cadenceS = Math.max(gaps[Math.floor(gaps.length / 2)]!, STALE_MIN_CADENCE_S);
  return { state: ageS > STALE_MULTIPLIER * cadenceS ? 'stale' : 'fresh', ageS, cadenceS };
}

export function freshnessText(f: Freshness): string {
  const age = `${journeyText(f.ageS)} ago`;
  if (f.cadenceS === null) return `${age} · cadence not yet measured`;
  const cadence = journeyText(f.cadenceS);
  return f.state === 'stale'
    ? `${age} — quiet past ${STALE_MULTIPLIER}× its ${cadence} cadence`
    : `${age} · every ${cadence}`;
}

// UNIT HEALTH (backlog d5efb80d; page audit 2cff1d6e, GAP 3). The page
// fetched host-units — 12 of its 20 rows, 1890 of 3143 in the log at
// measure — and drew none of it, so "is this unit alive", which the
// observer exists to answer as a record read (observe-units.sh, 729329c6),
// had no answer here and a units_unhealthy finding was invisible.

/** A host's units, green only when every watched unit says healthy. A
 *  unit whose health went unrecorded counts against it, and a reading
 *  with no units list is not "all healthy". */
export function unitsVerdict(node: ObservedNode): { ok: boolean; text: string } {
  if (!Array.isArray(node.units)) return { ok: false, text: 'no units list on the reading' };
  const n = node.units.length;
  const watched = `${n} unit${n === 1 ? '' : 's'} watched`;
  const sick = node.units.filter((u) => u.healthy !== true).map((u) => u.unit ?? 'unnamed unit');
  if (sick.length === 0) return { ok: true, text: `${watched}, all healthy` };
  return { ok: false, text: `${watched}, ${sick.length} unhealthy: ${sick.join(', ')}` };
}

export type EstateState = Readonly<{
  nodes: Remote<readonly EstateNode[]>;
  /** The cluster's observation series (scope kubernetes-nodes). */
  cluster: Remote<SeriesPage>;
  /** The cluster's newest comparison, read by its scope. */
  comparisons: Remote<readonly Comparison[]>;
  hostComparisons: Remote<HostComparisonPage>;
  hosts: Readonly<{ known: boolean; series: readonly HostSeries[] }>;
  loops: readonly LoopRow[];
}>;

// THE DEV WORKSPACE DOOR (design 5fc71f03, David 2026-09-18; backlog
// e4cedb46). One hostname, from anywhere, behind a Cloudflare Access
// SSH application that issues a certificate good for one session.
//
// It replaces a MetalLB VIP on the LAN, reached from outside through
// the boss-gcp WireGuard bastion on a key that lived forever — a jump
// this page had to spell out in three forms, because ssh:// cannot
// carry a ProxyJump. None of that is needed now, so none of it is
// here; the VIP still answers and is no longer advertised
// (infra/cluster/manifests/boss-dev.yaml, Service boss-dev-ssh).
//
// HARDCODED, and loudly so, for the same reason the VIP was: the
// hostname is DECLARED — the tunnel route in
// infra/cluster/tunnel-origins.toml and the application in
// infra/cluster/dns/access.toml — but the jobs API serves only
// /api/estate/nodes|observations|comparisons (boss-jobs http/mod.rs),
// so no read reaches either file. A fact that lives twice gets an
// equality test (CLAUDE.md §9a): the Rust test
// the_dev_door_is_an_access_ssh_application.rs holds the literal below
// to the route the connector serves, so a drift is a red test rather
// than a terminal block that opens nothing. When the estate reader
// lands (d471a8ce) this constant dies and the block renders from the
// registry like everything else on the page.
export const DEV_DOOR_HOST = 'dev.algedonic.dev';

/** One line of the terminal setup, with the reason it is there: a
 *  command an operator pastes blind is a command they cannot judge. */
export type DoorStep = Readonly<{ what: string; command: string; why: string }>;

/** The one-time terminal setup for the dev door, in order. Steps 1 and
 *  2 are done once per machine; step 3 is every session — and after
 *  step 2, so is any other ssh to the name (scp, rsync, ProxyJump),
 *  because the stanza teaches ssh itself how to reach it. */
export function devDoorSteps(host: string = DEV_DOOR_HOST): readonly DoorStep[] {
  return [
    {
      what: 'Install cloudflared, once per machine',
      command: 'cloudflared --version',
      why: 'it is the client half of the tunnel: ssh talks to it, it talks to the edge. Not found means not installed — take it from Cloudflare downloads, or your package manager, and run this again.',
    },
    {
      what: 'Teach ssh the route, once per machine',
      command: `grep -qsF 'Match host ${host} ' ~/.ssh/config || cloudflared access ssh-config --hostname ${host} --short-lived-cert | sed '/^Add to your/d' >> ~/.ssh/config`,
      why: `it appends a ProxyCommand stanza for ${host}; ssh then reaches it like any other host. The sed drops cloudflared's "Add to your …/.ssh/config:" banner, which ssh cannot parse, and the grep makes a second run a no-op.`,
    },
    {
      what: 'Open the workspace',
      command: `ssh root@${host}`,
      why: 'the browser asks who you are, Access issues a certificate for the session, and the pod accepts it. Nothing long-lived is stored.',
    },
  ];
}

function asArray(raw: unknown): readonly unknown[] {
  if (Array.isArray(raw)) return raw;
  if (raw && typeof raw === 'object' && Array.isArray((raw as { data?: unknown }).data)) {
    return (raw as { data: unknown[] }).data;
  }
  throw new Error('expected an array or {data: [...]}');
}

export function parseNodes(raw: unknown): readonly EstateNode[] {
  return asArray(raw).map((r) => {
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string' || typeof o.role !== 'string') {
      throw new Error('estate node row missing id/role');
    }
    return {
      id: o.id,
      label: typeof o.label === 'string' ? o.label : o.id,
      address: typeof o.address === 'string' ? o.address : null,
      role: o.role,
      roles: Array.isArray(o.roles) ? o.roles.filter((x): x is string => typeof x === 'string') : [],
      cpu: typeof o.cpu === 'number' ? o.cpu : null,
      memory_gb: typeof o.memory_gb === 'number' ? o.memory_gb : null,
      disk_gb: typeof o.disk_gb === 'number' ? o.disk_gb : null,
      notes: typeof o.notes === 'string' ? o.notes : null,
      retired: o.retired === true,
    };
  });
}

/** Event rows arrive as {payload: {...}} envelopes from the reader;
 *  the payload is the observation the observer POSTed, verbatim. */
export function parseObservations(raw: unknown): readonly Observation[] {
  return asArray(raw).flatMap((r) => {
    const p = (r as { payload?: unknown }).payload as Record<string, unknown> | undefined;
    if (!p || typeof p.scope !== 'string' || typeof p.observed_at !== 'string') return [];
    const nodes = Array.isArray(p.nodes) ? (p.nodes as ObservedNode[]) : [];
    return [{
      observed_at: p.observed_at,
      observer: typeof p.observer === 'string' ? p.observer : '?',
      scope: p.scope,
      nodes,
    }];
  });
}

export function parseComparisons(raw: unknown): readonly Comparison[] {
  return asArray(raw).flatMap((r) => {
    const p = (r as { payload?: unknown }).payload as Record<string, unknown> | undefined;
    if (!p || typeof p.scope !== 'string') return [];
    const c = (p.counts ?? {}) as Record<string, unknown>;
    const n = (k: string): number => (typeof c[k] === 'number' ? (c[k] as number) : 0);
    const f = (p.findings ?? {}) as Record<string, unknown>;
    // not_ready ids arrive bare (compare pushes strings); the {id} form
    // is read too, as estate_alarm's finding keys read both.
    const notReady = (Array.isArray(f.not_ready) ? (f.not_ready as unknown[]) : []).flatMap((x) => {
      if (typeof x === 'string') return [x];
      const id = (x as { id?: unknown } | null)?.id;
      return typeof id === 'string' ? [id] : [];
    });
    return [{
      observed_at: typeof p.observed_at === 'string' ? p.observed_at : '',
      scope: p.scope,
      host: typeof p.host === 'string' ? p.host : null,
      counts: {
        observed: n('observed'),
        participating_declared: n('participating_declared'),
        observed_not_declared: n('observed_not_declared'),
        declared_not_observed: n('declared_not_observed'),
        drift: n('drift'),
        // Absent from every row recorded before a520737f, and `n`
        // answers 0 for a missing key — a parser that dropped these
        // would render "no drift" over a full build node.
        disk_tight: n('disk_tight'),
        disk_unmeasured: n('disk_unmeasured'),
        // GAPS 5 AND 6 (ea5e0e8b, c2373cc4): dropped here until
        // 2026-09-27, so a NotReady node and a dispatcher losing dead
        // letters both read "no drift". A row from before the server
        // counted not_ready carries only the finding — the finding is
        // the record, so its length stands in for the missing count.
        not_ready: typeof c.not_ready === 'number' ? c.not_ready : notReady.length,
        dead_letters_unrecorded: n('dead_letters_unrecorded'),
      },
      not_ready: notReady,
      dispatcher_unread: typeof f.dispatcher_unread === 'string' ? f.dispatcher_unread : null,
    }];
  });
}

// (latestByScope and latestHostReadings kept the newest row per scope,
// then per host (3d1678ba), out of the one unscoped page — deleted with
// that page by 75027a93: each series is its own read now, and its first
// row is its newest.)

export function latestComparison(rows: readonly Comparison[], scope: string): Comparison | null {
  return rows.find((r) => r.scope === scope) ?? null;
}

/** The host series' page as the reader serves it (`{data, total}`),
 *  kept to host rows whatever came back. */
export function parseHostComparisons(raw: unknown): HostComparisonPage {
  const rows = parseComparisons(raw).filter((c) => c.scope === 'host');
  const total = (raw as { total?: unknown } | null)?.total;
  return {
    rows,
    total: typeof total === 'number' ? total : null,
  };
}

/** Whether the read is every host there is: the server counted, and the
 *  page holds that many (a limit below the host count shows as rows <
 *  total). An empty answer is whole either way — there is no tail. */
export function hostPageIsWhole(page: HostComparisonPage): boolean {
  const groups = latestPerHost(page.rows).length;
  if (page.total === null) return groups === 0;
  return groups >= page.total;
}

/** The hosts that owe a self-scoped comparison: every live declared
 *  node OUTSIDE the cluster — the complement of the cluster compare's
 *  participation rule (estate_compare.rs: role `talos-*`, not retired),
 *  since a cluster node is compared in the cluster verdict instead. */
export function declaredHosts(nodes: readonly EstateNode[]): readonly string[] {
  return nodes.filter((n) => !n.retired && !n.role.startsWith('talos-')).map((n) => n.id);
}

/** Every line the host comparison renders: the newest row of each host
 *  the read returned, plus a comparison-less line for each declared
 *  host it did not, in host order. An unread registry leaves the rows
 *  alone — it declares nothing the page could owe a line to. */
export function hostLines(nodes: Remote<readonly EstateNode[]>, page: HostComparisonPage): readonly HostLine[] {
  const latest = latestPerHost(page.rows);
  const seen = new Set(latest.map((c) => c.host ?? null));
  const owed = nodes.kind === 'ready' ? declaredHosts(nodes.data).filter((h) => !seen.has(h)) : [];
  const lines: readonly HostLine[] = [
    ...latest.map((c) => ({ host: c.host ?? null, cmp: c })),
    ...owed.map((h) => ({ host: h, cmp: null })),
  ];
  return [...lines].sort((a, b) => (a.host ?? '').localeCompare(b.host ?? ''));
}

/** What a declared host with no row reads. Only a whole read may say
 *  the host has none; otherwise the absence is the read's, and it says
 *  how much of the series it holds. */
export function missingHostText(page: HostComparisonPage): string {
  if (hostPageIsWhole(page)) return 'no host comparison recorded';
  const held = latestPerHost(page.rows).length;
  return page.total === null
    ? `not among the ${held} hosts this read returned, which did not say how many there are`
    : `not among the ${held} of ${page.total} hosts this read returned`;
}

/** The line under a read that is not every host, or null when it is:
 *  a truncated answer is never presented as whole. */
export function hostCoverText(page: HostComparisonPage): string | null {
  if (hostPageIsWhole(page)) return null;
  const held = latestPerHost(page.rows).length;
  const count = page.total === null
    ? `${held} hosts and did not say how many there are`
    : `${held} of ${page.total} hosts`;
  return `The host read returned ${count}: a host past it has no comparison shown here.`;
}

/** Newest comparison per host — rows arrive newest-first, so the first
 *  row naming a host is its latest word — in host order, so a refresh
 *  does not reshuffle the lines. */
export function latestPerHost(rows: readonly Comparison[]): readonly Comparison[] {
  const out = new Map<string, Comparison>();
  for (const r of rows) {
    const key = r.host ?? '';
    if (!out.has(key)) out.set(key, r);
  }
  return [...out.values()].sort((a, b) => (a.host ?? '').localeCompare(b.host ?? ''));
}

/** Zero everywhere-it-matters is the good state and says so; anything
 *  else names what disagrees. A self-scoped (host) comparison counts
 *  no declared total and observes only itself, so its words differ in
 *  those two places and nowhere else. */
export function comparisonVerdict(c: Comparison): { ok: boolean; text: string } {
  const k = c.counts;
  const selfScoped = c.host != null;
  const problems: string[] = [];
  if (k.observed_not_declared > 0) {
    problems.push(`${k.observed_not_declared} ${selfScoped ? 'observed but not declared' : 'in the cluster but undeclared'}`);
  }
  if (k.declared_not_observed > 0) problems.push(`${k.declared_not_observed} declared but not seen`);
  if (k.drift > 0) problems.push(`${k.drift} drifted from declaration`);
  // Headroom, not paperwork: a machine out of room stops the pipeline,
  // so "no drift" must not render beside it (a520737f).
  if ((k.disk_tight ?? 0) > 0) problems.push(`${k.disk_tight} short of disk`);
  if ((k.disk_unmeasured ?? 0) > 0) problems.push(`${k.disk_unmeasured} with no free-space reading`);
  // The HARD findings estate.alarm raises on (ea5e0e8b, c2373cc4): a
  // sick declared node (the 2026-09-11 cp-2 + w-1 class) and a
  // dispatcher whose dead letters left no record, or could not be read.
  if ((k.not_ready ?? 0) > 0) {
    const ids = c.not_ready ?? [];
    problems.push(`${k.not_ready} not ready${ids.length > 0 ? ` (${ids.join(', ')})` : ''}`);
  }
  if ((k.dead_letters_unrecorded ?? 0) > 0) {
    problems.push(`${k.dead_letters_unrecorded} dispatcher with unrecorded dead letters`);
  }
  if (c.dispatcher_unread) problems.push(`dispatcher dead-letter counters unread: ${c.dispatcher_unread}`);
  if (problems.length === 0) {
    if (selfScoped) return { ok: true, text: `${k.observed} observed — no drift` };
    return { ok: true, text: `${k.observed} observed, ${k.participating_declared} declared — no drift` };
  }
  return { ok: false, text: problems.join('; ') };
}

const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);

export function parseLoopPackets(raw: unknown): readonly LoopPacket[] {
  return asArray(raw).map((r) => {
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string' || typeof o.status !== 'string') {
      throw new Error('jobs row missing id/status');
    }
    const md = (o.metadata ?? {}) as Record<string, unknown>;
    const steps = Array.isArray(o.steps) ? (o.steps as Record<string, unknown>[]) : [];
    const run = steps.find((s) => s.spec_slug === 'run');
    const runMd = (run?.metadata ?? {}) as Record<string, unknown>;
    const open = o.status === 'open';
    return {
      id: o.id,
      status: o.status,
      outcome: open ? null : str(md.outcome),
      at: open ? (str(o.opened_at) ?? str(md.opened_at)) : (str(md.closed_at) ?? str(o.closed_on)),
      host: str(md.host) ?? str(runMd.node_id),
    };
  });
}

/** The rows the page reads: every declared loop, then the ops-request
 *  loop per live host declaring the runner role. With the registry
 *  unreadable the runner row is kept, unfiltered — "did ANY runner
 *  answer" is still a question the log can settle. */
export function loopPlan(nodes: Remote<readonly EstateNode[]>): readonly LoopPlan[] {
  const loops = ESTATE_LOOPS.map((l) => ({ ...l, host: null }));
  if (nodes.kind !== 'ready') return [...loops, { kind: OPS_REQUEST_KIND, label: 'ops-request', host: null }];
  const runners = nodes.data.filter((n) => !n.retired && n.roles.includes(OPS_RUNNER_ROLE));
  return [...loops, ...runners.map((n) => ({ kind: OPS_REQUEST_KIND, label: 'ops-request', host: n.id }))];
}

/** Newest terminal = the first closed row (the listing is newest-opened
 *  first); open = every open packet of the kind, typically none or one. */
export function loopQueries(kind: string, host: string | null): { latest: string; open: string } {
  const narrow = host === null ? '' : `&metadata=${encodeURIComponent(JSON.stringify({ host }))}`;
  return {
    latest: `/api/jobs?kind=${encodeURIComponent(kind)}&status=closed&limit=1&full=true${narrow}`,
    open: `/api/jobs?kind=${encodeURIComponent(kind)}&status=open&full=true${narrow}`,
  };
}

/** The host a row is about, from the query or the packet — never
 *  guessed (see THE LOOPS above). */
export function loopHost(row: LoopRow): string {
  if (row.host) return row.host;
  const fromLatest = row.latest.kind === 'ready' ? (row.latest.data?.host ?? null) : null;
  const fromOpen = row.open.kind === 'ready' ? (row.open.data.find((p) => p.host)?.host ?? null) : null;
  return fromLatest ?? fromOpen ?? 'not named on the packet';
}

/** How long ago, to the minute — a five-minute loop dated "today"
 *  answers nothing. The board's own reading (yard-floor sinceText). */
export function loopAge(at: string | null, now: Date): string {
  return at ? `${sinceText(at, now.getTime())} ago` : 'undated';
}

async function fetchLoop(plan: LoopPlan): Promise<LoopRow> {
  const q = loopQueries(plan.kind, plan.host);
  const [latest, open] = await Promise.all([
    fetchRemote(q.latest, (raw) => parseLoopPackets(raw)[0] ?? null),
    fetchRemote(q.open, parseLoopPackets),
  ]);
  return { ...plan, latest, open };
}

async function fetchHostSeries(host: string): Promise<HostSeries> {
  const [readings, units] = await Promise.all([
    fetchRemote(hostSeriesRead(HOST_SCOPE, host), (raw) => parseSeriesPage(raw, HOST_SCOPE, host)),
    fetchRemote(hostSeriesRead(UNITS_SCOPE, host), (raw) => parseSeriesPage(raw, UNITS_SCOPE, host)),
  ]);
  return { host, readings, units };
}

export async function fetchEstate(): Promise<EstateState> {
  const nodesRead = fetchRemote('/api/estate/nodes', parseNodes);
  const hostCmpRead = fetchRemote(HOST_COMPARISONS_READ, parseHostComparisons);
  // The host series wait on the two reads that name the hosts; every
  // other read goes out at once.
  const hostsRead = Promise.all([nodesRead, hostCmpRead]).then(async ([n, hc]) => {
    const plan = hostPlan(n, hc);
    return { known: plan.known, series: await Promise.all(plan.hosts.map(fetchHostSeries)) };
  });
  const [nodes, cluster, comparisons, hostComparisons, hosts, loops] = await Promise.all([
    nodesRead,
    fetchRemote(CLUSTER_OBSERVATIONS_READ, (raw) => parseSeriesPage(raw, CLUSTER_SCOPE, null)),
    fetchRemote(CLUSTER_COMPARISON_READ, parseComparisons),
    hostCmpRead,
    hostsRead,
    nodesRead.then((n) => Promise.all(loopPlan(n).map(fetchLoop))),
  ]);
  return { nodes, cluster, comparisons, hostComparisons, hosts, loops };
}
