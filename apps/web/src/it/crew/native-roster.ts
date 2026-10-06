// Explicit snapshots of copied native receipts, separate from workflow work.
// Five minutes is a display expiry, not a heartbeat or collection promise.
import { fetchEvery, wholeOrThrow } from '../../data/paginated';
export type NativeRow = Readonly<{ path: string; status: 'running' | 'finished' | 'unknown'; current_task: null; native_id: null; observed_model: null }>;
export type Capture = Readonly<{ captureId: string; namespace: string; source: string; from: string; to: string; receivedAt: string; observer: string; complete: boolean; rows: ReadonlyArray<NativeRow> }>;
export type Observation = Capture & Readonly<{ state: 'fresh' | 'stale' | 'unknown'; lastComplete: Capture | null }>;
export const SNAPSHOT_FRESH_MS = 5 * 60_000;

function object(v: unknown): Record<string, unknown> {
  if (!v || typeof v !== 'object' || Array.isArray(v)) throw new Error('Roster evidence is not an object');
  return v as Record<string, unknown>;
}
function text(v: unknown): string {
  if (typeof v !== 'string' || v.trim() === '') throw new Error('Roster evidence lacks a typed identity');
  return v;
}
// Keep the full fractional precision of native RFC3339 receipts. Date.parse alone
// truncates submilliseconds and can reorder successful and failed observations.
function parts(t: string): Readonly<{ whole: number; fraction: string }> {
  const match = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$/.exec(t);
  if (!match) throw new Error('Roster evidence lacks an RFC3339 time');
  const whole = Date.parse(`${match[1]}${match[3]}`);
  if (!Number.isFinite(whole)) throw new Error('Roster evidence lacks a valid time');
  return { whole, fraction: match[2] ?? '' };
}
function instant(v: unknown): string {
  const t = text(v);
  parts(t);
  return t;
}
function compareTime(a: string, b: string, shiftB = 0): number {
  const left = parts(a), right = parts(b);
  const whole = left.whole - (right.whole + shiftB);
  if (whole !== 0) return whole;
  const digits = Math.max(left.fraction.length, right.fraction.length);
  const x = left.fraction.padEnd(digits, '0'), y = right.fraction.padEnd(digits, '0');
  return x === y ? 0 : x < y ? -1 : 1;
}

export function parseCapture(raw: unknown): Capture {
  const job = object(raw);
  if (job.kind !== 'runtime-roster-capture' || job.partition !== 'real' || !Array.isArray(job.steps)) throw new Error('Roster packet identity unavailable');
  const steps = job.steps.map(object).filter((s) => s.spec_slug === 'capture');
  const capture = steps[0];
  if (steps.length !== 1 || capture === undefined || capture.status !== 'completed') throw new Error('Roster capture is not immutable completed evidence');
  const observer = text(capture.completed_by);
  const s = object(object(capture.metadata).snapshot);
  if (s.capture_id !== job.id || s.source !== 'codex.collaboration.list_agents' || typeof s.complete !== 'boolean' || !Array.isArray(s.rows)) throw new Error('Roster snapshot contract unavailable');
  const from = instant(s.collected_from), to = instant(s.collected_to);
  if (compareTime(from, to) > 0 || (s.complete ? s.result !== 'captured' : s.result !== 'failed')) throw new Error('Roster interval or result contradicts evidence');
  const paths = new Set<string>();
  const rows = s.rows.map((raw): NativeRow => {
    const r = object(raw), path = text(r.path);
    if (!/^\/root(?:\/[^/]+)*$/.test(path) || paths.has(path) || !['running', 'finished', 'unknown'].includes(text(r.status)) || r.current_task !== null || r.native_id !== null || r.observed_model !== null) throw new Error('Roster row has ambiguous identity or unsupported inferred facts');
    paths.add(path);
    return { path, status: r.status as NativeRow['status'], current_task: null, native_id: null, observed_model: null };
  });
  if (!s.complete && rows.length !== 0) throw new Error('A failed capture cannot publish complete rows');
  return { captureId: text(s.capture_id), namespace: text(s.namespace), source: text(s.source), from, to, receivedAt: instant(s.received_at), observer, complete: s.complete, rows };
}

export function projectRoster(captures: ReadonlyArray<Capture>, now: string): ReadonlyArray<Observation> {
  const at = instant(now);
  return [...new Set(captures.map((c) => c.namespace))].sort().map((namespace) => {
    const mine = captures.filter((c) => c.namespace === namespace).sort((a, b) => compareTime(b.to, a.to) || b.captureId.localeCompare(a.captureId));
    const latest = mine[0];
    if (latest === undefined) throw new Error('Roster namespace has no observation');
    return { ...latest, state: !latest.complete || compareTime(at, latest.to) < 0 ? 'unknown' : compareTime(at, latest.to, SNAPSHOT_FRESH_MS) >= 0 ? 'stale' : 'fresh', lastComplete: mine.find((c) => c.complete) ?? null };
  });
}

export async function readRoster(read: (url: string) => Promise<unknown>): Promise<ReadonlyArray<Capture>> {
  const ids = new Set<string>();
  let offset = 0, total: number | null = null;
  wholeOrThrow(await fetchEvery('/api/jobs?kind=runtime-roster-capture', { read: async (url) => {
    const page = object(await read(url));
    if (!Array.isArray(page.data) || !Number.isSafeInteger(page.total) || (page.total as number) < 0 || (total !== null && total !== page.total)) throw new Error('Roster history is incomplete or moved during pagination');
    total = page.total as number;
    for (const raw of page.data) {
      const row = object(raw), id = text(row.id);
      if (row.kind !== 'runtime-roster-capture' || ids.has(id)) throw new Error('Roster history identity or filter unverified');
      ids.add(id);
    }
    offset += page.data.length;
    if (offset > total) throw new Error('Roster history exceeded its count');
    return page;
  } }));
  return Promise.all([...ids].map(async (id) => {
    const detail = object(await read(`/api/jobs/${encodeURIComponent(id)}`));
    if (detail.id !== id) throw new Error('Roster detail belongs to another packet');
    return parseCapture(detail);
  }));
}
