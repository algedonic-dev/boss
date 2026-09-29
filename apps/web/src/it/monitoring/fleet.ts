// The Bottlenecks page's pure half (/it/operate/bottlenecks,
// FleetPage.svelte) — the picker's order, the address it writes, and
// the words a failed or stale read is painted with. Page audit
// ac519138 found the page's reads failing in silence and its first
// view landing on the alphabetically first kind; these are the
// functions its fixes stand on, tested here rather than only through
// the browser.
import { parseQueueAge } from '../marshalling/marshalling';

/// The polled reads, by the name the stale line calls each one.
export type PolledRead = 'fleet' | 'items' | 'flow';

/// In-flight steps per Workflow kind, counted off the queue-age lens
/// (`GET /api/jobs/queue-age`): one row per ready or active step of an
/// open packet, the same ready + active the fleet aggregate counts per
/// step. A jobs-API read — so, unlike the views aggregates, it is
/// readable through the pod door and a recorded probe (backlog
/// f949d19a). Throws on a body that is not the envelope.
export function inFlightByKind(raw: unknown): ReadonlyMap<string, number> {
  return parseQueueAge(raw)
    .waits.filter((w) => w.jobKind !== '')
    .reduce((acc, w) => acc.set(w.jobKind, (acc.get(w.jobKind) ?? 0) + 1), new Map<string, number>());
}

/// The picker's order: most in-flight steps first, then by name, so
/// the idle kinds sink to the bottom but stay listed — an idle kind is
/// an answer too. Without counts (the lens did not answer) the order
/// is alphabetical, which is all the page knew before f949d19a.
export function pickerOrder(
  kinds: ReadonlyArray<string>,
  counts: ReadonlyMap<string, number> | null,
): ReadonlyArray<string> {
  const n = (k: string): number => counts?.get(k) ?? 0;
  return [...kinds].sort((a, b) => n(b) - n(a) || a.localeCompare(b));
}

/// One option's label: the kind, and its in-flight count when read.
export function optionLabel(kind: string, counts: ReadonlyMap<string, number> | null): string {
  return counts ? `${kind} — ${counts.get(kind) ?? 0} in flight` : kind;
}

/// The kind the page opens on: the one the address asks for, if the
/// registry has it, else the head of the picker — the deepest kind
/// once the counts are read (it was `agent-run`, 10 of 457 in-flight
/// steps, while backlog-item held 363, 2026-09-27).
export function openingKind(ordered: ReadonlyArray<string>, asked: string | null): string | null {
  return asked && ordered.includes(asked) ? asked : (ordered[0] ?? null);
}

/// The query string with `?kind=` set to the chosen kind and every
/// other parameter kept — written with replaceState on each switch, so
/// a chosen kind survives a reload and can be linked to (c7c5c1de).
export function searchWithKind(search: string, kind: string): string {
  const p = new URLSearchParams(search);
  p.set('kind', kind);
  return `?${p.toString()}`;
}

/// A non-OK answer as the page says it: the read, its HTTP status, and
/// the server's own words when it sent any. The views service answers
/// a 503 with "stage durations need a postgres-backed views service";
/// the page threw that sentence away until ffc3444f. A JSON body's
/// `error` string is taken, otherwise the text as sent.
export function failureText(what: string, status: number, body: string): string {
  const said = serverWords(body);
  return said ? `${what}: HTTP ${status} — ${said}` : `${what}: HTTP ${status}`;
}

function serverWords(body: string): string {
  const text = body.trim();
  if (text === '') return '';
  try {
    const parsed: unknown = JSON.parse(text);
    const error = (parsed as { error?: unknown } | null)?.error;
    if (typeof error === 'string') return error;
  } catch {
    // Not JSON: the text is the message.
  }
  return text;
}

/// A stage duration, as the flow columns read it. It lived in
/// jobs/decorateDag.ts beside `decorateDagNodes`, which had no caller
/// and a header claiming it was "ONE definition shared by the
/// Bottlenecks page and the IT flow network" — false on main; this page
/// was its one reader, so it moved here and the file went (08650f01).
export function fmtDur(seconds: number): string {
  if (seconds < 90) return `${Math.round(seconds)}s`;
  const m = seconds / 60;
  if (m < 120) return `${Math.round(m)}m`;
  return `${(m / 60).toFixed(1)}h`;
}

/// An age, as the stale line reads it.
export function ageText(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 90) return `${s} s`;
  const m = Math.round(s / 60);
  if (m < 90) return `${m} min`;
  return `${(m / 60).toFixed(1)} h`;
}

/// The line a failed poll paints over numbers it could not refresh
/// (0fb86007): the values stay — the last good answer is still the
/// best one there is — but the page says which read failed, how, and
/// how old what it shows is. `null` when every read's last poll
/// answered, so the line clears itself on the next success.
export function staleLine(
  failures: Readonly<Partial<Record<PolledRead, string>>>,
  lastGood: Readonly<Partial<Record<PolledRead, number>>>,
  now: number,
): string | null {
  const parts = (['fleet', 'items', 'flow'] as const).flatMap((read) => {
    const error = failures[read];
    const at = lastGood[read];
    if (error === undefined || at === undefined) return [];
    return [`the ${read} read failed (${error}), so its numbers are its last good answer, ${ageText(now - at)} old`];
  });
  return parts.length === 0 ? null : `Not refreshed — ${parts.join('; ')}. This clears on the next good read.`;
}
