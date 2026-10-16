// THE INSPECTION SHED — the floor's last stage.
//
// The yard's mainline used to stop at `arrived` while the page's own
// subtitle told the reader the journey is gated → parked → boarded →
// departed → arrived → PROVEN. The page named a stage its map did not
// draw, and the only surface for a probe anywhere was one counter tile
// reading "awaiting proof" (backlog 6db785f7; David's call, 2026-09-11:
// extend the floor rather than add a panel).
//
// So the arrivals yard feeds a shed. A car that has landed but whose
// `proven` step is not yet stamped stands in one of three places:
//
//   the INSPECTION SHED — it recorded a probe. The dispatcher's
//     `run-car-probes-on-train-arrived` rule files one `run-car-probe`
//     ops-request per such car when its train arrives; the forge's
//     ops-runner drains them, runs the probe, and either completes the
//     `proven` step (the car leaves, stamped) or writes the failed run
//     back onto the car as `proof_attempt`.
//   the EVENT SIDING — it recorded only `proof_event`: prose naming an
//     event that has to happen. No probe can settle it, so no request is
//     ever filed, and it waits for the world rather than for a runner.
//   the NO-PROBE SIDING — it recorded neither. Nothing mechanical can
//     settle it; a person must run `boss prove --probe` by hand.
//
// The three are DISJOINT AND TOTAL, which is the floor's one-branch-one-
// wagon rule applied here: a car carrying both a probe and an event
// stands in the shed, because the probe is the half a machine can
// settle. See `shedPlace`.
//
// NOTHING HERE IS INVENTED. Every value is a field the packet already
// carries: `proof_probe`, `proof_expect`, `proof_event`, `proof_attempt`,
// the ops-request the arrival rule files, and the stamp the `proven` step
// takes. The one place this module must be careful is the ops-requests:
// the page reads a WINDOW of them, so "no request in the rows read" is
// not "no request was filed", and `ProbeRun`'s `waiting` says the weaker
// of the two (a-limit-is-not-a-filter).

import { proofOutcome, type CarProof, type CarRow, type JobLite, type ProofAttempt } from './yard';
import type { ProofNow } from './proof-clock';
import { judgeProof, type ProofFacts, type ProofJudgment } from './proof-judgment';

/** Where an arrived, unproven car stands. */
export type ShedPlace = 'inspection-shed' | 'siding-event' | 'siding-no-probe';

/** THE DECISION: a probe wins over an event.
 *
 *  A car may record both, and the floor keeps exactly one wagon per
 *  branch, so they cannot both hold it. The probe takes it, because a
 *  car whose probe can run is not waiting on anybody — the event is then
 *  extra context the entity panel still shows. The three places are
 *  therefore disjoint and total over every car: every car is in exactly
 *  one, always. */
export function shedPlace(proof: CarProof | null | undefined): ShedPlace {
  if (proof == null) return 'siding-no-probe';
  if (proof.probe !== null && proof.probe !== '') return 'inspection-shed';
  if (proof.event !== null && proof.event !== '') return 'siding-event';
  return 'siding-no-probe';
}

/** What is known about this car's probe RUN.
 *
 *  `queued` is an open `run-car-probe` ops-request the forge has not
 *  drained. `ran` is the last run recorded on the car, which is a run
 *  that did not settle it (a run that does completes the `proven` step
 *  and the car leaves the shed). `waiting` is the honest weak answer:
 *  no open request in the rows the page read, and no run on record. */
export type ProbeRun =
  | Readonly<{ kind: 'queued'; id: string; at: string | null }>
  | (Readonly<{ kind: 'ran' }> & ProofAttempt)
  | Readonly<{ kind: 'waiting' }>;

const md = (j: JobLite): Record<string, unknown> => (j.metadata ?? {}) as Record<string, unknown>;

const text = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

const ms = (s: string | null): number => {
  const n = s === null ? Number.NaN : Date.parse(s);
  return Number.isNaN(n) ? Number.NEGATIVE_INFINITY : n;
};

/** The `run-car-probe` ops-requests for this car in the rows the page
 *  read, newest-filed first. Matched on `metadata.car`, which the
 *  arrival rule sets to the car's packet id. */
function probeRequests(carId: string, requests: readonly JobLite[] | null): readonly JobLite[] {
  if (requests === null) return [];
  return requests
    .filter(j => md(j).verb === 'run-car-probe' && md(j).car === carId)
    .sort((a, b) => ms(text(md(b).opened_at) ?? b.opened_on) - ms(text(md(a).opened_at) ?? a.opened_on));
}

/** This car's probe run. An OPEN request outranks a recorded attempt:
 *  when a retry is queued, the live fact is the retry, not the last
 *  failure. */
export function probeRun(
  carId: string,
  proof: CarProof | null | undefined,
  requests: readonly JobLite[] | null,
): ProbeRun {
  const open = probeRequests(carId, requests).find(j => j.status === 'open');
  if (open !== undefined) {
    return { kind: 'queued', id: open.id, at: text(md(open).opened_at) ?? text(open.opened_on) };
  }
  const a = proof?.attempt ?? null;
  if (a !== null) {
    return {
      kind: 'ran',
      ...a,
    };
  }
  return { kind: 'waiting' };
}

/** A car standing in the shed or on one of its sidings. */
export type ShedCar = Readonly<{
  car: CarRow;
  place: ShedPlace;
  /** The probe command, on a car in the shed; null on a siding. */
  probe: string | null;
  /** The string the probe must print. Null when the car recorded a probe
   *  and no expectation — which is a real, defective state, not a shape
   *  to hide. */
  expect: string | null;
  /** The event prose, on the event siding; null elsewhere. */
  event: string | null;
  run: ProbeRun;
  judgment?: ProofJudgment;
}>;

/** The shed's population, in the order the cars were given. Every
 *  awaiting-proof car appears exactly once. */
export function inspectionShed(
  awaiting: readonly CarRow[],
  requests: readonly JobLite[] | null,
  nowMs: ProofNow = null,
): readonly ShedCar[] {
  return awaiting.map(car => {
    const proof = car.proof ?? null;
    const place = shedPlace(proof);
    const facts: ProofFacts = proof?.facts ?? { metadata: {
      ...(proof?.probe ? { proof_probe: proof.probe } : {}),
      ...(proof?.event ? { proof_event: proof.event } : {}),
      ...(proof?.attempt ? { proof_attempt: { exit: proof.attempt.exit, at: proof.attempt.at, why: proof.attempt.why } } : {}),
    }, ownerId: null, openedAt: null, steps: [] };
    return {
      car,
      place,
      probe: place === 'inspection-shed' ? proof?.probe ?? null : null,
      expect: place === 'inspection-shed' ? proof?.expect ?? null : null,
      event: place === 'siding-event' ? proof?.event ?? null : null,
      run: place === 'inspection-shed' ? probeRun(car.id, proof, requests) : { kind: 'waiting' },
      judgment: judgeProof(facts, nowMs),
    };
  });
}

/** A qualified not-yet execution remains separate from packets requiring action. */
const judgment = (s: ShedCar): ProofJudgment => s.judgment ?? (s.run.kind === 'ran'
  ? proofOutcome(s.run) : judgeProof(s.car.proof?.facts ?? { metadata: { proof_probe: s.probe, proof_event: s.event }, ownerId: null, openedAt: null, steps: [] }, null));
const ranNotYet = (s: ShedCar): boolean => !judgment(s).action && judgment(s).execution === 'not-yet';

/** An observed failing outcome, distinct from a missing outcome. */
const ranRed = (s: ShedCar): boolean => judgment(s).execution === 'failed';

const conditionSeen = (s: ShedCar): boolean => judgment(s).kind === 'satisfied-condition';

/** Missing outcome requires attention without asserting execution or failure. */
const ranUnread = (s: ShedCar): boolean => judgment(s).kind === 'unread';

export type ShedCounts = Readonly<{
  inspecting: number;
  /** Of those inspecting, how many have a failed run on record. */
  failed: number;
  /** Recorded attempts whose outcome cannot be read. */
  unread: number;
  /** The declared condition is recorded, but the proof still says not yet. */
  conditionSeen: number;
  /** Distinct packets requiring action, across all three places. The
   *  failed/unread/conditionSeen figures above are details, not addends. */
  needsAction: number;
  /** Of those inspecting, how many said "not yet" on their last run. */
  notYet: number;
  onEvent: number;
  noProbe: number;
}>;

export function shedCounts(cars: readonly ShedCar[]): ShedCounts {
  return {
    inspecting: cars.filter(s => s.place === 'inspection-shed').length,
    failed: cars.filter(s => s.place === 'inspection-shed' && ranRed(s)).length,
    unread: cars.filter(s => s.place === 'inspection-shed' && ranUnread(s)).length,
    conditionSeen: cars.filter(s => s.place === 'inspection-shed' && conditionSeen(s)).length,
    needsAction: cars.filter(s => judgment(s).action).length,
    notYet: cars.filter(s => s.place === 'inspection-shed' && ranNotYet(s)).length,
    onEvent: cars.filter(s => s.place === 'siding-event').length,
    noProbe: cars.filter(s => s.place === 'siding-no-probe').length,
  };
}

/** The shed machine's one line — the three places, and nothing else. */
export function shedLabel(c: ShedCounts): string {
  const parts = [
    ...(c.inspecting > 0 ? [`${c.inspecting} inspecting`] : []),
    ...(c.failed > 0 ? [`${c.failed} probe failed`] : []),
    ...(c.unread > 0 ? [`${c.unread} probe outcome unread`] : []),
    ...(c.conditionSeen > 0 ? [`${c.conditionSeen} condition already seen — proof needs attention`] : []),
    ...(c.needsAction > c.failed + c.unread + c.conditionSeen ? [`${c.needsAction - c.failed - c.unread - c.conditionSeen} proof records need attention`] : []),
    ...(c.notYet > 0 ? [`${c.notYet} not yet`] : []),
    ...(c.onEvent > 0 ? [`${c.onEvent} on an event`] : []),
    ...(c.noProbe > 0 ? [`${c.noProbe} with no probe`] : []),
  ];
  return parts.length > 0 ? parts.join(' · ') : 'clear';
}

/** The wagon's one line: where it stands and what its run says. */
export function shedStatus(s: ShedCar): string {
  const outcome = judgment(s);
  if (outcome.action) {
    if (outcome.kind === 'satisfied-condition') return `inspection shed · condition already seen: ${outcome.condition} at ${outcome.seenAt} — proof still says not yet; needs attention`;
    if (outcome.kind === 'failed' && outcome.execution === 'failed') return `inspection shed · probe failed, exit ${outcome.exitText ?? 'unread'}`;
    return `${s.place === 'inspection-shed' ? 'inspection shed' : 'siding'} · ${outcome.reason} — needs attention`;
  }
  switch (s.place) {
    case 'siding-event':
      return 'siding · waiting on an event, no probe can settle it';
    case 'siding-no-probe':
      return 'siding · no probe recorded';
    case 'inspection-shed':
      switch (s.run.kind) {
        case 'queued':
          return 'inspection shed · probe queued on the forge';
        case 'ran': {
          return s.run.exit === 0
              ? 'inspection shed · probe exit 0, not stamped'
              : outcome.kind === 'not-yet'
                ? 'inspection shed · probe says not yet — rechecked daily'
                : `inspection shed · probe failed, exit ${s.run.exit}`;
        }
        case 'waiting':
          // Deliberately the weaker claim: the page read a window of
          // ops-requests, and an absence there is not an absence.
          return 'inspection shed · no probe run in the packets read';
      }
  }
}

/** Packet actionability applies in every place, independently of execution. */
export function shedTone(s: ShedCar): 'ok' | 'warn' | 'red' | 'static' {
  if (judgment(s).action) return 'red';
  if (s.place !== 'inspection-shed') return 'static';
  return 'ok';
}

export function shedLamp(s: ShedCar): 'ok' | 'working' | 'warn' | 'err' | 'off' {
  if (judgment(s).action) return 'err';
  if (s.place !== 'inspection-shed') return 'off';
  return 'working';
}

/** When the run happened, for a wagon with no better entry stamp. */
export function runAt(r: ProbeRun): string | null {
  return r.kind === 'waiting' ? null : r.at;
}

// THE FLAKE TALLY — reds that were not the branch's, by check.
//
// Retro 27fad542 counted 4 of 11 red car gates in two days that were not
// the branch's fault, each recorded exactly like an author's red, so the
// flakiest check was a memory (backlog 36cc4913). The rule lives in
// `boss_jobs::flake` and is decided by the record, never by a check's
// name: `boss gate` re-run at an UNCHANGED head stamps `regate_of` (the
// prior red) on the fresh gate-run, and a GREEN verdict there stamps
// `flake_of` + `flaky_checks` (the prior's failing checks). This tally
// reads those stamps off the gate-run packets the page already holds —
// the same window the signals read — so the shed lists the same number
// `boss orient`'s FLAKES line prints. A stamped run whose prior named no
// check (a lost run, a refusal) counts under NO_CHECK_NAMED rather than
// vanishing.

/** The tally key for a flake whose prior run named no check —
 *  `boss_jobs::flake::NO_CHECK_NAMED`, spelled once there and once here. */
export const NO_CHECK_NAMED = '(no check named)';

export type FlakeCount = Readonly<{ check: string; count: number }>;

const strings = (v: unknown): readonly string[] =>
  Array.isArray(v) ? v.filter((s): s is string => typeof s === 'string') : [];

/** How many times each check went red then green at one head, over the
 *  gate-runs given — most flaky first, ties by name. Runs without a
 *  `flake_of` stamp (a plain green, a re-gate that stayed red) do not
 *  count: a persistent red is the branch's. */
export function flakeTally(gateRuns: readonly JobLite[]): readonly FlakeCount[] {
  const counts = new Map<string, number>();
  gateRuns.forEach(r => {
    const m = md(r);
    if (text(m.flake_of) === null) return;
    const checks = strings(m.flaky_checks);
    (checks.length > 0 ? checks : [NO_CHECK_NAMED]).forEach(c => counts.set(c, (counts.get(c) ?? 0) + 1));
  });
  return [...counts.entries()]
    .map(([check, count]) => ({ check, count }))
    .sort((a, b) => b.count - a.count || (a.check < b.check ? -1 : a.check > b.check ? 1 : 0));
}

/** The tally's one line: the checks by count, or a stated none. The
 *  page reads a WINDOW of gate-runs, so a none is "in the runs read". */
export function flakeLabel(tally: readonly FlakeCount[]): string {
  return tally.length === 0
    ? 'no flakes in the runs read'
    : ['flakes', ...tally.map(f => `${f.check}: ${f.count}`)].join(' · ');
}
