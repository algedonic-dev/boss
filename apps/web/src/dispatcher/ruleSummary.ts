// The rule editor's read-only summary (backlog 0034d5ef, page-audit
// bd5018e1 gap 3). /it/registry/rules/:ruleName is where Retire is
// decided, and it showed none of what the rules list shows about a rule
// — why, who declared it, when it last fired, whether it is failing —
// and nothing anywhere said when a schedule rule next fires or which
// kinds its handlers emit. A retire decided without those is a guess,
// and every one of them is already served.
//
// Why/source and last-fired/failing reuse the list's own helpers
// (ruleProvenance, ruleActivity) over the list's own reads. What lives
// here is the rest: the next due of a schedule rule, read off
// `GET /api/dispatcher/schedule` (boss-dispatcher http.rs `schedule`,
// design ea906603), and the kinds a rule's handlers emit, read off the
// `handler_emits` roster the rules read already carries. Pure, except
// the one fetch.

import { fetchRemote, type Remote } from '../data/remote';
import { ageText } from './ruleFirings';
import type { RuleStatus } from './ruleAuthoring';
import type { AuthoredRegistry, DispatcherRule } from './types';

/** One scheduled rule's next firing, as the dispatcher answers it. */
export type ScheduleRow = Readonly<{
  name: string;
  /** `null` when the runner cannot place it; `next_due_why` says why. */
  next_due: string | null;
  next_due_why: string | null;
}>;

/** The schedule read: rows, or `null` beside the reason it was withheld
 *  or could not load — never an empty list for either. */
export type ScheduleRead = Readonly<{
  now: string;
  schedule: ReadonlyArray<ScheduleRow> | null;
  schedule_error: string | null;
}>;

const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);

/** The payload, or a throw: a body without the `schedule` key is a wrong
 *  server (or the crawl's `[]` fallthrough), not a schedule with no rows. */
export function parseDispatcherSchedule(raw: unknown): ScheduleRead {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw) || !('schedule' in raw)) {
    throw new Error('dispatcher schedule: expected an object with a schedule');
  }
  const o = raw as Record<string, unknown>;
  const schedule = Array.isArray(o.schedule)
    ? o.schedule.map((row) => {
        const r = (row ?? {}) as Record<string, unknown>;
        return { name: String(r.name ?? ''), next_due: str(r.next_due), next_due_why: str(r.next_due_why) };
      })
    : null;
  return {
    now: String(o.now ?? ''),
    schedule,
    schedule_error: schedule === null ? (str(o.schedule_error) ?? 'the schedule was not served') : null,
  };
}

export async function fetchDispatcherSchedule(): Promise<Remote<ScheduleRead>> {
  return fetchRemote('/api/dispatcher/schedule', parseDispatcherSchedule);
}

/** When the dispatcher next fires `name`: a distance, with the instant
 *  (or the reason there is none) as its hover. Unread is "unknown",
 *  never "not scheduled". */
export function nextDue(name: string, read: ScheduleRead): Readonly<{ text: string; why: string }> {
  if (read.schedule === null) return { text: 'unknown', why: read.schedule_error ?? '' };
  const row = read.schedule.find((r) => r.name === name);
  if (!row) {
    return {
      text: 'not on the dispatcher’s schedule',
      why: 'the schedule lists every active schedule rule, and this one is not among them',
    };
  }
  if (row.next_due === null) return { text: 'unknown', why: row.next_due_why ?? '' };
  // ageText measures `now - at`; the next firing is ahead, so swap them.
  const until = ageText(read.now, row.next_due);
  return {
    text: until === '' ? row.next_due : `in ${until}`,
    why: row.next_due_why ?? `next due ${row.next_due}`,
  };
}

/** The rule's standing justification, or what the read can say in its
 *  place. `rule` is the ENFORCED row (`null` when no version is active);
 *  `registry` is where the whys were read from. A `why: null` is told
 *  apart from an unread registry, as the list does (backlog f9e34a2c). */
export function ruleWhy(
  rule: Pick<DispatcherRule, 'why' | 'source' | 'authored'> | null,
  registry: AuthoredRegistry | null,
): string {
  if (rule === null) return 'not enforced — no version of this rule is active';
  const why = rule.why?.trim() ?? '';
  if (why.length > 0) return why;
  if (rule.source?.startsWith('tenant:')) {
    return 'kept in the tenant’s seeds/rules.toml, which this dispatcher does not read';
  }
  if (registry === null || registry.error !== null) return 'unknown — the authored registry could not be read';
  return 'no file records a why for this rule';
}

/** Who performed each act this row's status implies (backlog 9550ac95):
 *  every row was drafted; an active or retired one was published (only
 *  an active row is ever retired); a retired one was retired. `null` is
 *  a door that did not record the act — the boot seed, a migration, a
 *  row older than the column — and prints "(unrecorded)". A draft names
 *  no publisher: it was never published, which is not unrecorded. */
export function authorship(
  v: Readonly<{ status: RuleStatus; created_by: string | null; published_by: string | null; retired_by: string | null }>,
): string {
  const who = (id: string | null): string => id ?? '(unrecorded)';
  const acts = [`drafted by ${who(v.created_by)}`];
  if (v.status !== 'draft') acts.push(`published by ${who(v.published_by)}`);
  if (v.status === 'retired') acts.push(`retired by ${who(v.retired_by)}`);
  return acts.join(' · ');
}

/** The kinds a rule's handlers cause to be emitted, each once, in `do`
 *  order — or that they emit nothing, or which handler the roster does
 *  not carry (a name that dead-letters at dispatch as UnknownHandler). */
export function emittedKinds(
  rule: Pick<DispatcherRule, 'do'>,
  handlerEmits: Readonly<Record<string, ReadonlyArray<string>>>,
): string {
  const handlers = [...new Set(rule.do.map((d) => d.handler))];
  const missing = handlers.filter((h) => !(h in handlerEmits));
  if (missing.length > 0) {
    return `unknown — ${missing.join(', ')} ${missing.length === 1 ? 'is' : 'are'} not in this dispatcher’s handler roster`;
  }
  const kinds = [...new Set(handlers.flatMap((h) => handlerEmits[h] ?? []))];
  if (kinds.length > 0) return kinds.join(', ');
  return handlers.length === 1 ? 'nothing — its handler is a sink' : 'nothing — its handlers are sinks';
}
