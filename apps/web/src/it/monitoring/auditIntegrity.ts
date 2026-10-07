const KIND = 'maintenance-audit-integrity';
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
type ObjectValue = Readonly<Record<string, unknown>>;
const object = (value: unknown): value is ObjectValue =>
  typeof value === 'object' && value !== null && !Array.isArray(value);
const count = (value: unknown): value is number =>
  typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;

// Native timestamps retain nanoseconds. Comparing Date's milliseconds alone
// would choose an older packet arbitrarily when admissions share a millisecond.
export function nativeTime(value: unknown): bigint | null {
  if (typeof value !== 'string') return null;
  const match = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})$/.exec(value);
  if (!match) return null;
  const parts = match[1]!.split(/[-T:]/).map(Number);
  const [year, month, day, hour, minute, second] = parts;
  if (year === undefined || month === undefined || day === undefined
      || hour === undefined || minute === undefined || second === undefined) return null;
  const calendar = new Date(0);
  calendar.setUTCFullYear(year, month - 1, day);
  calendar.setUTCHours(hour, minute, second, 0);
  if (calendar.getUTCFullYear() !== year || calendar.getUTCMonth() !== month - 1
      || calendar.getUTCDate() !== day || calendar.getUTCHours() !== hour
      || calendar.getUTCMinutes() !== minute || calendar.getUTCSeconds() !== second) return null;
  const seconds = Date.parse(`${match[1]}${match[3]}`);
  if (!Number.isFinite(seconds)) return null;
  return BigInt(seconds) * 1_000_000n + BigInt((match[2] ?? '').padEnd(9, '0'));
}

export type Observation = Readonly<{
  checkedAt: string;
  chain: 'intact' | 'broken';
  totalRows: number;
  breaks: number;
  regressions: number;
  dangling: number;
  gaps: number;
  missing: number;
  gapReading: 'none' | 'sequence-burn' | 'possible-deletion';
  drift: Readonly<{ kind: 'covered' }> | Readonly<{ kind: 'undeclared'; kinds: readonly string[] }>
    | Readonly<{ kind: 'unavailable'; error: string }>;
}>;

export function observation(value: unknown): Observation | null {
  if (!object(value) || value.version !== 1 || nativeTime(value.checked_at) === null
      || typeof value.checked_at !== 'string' || !object(value.chain) || !object(value.drift)) return null;
  const c = value.chain;
  if ((c.state !== 'intact' && c.state !== 'broken')
      || !count(c.total_rows) || !count(c.chain_break_count) || !count(c.regression_count)
      || !count(c.dangling_ref_count) || !count(c.gap_count) || !count(c.missing_ids)
      || typeof c.gap_reading !== 'string'
      || !['none', 'sequence-burn', 'possible-deletion'].includes(c.gap_reading)) return null;
  if ((c.state === 'intact') !== (c.chain_break_count === 0)
      || (c.gap_count === 0) !== (c.gap_reading === 'none')
      || (c.gap_count === 0) !== (c.missing_ids === 0)
      || (c.gap_reading === 'sequence-burn' && c.state !== 'intact')
      || (c.gap_reading === 'possible-deletion' && c.state !== 'broken')) return null;
  const d = value.drift;
  let drift: Observation['drift'];
  if (d.state === 'unavailable' && typeof d.error === 'string' && d.error.trim()) {
    drift = { kind: 'unavailable', error: d.error };
  } else if ((d.state === 'covered' || d.state === 'undeclared') && Array.isArray(d.kinds)
      && d.kinds.every((k): k is string => typeof k === 'string' && k.trim().length > 0)
      && new Set(d.kinds).size === d.kinds.length
      && ((d.state === 'covered') === (d.kinds.length === 0))) {
    drift = d.state === 'covered' ? { kind: 'covered' } : { kind: 'undeclared', kinds: d.kinds };
  } else return null;
  return { checkedAt: value.checked_at, chain: c.state, totalRows: c.total_rows,
    breaks: c.chain_break_count, regressions: c.regression_count, dangling: c.dangling_ref_count,
    gaps: c.gap_count, missing: c.missing_ids, gapReading: c.gap_reading as Observation['gapReading'], drift };
}

type Row = Readonly<{ id: string; opened_at: string; status: 'draft' | 'open' | 'closed' | 'cancelled' }>;
function row(value: unknown): Row | null {
  if (!object(value) || typeof value.id !== 'string' || !UUID.test(value.id)
      || value.kind !== KIND || value.partition !== 'real' || value.simulated !== false
      || typeof value.opened_at !== 'string' || nativeTime(value.opened_at) === null
      || typeof value.status !== 'string'
      || !['draft', 'open', 'closed', 'cancelled'].includes(value.status)) return null;
  return { id: value.id, opened_at: value.opened_at, status: value.status as Row['status'] };
}

export type IntegrityState = Readonly<{ kind: 'loading' }> | Readonly<{ kind: 'empty' }>
  | Readonly<{ kind: 'unavailable'; message: string }>
  | Readonly<{ kind: 'ready'; id: string; admittedAt: string; finishedAt: string | null;
    status: Row['status']; result: string | null; exitStatus: string | null;
    observation: Observation | null; rawReport: unknown; output: string | null }>;

type Fetch = (url: string) => Promise<Response>;
export async function readIntegrity(fetcher: Fetch): Promise<IntegrityState> {
  try {
    const rows: Row[] = [];
    const ids = new Set<string>();
    let total: number | null = null;
    do {
      const params = new URLSearchParams({ kind: KIND, partition: 'real', limit: '100', offset: String(rows.length) });
      const response = await fetcher(`/api/jobs?${params}`);
      if (!response.ok) return { kind: 'unavailable', message: `HTTP ${response.status}` };
      const body: unknown = await response.json();
      if (!object(body) || !count(body.total) || !Array.isArray(body.data)
          || (total !== null && total !== body.total) || body.total > 10_000) {
        return { kind: 'unavailable', message: 'Incomplete or malformed integrity packet population' };
      }
      total = body.total;
      if (rows.length + body.data.length > total || (body.data.length === 0 && rows.length < total)) {
        return { kind: 'unavailable', message: 'Incomplete integrity packet population' };
      }
      for (const value of body.data) {
        const parsed = row(value);
        if (!parsed || ids.has(parsed.id)) return { kind: 'unavailable', message: 'Malformed or repeated integrity packet' };
        rows.push(parsed); ids.add(parsed.id);
      }
    } while (rows.length < (total ?? 0));
    if (rows.length === 0) return { kind: 'empty' };
    const ordered = [...rows].sort((a, b) => {
      const left = nativeTime(a.opened_at) ?? 0n;
      const right = nativeTime(b.opened_at) ?? 0n;
      return left > right ? -1 : left < right ? 1 : 0;
    });
    const latest = ordered[0];
    if (!latest || (ordered[1] && nativeTime(latest.opened_at) === nativeTime(ordered[1].opened_at))) {
      return { kind: 'unavailable', message: 'Newest integrity packet is ambiguous' };
    }
    const response = await fetcher(`/api/jobs/${latest.id}`);
    if (!response.ok) return { kind: 'unavailable', message: `HTTP ${response.status}` };
    const body: unknown = await response.json();
    const detail = row(body);
    if (!detail || detail.id !== latest.id || detail.opened_at !== latest.opened_at
        || !object(body) || !Array.isArray(body.steps)) return { kind: 'unavailable', message: 'Malformed integrity packet detail' };
    const runs = body.steps.filter((s: unknown) => object(s) && s.spec_slug === 'run');
    if (runs.length !== 1 || !object(runs[0]) || !object(runs[0].metadata)) {
      return { kind: 'unavailable', message: 'Integrity packet lacks its unique run record' };
    }
    const run = runs[0];
    const metadata = run.metadata as ObjectValue;
    const observed = observation(metadata.audit_integrity);
    const finish = typeof run.completed_at === 'string' && nativeTime(run.completed_at) !== null ? run.completed_at : null;
    const bounded = observed && nativeTime(observed.checkedAt)! >= nativeTime(detail.opened_at)!
      && (finish === null || nativeTime(observed.checkedAt)! <= nativeTime(finish)!);
    return { kind: 'ready', id: detail.id, admittedAt: detail.opened_at, status: detail.status,
      finishedAt: finish, observation: bounded ? observed : null,
      result: typeof metadata.result === 'string' ? metadata.result : null,
      exitStatus: typeof metadata.exit_status === 'string' ? metadata.exit_status : null,
      rawReport: metadata.audit_integrity ?? null,
      output: typeof metadata.output === 'string' ? metadata.output : null };
  } catch (error) {
    return { kind: 'unavailable', message: error instanceof Error && error.message ? error.message : 'Read failed without a diagnostic' };
  }
}

export function ageText(timestamp: string, now: number): string {
  const at = Date.parse(timestamp);
  if (!Number.isFinite(at) || at > now) return 'age unavailable';
  const minutes = Math.floor((now - at) / 60_000);
  return minutes < 1 ? 'less than a minute ago' : minutes < 60 ? `${minutes} min ago`
    : minutes < 1440 ? `${Math.floor(minutes / 60)} h ago` : `${Math.floor(minutes / 1440)} days ago`;
}
