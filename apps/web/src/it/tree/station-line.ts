// THE STATION LINE — the engineering page body (design 08d1530e car B,
// approved by David 2026-11-12 ~02:20Z with Q1 "interiors inline on the
// line"; exhibit E2 on its review step; backlog 9d0b0944).
//
// David, 2026-11-12: "we want to be able to see the jobs actually moving
// as work is completed at stations, or machines within stations", then on
// E1: "We need an admission queue for work that the station hasn't
// intaked yet" and "draw all the lines between all the internal station
// parts ... maybe we consolidate at some number". So:
//
//   The chooser  the station's own protocols as count chips; the default
//                is the one with the most open packets (GET /api/jobs/kinds),
//                never the first by name — the old band opened on a
//                protocol with 0 packets beside one with 89.
//   A stop       one step of that protocol, drawn as the station box its
//                step names: the step's own spec first (station-motion.ts
//                `placeOf`), else the ONE station whose admission
//                predicate names this protocol's step. A step neither
//                names stands as a stop with no box.
//   Its inside   admission ('in': a ready step with no open claim), then
//                its seats (the row's capacity) as cells holding named
//                packets with their dwell, then the receipt, and every
//                path between them; more than BUNDLE_ABOVE parallel paths
//                are one counted line that fans out at the parts.
//   The gates    whether a gate-run holds a seat is read off its metadata —
//                a `gate_bay` object seats it, `gate_reservation.consumed
//                =false` is the machine's own queue — because the step
//                stays ready and unclaimed throughout and no claim names
//                the seat (E2's finding). The box says so. The seat's own
//                name is not drawn: the station tree's words count a
//                station's capacity, and name no seat (factory-taxonomy-v2).
//   Marks        the claim's `executor_kind` (person, robot, automaton);
//                no claim, no mark — never the robot mark by default.
//   The tray     a packet the record does not place: at a step naming no
//                station, pinned to a version this line does not read the
//                placement of, or at no step of the line. Counted by step
//                and pinned version, never drawn at a station by guessing.
//
// REAL, CURRENT READS ONLY (David, 2026-11-08 00:50Z): the protocol rows,
// the open count per protocol, the chosen protocol's open packets and the
// open claims. Steps carry no `ready_at`, so an admission age runs from
// the step before it completing (said beside it), and a gate seat's dwell
// from when the packet was queued for it.
//
// Pure functions of the reads, pinned in station-line.test.ts;
// StationLine.svelte owns the reads, the stream and the strokes.

import { HOLDER_MARK, type HeldClaim } from './station-page';
import { jobHref, parseProtocols, placeOf, type Protocol, type ProtocolChoice, type ProtocolStep } from './station-motion';
import { subtree, type StationTree, type TreeNode } from './station-tree';

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj | null => (v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Obj) : null);
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);

/** Past this many parallel paths, one counted line (David's "consolidate at some number"). */
export const BUNDLE_ABOVE = 4;
/** Past this many cells, a box counts the rest. */
export const SEATS_MAX = 8;
/** Dashed tags an admission column or a machine queue draws before its count. */
export const TAGS_MAX = 3;

/** An age as a person reads it: minutes, then hours, then days. */
export function ago(ms: number): string {
  const m = Math.max(0, Math.floor(ms / 60_000));
  if (m < 60) return `${m} m`;
  const h = Math.floor(m / 60);
  return h < 48 ? `${h} h` : `${Math.floor(h / 24)} d`;
}

// ---------------------------------------------------------------------
// The protocols and their open counts.
// ---------------------------------------------------------------------

export type LineProtocols = Readonly<{ protocols: ReadonlyArray<Protocol>; versions: ReadonlyMap<string, number> }>;

/** GET /api/workflows, with each row's version: a packet pinned to another
 *  version is placed only where the station admits it whatever its version. */
export function parseLineProtocols(raw: unknown): LineProtocols {
  const protocols = parseProtocols(raw);
  const versions = new Map((raw as ReadonlyArray<unknown>).flatMap((r) => {
    const kind = str(obj(r)?.kind);
    const v = num(obj(r)?.version);
    return kind === null || v === null ? [] : [[kind, v] as const];
  }));
  return { protocols, versions };
}

export type KindCount = Readonly<{ kind: string; open: number; byVersion: Readonly<Record<string, number>> }>;

/** GET /api/jobs/kinds. A wrong shape throws: no counts is not zero. */
export function parseKindCounts(raw: unknown): ReadonlyMap<string, KindCount> {
  const rows = obj(raw)?.kinds;
  if (!Array.isArray(rows)) throw new Error('kinds read: expected {kinds: [...]}');
  return new Map(rows.flatMap((r) => {
    const o = obj(r);
    const kind = str(o?.kind);
    const open = num(o?.open);
    if (o === null || kind === null || open === null) return [];
    const by = Object.fromEntries(Object.entries(obj(o.open_by_version) ?? {}).flatMap(([v, n]) => (num(n) === null ? [] : [[v, num(n) as number]])));
    return [[kind, { kind, open, byVersion: by }] as const];
  }));
}

/** The owned protocol with the most open packets; the chooser's first
 *  when the counts are unread or every one is empty. */
export function defaultProtocol(choices: ReadonlyArray<ProtocolChoice>, counts: ReadonlyMap<string, KindCount> | null): string | null {
  const open = (k: string) => counts?.get(k)?.open ?? 0;
  const best = choices.reduce<ProtocolChoice | null>((b, c) => (b === null || open(c.kind) > open(b.kind) ? c : b), null);
  return best?.kind ?? null;
}

export type Chip = Readonly<{ kind: string; owner: string; open: number | null }>;

/** The chips: every owned protocol with an open packet, most first, and
 *  the chosen one; the rest are only counted. Unread counts draw them all. */
export function protocolChips(choices: ReadonlyArray<ProtocolChoice>, counts: ReadonlyMap<string, KindCount> | null, chosen: string | null): Readonly<{ chips: ReadonlyArray<Chip>; none: number }> {
  if (counts === null) return { chips: choices.map((c) => ({ kind: c.kind, owner: c.owner, open: null })), none: 0 };
  const all = choices.map((c) => ({ kind: c.kind, owner: c.owner, open: counts.get(c.kind)?.open ?? 0 }));
  const chips = all.filter((c) => c.open > 0 || c.kind === chosen).sort((a, b) => b.open - a.open);
  return { chips, none: all.length - chips.length };
}

// ---------------------------------------------------------------------
// The packets.
// ---------------------------------------------------------------------

/** Where a packet's metadata says it stands at a station's machine:
 *  queued for a seat, or seated (by `gate_bay`, naming its station). */
export type SeatMark =
  | Readonly<{ state: 'none' }>
  | Readonly<{ state: 'queued'; since: string | null }>
  | Readonly<{ state: 'seated'; station: string | null; since: string | null }>;

export type LineStepRow = Readonly<{ id: string | null; slug: string; status: string; completedAt: string | null }>;

export type LinePacket = Readonly<{
  id: string;
  title: string;
  open: boolean;
  /** The protocol version the packet is pinned to; null unread. */
  version: number | null;
  openedAt: string | null;
  steps: ReadonlyArray<LineStepRow>;
  seat: SeatMark;
}>;

function seatMark(md: Obj | null): SeatMark {
  const gr = obj(md?.gate_reservation);
  const gb = obj(md?.gate_bay);
  const since = str(gr?.queued_at);
  if (gr?.consumed === false) return { state: 'queued', since };
  if (gb !== null || gr?.consumed === true) return { state: 'seated', station: str(obj(gb?.station)?.name), since };
  return { state: 'none' };
}

/** One packet as GET /api/jobs/{id} (or a row of the list) serves it. */
export function parseLinePacket(raw: unknown): LinePacket | null {
  const o = obj(raw);
  const id = str(o?.id);
  if (o === null || id === null || !Array.isArray(o.steps)) return null;
  const steps = o.steps.flatMap((s): LineStepRow[] => {
    const row = obj(s);
    const slug = str(row?.spec_slug);
    const status = str(row?.status);
    return row === null || slug === null || status === null ? [] : [{ id: str(row.id), slug, status, completedAt: str(row.completed_at) }];
  });
  return {
    id,
    title: str(o.title) ?? id,
    open: str(o.status) === 'open',
    version: num(o.workflow_version),
    openedAt: str(o.opened_at),
    steps,
    seat: seatMark(obj(o.metadata)),
  };
}

export type LinePacketsRead = Readonly<{ packets: ReadonlyArray<LinePacket>; total: number }>;

/** GET /api/jobs?kind=…&status=open. A wrong shape throws: an empty list would draw an idle line. */
export function parseLinePackets(raw: unknown): LinePacketsRead {
  const o = obj(raw);
  if (o === null || !Array.isArray(o.data)) throw new Error('packets read: expected {data: [...]}');
  const packets = o.data.flatMap((r) => parseLinePacket(r) ?? []);
  return { packets, total: num(o.total) ?? packets.length };
}

// ---------------------------------------------------------------------
// The line.
// ---------------------------------------------------------------------

export type Tag = Readonly<{ id: string; title: string; href: string }>;
export type Waiting = Readonly<{ count: number; tags: ReadonlyArray<Tag>; oldest: string | null }>;

export type Cell = Readonly<{
  key: string;
  /** The seat's holder, `seated` for a gate seat, `a seat` when free. */
  label: string;
  packet: Tag | null;
  holder: string | null;
  /** The claim's executor_kind as its mark; null with no claim. */
  mark: string | null;
  dwell: string | null;
  free: boolean;
}>;

export type PlacedBy = 'at' | 'executor' | 'audience' | 'admission predicate';

export type LineStop = Readonly<{
  slug: string;
  /** The tree station the step names; null when it names none. */
  station: string | null;
  placedBy: PlacedBy | null;
  handoff: string | null;
  /** Why the stop has no box, or where it stands when not inside the page's station. */
  note: string | null;
  machines: ReadonlyArray<string>;
  /** Declared seats: the row's capacity; null where the row declares none. */
  seats: number | null;
  in: Waiting;
  /** The machine's own queue; null where no packet here carries a seat mark. */
  queue: Waiting | null;
  cells: ReadonlyArray<Cell>;
  /** Held cells past SEATS_MAX. */
  more: number;
  /** Packets at this step the tray holds instead. */
  tray: number;
  /** A queue waits and every declared seat is free. */
  idle: boolean;
  /** What the record cannot say here, each a sentence. */
  notes: ReadonlyArray<string>;
}>;

export type TrayRow = Readonly<{ reason: string; step: string | null; version: number | null; count: number; sample: ReadonlyArray<Tag> }>;

export type StationLineModel = Readonly<{
  kind: string;
  label: string;
  version: number | null;
  stops: ReadonlyArray<LineStop>;
  tray: ReadonlyArray<TrayRow>;
  read: Readonly<{ shown: number; total: number; unread: number }>;
}>;

type Placement = Readonly<{ station: string | null; by: PlacedBy | null; handoff: string | null; note: string | null; anyVersion: boolean }>;

/** The stations whose admission predicate names `kind`'s step `slug`. */
const admitting = (tree: StationTree, kind: string, slug: string): ReadonlyArray<TreeNode> =>
  tree.nodes.filter((n) => n.admits !== null && n.admits.kind === kind && n.admits.step === slug);

function placement(tree: StationTree, kind: string, step: ProtocolStep): Placement {
  const p = placeOf(tree, step);
  const byPredicate = admitting(tree, kind, step.slug);
  // A predicate with a further condition (the dock's: a branch, no train,
  // no hold) admits only SOME packets at the step; drawing every one there
  // would overstate what the station holds, so it places nothing.
  const sole = byPredicate.length === 1 ? byPredicate[0]! : null;
  const whole = sole !== null && sole.admits?.narrower !== true ? sole : null;
  const none = (note: string): Placement => ({ station: null, by: null, handoff: null, note, anyVersion: false });
  switch (p.how) {
    case 'hand-off':
      return { station: null, by: null, handoff: p.kind, note: `hands off to ${p.kind}`, anyVersion: false };
    case 'unknown station':
      return none(`its step names ${p.ref}, which this read does not hold`);
    case 'not declared':
      if (whole !== null) return { station: whole.name, by: 'admission predicate', handoff: null, note: null, anyVersion: true };
      if (sole !== null) return none(`its step names no station, and ${sole.name} admits only some of them, by a predicate with more conditions`);
      return none(byPredicate.length > 1 ? `its step names no station, and ${byPredicate.length} stations admit it` : 'its step names no station');
    default:
      return { station: p.station, by: p.how, handoff: null, note: null, anyVersion: whole?.name === p.station };
  }
}

/** The stop a packet stands at: its first active step on the line, else its first ready one. */
function currentSlug(slugs: ReadonlyArray<string>, p: LinePacket): string | null {
  if (!p.open) return null;
  const status = new Map(p.steps.map((s) => [s.slug, s.status] as const));
  return slugs.find((s) => status.get(s) === 'active') ?? slugs.find((s) => status.get(s) === 'ready') ?? null;
}

const tagOf = (p: LinePacket): Tag => ({ id: p.id, title: p.title, href: jobHref(p.id) });
const since = (now: number, at: string | null): number | null => {
  const t = at === null ? Number.NaN : Date.parse(at);
  return Number.isFinite(t) ? now - t : null;
};
/** When a packet arrived at a step: the latest completion before it, else its opening. */
const arrivedAt = (p: LinePacket): string | null =>
  p.steps.reduce<string | null>((latest, s) => (s.completedAt !== null && (latest === null || s.completedAt > latest) ? s.completedAt : latest), null) ?? p.openedAt;

function waiting(now: number, rows: ReadonlyArray<Readonly<{ p: LinePacket; at: string | null }>>): Waiting {
  const ages = rows.map((r) => since(now, r.at)).filter((a): a is number => a !== null);
  const sorted = [...rows].sort((a, b) => (since(now, b.at) ?? -1) - (since(now, a.at) ?? -1));
  return { count: rows.length, tags: sorted.slice(0, TAGS_MAX).map((r) => tagOf(r.p)), oldest: ages.length === 0 ? null : ago(Math.max(...ages)) };
}

type Held = Readonly<{ cell: Cell; dwellMs: number | null }>;

/** The protocol laid along its stations; null for a station the tree does not hold. */
export function stationLine(i: Readonly<{
  tree: StationTree;
  station: string;
  protocol: Protocol;
  /** The row's version the placements were read from. */
  version: number | null;
  packets: LinePacketsRead;
  /** The open claims; null unread. */
  claims: ReadonlyArray<HeldClaim> | null;
  now: number;
}>): StationLineModel | null {
  const { tree, station, protocol, version, packets, claims, now } = i;
  if (!tree.byName.has(station)) return null;
  const inside = subtree(tree, station);
  const onLine = protocol.steps.filter((s) => !s.trigger && !s.terminal);
  const placed = onLine.map((s) => [s, placement(tree, protocol.kind, s)] as const);
  // A hand-off is where the packet leaves this line: last, whatever its order.
  const ordered = [...placed.filter(([, p]) => p.handoff === null), ...placed.filter(([, p]) => p.handoff !== null)];
  const slugs = ordered.map(([s]) => s.slug);
  const claimOf = new Map((claims ?? []).flatMap((c) => (c.stepId === null ? [] : [[c.stepId, c] as const])));

  // Each packet to its stop, or to the tray with the reason.
  type Tray = { reason: string; step: string | null; version: number | null; packets: LinePacket[] };
  const trays = new Map<string, Tray>();
  const toTray = (reason: string, step: string | null, p: LinePacket) => {
    const key = `${reason}\u0000${step ?? ''}\u0000${p.version ?? ''}`;
    const t = trays.get(key) ?? { reason, step, version: p.version, packets: [] };
    t.packets.push(p);
    trays.set(key, t);
  };
  const at = new Map<string, LinePacket[]>(slugs.map((s) => [s, []]));
  const trayed = new Map<string, number>(slugs.map((s) => [s, 0]));
  for (const p of packets.packets) {
    const slug = currentSlug(slugs, p);
    const pl = slug === null ? undefined : ordered.find(([s]) => s.slug === slug)?.[1];
    if (slug === null || pl === undefined) { toTray('at no step of this line', null, p); continue; }
    const stale = p.version !== null && version !== null && p.version !== version && !pl.anyVersion;
    if (pl.station === null || stale) {
      toTray(pl.station === null ? (pl.handoff !== null ? `handed off to ${pl.handoff}` : (pl.note ?? 'its step names no station')) : `pinned to v${p.version}, and this line places that step by v${version}’s row`, slug, p);
      trayed.set(slug, (trayed.get(slug) ?? 0) + 1);
      continue;
    }
    at.get(slug)?.push(p);
  }

  const stops = ordered.map(([s, pl]): LineStop => {
    const node = pl.station === null ? undefined : tree.byName.get(pl.station);
    const here = at.get(s.slug) ?? [];
    const seats = node?.capacity ?? null;
    const notes: string[] = [];
    const admission: { p: LinePacket; at: string | null }[] = [];
    const queued: { p: LinePacket; at: string | null }[] = [];
    const held: Held[] = [];
    let marked = false;
    for (const p of here) {
      const step = p.steps.find((x) => x.slug === s.slug);
      const claim = step?.id == null ? undefined : claimOf.get(step.id);
      const m = p.seat;
      if (m.state !== 'none') marked = true;
      if (m.state === 'queued') { queued.push({ p, at: m.since }); continue; }
      if (m.state === 'seated') {
        const d = since(now, m.since);
        if (m.station !== null && m.station !== pl.station) notes.push(`${p.title} is seated at ${m.station}`);
        held.push({ cell: { key: `${p.id}`, label: 'seated', packet: tagOf(p), holder: null, mark: null, dwell: d === null ? null : ago(d), free: false }, dwellMs: d });
        continue;
      }
      if (claim !== undefined || step?.status === 'active') {
        const d = claim === undefined ? null : since(now, claim.since);
        held.push({
          cell: {
            key: `${p.id}`,
            label: claim?.holder ?? 'no claim recorded',
            packet: tagOf(p),
            holder: claim === undefined ? 'no claim recorded' : (claim.holder ?? 'holder not recorded'),
            mark: claim?.holderKind == null ? null : HOLDER_MARK[claim.holderKind],
            dwell: d === null ? null : ago(d),
            free: false,
          },
          dwellMs: d,
        });
        continue;
      }
      admission.push({ p, at: arrivedAt(p) });
    }
    // Longest-held first; a seat with no claim, so no dwell, last.
    const sorted = [...held].sort((a, b) => (b.dwellMs ?? -1) - (a.dwellMs ?? -1));
    const freeCount = seats === null || pl.station === null ? 0 : Math.max(0, seats - held.length);
    const free: Cell[] = Array.from({ length: freeCount }, (_, k) => ({
      key: `free-${k}`, label: 'a seat', packet: null, holder: null, mark: null, dwell: null, free: true,
    }));
    const cells = [...sorted.map((h) => h.cell), ...free];

    if (pl.station !== null) {
      if (claims === null) notes.push('the claims are unread, so a claimed ready step is not told apart from a waiting one');
      if (seats !== null && held.length > seats) notes.push(`${held.length} seats held, more than the ${seats} the row declares`);
      if (seats === null) notes.push('the row declares no seats');
      if (marked) notes.push('a seat here is read from packet metadata (gate_bay, gate_reservation): the step stays ready and unclaimed, and no claim names the seat, so its dwell is counted from when the packet was queued');
      if (admission.length > 0) notes.push('an admission age runs from the step before it completing: steps carry no ready_at');
      if (!inside.has(pl.station)) notes.push(`at ${pl.station}, outside ${station}`);
    }
    const queue = marked ? waiting(now, queued) : null;
    const inCol = waiting(now, admission);
    return {
      slug: s.slug,
      station: pl.station,
      placedBy: pl.by,
      handoff: pl.handoff,
      note: pl.note,
      machines: node?.machines ?? [],
      seats,
      in: inCol,
      queue,
      cells: cells.slice(0, SEATS_MAX),
      more: Math.max(0, cells.length - SEATS_MAX),
      tray: trayed.get(s.slug) ?? 0,
      idle: seats !== null && seats > 0 && held.length === 0 && inCol.count + (queue?.count ?? 0) > 0,
      notes,
    };
  });

  const tray = [...trays.values()]
    .map((t): TrayRow => ({ reason: t.reason, step: t.step, version: t.version, count: t.packets.length, sample: t.packets.slice(0, TAGS_MAX).map(tagOf) }))
    .sort((a, b) => b.count - a.count);
  return {
    kind: protocol.kind,
    label: protocol.label,
    version,
    stops,
    tray,
    read: { shown: packets.packets.length, total: packets.total, unread: Math.max(0, packets.total - packets.packets.length) },
  };
}

// ---------------------------------------------------------------------
// The box: where every part and every path inside it sits.
// ---------------------------------------------------------------------

/** One box's interior, in its own pixels (E2's measure). */
export const BOX = { h: 236, mid: 124, inW: 50, cellW: 104, cellH: 16, noStation: 132 } as const;

export type PathPart = 'admit' | 'fan-in' | 'fan-out' | 'exit';
export type Path = Readonly<{ d: string; idle: boolean; part: PathPart }>;
export type Bundle = Readonly<{ d: string; count: number; x: number; y: number }>;
export type Geometry = Readonly<{
  width: number;
  cells: ReadonlyArray<Readonly<{ cell: Cell; x: number; y: number }>>;
  queueNode: Readonly<{ x: number; y: number }> | null;
  receipt: Readonly<{ x: number; y: number }>;
  paths: ReadonlyArray<Path>;
  bundles: ReadonlyArray<Bundle>;
}>;

/** Admission, the machine's queue when it has one, each cell, the
 *  receipt, out: every path, idle (dashed) where it leads to a free cell. */
export function boxGeometry(stop: LineStop): Geometry {
  const { mid, inW, cellW, cellH } = BOX;
  const queueX = stop.queue === null ? null : inW + 30;
  const cellX = queueX === null ? inW + 66 : queueX + 50;
  const recX = cellX + cellW + 24;
  const width = recX + 22;
  const n = stop.cells.length;
  const step = n <= 3 ? 48 : n === 4 ? 40 : 20;
  const ys = stop.cells.map((_, k) => mid + (k - (n - 1) / 2) * step);
  const bundled = n > BUNDLE_ABOVE;
  const start = queueX === null ? inW + 4 : queueX + 12;
  const fanX = cellX - 18;
  const outX = cellX + cellW;
  const paths: Path[] = [];
  const bundles: Bundle[] = [];
  const allIdle = stop.cells.every((c) => c.free);
  if (queueX !== null) paths.push({ d: `M${inW + 4} ${mid} H${queueX}`, idle: false, part: 'admit' });
  if (n === 0) paths.push({ d: `M${start} ${mid} H${recX - 5}`, idle: true, part: 'fan-in' });
  if (bundled) {
    bundles.push({ d: `M${start} ${mid} H${fanX}`, count: n, x: (start + fanX) / 2, y: mid });
    bundles.push({ d: `M${outX + 18} ${mid} H${recX - 5}`, count: n, x: outX + 18, y: mid });
  } else if (n > 0) {
    paths.push({ d: `M${start} ${mid} H${fanX}`, idle: allIdle, part: 'admit' });
  }
  stop.cells.forEach((c, k) => {
    const y = ys[k]!;
    paths.push({ d: `M${fanX} ${mid} C${fanX + 10} ${mid} ${fanX + 8} ${y} ${cellX} ${y}`, idle: c.free, part: 'fan-in' });
    const end = bundled ? outX + 18 : recX - 5;
    paths.push({ d: `M${outX} ${y} C${outX + 10} ${y} ${end - 8} ${mid} ${end} ${mid}`, idle: c.free, part: 'fan-out' });
  });
  paths.push({ d: `M${recX + 5} ${mid} H${width}`, idle: allIdle, part: 'exit' });
  return {
    width,
    cells: stop.cells.map((cell, k) => ({ cell, x: cellX, y: ys[k]! - cellH / 2 })),
    queueNode: queueX === null ? null : { x: queueX, y: mid },
    receipt: { x: recX, y: mid },
    paths,
    bundles,
  };
}
