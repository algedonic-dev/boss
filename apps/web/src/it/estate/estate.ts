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
// plus the packets the estate's own loops leave, read from the jobs API
// — the loops, the open alarms, and the DNS zone readings that carry
// the declared edge (THE EDGE, e0e183fb).
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
  /** Running `boss-*` units outside the declared roster (6647ac9a). */
  undeclared_units?: readonly ObservedUnit[];
  /** Why the observer could not ask what runs, when it could not. */
  undeclared_unread?: string;
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
  /** The machines behind `counts.drift`, each with the fields that
   *  differ (ab3c54d7) — so the page names WHICH machine drifted on
   *  WHAT, not only how many. Absent on a literal built without it. */
  drift?: readonly DriftFinding[];
}>;

/** One field a comparison found different: both sides, as compared. */
export type DriftField = Readonly<{ declared: unknown; observed: unknown }>;

/** A drift finding, in compare()'s own shape (estate_compare.rs):
 *  `{id, fields: {<field>: {declared, observed}}}`. */
export type DriftFinding = Readonly<{ id: string; fields: Readonly<Record<string, DriftField>> }>;

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

/** The loop whose run step also records the tunnel routes (THE EDGE). */
export const CLUSTER_CONVERGE_KIND = 'maintenance-cluster-converge';

export const ESTATE_LOOPS: readonly EstateLoop[] = [
  { kind: 'maintenance-forge-converge', label: 'forge converge' },
  { kind: 'maintenance-boss-gcp-converge', label: 'boss-gcp converge' },
  { kind: CLUSTER_CONVERGE_KIND, label: 'cluster converge' },
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
 *  files by estate.test.ts. `nothing-to-do` is ops-request's close for
 *  a plan that named no change (backlog b2f78bb9): the runner answered,
 *  and nothing needing doing is not trouble. */
export const LOOP_OK_OUTCOMES: ReadonlySet<string> = new Set(['completed', 'answered', 'nothing-to-do']);

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
  /** The tunnel routes the run applied and its connector's state, when
   *  the run step recorded them — only the cluster converge does (THE
   *  EDGE below); null on every other loop. */
  tunnel: TunnelRoutes | null;
}>;

/** What a cluster converge's run step records of the tunnel
 *  (infra/forge/cluster-deploy-lib.sh, `tunnel_ingress` + `cloudflared`). */
export type TunnelRoutes = Readonly<{ connector: string | null; routes: readonly string[] }>;

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

// THE INSTANCE VOLUMES (backlog 21ee3b4e, incident d3c0a67c). On
// 2026-10-01 the system of record's Postgres volume filled and writes
// failed with No space left on device; no surface showed any claim, and
// a builder found it through a 500. The forge now reads every claim in
// every instance namespace (infra/estate/observe-volumes.sh) and
// estate.compare judges each against the volume floor — and the
// COMPARISON carries every row whole, judged (`volumes`, each with its
// floor and `tight`), so this page draws the comparator's verdict and
// never a second copy of the floor (CLAUDE.md §9a). Ten rows, as every
// series here, so its age reads against its own cadence.
export const VOLUMES_SCOPE = 'instance-volumes';
export const VOLUMES_READ = `/api/estate/comparisons?scope=${VOLUMES_SCOPE}&limit=${SERIES_SAMPLE}`;

/** One claim as the volume comparison records it. A claim the forge
 *  could not read carries null figures, `tight: null` and `unread`. */
export type VolumeRow = Readonly<{
  id: string;
  namespace: string | null;
  claim: string | null;
  volume: string | null;
  capacity_bytes: number | null;
  used_bytes: number | null;
  free_bytes: number | null;
  floor_bytes: number | null;
  tight: boolean | null;
  unread: string | null;
  requested: string | null;
  capacity_intent: CapacityIntent;
}>;

export type CapacityAssignment = Readonly<{
  packet: string;
  step: string;
  question: string;
  decided_by: string;
  decided_at: string;
}>;
export type CapacityIntent = Readonly<{
  verdict: 'match' | 'drift' | 'unknown';
  desired_bytes: number | null;
  requested_bytes: number | null;
  assignment: CapacityAssignment | null;
  reason: string | null;
}>;

const byteCount = (v: unknown): number | null => typeof v === 'number' && Number.isSafeInteger(v) && v > 0 ? v : null;
const unknownIntent = (reason: string): CapacityIntent => ({ verdict: 'unknown', desired_bytes: null, requested_bytes: null, assignment: null, reason });

/** Retain the recorded verdict; contradictory or incomplete evidence is
 * unavailable, never a browser-inferred target or match. */
export function parseCapacityIntent(raw: unknown): CapacityIntent {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return unknownIntent('no declared capacity comparison recorded');
  const o = raw as Record<string, unknown>;
  const a = o.assignment;
  const provenance = a && typeof a === 'object' && !Array.isArray(a) ? a as Record<string, unknown> : null;
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  const assignment = provenance && ['packet', 'step', 'question', 'decided_by', 'decided_at'].every((k) => typeof provenance[k] === 'string' && (provenance[k] as string).trim().length > 0)
    && uuid.test(provenance.packet as string) && uuid.test(provenance.step as string) && Number.isFinite(Date.parse(provenance.decided_at as string))
    ? { packet: provenance.packet as string, step: provenance.step as string, question: provenance.question as string, decided_by: provenance.decided_by as string, decided_at: provenance.decided_at as string } : null;
  const desired_bytes = byteCount(o.desired_bytes);
  const requested_bytes = byteCount(o.requested_bytes);
  if (o.verdict === 'unknown' && typeof o.reason === 'string' && o.reason.trim()) return { verdict: 'unknown', desired_bytes: assignment ? desired_bytes : null, requested_bytes, assignment, reason: o.reason };
  if (assignment && desired_bytes !== null && requested_bytes !== null && ((o.verdict === 'match' && desired_bytes === requested_bytes) || (o.verdict === 'drift' && desired_bytes !== requested_bytes))) {
    return { verdict: o.verdict as 'match' | 'drift', desired_bytes, requested_bytes, assignment, reason: null };
  }
  return unknownIntent('capacity comparison evidence is malformed or contradictory');
}

export type VolumeReading = Readonly<{
  observed_at: string;
  observer: string;
  volumes: readonly VolumeRow[];
}>;

const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);

/** The volume comparisons, this scope's only, in the order read. */
export function parseVolumeReadings(raw: unknown): readonly VolumeReading[] {
  return asArray(raw).flatMap((r) => {
    const p = (r as { payload?: unknown }).payload as Record<string, unknown> | undefined;
    if (!p || p.scope !== VOLUMES_SCOPE || typeof p.observed_at !== 'string') return [];
    const volumes = (Array.isArray(p.volumes) ? (p.volumes as unknown[]) : []).flatMap((x) => {
      const o = (x ?? {}) as Record<string, unknown>;
      if (typeof o.id !== 'string') return [];
      return [{
        id: o.id,
        namespace: str(o.namespace),
        claim: str(o.claim),
        volume: str(o.volume),
        capacity_bytes: num(o.capacity_bytes),
        used_bytes: num(o.used_bytes),
        free_bytes: num(o.free_bytes),
        floor_bytes: num(o.floor_bytes),
        tight: typeof o.tight === 'boolean' ? o.tight : null,
        unread: str(o.unread),
        requested: str(o.requested),
        capacity_intent: parseCapacityIntent(o.capacity_intent),
      }];
    });
    return [{ observed_at: p.observed_at, observer: str(p.observer) ?? '?', volumes }];
  });
}

const gibText = (bytes: number): string => `${(bytes / 1024 ** 3).toFixed(1)}G`;

export function capacityIntentLine(v: VolumeRow): string {
  const c = v.capacity_intent;
  const desired = c.desired_bytes === null ? 'unknown' : `${c.desired_bytes} bytes`;
  const requested = v.requested ?? 'unread';
  return c.verdict === 'unknown'
    ? `Capacity intent unknown: ${c.reason ?? 'no comparison evidence'} · desired ${desired} · requested ${requested}`
    : `Capacity ${c.verdict}: desired ${desired} · requested ${requested} (${c.requested_bytes} bytes)`;
}

/** One claim's line. Unread unless the row carries the comparator's
 *  verdict AND the three figures it was reached on — a verdict with no
 *  figures behind it is not taken at its word. */
export function volumeLine(v: VolumeRow): { state: 'tight' | 'ok' | 'unread'; text: string } {
  const { free_bytes: free, capacity_bytes: cap, floor_bytes: floor } = v;
  if (v.tight === null || free === null || cap === null || floor === null) {
    return { state: 'unread', text: `unread: ${v.unread ?? 'the reading carries no figures for this claim'}` };
  }
  return v.tight
    ? { state: 'tight', text: `${gibText(free)} free of ${gibText(cap)} — under its ${gibText(floor)} floor` }
    : { state: 'ok', text: `${gibText(free)} free of ${gibText(cap)} · floor ${gibText(floor)}` };
}

/** The open alarms about one claim, keyed as estate.alarm keys them:
 *  `disk_tight:<ns>/<claim>`, or `blind:disk_tight/<ns>/<claim>` for one
 *  nobody could read three times running. */
export function volumeAlarms(alarms: Remote<AlarmPage>, id: string): readonly EstateAlarm[] {
  if (alarms.kind !== 'ready') return [];
  return alarms.data.rows.filter((a) => a.finding === `disk_tight:${id}` || a.finding === `blind:disk_tight/${id}`);
}

/**
 * The registry read's failure, said as what it was (backlog e5f7b51e).
 * The estate reads ask policy now, so a 401/403 is the registry
 * REFUSING this session — a fact about who is reading — and naming it
 * "did not answer" would send the reader to look for an outage that is
 * not there. Anything else is the unreachable registry it always was.
 */
export function registryFailure(error: string): string {
  return /: HTTP 40[13]$/.test(error)
    ? `The registry refused this session: ${error}. The estate reads ask policy — reading them needs Read on estate at scope all (platform-admin, break-glass, or the audit read).`
    : `The registry did not answer: ${error}. This page refuses to guess — an unreachable registry is not an empty estate.`;
}

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
export function seriesFreshness(rows: readonly Readonly<{ observed_at: string }>[], now: Date): Freshness | null {
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
  const parts = [sick.length === 0 ? `${watched}, all healthy` : `${watched}, ${sick.length} unhealthy: ${sick.join(', ')}`];
  // What runs outside the roster (backlog 6647ac9a): the retired
  // boss-ml-api ran on boss-gcp for days under "all healthy", because
  // the roster is the declaration. An observer that could not ask says
  // so; a reading from before it asked carries neither key.
  const undeclared = (node.undeclared_units ?? []).map((u) => u.unit ?? 'unnamed unit');
  if (undeclared.length > 0) parts.push(`${undeclared.length} running but not declared: ${undeclared.join(', ')}`);
  if (typeof node.undeclared_unread === 'string') parts.push(`running units not enumerated: ${node.undeclared_unread}`);
  return { ok: parts.length === 1 && sick.length === 0, text: parts.join('; ') };
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
  /** The open ESTATE ALARM packets (48ef9961). */
  alarms: Remote<AlarmPage>;
  /** The DNS zone readings, newest per zone, and the open ones (THE EDGE). */
  zones: ZonesState;
  /** The instance volumes' judged series, newest first (21ee3b4e). */
  volumes: Remote<readonly VolumeReading[]>;
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
// ONE COPY, TWO READERS (design 125d405d, backlog fd6d6c08). The
// hostname and the three client steps live in ./dev-door.json, which
// this page imports and the printed recovery sheet reads
// (infra/recovery/re-entry.toml, `boss recovery sheet`). Until
// 2026-09-29 they were literals here and the paper would have been a
// second spelling; one file cannot drift from itself (CLAUDE.md §9a).
// The hostname is still DECLARED twice more — the tunnel route in
// infra/cluster/tunnel-origins.toml and the application in
// infra/cluster/dns/access.toml — and the Rust test
// the_dev_door_is_an_access_ssh_application.rs holds the data file to
// the route the connector serves, so a drift is a red test rather than
// a terminal block that opens nothing.
import devDoor from './dev-door.json';

export const DEV_DOOR_HOST: string = devDoor.host;

/** One line of the terminal setup, with the reason it is there: a
 *  command an operator pastes blind is a command they cannot judge. */
export type DoorStep = Readonly<{ what: string; command: string; why: string }>;

/** The one-time terminal setup for the dev door, in order. Steps 1 and
 *  2 are done once per machine; step 3 is every session — and after
 *  step 2, so is any other ssh to the name (scp, rsync, ProxyJump),
 *  because the stanza teaches ssh itself how to reach it. `{host}` in
 *  the data file is the host this is given — the sheet's renderer
 *  fills it with the file's own `host`. */
export function devDoorSteps(host: string = DEV_DOOR_HOST): readonly DoorStep[] {
  const fill = (s: string) => s.replaceAll('{host}', host);
  return devDoor.steps.map((s) => ({ what: fill(s.what), command: fill(s.command), why: fill(s.why) }));
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
      drift: parseDrift(f.drift),
    }];
  });
}

function parseDrift(raw: unknown): readonly DriftFinding[] {
  return (Array.isArray(raw) ? (raw as unknown[]) : []).flatMap((x) => {
    const d = (x ?? {}) as { id?: unknown; fields?: unknown };
    if (typeof d.id !== 'string' || !d.fields || typeof d.fields !== 'object') return [];
    const fields = Object.fromEntries(
      Object.entries(d.fields as Record<string, unknown>).map(([k, v]) => {
        const pair = (v ?? {}) as { declared?: unknown; observed?: unknown };
        return [k, { declared: pair.declared ?? null, observed: pair.observed ?? null }];
      }),
    );
    return [{ id: d.id, fields }];
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
  if (k.drift > 0) {
    // Named as the finding names them (ab3c54d7): "1 drifted" alone sent
    // the reader to the comparison series to learn which machine, on what.
    const named = (c.drift ?? []).flatMap((d) =>
      Object.entries(d.fields).map(([f, v]) => `${d.id} ${f} ${plain(v.declared)} → ${plain(v.observed)}`),
    );
    problems.push(`${k.drift} drifted from declaration${named.length > 0 ? ` (${named.join(', ')})` : ''}`);
  }
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

/** A compared value as the finding holds it; a missing side is a dash. */
function plain(v: unknown): string {
  return v === null || v === undefined ? '—' : String(v);
}

// DECLARED BESIDE OBSERVED (backlog ab3c54d7; page audit 2cff1d6e, GAP
// 7). The subtitle promises it and the machines table drew declared
// values only: observed ones reached the page as free-disk text in
// another section, and a drift finding — which names the machine and the
// field — reached it as a count. Measured 2026-09-28: forge memory_gb
// declared 30, observed 31; boss-gcp declared 15, observed 16; the page
// said "1 drifted from declaration" twice and named neither.
//
// Each machine's observed side comes from the series that observes it —
// the cluster's for a `talos-*` node (the compare's participation rule),
// the host's own series otherwise (declaredHosts) — and a field is
// marked from the newest comparison OF THAT SERIES, the record of what
// was judged. No read is added: every one of these was already made.

/** The table's four value columns, in order, as the registry keys them. */
export const MACHINE_FIELDS = ['address', 'cpu', 'memory_gb', 'disk_gb'] as const;
export type MachineField = (typeof MACHINE_FIELDS)[number];

/** A value as the machines table spells it: gigabytes with a G, a
 *  missing one as a dash, never a zero. */
export function machineValue(field: MachineField, v: unknown): string {
  if (v === null || v === undefined) return '—';
  return field === 'memory_gb' || field === 'disk_gb' ? `${String(v)}G` : String(v);
}

/** What the observing series says of one machine: seen in its newest
 *  reading; absent from it; no reading at all; or unread (the read
 *  failed, or no source could name the series). Four answers, because
 *  "not seen" said of an unread series is the false-empty this page
 *  exists to refuse. */
export type MachineSight =
  | Readonly<{ kind: 'seen'; node: ObservedNode }>
  | Readonly<{ kind: 'absent' }>
  | Readonly<{ kind: 'no-reading' }>
  | Readonly<{ kind: 'unread' }>;

const observesFromCluster = (n: EstateNode): boolean => n.role.startsWith('talos-');

export function machineSight(estate: EstateState, n: EstateNode): MachineSight {
  const series = observesFromCluster(n)
    ? estate.cluster
    : estate.hosts.series.find((h) => h.host === n.id)?.readings;
  if (!series || series.kind !== 'ready') return { kind: 'unread' };
  const newest = series.data.rows[0];
  if (!newest) return { kind: 'no-reading' };
  const seen = newest.nodes.find((o) => o.id === n.id);
  return seen ? { kind: 'seen', node: seen } : { kind: 'absent' };
}

/** The fields the newest comparisons name as drift for one machine —
 *  the cluster verdict's and the host's own — or none. */
export function machineDrift(estate: EstateState, id: string): Readonly<Record<string, DriftField>> {
  const cluster = estate.comparisons.kind === 'ready' ? latestComparison(estate.comparisons.data, CLUSTER_SCOPE) : null;
  const host = estate.hostComparisons.kind === 'ready'
    ? (latestPerHost(estate.hostComparisons.data.rows).find((c) => c.host === id) ?? null)
    : null;
  return Object.assign(
    {},
    ...[cluster, host].flatMap((c) => (c?.drift ?? []).filter((d) => d.id === id).map((d) => d.fields)),
  ) as Record<string, DriftField>;
}

export type SeenCell = Readonly<{ text: string; drift: DriftField | null }>;

/** The observed half of one cell. A field the finding names shows the
 *  value the comparison JUDGED, so the mark and the number agree even
 *  when a newer reading has moved on. */
export function seenCell(
  sight: MachineSight,
  drift: Readonly<Record<string, DriftField>>,
  field: MachineField,
): SeenCell {
  const d = drift[field];
  if (d) return { text: `seen ${machineValue(field, d.observed)} · drifted`, drift: d };
  switch (sight.kind) {
    case 'seen':
      return { text: `seen ${machineValue(field, sight.node[field])}`, drift: null };
    case 'absent':
      return { text: 'not seen', drift: null };
    case 'no-reading':
      return { text: 'no reading', drift: null };
    case 'unread':
      return { text: 'unread', drift: null };
  }
}

// OPEN ESTATE ALARMS (backlog 48ef9961; page audit 2cff1d6e, GAP 9). The
// page drew amber lines and never the alarm packet that explains one:
// measured at filing, 20 ESTATE ALARM items and one open, and this page
// had no link at all. estate.alarm stamps every packet it files with
// `estate_finding` — the key it dedups on, alongside `scope` and `host`
// (estate_alarm.rs alarm_body, staleness_body, door_body) — so that key
// is the filter, rather than `area`: measured 2026-09-28, area=estate
// held 9 open items of which 1 was an alarm, and an ops-queue alarm
// carries area ops-runner.
export const OPEN_ALARMS_READ = '/api/jobs?kind=backlog-item&status=open&metadata_has=estate_finding&limit=50';

export type EstateAlarm = Readonly<{
  id: string;
  title: string;
  /** The raiser's key: `disk_tight:boss-gcp`, `unobserved:forge`, … */
  finding: string | null;
  /** The series the alarm is about — the verdict line it explains. */
  scope: string | null;
  host: string | null;
  /** When it opened. */
  at: string | null;
}>;

export type AlarmPage = Readonly<{ rows: readonly EstateAlarm[]; total: number | null }>;

export function parseAlarms(raw: unknown): AlarmPage {
  const rows = asArray(raw).map((r) => {
    const o = r as Record<string, unknown>;
    if (typeof o.id !== 'string') throw new Error('jobs row missing id');
    const md = (o.metadata ?? {}) as Record<string, unknown>;
    return {
      id: o.id,
      title: str(o.title) ?? '',
      finding: str(md.estate_finding),
      scope: str(md.scope),
      host: str(md.host),
      at: str(o.opened_at) ?? str(md.opened_at),
    };
  });
  const total = (raw as { total?: unknown } | null)?.total;
  return { rows, total: typeof total === 'number' ? total : null };
}

/** The open alarms on one verdict line's series: same scope, same host
 *  (none for the cluster's). An unread alarm list puts none beside it;
 *  its own failure line says why. */
export function alarmsOn(alarms: Remote<AlarmPage>, scope: string, host: string | null): readonly EstateAlarm[] {
  if (alarms.kind !== 'ready') return [];
  return alarms.data.rows.filter((a) => a.scope === scope && a.host === host);
}

/** The line under an alarm read that holds fewer than it counted, or
 *  null when it holds them all (or the reader did not count). */
export function alarmCoverText(page: AlarmPage): string | null {
  if (page.total === null || page.rows.length >= page.total) return null;
  return `The alarm read returned ${page.rows.length} of ${page.total} open estate alarms: the rest are not listed here.`;
}

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
    const ingress = str(runMd.tunnel_ingress);
    return {
      id: o.id,
      status: o.status,
      outcome: open ? null : str(md.outcome),
      at: open ? (str(o.opened_at) ?? str(md.opened_at)) : (str(md.closed_at) ?? str(o.closed_on)),
      host: str(md.host) ?? str(runMd.node_id),
      tunnel:
        ingress === null
          ? null
          : { connector: str(runMd.cloudflared), routes: ingress.split(/;\s*/).filter((r) => r.length > 0) },
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

// THE EDGE (backlog e0e183fb; page audit 2cff1d6e, GAP 11). The DNS
// zone, the Cloudflare Access applications in front of it and the
// tunnel routes behind it are declared in the tree
// (infra/cluster/dns/<zone>.toml, access.toml, tunnel-origins.toml) —
// and until this the page rendered only machines, so the estate alarm a
// drifted zone raises (dns_drift:<zone>) had no line to stand beside.
//
// NO NEW READER. The system of record already holds all of it, as
// declared AND as compared: the daily `dns-zone-observation` packet's
// observe step carries one verdict per record — the tree's declaration,
// what the zone holds, the interlock that fronts it — and one per
// Access application (dns_observe.rs, observe_fields), and the cluster
// converge's run step carries the tunnel routes it applied and the
// connector's state (`tunnel_ingress`, `cloudflared`). This page reads
// those packets; it spells no zone, hostname or address of its own (the
// instance declares its values, design c6f08b60).
//
// SEVEN readings, newest first: the newest per zone wins. One zone is
// read a day (infra/dispatcher/rules/dns-zone-observe-daily.toml), so a
// week of readings holds every zone observed this week; a zone whose
// reading could not complete stays OPEN at `observe`, and the open read
// below — every open one, unlimited — is where it shows.
export const ZONE_KIND = 'dns-zone-observation';
export const ZONE_SAMPLE = 7;
export const ZONE_READINGS_READ = `/api/jobs?kind=${ZONE_KIND}&status=closed&limit=${ZONE_SAMPLE}&full=true`;
export const ZONE_OPEN_READ = `/api/jobs?kind=${ZONE_KIND}&status=open`;

/** One declared zone record, as the observe step judged it. */
export type ZoneRecord = Readonly<{
  /** `<name> <type>`, the comparator's own key. */
  record: string;
  /** MATCH, DRIFT, ABSENT, HELD or REFUSED. */
  verdict: string;
  /** The tree's spelling: the target reference (`tunnel:<credential>`)
   *  when the declaration names one, else the declared content. */
  declared: string | null;
  /** What the zone holds; null when the record is absent. */
  live: string | null;
  /** What stands in front of it: `access: present`, `tunnel: routed`. */
  front: string | null;
  /** Why a flip was held, or what the zone refused. */
  note: string | null;
  why: string | null;
}>;

/** One declared Access application, as the observe step judged it. */
export type AccessApp = Readonly<{
  domain: string;
  type: string | null;
  /** Each policy by name and decision — never the people it admits. */
  policies: readonly string[];
  verdict: string;
  /** What the account refused, in its own words. */
  note: string | null;
  why: string | null;
}>;

export type ZoneReading = Readonly<{
  id: string;
  zone: string;
  /** When the reading closed. */
  at: string | null;
  records: readonly ZoneRecord[];
  /** Live records the tree names nowhere — reported, never a failure. */
  undeclared: number;
  access: readonly AccessApp[];
  accessUndeclared: number;
}>;

/** A reading still open: a zone that could not be read or compared. */
export type ZoneOpen = Readonly<{ id: string; zone: string; at: string | null }>;

export type ZonesState = Readonly<{
  latest: Remote<readonly ZoneReading[]>;
  open: Remote<readonly ZoneOpen[]>;
}>;

const obj = (v: unknown): Record<string, unknown> =>
  v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
const objs = (v: unknown): readonly Record<string, unknown>[] => (Array.isArray(v) ? v.map(obj) : []);

function zoneOf(o: Record<string, unknown>): string {
  if (typeof o.id !== 'string') throw new Error('jobs row missing id');
  return str(obj(o.metadata).zone) ?? str(obj(o.subject).id) ?? 'zone not named on the packet';
}

function zoneRecord(v: Record<string, unknown>): ZoneRecord {
  const declared = obj(v.declared);
  const front = str(v.access) !== null ? `access: ${str(v.access)}` : str(v.tunnel) !== null ? `tunnel: ${str(v.tunnel)}` : null;
  return {
    record: str(v.record) ?? str(v.name) ?? 'record not named',
    verdict: str(v.verdict) ?? 'no verdict',
    declared: str(declared.target) ?? str(declared.content),
    live: str(obj(v.live).content),
    front,
    note: str(v.held) ?? str(v.refused),
    why: str(v.why),
  };
}

function accessApp(v: Record<string, unknown>): AccessApp {
  const side = v.declared ? obj(v.declared) : obj(v.live);
  const write = str(v.write);
  const error = str(v.error);
  return {
    domain: str(v.domain) ?? str(v.application) ?? 'application not named',
    type: str(side.type),
    policies: objs(side.policies).map((p) => `${str(p.name) ?? 'unnamed'} (${str(p.decision) ?? 'no decision'})`),
    verdict: str(v.verdict) ?? 'no verdict',
    note: error === null ? null : write === null ? error : `${write}: ${error}`,
    why: str(v.why),
  };
}

/** The newest closed reading of each zone (the listing is newest
 *  first), its observe step read whole. */
export function parseZoneReadings(raw: unknown): readonly ZoneReading[] {
  const seen = new Set<string>();
  return asArray(raw).flatMap((r) => {
    const o = obj(r);
    const zone = zoneOf(o);
    if (seen.has(zone)) return [];
    seen.add(zone);
    const md = obj(o.metadata);
    const step = objs(o.steps).find((s) => s.spec_slug === 'observe');
    const smd = obj(step?.metadata);
    const verdicts = objs(smd.verdicts);
    const access = objs(smd.access);
    const isUndeclared = (v: Record<string, unknown>) => v.verdict === 'UNDECLARED';
    return [{
      id: o.id as string,
      zone,
      at: str(md.closed_at) ?? str(o.closed_on),
      records: verdicts.filter((v) => !isUndeclared(v)).map(zoneRecord),
      undeclared: verdicts.filter(isUndeclared).length,
      access: access.filter((v) => !isUndeclared(v)).map(accessApp),
      accessUndeclared: access.filter(isUndeclared).length,
    }];
  });
}

export function parseZoneOpen(raw: unknown): readonly ZoneOpen[] {
  return asArray(raw).map((r) => {
    const o = obj(r);
    const zone = zoneOf(o);
    return { id: o.id as string, zone, at: str(o.opened_at) ?? str(obj(o.metadata).opened_at) };
  });
}

/** The verdicts the observer counts as findings (dns_observe.rs
 *  `is_hard`); HELD is a designed wait and UNDECLARED is paperwork. */
const ZONE_HARD: ReadonlySet<string> = new Set(['DRIFT', 'ABSENT', 'REFUSED']);

/** The zone's line: how much of what is declared matches, and whether
 *  any of it is a finding. */
export function zoneVerdict(z: ZoneReading): { ok: boolean; text: string } {
  const match = (rows: readonly { verdict: string }[]) => rows.filter((r) => r.verdict === 'MATCH').length;
  const held = z.records.filter((r) => r.verdict === 'HELD').length;
  const ok = [...z.records, ...z.access].every((r) => !ZONE_HARD.has(r.verdict));
  const records = `${match(z.records)} of ${z.records.length} declared records match${held > 0 ? `, ${held} held` : ''}`;
  return { ok, text: `${records} · ${match(z.access)} of ${z.access.length} Access applications match` };
}

/** The open alarm a drifted zone raised, keyed as its raiser keys it
 *  (dns_observe.rs `alarm_key`). */
export function zoneAlarms(alarms: Remote<AlarmPage>, zone: string): readonly EstateAlarm[] {
  if (alarms.kind !== 'ready') return [];
  return alarms.data.rows.filter((a) => a.finding === `dns_drift:${zone}`);
}

export type TunnelLine = Readonly<{
  ok: boolean;
  text: string;
  /** The converge packet the routes were read off, to link. */
  id: string | null;
  at: string | null;
  routes: readonly string[];
}>;

/** The tunnel routes as the newest cluster converge applied them, read
 *  off the loop row the page already fetched — and, when there are
 *  none to show, which kind of none. */
export function tunnelLine(loops: readonly LoopRow[]): TunnelLine {
  const none = (text: string, id: string | null = null, at: string | null = null): TunnelLine =>
    ({ ok: false, text, id, at, routes: [] });
  const row = loops.find((l) => l.kind === CLUSTER_CONVERGE_KIND);
  if (!row || row.latest.kind === 'loading') return none('the cluster converge was not read');
  if (row.latest.kind === 'failed') return none(`unread: ${row.latest.error}`);
  const p = row.latest.data;
  if (!p) return none('no cluster converge has finished, so no route is recorded');
  if (!p.tunnel) return none('the newest cluster converge recorded no tunnel routes', p.id, p.at);
  const connector = p.tunnel.connector ?? 'not recorded';
  return {
    ok: p.tunnel.connector === 'connected',
    text: `connector ${connector} · ${p.tunnel.routes.length} routes`,
    id: p.id,
    at: p.at,
    routes: p.tunnel.routes,
  };
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
  const [nodes, cluster, comparisons, hostComparisons, hosts, loops, alarms, latestZones, openZones, volumes] = await Promise.all([
    nodesRead,
    fetchRemote(CLUSTER_OBSERVATIONS_READ, (raw) => parseSeriesPage(raw, CLUSTER_SCOPE, null)),
    fetchRemote(CLUSTER_COMPARISON_READ, parseComparisons),
    hostCmpRead,
    hostsRead,
    nodesRead.then((n) => Promise.all(loopPlan(n).map(fetchLoop))),
    fetchRemote(OPEN_ALARMS_READ, parseAlarms),
    fetchRemote(ZONE_READINGS_READ, parseZoneReadings),
    fetchRemote(ZONE_OPEN_READ, parseZoneOpen),
    fetchRemote(VOLUMES_READ, parseVolumeReadings),
  ]);
  return { nodes, cluster, comparisons, hostComparisons, hosts, loops, alarms, zones: { latest: latestZones, open: openZones }, volumes };
}
