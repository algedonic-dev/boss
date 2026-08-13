// The station map's data — the network's nodes, straight from the
// station registry (docs/design/stations.md: priority queues,
// stations, and network nodes are one concept). Every node here is a
// registry row rendered; a station published tomorrow appears with
// zero code change, which is the point. No edges in v1 — routes need
// the evented-motion follow-up — and reads only, so the guest landing
// renders whole.

import {
  protocolHue,
  type PacketCardData,
} from '@boss/web-kit/ui/packet-card';
import {
  isSim,
  upstreamButton,
  type JobLite,
  type StationUpstream,
  type UpstreamButton,
} from '../yard/yard';

// The registry row as `GET /api/stations` serializes it (StationSpec
// in boss-jobs). `kind` stays an open string: the chip colors by
// protocolHue(kind), so a future taxonomy entry needs no code here.
export type StationRow = Readonly<{
  name: string;
  version: number;
  title: string;
  kind: string;
  discipline?: readonly string[];
  wip_limit?: number | null;
  rollup_parent?: string | null;
}>;

// One node on the map. `depth` is null until the first queue poll
// answers — unknown renders as unknown, never as zero.
export type StationNode = Readonly<{
  name: string;
  title: string;
  kind: string;
  hue: string;
  discipline: string;
  wipLimit: number | null;
  depth: number | null;
  overLimit: boolean;
}>;

export type MapState =
  | { kind: 'loading' }
  /// The endpoint 404s (older cluster) or 503s (registry not
  /// configured): the registry hasn't reached this deployment yet.
  | { kind: 'unavailable' }
  | { kind: 'error' }
  | { kind: 'ready'; nodes: readonly StationNode[] };

// A station's evaluated queue, shaped for rendering: the yard's
// packet-card grammar plus the lens-header facts (discipline named,
// advisory over-limit verdict kept — stations.md Q2/Q3).
export type StationQueueView = Readonly<{
  station: string;
  discipline: string;
  wipLimit: number | null;
  overLimit: boolean;
  total: number;
  cards: readonly PacketCardData[];
  /// The walk upstream, when the registry row declares one. Same
  /// affordance the yard's dock renders, from the same envelope field
  /// and the same mapper — a node that reads shallower than expected
  /// is diagnosed one click upstream, and the map is where an operator
  /// notices the depth.
  upstream: UpstreamButton | null;
}>;

/// The ratified default discipline, phrased the way the doc phrases
/// it: an operator should never wonder why the queue is in this order.
export function disciplineLabel(keys: readonly string[] | undefined): string {
  const effective = keys && keys.length > 0 ? keys : ['priority', 'age'];
  return effective.join(', then ');
}

export function toStationNode(row: StationRow): StationNode {
  return {
    name: row.name,
    title: row.title || row.name,
    kind: row.kind,
    hue: protocolHue(row.kind),
    discipline: disciplineLabel(row.discipline),
    wipLimit: row.wip_limit ?? null,
    depth: null,
    overLimit: false,
  };
}

export function withDepth(
  node: StationNode,
  queue: Readonly<{ total: number; over_limit: boolean }>,
): StationNode {
  return { ...node, depth: queue.total, overLimit: queue.over_limit };
}

/// Pure classification of the `GET /api/stations` answer. 404 and 503
/// are the "registry hasn't reached this deployment" states, not
/// errors; a bare-array body (older mock harnesses, defensive) reads
/// as zero rows rather than crashing on `.data`.
export function stationsStateFromResponse(status: number, body: unknown): MapState {
  if (status === 404 || status === 503) return { kind: 'unavailable' };
  if (status < 200 || status >= 300) return { kind: 'error' };
  const rows: StationRow[] = Array.isArray(body)
    ? (body as StationRow[])
    : ((body as { data?: StationRow[] } | null)?.data ?? []);
  return { kind: 'ready', nodes: rows.map(toStationNode) };
}

/// The yard grammar for a queue packet. Station queues return bare
/// Jobs (no steps), so the mono provenance line is the branch when the
/// packet names one and the opened date otherwise.
export function toQueueCard(j: JobLite): PacketCardData {
  const md = (j.metadata ?? {}) as { branch?: string };
  return {
    id: j.id,
    kind: j.kind,
    branch: md.branch ?? `opened ${j.opened_on}`,
    title: j.title,
    tags: j.tags ?? [],
    sim: isSim(j),
  };
}

/// Shape-checked mapping of the `GET /api/stations/{name}/queue`
/// envelope. Anything that isn't envelope-shaped — an error body, the
/// mocked-crawl `[]` catch-all — is null, and the page says so.
export function queueViewFromBody(body: unknown): StationQueueView | null {
  if (body === null || typeof body !== 'object' || Array.isArray(body)) return null;
  const e = body as {
    station?: unknown;
    discipline?: readonly string[];
    wip_limit?: number | null;
    over_limit?: unknown;
    upstream?: StationUpstream | null;
    total?: unknown;
    data?: unknown;
  };
  if (typeof e.station !== 'string' || !Array.isArray(e.data)) return null;
  const jobs = e.data as JobLite[];
  return {
    station: e.station,
    discipline: disciplineLabel(e.discipline),
    wipLimit: e.wip_limit ?? null,
    overLimit: e.over_limit === true,
    total: typeof e.total === 'number' ? e.total : jobs.length,
    cards: jobs.map(toQueueCard),
    upstream: upstreamButton(e.upstream),
  };
}

// ---------------------------------------------------------------------------
// I/O edges — thin fetch wrappers over the pure mappers above.
// ---------------------------------------------------------------------------

export async function fetchStations(): Promise<MapState> {
  try {
    const r = await fetch('/api/stations');
    const body: unknown = await r.json().catch(() => null);
    return stationsStateFromResponse(r.status, body);
  } catch {
    return { kind: 'error' };
  }
}

export async function fetchQueue(name: string): Promise<StationQueueView | null> {
  try {
    const r = await fetch(`/api/stations/${encodeURIComponent(name)}/queue`);
    if (!r.ok) return null;
    return queueViewFromBody(await r.json().catch(() => null));
  } catch {
    return null;
  }
}
