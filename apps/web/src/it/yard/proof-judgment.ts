import { proofString as string, nonBlankProofString as nonBlank, rustTrim, isWorldOwner } from './proof-strings';
import { proofInstant, proofNow, compareInstants, elapsedSeconds, checkedDeadline, clockText, type ProofInstant, type ProofNow } from './proof-clock';
import { serdeI64, serdeU64, type ProofNumbers } from './proof-numbers';

/** The inspection reader's backend contract. proof-contract.json is exercised
 *  by the actual Rust classifiers and this reader; neither copy may drift. */
export type ProofFacts = Readonly<{
  metadata: Readonly<Record<string, unknown>>;
  ownerId: string | null;
  openedAt: string | null;
  numbers?: ProofNumbers;
  steps: readonly Readonly<{ spec_slug?: string | null; status: string; assignee_id?: string | null }>[];
}>;

export type ProofJudgment = Readonly<{
  kind: 'unread' | 'failed' | 'success' | 'not-yet' | 'pending' | 'event' | 'unproven'
    | 'satisfied-condition' | 'unowned' | 'unobserved' | 'overdue' | 'unqualified-wait' | 'stale';
  execution: 'unread' | 'failed' | 'success' | 'not-yet' | 'none';
  action: boolean;
  fault: 'Unread' | 'Failing' | 'Unproven' | 'SatisfiedCondition' | 'Unowned' | 'Unobserved' | 'Overdue' | null;
  wait: 'qualified' | 'none' | 'unqualified';
  reason: string;
  condition?: string;
  seenAt?: string;
  exitText?: string;
  streak?: Readonly<{ hours: string; runs: string | null }>;
}>;

const object = (v: unknown): Readonly<Record<string, unknown>> => typeof v === 'object' && v !== null && !Array.isArray(v)
  ? v as Readonly<Record<string, unknown>> : {};

export function waitObservation(a: Readonly<Record<string, unknown>>, numbers?: ProofNumbers): 'seen' | 'unseen' | 'unread' {
  if (('waits_on_missing_tools' in a && (!Array.isArray(a.waits_on_missing_tools) || a.waits_on_missing_tools.length > 0))
    || ['waits_on_error', 'waits_on_refusal'].some(k => k in a && a[k] !== null)) return 'unread';
  const exit = serdeI64(a.waits_on_exit, numbers?.['/proof_attempt/waits_on_exit']);
  return exit === 0n ? 'seen'
    : exit === 1n && a.waits_on_observation === 'unseen' ? 'unseen' : 'unread';
}

/** Mirror shed_place -> starved/owned_wait -> proof_fault, then qualify an
 *  owned wait using the native action owner, clock, bindings and coverage. */
export function judgeProof(facts: ProofFacts, nowValue: ProofNow): ProofJudgment {
  const now = proofNow(nowValue);
  const md = facts.metadata;
  const a = object(md.proof_attempt);
  const w = object(md.waits_on);
  const on = nonBlank(w.on);
  const seen = nonBlank(w.seen);
  const owner = nonBlank(w.owner);
  const observation = waitObservation(a, facts.numbers);
  const reading = serdeI64(a.exit, facts.numbers?.['/proof_attempt/exit']);
  const exit = typeof reading === 'bigint' ? reading : null;
  const proof = string(md.proof_probe) ?? '';
  const event = string(md.proof_event) ?? '';
  const execution: ProofJudgment['execution'] = !('proof_attempt' in md) ? 'none'
    : exit === null ? 'unread' : exit === 75n ? 'not-yet' : exit === 0n ? 'success' : 'failed';
  const opened = proofInstant(facts.openedAt) ?? proofInstant(md.opened_at);
  const age = now !== null && opened !== null ? elapsedSeconds(now, opened) : null;
  const max = serdeI64(w.max_wait_hours, facts.numbers?.['/waits_on/max_wait_hours']);
  const bound = max !== null && max !== 'unread' && max > 0n ? max : null;
  const at = proofInstant(a.at);
  const matching = typeof a.probe !== 'string' || typeof md.proof_probe !== 'string' || a.probe === md.proof_probe;
  const streak = (exit === 75n || exit === null && a.environment === true && a.not_yet === true) && matching && at !== null;
  const since = proofInstant(a.not_yet_since) ?? at;
  const runs = serdeU64(a.not_yet_runs, facts.numbers?.['/proof_attempt/not_yet_runs']);
  const streakEvidence = streak ? { hours: (elapsedSeconds(at!, since!) / 3600n).toString(),
    runs: runs === 'unread' ? null : (runs !== null && runs > 0n ? runs : 1n).toString() } : undefined;
  const starved = streak && on === null && since !== null && elapsedSeconds(at!, since) / 3600n >= 72n;
  const sighting = streak && on !== null && typeof a.waits_on_seen_at === 'string';
  const observerUnread = on !== null && seen !== null && observation === 'unread';
  const owned = on !== null && owner !== null && (!isWorldOwner(owner) || seen !== null)
    && !observerUnread && a.waits_on_exit !== null
    && !(seen !== null && 'proof_attempt' in md && observation !== 'unseen') && !sighting && !starved;
  const result = (kind: ProofJudgment['kind'], fault: ProofJudgment['fault'], reason: string,
    action = fault !== null, wait: ProofJudgment['wait'] = 'none'): ProofJudgment => ({ kind, fault, reason, action, wait, execution,
      ...(exit === null ? {} : { exitText: exit.toString() }),
      ...(streakEvidence === undefined ? {} : { streak: streakEvidence }) });

  let stale: 'unproven' | 'unread' | 'failing' | 'pending' | 'event' | 'starved' | 'seen'
    | 'undeclared' | 'unobserved' | 'unowned' | 'theirs';
  if (proof !== '') {
    if (exit === 75n) stale = observerUnread || a.waits_on_exit === null ? 'unread'
      : sighting ? 'seen' : starved ? 'starved' : on === null ? 'undeclared'
        : owned ? 'theirs' : seen === null ? 'unobserved' : 'unowned';
    else stale = 'proof_attempt' in md && exit === null ? 'unread'
      : exit !== null && exit !== 0n || (string(a.why) ?? '') !== '' ? 'failing' : 'pending';
  } else stale = event !== '' ? owned && !isWorldOwner(owner!) ? 'theirs' : 'event' : 'unproven';

  if (stale === 'unproven') return result('unproven', 'Unproven', 'no proof or event recorded');
  if (stale === 'unread') return result('unread', 'Unread', exit === null ? 'probe outcome unread' : 'observer outcome unread');
  if (stale === 'failing') return result('failed', 'Failing', string(a.why) || `probe exited ${exit}`);
  if (stale === 'unowned') return result('unowned', 'Unowned', 'declared wait is not backed by an owned negative observation');
  if (stale === 'unobserved') return result('unobserved', 'Unobserved', 'declared wait has no supporting observer');
  if (stale === 'seen') return { ...result('satisfied-condition', 'SatisfiedCondition', 'condition already seen'), condition: on!, seenAt: a.waits_on_seen_at as string };
  if (stale === 'theirs' && bound !== null && age !== null && age > bound * 3600n)
    return result('overdue', 'Overdue', 'past the max wait declared by its owner');
  if (stale !== 'theirs') {
    const late = age !== null && age / 3600n >= 24n;
    return result(late ? 'stale' : execution === 'not-yet' ? 'not-yet' : execution === 'success' ? 'success'
      : stale === 'event' ? 'event' : 'pending', null, late ? 'proof past its stale band; ours to read' : 'no immediate proof fault', late);
  }

  const unqualified = (why: string) => result('unqualified-wait', null, why, true, 'unqualified');
  if (max === 'unread') return unqualified('required exact clock-number reading absent: /waits_on/max_wait_hours');
  if (bound !== null && age === null) return unqualified('bounded wait start clock is unread');
  if (now === null) return unqualified('wait observation clock is unread');
  const readTime = (value: unknown, pointer: string): ProofInstant | string => typeof value !== 'string'
    ? `required wait reading absent: ${pointer}` : proofInstant(value) ?? `required wait time unread: ${pointer}`;
  const next = readTime(w.next_check_at, '/waits_on/next_check_at');
  if (typeof next === 'string') return unqualified(next);
  if (compareInstants(next, now) <= 0) return unqualified(`missed recorded next check at ${clockText(next)}`);
  const step = facts.steps.find(s => s.spec_slug === 'proven');
  const nativeOwner = rustTrim(step?.assignee_id ?? facts.ownerId ?? '');
  if (nativeOwner === '') return unqualified('required native action owner absent');
  if (!isWorldOwner(owner!) && owner !== nativeOwner && owner !== facts.ownerId)
    return unqualified('declared wait owner is not its native owner or step assignee');
  if (!isWorldOwner(owner!) && !('clean_window' in w) && seen === null) {
    if (!facts.steps.some(s => s.spec_slug === 'proven' && s.status === 'active' && s.assignee_id === owner))
      return unqualified('required human claim is absent; nomination is not a claim');
    if (md.proof_event !== on) return unqualified('human claim does not bind the recorded proof condition');
    return result('not-yet', null, 'qualified', false, 'qualified');
  }
  const readAt = readTime(a.at, '/proof_attempt/at');
  if (typeof readAt === 'string') return unqualified(readAt);
  if (compareInstants(readAt, now) > 0) return unqualified('proof observation is dated in the future');
  if (seen === null) return unqualified('required supporting observation absent');
  if (string(a.waits_on_probe) !== seen) return unqualified('observer outcome does not bind the declared observer');
  if (string(a.probe) !== string(md.proof_probe)) return unqualified('proof outcome does not bind the declared proof');
  if (typeof serdeI64(a.waits_on_exit, facts.numbers?.['/proof_attempt/waits_on_exit']) !== 'bigint') return unqualified('required observer outcome unread');
  if (a.waits_on_exit === 0n) return unqualified('required condition was seen but the proof still waits');
  if (observation !== 'unseen') return unqualified('required observer did not record an explicit successful negative reading');
  if ('clean_window' in w) {
    const window = object(w.clean_window);
    const coverage = object(a.waits_on_coverage);
    const begin = readTime(window.begin_at, '/waits_on/clean_window/begin_at');
    if (typeof begin === 'string') return unqualified(begin);
    const duration = serdeI64(window.duration_seconds, facts.numbers?.['/waits_on/clean_window/duration_seconds']);
    if (duration === 'unread') return unqualified('required exact clock-number reading absent: /waits_on/clean_window/duration_seconds');
    if (duration === null || duration <= 0n) return unqualified('clean-window duration absent or invalid');
    const deadline = checkedDeadline(begin, duration);
    if (deadline === null) return unqualified('clean-window deadline is outside the clock range');
    const coverageBegin = readTime(coverage.begin_at, '/proof_attempt/waits_on_coverage/begin_at');
    if (typeof coverageBegin === 'string') return unqualified(coverageBegin);
    const through = readTime(coverage.through_at, '/proof_attempt/waits_on_coverage/through_at');
    if (typeof through === 'string') return unqualified(through);
    if (!Array.isArray(coverage.gaps)) return unqualified('clean-window gaps reading absent');
    if (compareInstants(coverageBegin, begin) !== 0 || compareInstants(through, begin) < 0 || compareInstants(through, readAt) > 0 || coverage.gaps.length > 0)
      return unqualified('clean-window coverage does not cover its recorded beginning without gaps');
    if (coverage.source !== seen) return unqualified('clean-window coverage does not bind the executed observer');
    if (compareInstants(now, deadline) >= 0) return unqualified('clean-window deadline reached; a new observation or completed proof is required');
  }
  return result('not-yet', null, 'qualified', false, 'qualified');
}
