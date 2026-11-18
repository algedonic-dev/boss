// THE ANSWER BAND AND THE RIGHT RAIL — what a station page says in five
// seconds (design 08d1530e, approved by David 2026-11-12 ~02:20Z; car A,
// decided items 1, 6 and 7; backlog 950ad972). E2 on the design's review
// step is the target look.
//
//   The band   Working · Stuck · Needs you as linked counts, and the
//              binding constraint. The always-CLEAR plate, STANDING and
//              the title that repeated the name went: the plate turned
//              red only over a limit, so at 01:30Z it said CLEAR over a
//              probe shed holding 33 packets with an idle seat.
//   The rail   the waits ladder (not admitted, worst first), the pools
//              (station-page.ts `constraintBand`, drawn as one strip
//              each), Stuck, Needs you, and the moves this session saw.
//
// EVERY FIGURE IS A READ'S, AND A FIGURE NO READ STATES IS SAID UNREAD
// (David, 2026-11-12: "keep the data honest"). What each one reads:
//
//   working    the open claims placed inside the station, by part. A
//              claim the record cannot place is not working here; it is
//              on the stuck list.
//   stuck      an idle machine with a queue — a station that declares
//              seats (station-tree.ts `seatsOf`), has packets waiting,
//              and holds no claim — judged only when every claim in the
//              read is placed, by the card's own rule (`stationCard`
//              seats: one claim the record cannot place could be in any
//              of its seats); and the claims that stand at no station. Q3's
//              amber (past its step budget) is NOT judged: no read this
//              page takes carries a step's duration_hours or the instant
//              a step became ready, so the ladder says so once.
//   needs you  the canonical human-required view (human-needs.ts; David
//              2026-10-31: needs-you is the human-only view), over the
//              protocols this station's own stations queue — the load
//              read's `workflow_kind` per station. The root asks for all.
//   binding    the claims read's `binding`: the server's one rule
//              (claims_console.rs `binding`: the pool with the most
//              refused reservations in the window). None named is
//              "nothing refused", and a read without the key is unread.
//   moves      the recorded moves stream, of the protocols held here,
//              newest first, this session only — nothing replays.
//
// Pure functions of the reads, pinned in station-answer.test.ts;
// StationPage.svelte and StationRail.svelte own the strokes.

import type { Remote } from '../../data/remote';
import type { StationLoadRow } from '../marshalling/marshalling';
import { NEEDS_YOU_LIMIT, type NeedsYou } from '../yard/human-needs';
import type { Move } from './move-wire';
import { seatsOf, subtree, type StationTree } from './station-tree';
import { stationHref, type HeldClaim, type PageClaims } from './station-page';

type Load = Remote<ReadonlyMap<string, StationLoadRow>>;

// ---------------------------------------------------------------------
// Words.
// ---------------------------------------------------------------------

const plural = (n: number, one: string, many = `${one}s`): string => `${n} ${n === 1 ? one : many}`;

/** An age in the coarsest whole unit: `45 d`, `9 h`, `11 m`. */
export function ageWords(days: number): string {
  if (days >= 1) return `${Math.floor(days)} d`;
  const hours = days * 24;
  if (hours >= 1) return `${Math.floor(hours)} h`;
  return `${Math.max(0, Math.floor(hours * 60))} m`;
}

const daysSince = (at: string, now: number): number | null => {
  const t = Date.parse(at);
  return Number.isFinite(t) ? Math.max(0, (now - t) / 86_400_000) : null;
};

/** The row title less a leading repeat of the station's own name —
 *  `Engineering — makes the change` under `engineering` is "makes the
 *  change"; a title that is only the name says nothing more. */
export function subtitleOf(name: string, title: string): string | null {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const rest = title.replace(new RegExp(`^${escaped}\\s*(?:[—–-]\\s*)?`, 'i'), '').trim();
  return rest === '' ? null : rest;
}

const failedWhy = (what: string, r: Readonly<{ kind: 'loading' }> | Readonly<{ kind: 'failed'; error: string }>): string =>
  r.kind === 'loading' ? `reading the ${what}` : `the ${what} read failed: ${r.error}`;

/** The claims as the page may judge them: ready and fresh, or why not. */
function claimsOf(claims: Remote<PageClaims>): Readonly<{ held: ReadonlyArray<HeldClaim> }> | Readonly<{ why: string }> {
  if (claims.kind !== 'ready') return { why: failedWhy('claims', claims) };
  if (claims.data.card.sourceUnavailable) return { why: 'the claims read says its source could not be read' };
  return { held: claims.data.held };
}

// ---------------------------------------------------------------------
// The waits ladder.
// ---------------------------------------------------------------------

export type Rung = Readonly<{
  station: string;
  href: string;
  /** The station's queue depth, as the load read counts it. */
  waiting: number;
  /** Waiting less the claims placed at the station; null when the claims
   *  were not read, so admission cannot be told apart. */
  notAdmitted: number | null;
  /** The oldest MEMBER PACKET's age (marshalling.ts: packet age, not time
   *  in this queue — it over-reports rather than under-reports). */
  oldest: string | null;
  state: 'stuck' | 'waiting';
  words: string;
}>;

export type Ladder =
  | Readonly<{ kind: 'rungs'; rungs: ReadonlyArray<Rung>; caveat: string }>
  | Readonly<{ kind: 'unread'; why: string }>;

export const LADDER_CAVEAT =
  'Ages are the oldest packet’s, not time in the queue. No read here carries a step’s budget, so nothing is judged past one.';

export function waitsLadder(tree: StationTree, name: string, load: Load, claims: Remote<PageClaims>): Ladder {
  if (load.kind !== 'ready') return { kind: 'unread', why: failedWhy('load', load) };
  const inside = subtree(tree, name);
  const read = claimsOf(claims);
  const held = 'held' in read ? read.held : null;
  const placeless = held === null ? 0 : held.filter((c) => c.station === null).length;
  const rungs = tree.nodes
    .filter((n) => inside.has(n.name))
    .flatMap((n): Rung[] => {
      const row = load.data.get(n.name);
      if (row === undefined || row.depth <= 0) return [];
      const seats = seatsOf(n);
      const here = held === null ? null : held.filter((c) => c.station === n.name).length;
      const notAdmitted = here === null ? null : Math.max(0, row.depth - here);
      const idle = seats !== null && seats > 0 && here === 0 && placeless === 0;
      const words =
        here === null ? 'Claims unread, so admission is not told apart.'
        : idle ? `${seats === 1 ? 'Its seat is' : `All ${seats} seats are`} idle while ${row.depth} wait.`
        : seats !== null && seats > 0 && placeless > 0 ? `Seats busy not recorded: ${plural(placeless, 'claim stands', 'claims stand')} at no station.`
        : seats !== null && seats > 0 ? `${here} of ${seats} seats held.`
        : here === 0 ? 'Nothing is claimed here.' : `${plural(here, 'claim')} held here.`;
      return [{
        station: n.name,
        href: stationHref(n.name),
        waiting: row.depth,
        notAdmitted,
        oldest: row.oldestAgeDays === null ? null : ageWords(row.oldestAgeDays),
        state: idle ? 'stuck' : 'waiting',
        words,
      }];
    });
  const rank = (r: Rung): readonly [number, number, number] => [r.state === 'stuck' ? 0 : 1, -(r.notAdmitted ?? r.waiting), -(load.data.get(r.station)?.oldestAgeDays ?? 0)];
  const sorted = [...rungs].sort((a, b) => {
    const [x, y] = [rank(a), rank(b)];
    return x[0] - y[0] || x[1] - y[1] || x[2] - y[2];
  });
  return { kind: 'rungs', rungs: sorted, caveat: LADDER_CAVEAT };
}

// ---------------------------------------------------------------------
// The stuck list.
// ---------------------------------------------------------------------

export type StuckItem = Readonly<{ key: string; words: string; age: string | null; href: string | null }>;
export type Stuck = Readonly<{ kind: 'items'; items: ReadonlyArray<StuckItem> }> | Readonly<{ kind: 'unread'; why: string }>;

export function stuckList(tree: StationTree, name: string, load: Load, claims: Remote<PageClaims>, now: number): Stuck {
  const read = claimsOf(claims);
  if (!('held' in read)) return { kind: 'unread', why: read.why };
  const ladder = waitsLadder(tree, name, load, claims);
  if (ladder.kind === 'unread') return ladder;
  const nodeSeats = (s: string): number | null => {
    const n = tree.byName.get(s);
    return n === undefined ? null : seatsOf(n);
  };
  const idle = ladder.rungs.filter((r) => r.state === 'stuck').map((r): StuckItem => ({
    key: `idle:${r.station}`,
    words: `${r.station}: ${r.notAdmitted ?? r.waiting} not admitted, ${(nodeSeats(r.station) ?? 1) === 1 ? 'its seat' : 'its seats'} idle`,
    age: r.oldest,
    href: r.href,
  }));
  const placeless = read.held.filter((c) => c.station === null);
  const ages = placeless.flatMap((c) => (c.since === null ? [] : [daysSince(c.since, now)])).filter((d): d is number => d !== null);
  const nowhere: StuckItem[] = placeless.length === 0 ? [] : [{
    key: 'placeless',
    words: `${plural(placeless.length, 'claim')} at no station`,
    age: ages.length === 0 ? null : ageWords(Math.max(...ages)),
    href: null,
  }];
  return { kind: 'items', items: [...idle, ...nowhere] };
}

// ---------------------------------------------------------------------
// The binding constraint.
// ---------------------------------------------------------------------

export type BindingPlate =
  | Readonly<{ kind: 'pool'; pool: string; figure: string; refused: string }>
  | Readonly<{ kind: 'none'; words: string }>
  | Readonly<{ kind: 'unread'; why: string }>;

const qty = (n: number): string => String(Math.round(n * 100) / 100);

export function bindingPlate(claims: Remote<PageClaims>): BindingPlate {
  const read = claimsOf(claims);
  if (!('held' in read) || claims.kind !== 'ready') return { kind: 'unread', why: 'why' in read ? read.why : 'reading the claims' };
  const b = claims.data.binding;
  if (b.kind === 'unread') return { kind: 'unread', why: 'the claims read names no binding' };
  const window = b.windowHours === null ? 'the window' : `${b.windowHours} h`;
  if (b.kind === 'none') return { kind: 'none', words: `no reservation refused in ${window}` };
  const r = b.reading;
  if (r === null) return { kind: 'pool', pool: b.pool, figure: 'reserved unread', refused: 'refusals unread' };
  const unit = r.unit === '' ? '' : ` ${r.unit}`;
  const figure = r.reserved === null ? 'reserved unread'
    : r.capacity === null ? `${qty(r.reserved)}${unit} reserved, capacity unknown`
    : `${qty(r.reserved)} of ${qty(r.capacity)}${unit} reserved`;
  return { kind: 'pool', pool: b.pool, figure, refused: `${plural(r.refused, 'reservation')} refused in ${window}` };
}

// ---------------------------------------------------------------------
// Needs you.
// ---------------------------------------------------------------------

/** The protocols the human-required view is asked over. */
export type HeldKinds =
  | Readonly<{ kind: 'all' }>
  | Readonly<{ kind: 'kinds'; kinds: ReadonlyArray<string> }>
  | Readonly<{ kind: 'none'; why: string }>
  | Readonly<{ kind: 'unread'; why: string }>;

export function heldKinds(tree: StationTree, name: string, load: Load): HeldKinds {
  if (tree.byName.get(name)?.parent === null) return { kind: 'all' };
  if (load.kind !== 'ready') return { kind: 'unread', why: failedWhy('load', load) };
  const inside = subtree(tree, name);
  const kinds = [...new Set([...load.data.values()].filter((r) => inside.has(r.station)).flatMap((r) => (r.workflowKind === null ? [] : [r.workflowKind])))].sort();
  return kinds.length === 0 ? { kind: 'none', why: 'no station here queues a protocol' } : { kind: 'kinds', kinds };
}

/** The view's first page; human-needs.ts `fetchNeedsYou` pages it. The
 *  server parses `kinds` as a JSON array and answers 400 otherwise. */
export function needsYouUrl(k: Readonly<{ kind: 'all' }> | Readonly<{ kind: 'kinds'; kinds: ReadonlyArray<string> }>): string {
  const base = `/api/jobs?status=open&human_required_view=true&limit=${NEEDS_YOU_LIMIT}`;
  return k.kind === 'all' ? base : `${base}&kinds=${encodeURIComponent(JSON.stringify(k.kinds))}`;
}

export type Count =
  | Readonly<{ kind: 'count'; n: number; floor: boolean }>
  | Readonly<{ kind: 'unread'; why: string }>;
export type NeedLine = Readonly<{ id: string; title: string; step: string; age: string; href: string }>;
export type NeedsList =
  | Readonly<{ kind: 'lines'; count: Count; lines: ReadonlyArray<NeedLine>; more: number; cut: string | null }>
  | Readonly<{ kind: 'unread'; why: string; count: Count }>;

/** How many needs the rail lists before it counts the rest. */
export const NEEDS_SHOWN = 5;

export function needsYouList(needs: Remote<NeedsYou>, now: number): NeedsList {
  if (needs.kind !== 'ready') {
    const why = needs.kind === 'loading' ? 'reading the human-required view' : `the human-required read failed: ${needs.error}`;
    return { kind: 'unread', why, count: { kind: 'unread', why } };
  }
  const lines = needs.data.rows
    .filter((j) => j.status === 'open')
    .flatMap((j) => j.steps
      .filter((s) => s.human_required && (s.status === 'ready' || s.status === 'active'))
      .map((s): NeedLine => {
        const d = j.opened_on === null ? null : daysSince(`${j.opened_on.slice(0, 10)}T00:00:00Z`, now);
        return {
          id: s.id, title: j.title, step: s.title, age: d === null ? '?' : ageWords(d),
          href: `/ux/jobs/${encodeURIComponent(j.id)}/steps/${encodeURIComponent(s.id)}`,
        };
      }));
  const { cut } = needs.data;
  return {
    kind: 'lines',
    count: { kind: 'count', n: lines.length, floor: cut !== null },
    lines: lines.slice(0, NEEDS_SHOWN),
    more: Math.max(0, lines.length - NEEDS_SHOWN),
    cut,
  };
}

// ---------------------------------------------------------------------
// The band.
// ---------------------------------------------------------------------

export type BandCount = (Readonly<{ kind: 'count'; n: number; floor: boolean; words: string }>) | Readonly<{ kind: 'unread'; why: string }>;
export type AnswerBand = Readonly<{ working: BandCount; stuck: BandCount }>;

export function answerBand(tree: StationTree, name: string, load: Load, claims: Remote<PageClaims>, now: number): AnswerBand {
  const read = claimsOf(claims);
  const node = tree.byName.get(name);
  const working = ((): BandCount => {
    if (!('held' in read) || node === undefined) return { kind: 'unread', why: 'why' in read ? read.why : 'no such station' };
    const inside = subtree(tree, name);
    const kids = node.children.map((k) => [k, subtree(tree, k)] as const);
    const placed = read.held.filter((c) => c.station !== null && inside.has(c.station));
    const partOf = (s: string): string => kids.find(([, t]) => t.has(s))?.[0] ?? name;
    const parts = [name, ...node.children]
      .map((p) => [p, placed.filter((c) => partOf(c.station!) === p).length] as const)
      .filter(([, n]) => n > 0)
      .map(([p, n]) => `${n} at ${p === name ? `${p} itself` : p}`);
    return { kind: 'count', n: placed.length, floor: false, words: parts.length === 0 ? 'working: nothing claimed here' : `working: ${parts.join(', ')}` };
  })();
  const stuck = ((): BandCount => {
    const s = stuckList(tree, name, load, claims, now);
    if (s.kind === 'unread') return s;
    return { kind: 'count', n: s.items.length, floor: false, words: s.items.length === 0 ? 'stuck: nothing judged stuck' : `stuck: ${s.items.map((i) => i.words).join('; ')}` };
  })();
  return { working, stuck };
}

/** The band's Needs-you count, from the rail's list. */
export function needsBand(list: NeedsList): BandCount {
  if (list.kind === 'unread') return { kind: 'unread', why: list.why };
  const { count } = list;
  if (count.kind === 'unread') return count;
  const words = count.n === 0 && !count.floor ? 'need you: no human-required step here'
    : `need you: ${count.floor ? 'at least ' : ''}${plural(count.n, 'human-required step')}`;
  return { kind: 'count', n: count.n, floor: count.floor, words };
}

// ---------------------------------------------------------------------
// Moves.
// ---------------------------------------------------------------------

/** How many of this session's moves the rail keeps. */
export const RAIL_MOVES_MAX = 8;

export type RailMove = Readonly<{ seq: number; at: string; label: string; href: string; route: string; cause: string }>;

/** The list with `mv` admitted when it is a protocol held here: newest
 *  first, each seq once, bounded. Until the held protocols are read,
 *  nothing is admitted — a move is never placed by guessing. The route
 *  is the record's own words; moves.rs still keys them by the retired
 *  regions (design 08d1530e item 4), which the rail says. */
export function admitRailMove(list: ReadonlyArray<RailMove>, mv: Move, kinds: HeldKinds): ReadonlyArray<RailMove> {
  if (kinds.kind === 'unread' || kinds.kind === 'none') return list;
  if (kinds.kind === 'kinds' && !kinds.kinds.includes(mv.kind)) return list;
  if (list.some((m) => m.seq === mv.seq)) return list;
  const entry: RailMove = {
    seq: mv.seq,
    at: `${mv.at.slice(11, 19)}Z`,
    label: mv.label,
    href: `/ux/jobs/${encodeURIComponent(mv.packet)}`,
    route: `${mv.from ?? 'filed'} › ${mv.to ?? 'off the map'}`,
    cause: mv.cause_kind,
  };
  return [entry, ...list].sort((a, b) => b.seq - a.seq).slice(0, RAIL_MOVES_MAX);
}
