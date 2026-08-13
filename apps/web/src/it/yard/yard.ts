// The train yard's data — a lens over the pipeline's queues
// (departure-board.md; pages-as-lenses). Every row derives from
// pr-train and ship-a-change Jobs the conductor already writes;
// audit-readonly reads only, so the guest landing renders whole.
//
// The loading dock is the first registry-backed lens (stations.md):
// its rows come from `GET /api/stations/loading-dock/queue` — the
// station's predicate and discipline evaluated server-side — with the
// old hand-rolled derivation kept as fallback for clusters that
// predate the registry.

export type StepLite = Readonly<{
  spec_slug?: string | null;
  title: string;
  status: string;
  metadata?: Record<string, unknown> | null;
  completed_on?: string | null;
}>;

export type JobLite = Readonly<{
  id: string;
  kind: string;
  title: string;
  status: string;
  opened_on: string;
  tags?: readonly string[];
  metadata?: Record<string, unknown> | null;
  steps?: readonly StepLite[];
  /** Admission-fixed sim-vs-real flag on the Job row itself. */
  simulated?: boolean;
}>;

// A car in the yard is a job packet, and it renders as a card (David's
// call, 2026-08-12): protocol names the color, tags ride along, and a
// simulated packet is visibly not a real one. The same card grammar is
// meant to travel to every queue lens, so everything here derives from
// packet data — no per-kind code paths.
export type CarRow = Readonly<{
  id: string;
  kind: string;
  branch: string;
  title: string;
  tags: readonly string[];
  sim: boolean;
  skipReason?: string | null;
}>;

// The protocol palette + kind → hue hash moved to web-kit with the
// card itself (@boss/web-kit/ui/packet-card) so every queue surface
// colors packets identically. Re-exported so the definitions live
// exactly once (CLAUDE.md §9a) and yard consumers need no change.
export { PROTOCOL_PALETTE, protocolHue } from '@boss/web-kit/ui/packet-card';

// Simulated is a fact on the packet, never an inference from where it
// came from. The Job's own admission-fixed `simulated` field is the
// source of truth; the tag / metadata conventions stay as fallback for
// packets that predate the field.
export function isSim(j: Pick<JobLite, 'simulated' | 'tags' | 'metadata'>): boolean {
  if (j.simulated === true) return true;
  const tagged = (j.tags ?? []).some(t =>
    ['sim', 'simulated', 'synthetic'].includes(t.toLowerCase()),
  );
  return tagged || (j.metadata as { simulated?: boolean } | null)?.simulated === true;
}

export type TrainStatus = 'BOARDING' | 'BOARDED' | 'DEPARTED' | 'ARRIVED';
export type Lamp = 'green' | 'failing' | 'pending';

export type TrainRow = Readonly<{
  id: string;
  title: string;
  prUrl?: string | null;
  status: TrainStatus;
  lamp: Lamp;
  mergeRef?: string | null;
  deployed?: string | null;
  cars: readonly CarRow[];
  live: boolean;
}>;

// The `GET /api/stations/{name}/queue` envelope (stations.md; the
// StationQueue struct in boss-jobs/src/station_queue.rs). Discipline
// keys and station kinds stay plain strings on this side: the lens
// renders whatever vocabulary the registry declares — a key published
// tomorrow needs zero code change here.
export type StationQueueEnvelope = Readonly<{
  station: string;
  kind: string;
  discipline: readonly string[];
  wip_limit?: number | null;
  over_limit: boolean;
  total: number;
  data: readonly JobLite[];
}>;

// Where the dock's rows came from, plus the station's own facts when
// the registry served them. `derived` is the fallback for a deployed
// cluster that predates the station registry — the yard renders
// whole either way, it just can't show ordering rule or bandwidth
// state it never received.
export type DockStation =
  | Readonly<{
      source: 'station';
      discipline: readonly string[];
      wipLimit: number | null;
      overLimit: boolean;
      total: number;
    }>
  | Readonly<{ source: 'derived' }>;

// Q2's resolution rendered: the ordering rule sits in the lens
// header in the mono-caps idiom — an operator should never wonder
// why the queue is in this order.
export function disciplineLabel(discipline: readonly string[]): string {
  return discipline.map(k => k.toUpperCase()).join(' → ');
}

// Q3's resolution rendered: `wip_limit` is advisory — a lens warning,
// never enforcement. Chip text only when the station declared a limit
// AND the server's verdict says the queue exceeds it.
export function wipAdvisory(station: DockStation): string | null {
  if (station.source !== 'station') return null;
  if (!station.overLimit || station.wipLimit === null) return null;
  return `WIP ${station.total}/${station.wipLimit}`;
}

export type YardState = Readonly<{
  inFlight: readonly TrainRow[];
  dock: readonly CarRow[];
  dockStation: DockStation;
  arrivals: readonly TrainRow[];
}>;

function step(j: JobLite, slug: string, titleFallback: string): StepLite | null {
  return (
    j.steps?.find(
      s => (s.spec_slug ?? '') === slug || s.title === titleFallback,
    ) ?? null
  );
}

const done = (s: StepLite | null) =>
  !!s && (s.status === 'completed' || s.status === 'skipped');

export function trainStatus(j: JobLite): TrainStatus {
  if (done(step(j, 'deployed', 'Deployed to the playground')) || j.status === 'closed')
    return 'ARRIVED';
  if (done(step(j, 'merged', 'Merged into main'))) return 'DEPARTED';
  if (done(step(j, 'pr', 'Open the batched PR'))) return 'BOARDED';
  return 'BOARDING';
}

export function ciLamp(j: JobLite): Lamp {
  const ci = step(j, 'ci', 'CI verdict');
  const result = (ci?.metadata as { result?: string } | null)?.result;
  if (result === 'green') return 'green';
  if (result === 'failing') return 'failing';
  return 'pending';
}

export function toTrainRow(
  j: JobLite,
  shipById: ReadonlyMap<string, JobLite>,
  live: boolean,
): TrainRow {
  const md = (j.metadata ?? {}) as {
    boarded_jobs?: string[];
  };
  const pr = step(j, 'pr', 'Open the batched PR');
  const merged = step(j, 'merged', 'Merged into main');
  const deployed = step(j, 'deployed', 'Deployed to the playground');
  const cars: CarRow[] = (md.boarded_jobs ?? []).map(id => {
    const car = shipById.get(id);
    const cmd = (car?.metadata ?? {}) as {
      branch?: string;
      skip_reason?: string;
    };
    return {
      id,
      kind: car?.kind ?? 'ship-a-change',
      branch: cmd.branch ?? id.slice(0, 8),
      title: car?.title ?? '(car not in window)',
      tags: car?.tags ?? [],
      sim: car ? isSim(car) : false,
      skipReason: cmd.skip_reason ?? null,
    };
  });
  return {
    id: j.id,
    title: j.title,
    prUrl: ((pr?.metadata ?? {}) as { pr_url?: string }).pr_url ?? null,
    status: trainStatus(j),
    lamp: ciLamp(j),
    mergeRef: ((merged?.metadata ?? {}) as { merge_ref?: string }).merge_ref ?? null,
    deployed: ((deployed?.metadata ?? {}) as { deployed?: string }).deployed ?? null,
    cars,
    live,
  };
}

// One packet → one card, whoever chose the packet. Both dock paths —
// the station envelope and the local derivation — map through here,
// so the card grammar cannot fork between them.
function carRow(j: JobLite): CarRow {
  const md = (j.metadata ?? {}) as { branch?: string; skip_reason?: string };
  return {
    id: j.id,
    kind: j.kind,
    branch: md.branch ?? '',
    title: j.title,
    tags: j.tags ?? [],
    sim: isSim(j),
    skipReason: md.skip_reason ?? null,
  };
}

// The fallback derivation: the loading-dock predicate hand-rolled in
// code, kept only for clusters that predate the station registry.
// When `GET /api/stations/loading-dock/queue` serves, the registry
// row (predicate + discipline) replaces all of this.
export function dockRows(ships: readonly JobLite[]): CarRow[] {
  return ships
    .filter(j => {
      const md = (j.metadata ?? {}) as { branch?: string; train?: string };
      if (j.status !== 'open' || !md.branch || md.train) return false;
      const review = step(j, 'review', 'Open for review');
      return !!review && (review.status === 'ready' || review.status === 'active');
    })
    .map(carRow);
}

export function assembleYard(
  trains: readonly JobLite[],
  ships: readonly JobLite[],
  dockQueue: StationQueueEnvelope | null = null,
): YardState {
  const shipById = new Map(ships.map(j => [j.id, j]));
  const open = trains.filter(t => t.status === 'open');
  const closed = trains
    .filter(t => t.status === 'closed')
    .sort((a, b) => b.opened_on.localeCompare(a.opened_on))
    .slice(0, 5);
  // The one signal-green element: the oldest still-moving train.
  const liveId = open.find(t => trainStatus(t) !== 'ARRIVED')?.id;
  return {
    inFlight: open.map(t => toTrainRow(t, shipById, t.id === liveId)),
    // The envelope is authoritative when it served: membership came
    // from the registry predicate and order from the declared
    // discipline — a client re-sort would silently overrule the
    // station row, so the rows map 1:1 in server order.
    dock: dockQueue ? dockQueue.data.map(carRow) : dockRows(ships),
    dockStation: dockQueue
      ? {
          source: 'station',
          discipline: dockQueue.discipline,
          wipLimit: dockQueue.wip_limit ?? null,
          overLimit: dockQueue.over_limit,
          total: dockQueue.total,
        }
      : { source: 'derived' },
    arrivals: closed.map(t => toTrainRow(t, shipById, false)),
  };
}

// The dock's station row, or null when the cluster can't serve one —
// 404 (no such station), 503 (registry not configured), a network
// fault, or a 200 that isn't the envelope all mean the same thing:
// fall back to deriving the dock locally. Never an error the yard
// surfaces; the fallback costs nothing because the ships list is
// fetched anyway for the consist join.
async function fetchDockQueue(): Promise<StationQueueEnvelope | null> {
  try {
    const r = await fetch('/api/stations/loading-dock/queue');
    if (!r.ok) return null;
    const env = (await r.json()) as StationQueueEnvelope;
    return Array.isArray(env?.data) && Array.isArray(env?.discipline) ? env : null;
  } catch {
    return null;
  }
}

export async function fetchYard(): Promise<YardState | null> {
  const [tr, sr, dockQueue] = await Promise.all([
    fetch('/api/jobs?kind=pr-train&limit=20'),
    fetch('/api/jobs?kind=ship-a-change&limit=200'),
    fetchDockQueue(),
  ]);
  if (!tr.ok || !sr.ok) return null;
  const trains = ((await tr.json()) as { data?: JobLite[] }).data ?? [];
  const ships = ((await sr.json()) as { data?: JobLite[] }).data ?? [];
  return assembleYard(trains, ships, dockQueue);
}
