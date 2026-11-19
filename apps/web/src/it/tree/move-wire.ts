// A RECORDED MOVE, AS GET /api/yard/moves/stream SENDS IT (car M1,
// crates/core/boss-jobs/src/moves.rs `Recorded`): one packet whose place
// changed, stamped with the event that changed it. The station page's
// line (StationLine.svelte, design 08d1530e car B) reads the stream
// through these two parsers. They were the wire half of live-motion.ts, the transit map's
// motion layer, which retired with the map (R3, backlog 23a4b4d7); the
// drawing half went with it.

/** One recorded move, as the stream's `move` frame carries it: the
 *  server's `Recorded` with its `Move` flattened in. `from` / `to` are
 *  null off the map — a packet filed onto it, or leaving by an off-ramp.
 *  `declared` is whether a derived route supports it (null: unjudged). */
export type Move = Readonly<{
  seq: number;
  at: string;
  packet: string;
  kind: string;
  label: string;
  from: string | null;
  to: string | null;
  declared: boolean | null;
  cause_event_id: string;
  cause_seq: number;
  cause_kind: string;
  handoff_from: string | null;
  lineage: string | null;
  aboard: ReadonlyArray<string>;
  /** On an exit, the terminal the packet left by — the branch of its
   *  station's exit fan it runs down (backlog e23005c0, 3c8b5653); null
   *  or absent where the server could not read it back. */
  terminal?: string | null;
}>;

const str = (v: unknown): v is string => typeof v === 'string';
const strOrNull = (v: unknown): v is string | null => v === null || typeof v === 'string';
const num = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

function json(data: string): Record<string, unknown> | null {
  try {
    const v: unknown = JSON.parse(data);
    return v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

/** A `move` frame's data, or null for anything that is not a whole move —
 *  half a move is never drawn. */
export function parseMove(data: string): Move | null {
  const o = json(data);
  if (o === null) return null;
  const { seq, at, packet, kind, label, from, to, declared, cause_event_id, cause_seq, cause_kind, handoff_from, lineage, aboard, terminal } = o;
  const ok =
    num(seq) && str(at) && str(packet) && str(kind) && str(label) && strOrNull(from) && strOrNull(to) &&
    (declared === null || declared === undefined || typeof declared === 'boolean') &&
    str(cause_event_id) && num(cause_seq) && str(cause_kind) &&
    (handoff_from === undefined || strOrNull(handoff_from)) && (lineage === undefined || strOrNull(lineage)) &&
    (aboard === undefined || (Array.isArray(aboard) && aboard.every(str))) &&
    (terminal === undefined || strOrNull(terminal));
  if (!ok) return null;
  return {
    seq, at, packet, kind, label, from, to,
    declared: declared ?? null,
    cause_event_id, cause_seq, cause_kind,
    handoff_from: handoff_from ?? null,
    lineage: lineage ?? null,
    aboard: aboard ?? [],
    terminal: terminal ?? null,
  };
}

/** A `resync` frame: take the current placement as the baseline. */
export function parseResync(data: string): Readonly<{ seq: number; reason: string }> | null {
  const o = json(data);
  return o !== null && num(o.seq) && str(o.reason) ? { seq: o.seq, reason: o.reason } : null;
}
