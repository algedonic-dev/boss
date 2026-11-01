import { fetchRemote, type Remote } from '../../data/remote';

/** The background flow snapshot status, not a client-side count model. */
export type Flow = Readonly<{ window_hours: number } & (
  | { state: 'pending'; as_of: null; why: string }
  | { state: 'unread'; as_of: null; why: string }
  | { state: 'ready'; as_of: string; why: string | null }
  | { state: 'withheld'; as_of: null; why: string }
)>;

export function parseFlow(raw: unknown): Flow {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new Error('flow reading is not an object');
  }
  const row = raw as Record<string, unknown>;
  const hours = row.window_hours;
  if (typeof hours !== 'number' || !Number.isInteger(hours) || hours <= 0) {
    throw new Error('flow reading has no valid window');
  }
  if (row.state === 'ready' && typeof row.as_of === 'string'
    && /^\d{4}-\d{2}-\d{2}T/.test(row.as_of) && Number.isFinite(Date.parse(row.as_of))
    && (row.why === null || (typeof row.why === 'string' && row.why.trim() !== ''))) {
    return { state: 'ready', as_of: row.as_of, window_hours: hours, why: row.why };
  }
  if ((row.state === 'pending' || row.state === 'unread' || row.state === 'withheld') && row.as_of === null
    && typeof row.why === 'string' && row.why.trim() !== '') {
    return { state: row.state, as_of: null, window_hours: hours, why: row.why };
  }
  throw new Error('flow reading has an invalid state, instant or reason');
}

/** HTTP202 is successful transport carrying a pending domain reading. */
export function fetchFlow(): Promise<Remote<Flow>> {
  return fetchRemote('/api/yard/flow', parseFlow);
}
