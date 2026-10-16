import { nonBlankProofString } from './proof-strings';

/** The server's qualified wait. Readers preserve its evidence and judgment;
 *  they do not infer a new owner, deadline or verdict in the browser. */
export type RecordedWait = Readonly<{
  packet: string;
  owner: string;
  condition_owner: string;
  title: string;
  condition: string;
  next_check_at: string;
  evidence: Readonly<Record<string, unknown>>;
}>;

function object(raw: unknown): Record<string, unknown> {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new Error('qualified wait: expected an object');
  }
  return raw as Record<string, unknown>;
}

export function parseRecordedWait(raw: unknown): RecordedWait {
  const row = object(raw);
  const text = (key: string): string => {
    const value = row[key];
    if (typeof value !== 'string' || nonBlankProofString(value) === null) {
      throw new Error(`qualified wait: missing ${key}`);
    }
    return value;
  };
  const evidence = object(row.evidence);
  if (typeof evidence.kind !== 'string' || nonBlankProofString(evidence.kind) === null) {
    throw new Error('qualified wait: missing evidence kind');
  }
  return {
    packet: text('packet'), owner: text('owner'), condition_owner: text('condition_owner'),
    title: text('title'), condition: text('condition'), next_check_at: text('next_check_at'),
    evidence: { ...evidence },
  };
}

export function parseRecordedWaits(raw: unknown): ReadonlyArray<RecordedWait> {
  if (!Array.isArray(raw)) throw new Error('qualified waits: expected a list');
  return raw.map((value: unknown) => parseRecordedWait(value));
}

export function recordedWaitText(wait: RecordedWait): string {
  return `${wait.title} — owner ${wait.owner}; waiting on ${wait.condition_owner}: ${wait.condition}; next check ${wait.next_check_at}; ${String(wait.evidence.kind)} evidence`;
}
