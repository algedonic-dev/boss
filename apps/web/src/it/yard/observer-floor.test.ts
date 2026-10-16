import { expect, test } from 'bun:test';
import { readCarProof } from './yard';
import { parseProofJobsJSON } from './proof-json';
import { inspectionShed, shedCounts, shedLamp, shedStatus, shedTone } from './yard-shed';

const declaration = { on: 'the event', owner: 'world', seen: 'event-check', next_check_at: '2026-10-15T11:00:00Z' };
const attempt = { at: '2026-10-15T09:00:00Z', probe: 'proof-check', exit: 75, not_yet: true, waits_on_probe: 'event-check' };

function floor(evidence: Readonly<Record<string, unknown>>, waitsOn: unknown = declaration) {
  const job = { id: 'observer-unread', kind: 'ship-a-change', title: 'observer', status: 'open', owner_id: 'emp-david', opened_on: '2026-10-15',
    metadata: { proof_probe: 'proof-check', proof_attempt: evidence, waits_on: waitsOn } };
  const wire = (parseProofJobsJSON(JSON.stringify({ data: [job] })) as { data: typeof job[] }).data[0]!;
  const proof = readCarProof(wire);
  const cars = inspectionShed([{ id: job.id, kind: job.kind, branch: 'fix/example', title: job.title,
    tags: [], sim: false, skipReason: null, head: 'a'.repeat(40), deliveryChannel: 'software', proof }], null, Date.parse('2026-10-15T10:00:00Z'));
  return { proof, cars, car: cars[0]! };
}

const unreadCases: readonly (readonly [string, Readonly<Record<string, unknown>>])[] = [
  ['execution error', { waits_on_exit: 2, waits_on_raw_exit: 2, waits_on_stderr: 'read failed', waits_on_observation: null }],
  ['missing', {}], ['null', { waits_on_exit: null }], ['string', { waits_on_exit: '1', waits_on_observation: 'unseen' }],
  ['boolean', { waits_on_exit: true }], ['fraction', { waits_on_exit: 1.5 }], ['array', { waits_on_exit: [1] }],
  ['legacy negative', { waits_on_exit: 1 }], ['wrong observation', { waits_on_exit: 1, waits_on_observation: 'seen' }],
  ['refused', { waits_on_exit: 1, waits_on_observation: 'unseen', waits_on_refusal: 'reader refused' }],
  ['environment error', { waits_on_exit: 0, waits_on_error: 'reader unavailable' }],
  ['missing tools', { waits_on_exit: 1, waits_on_observation: 'unseen', waits_on_missing_tools: ['jq'] }],
  ['malformed tools', { waits_on_exit: 0, waits_on_missing_tools: null }],
  ['false error is still recorded', { waits_on_exit: 0, waits_on_error: false }],
];

for (const [name, fields] of unreadCases) {
  test(`parser to inspection floor preserves unread observer: ${name}`, () => {
    const evidence = { ...attempt, waits_on_probe: 'event-check', ...fields };
    const { proof, cars, car } = floor(evidence);
    expect(proof?.attempt?.exit).toBe(75);
    expect(shedCounts(cars)).toEqual({ inspecting: 1, needsAction: 1, failed: 0, unread: 1, conditionSeen: 0, notYet: 0, onEvent: 0, noProbe: 0 });
    expect(shedTone(car)).toBe('red');
    expect(shedLamp(car)).toBe('err');
    expect(shedStatus(car)).toContain('observer outcome unread');
    expect(shedStatus(car)).not.toContain('probe says not yet');
  });
}

for (const fields of [
  { waits_on_exit: 1, waits_on_observation: 'unseen', waits_on_stdout: '{"observation":"unseen"}', waits_on_missing_tools: [] },
  { waits_on_exit: 0, waits_on_seen_at: '2026-10-15T09:01:00Z', waits_on_error: null, waits_on_refusal: null },
]) {
  test(`an executed ${fields.waits_on_exit === 0 ? 'seen' : 'unseen'} observation keeps its own judgment`, () => {
    const { cars, car } = floor({ ...attempt, ...fields });
    expect(shedCounts(cars).notYet).toBe(fields.waits_on_exit === 0 ? 0 : 1);
    expect(shedCounts(cars).unread).toBe(0);
    expect(shedTone(car)).toBe(fields.waits_on_exit === 0 ? 'red' : 'ok');
  });
}

test('the recorded condition already seen while the proof still says not yet requires action', () => {
  const evidence = { ...attempt, not_yet_since: '2026-10-15T08:00:00Z', not_yet_runs: 2,
    waits_on_exit: 0, waits_on_probe: 'event-check', waits_on_seen_at: '2026-10-15T09:00:00Z' };
  const { proof, cars, car } = floor(evidence);
  expect(proof?.attempt?.exit).toBe(75);
  expect(proof?.attempt?.observer?.observation).toBe('seen');
  expect(shedCounts(cars).notYet).toBe(0);
  expect(shedCounts(cars).unread).toBe(0);
  expect(shedCounts(cars).failed).toBe(0);
  expect(shedCounts(cars).conditionSeen).toBe(1);
  expect(shedTone(car)).toBe('red');
  expect(shedLamp(car)).toBe('err');
  expect(shedStatus(car)).toContain('condition already seen: the event');
  expect(shedStatus(car)).toContain('2026-10-15T09:00:00Z');
  expect(shedStatus(car)).not.toContain('probe says not yet');
  expect(proof?.attempt?.observer?.evidence).toEqual(evidence);
  const unseen = floor({ ...evidence, waits_on_exit: 1, waits_on_observation: 'unseen', waits_on_seen_at: null });
  expect(shedTone(unseen.car)).toBe('ok');
  expect(shedCounts(unseen.cars).notYet).toBe(1);
});

test('bare legacy exit75 has no declared observation obligation, while explicit null is unread', () => {
  for (const waitsOn of [null, {}, { seen: 'observer' }]) {
    const { cars } = floor(attempt, waitsOn);
    expect(shedCounts(cars).notYet).toBe(1);
    expect(shedCounts(cars).unread).toBe(0);
  }
  expect(shedTone(floor(attempt, { on: 'event', owner: 'world', seen: ' ' }).car)).toBe('red');
  expect(shedCounts(floor({ ...attempt, waits_on_exit: null }, null).cars).unread).toBe(1);
  expect(shedCounts(floor({ ...attempt, waits_on_exit: 2 }, null).cars).notYet).toBe(1);
});

test('observer evidence and declaration survive parsing and the floor without mutation or truncation', () => {
  for (const fields of [...unreadCases.map(([, fields]) => fields), { waits_on_exit: 0 }, { waits_on_exit: 1, waits_on_observation: 'unseen' }]) {
    const evidence = { ...attempt, ...fields, waits_on_stdout: 'original\n'.repeat(5000), waits_on_stderr: 'stderr\n',
      waits_on_future: { nested: ['all', null, 17] }, waits_on_raw_exit: 2 };
    const snapshot = JSON.stringify({ evidence, declaration });
    const { proof, car } = floor(evidence);
    expect(proof?.attempt?.observer?.evidence).toEqual(evidence);
    expect(proof?.attempt?.observer?.declaration).toEqual(declaration);
    expect(car.run.kind).toBe('ran');
    if (car.run.kind === 'ran') expect(car.run.observer).toEqual(proof?.attempt?.observer);
    expect(JSON.stringify({ evidence, declaration })).toBe(snapshot);
  }
});

test('a proof failure or missing proof outcome keeps its own verdict alongside observer evidence', () => {
  for (const exit of [1, null]) {
    const { proof, cars } = floor({ ...attempt, exit, waits_on_exit: 0 });
    expect(proof?.attempt?.exit).toBe(exit);
    expect(shedCounts(cars).notYet).toBe(0);
    expect(shedCounts(cars).failed).toBe(exit === 1 ? 1 : 0);
    expect(shedCounts(cars).unread).toBe(exit === null ? 1 : 0);
  }
});
