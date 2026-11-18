// MOTION ON THE STATION PAGE — how the parts move a job (design c4854fd4
// E3, decided by David 2026-11-07 23:38Z: "Draw it as in E3. The parts
// sit on the chosen protocol's line, and a car glides once at the time
// its move was recorded, with its resources moving in the constraint
// band. The list of moves sits under the line, and nothing replays.";
// backlog 87c5293f, car 3 of 3). The motion band it drew
// (StationMotion.svelte) was replaced by the station line (StationLine.svelte,
// design 08d1530e car B) and deleted with its line, cars and glide
// (backlog 462fb22a). What stays is what the line still reads:
//
//   The chooser  the station's own protocols: every workflow row whose
//                `metadata.owner` (T6 car A, a station's stable id) is
//                the station or a station inside it, its own first.
//   The place    where a step runs, read off the row, in this order: T6's
//                `metadata_defaults.at` (a station id) or `fulfilled_by`
//                (a hand-off to another protocol); else the step's station
//                spec (`executor = "equipment:<station>"`); else its
//                audience's station; else "placement not declared".
//   The band     what a moved packet's claims hold after less what they
//                held before — or said not carried.
//   The list     the moves this page session saw, newest first.
//   The feed     a stream the browser gave up is STALE, says why, and
//                moves nothing.
//
// REAL, CURRENT ACTIVITY ONLY (David, 2026-11-08 00:50Z). Nothing here
// reads a membership generation, a flow read, a border rate or an "as
// of" cut, and nothing folds history.
//
// Pure functions of the reads, pinned in station-motion.test.ts;
// StationLine.svelte owns the stream and the strokes.

import { subtree, type StationTree } from './station-tree';
import type { HeldClaim } from './station-page';

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj | null => (v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Obj) : null);
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

// ---------------------------------------------------------------------
// The protocols a station owns.
// ---------------------------------------------------------------------

/** One protocol step, with every key the line places it by. */
export type ProtocolStep = Readonly<{
  slug: string;
  /** A trigger opens the packet (its defaults carry `trigger_kind`): it
   *  is where a packet comes from, not a stop on the line. */
  trigger: boolean;
  /** A terminal closes it: where it leaves, not a stop either. */
  terminal: boolean;
  /** T6: a station's stable id. */
  at: string | null;
  /** T6: the protocol this step hands its packet to. */
  fulfilledBy: string | null;
  /** The step's station spec, e.g. `equipment:gates`. */
  executor: string | null;
  audienceStation: string | null;
}>;

export type Protocol = Readonly<{
  kind: string;
  label: string;
  /** The owning station's stable id (T6 car A); null on a row naming none. */
  owner: string | null;
  steps: ReadonlyArray<ProtocolStep>;
}>;

function parseStep(raw: unknown): ProtocolStep | null {
  const s = obj(raw);
  const slug = str(s?.slug) ?? str(s?.title);
  if (s === null || slug === null) return null;
  const defaults = obj(s.metadata_defaults);
  return {
    slug,
    trigger: str(defaults?.trigger_kind) !== null,
    terminal: obj(s.terminal) !== null,
    at: str(defaults?.at),
    fulfilledBy: str(defaults?.fulfilled_by),
    executor: str(s.executor),
    audienceStation: str(obj(defaults?.audience)?.station),
  };
}

/** GET /api/workflows: one row per protocol. A wrong shape throws — an
 *  empty chooser would read as a station that owns nothing. */
export function parseProtocols(raw: unknown): ReadonlyArray<Protocol> {
  if (!Array.isArray(raw)) throw new Error('workflows read: expected [...]');
  return raw.flatMap((r): Protocol[] => {
    const row = obj(r);
    const kind = str(row?.kind);
    if (row === null || kind === null) return [];
    const steps = (Array.isArray(row.steps) ? row.steps : []).flatMap((s) => parseStep(s) ?? []);
    return [{ kind, label: str(row.label) ?? kind, owner: str(obj(row.metadata)?.owner), steps }];
  });
}

export type ProtocolChoice = Readonly<{ kind: string; label: string; owner: string }>;

/** The station's own protocols: owned by it, then by the stations
 *  inside it, each group by kind. */
export function stationProtocols(tree: StationTree, station: string, protocols: ReadonlyArray<Protocol>): ReadonlyArray<ProtocolChoice> {
  const inside = subtree(tree, station);
  const nameOf = new Map(tree.nodes.flatMap((n) => (n.id === null ? [] : [[n.id, n.name] as const])));
  return protocols
    .flatMap((p): ProtocolChoice[] => {
      const owner = p.owner === null ? undefined : nameOf.get(p.owner);
      return owner !== undefined && inside.has(owner) ? [{ kind: p.kind, label: p.label, owner }] : [];
    })
    .sort((a, b) => Number(a.owner !== station) - Number(b.owner !== station) || a.kind.localeCompare(b.kind));
}

// ---------------------------------------------------------------------
// Where a step runs.
// ---------------------------------------------------------------------

export type StepPlace =
  | Readonly<{ how: 'at' | 'executor' | 'audience'; station: string }>
  | Readonly<{ how: 'hand-off'; kind: string }>
  | Readonly<{ how: 'unknown station'; ref: string }>
  | Readonly<{ how: 'not declared' }>;

const EQUIPMENT = 'equipment:';

/** Where a step runs, by the first key that says. A station id or name
 *  this read does not hold is said, never placed. */
export function placeOf(tree: StationTree, step: ProtocolStep): StepPlace {
  if (step.at !== null) {
    const node = tree.nodes.find((n) => n.id === step.at);
    return node === undefined ? { how: 'unknown station', ref: step.at } : { how: 'at', station: node.name };
  }
  if (step.fulfilledBy !== null) return { how: 'hand-off', kind: step.fulfilledBy };
  const named = (how: 'executor' | 'audience', name: string): StepPlace =>
    tree.byName.has(name) ? { how, station: name } : { how: 'unknown station', ref: name };
  if (step.executor !== null && step.executor.startsWith(EQUIPMENT)) return named('executor', step.executor.slice(EQUIPMENT.length));
  if (step.audienceStation !== null) return named('audience', step.audienceStation);
  return { how: 'not declared' };
}

export const jobHref = (id: string): string => `/ux/jobs/${encodeURIComponent(id)}`;

// ---------------------------------------------------------------------
// What moves with a packet.
// ---------------------------------------------------------------------

/** One signed change in what a packet's claims hold. */
export type Delta = Readonly<{ pool: string; station: string | null; amount: number }>;

/** What `packet`'s claims hold after less what they held before, per
 *  pool and station; null when neither read carries an allocation for
 *  it — not carried, which is not zero. */
export function resourceDelta(before: ReadonlyArray<HeldClaim>, after: ReadonlyArray<HeldClaim>, packet: string): ReadonlyArray<Delta> | null {
  const held = (claims: ReadonlyArray<HeldClaim>) =>
    claims.filter((c) => c.jobId === packet).flatMap((c) => c.allocations.map((a) => ({ key: `${a.pool}\u0000${c.station ?? ''}`, pool: a.pool, station: c.station, amount: a.amount })));
  const was = held(before);
  const now = held(after);
  if (was.length === 0 && now.length === 0) return null;
  const keys = [...new Map([...was, ...now].map((a) => [a.key, a] as const)).values()];
  const total = (xs: typeof was, key: string) => xs.filter((a) => a.key === key).reduce((n, a) => n + (a.amount ?? 0), 0);
  return keys
    .map((k) => ({ pool: k.pool, station: k.station, amount: total(now, k.key) - total(was, k.key) }))
    .filter((d) => d.amount !== 0)
    .sort((a, b) => a.amount - b.amount);
}

const qty = (n: number): string => String(Math.round(n * 100) / 100);

export function deltaWords(d: ReadonlyArray<Delta> | null): string {
  if (d === null) return 'the claims read carries no allocation for it';
  if (d.length === 0) return 'its claims hold what they held';
  return d
    .map((x) => `${x.station ?? 'a station the record cannot name'} ${x.amount < 0 ? 'releases' : 'takes'} ${qty(Math.abs(x.amount))} of ${x.pool}`)
    .join('; ');
}

/** What the constraint band draws while a car glides: the pools its
 *  claims moved in, and the sentence. */
export type Moving = Readonly<{ label: string; pools: ReadonlyArray<string>; words: string }>;

// ---------------------------------------------------------------------
// The list of moves.
// ---------------------------------------------------------------------

/** How many of this session's moves the list keeps. */
export const MOVES_MAX = 30;

export type SessionMove = Readonly<{
  seq: number;
  /** The recorded instant as the stream sent it; the list draws it local
   *  and UTC through the shared formatter (design b03ef2d4). */
  at: string;
  packet: string;
  label: string;
  href: string;
  kind: string;
  /** The stops it moved between, in the line's words. */
  route: string;
  /** The places the record names, as the moves stream sends them. */
  regions: string;
  delta: string | null;
}>;

/** The list with `e` admitted: newest first, each seq once, bounded. */
export function admitMove(list: ReadonlyArray<SessionMove>, e: SessionMove): ReadonlyArray<SessionMove> {
  if (list.some((m) => m.seq === e.seq)) return list;
  return [e, ...list].sort((a, b) => b.seq - a.seq).slice(0, MOVES_MAX);
}

// ---------------------------------------------------------------------
// The feed.
// ---------------------------------------------------------------------

export type Feed = 'connecting' | 'live' | 'down';

export type FeedState = Readonly<{ plate: 'live' | 'reading' | 'stale'; word: string; moves: boolean; why: string | null }>;

export function feedState(feed: Feed, why: string): FeedState {
  if (feed === 'live') return { plate: 'live', word: 'live', moves: true, why: null };
  if (feed === 'connecting') return { plate: 'reading', word: 'connecting', moves: false, why: null };
  const said = why.trim() === '' ? '' : ` (${why.trim()})`;
  return { plate: 'stale', word: 'stale', moves: false, why: `the moves stream is down${said}, so nothing on this line is drawn moving until it answers` };
}
