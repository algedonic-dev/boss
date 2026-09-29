// Every kind's packet ledger — GET /api/jobs/kinds, read for the
// registry catalog (backlogs 112c0535 and 5eacf6db, page audit 9da74410).
//
// The catalog showed a kind's active `v{n}` and how many of its packets
// were in flight, and nothing else about them. So a procedure edit that
// never reached in-flight packets (374 of 540 open packets ran a version
// below their kind's active one, measured 2026-09-27) and a protocol
// nobody runs (16 of 64 active kinds had never had a packet) were drawn
// exactly like a kind whose packets were all current and busy. The jobs
// API counts, per kind, the open packets by version, the packets ever,
// and the newest terminal; this module turns that into what a row says.
//
// The read is its own: a failed ledger says so and draws no "never run"
// on any kind, because a kind the ledger was not read for is unknown,
// not empty.

import { fetchRemote, type Remote } from '../../data/remote';

export type NewestTerminal = Readonly<{
  id: string;
  title: string;
  outcome: string | null;
  closed_on: string | null;
}>;

export type KindLedgerRow = Readonly<{
  kind: string;
  packets: number;
  open: number;
  /** Open packets by the version each is pinned to; the key is the
   *  version as a JSON object key, so a string. */
  open_by_version: Readonly<Record<string, number>>;
  newest_terminal: NewestTerminal | null;
}>;

export type KindLedger = ReadonlyMap<string, KindLedgerRow>;

export type KindHistory =
  | { kind: 'never-run' }
  | {
      kind: 'ran';
      packets: number;
      inFlight: number;
      /** In-flight packets pinned to a version below the active one. */
      older: number;
      /** `[version, open]` for each version below the active one, oldest first. */
      olderVersions: ReadonlyArray<readonly [number, number]>;
      newest: NewestTerminal | null;
    };

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);
const isCount = (v: unknown): v is number => typeof v === 'number' && Number.isInteger(v) && v >= 0;

function parseRow(raw: unknown): KindLedgerRow {
  if (!isRecord(raw) || typeof raw.kind !== 'string' || !isCount(raw.packets) || !isCount(raw.open)) {
    throw new Error(`a ledger row without kind, packets and open: ${JSON.stringify(raw)}`);
  }
  const byVersion = raw.open_by_version;
  if (!isRecord(byVersion) || !Object.values(byVersion).every(isCount)) {
    throw new Error(`ledger row ${raw.kind}: open_by_version is not a version → count object`);
  }
  const t = raw.newest_terminal;
  const newest: NewestTerminal | null =
    isRecord(t) && typeof t.id === 'string'
      ? {
          id: t.id,
          title: typeof t.title === 'string' ? t.title : t.id,
          outcome: typeof t.outcome === 'string' ? t.outcome : null,
          closed_on: typeof t.closed_on === 'string' ? t.closed_on : null,
        }
      : null;
  return {
    kind: raw.kind,
    packets: raw.packets,
    open: raw.open,
    open_by_version: byVersion as Record<string, number>,
    newest_terminal: newest,
  };
}

/** The ledger by kind. Throws on a body that is not one — `[]` or an
 *  object without `kinds` is a wrong or missing endpoint, and read as an
 *  empty ledger it would draw every kind as never run. */
export function parseKindLedger(raw: unknown): KindLedger {
  if (!isRecord(raw) || !Array.isArray(raw.kinds)) {
    throw new Error('GET /api/jobs/kinds answered a body without a kinds list');
  }
  return new Map(raw.kinds.map(parseRow).map((r) => [r.kind, r] as const));
}

export function loadKindLedger(): Promise<Exclude<Remote<KindLedger>, { kind: 'loading' }>> {
  return fetchRemote('/api/jobs/kinds', parseKindLedger);
}

/** What the ledger says of one kind whose active version is `active`. */
export function historyOf(ledger: KindLedger, kind: string, active: number): KindHistory {
  const row = ledger.get(kind);
  if (!row) return { kind: 'never-run' };
  const olderVersions = Object.entries(row.open_by_version)
    .map(([v, n]) => [Number(v), n] as const)
    .filter(([v, n]) => v < active && n > 0)
    .sort(([a], [b]) => a - b);
  return {
    kind: 'ran',
    packets: row.packets,
    inFlight: row.open,
    older: olderVersions.reduce((sum, [, n]) => sum + n, 0),
    olderVersions,
    newest: row.newest_terminal,
  };
}
