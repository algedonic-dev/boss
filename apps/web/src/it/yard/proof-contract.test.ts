import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import type originalCases from './proof-contract.json';
import { parseProofJobsJSON } from './proof-json';
import type { ProofNumbers } from './proof-numbers';
import { readCarProof, type CarRow } from './yard';
import { inspectionShed, shedCounts, shedLamp, shedTone } from './yard-shed';
const source = readFileSync(new URL('./proof-contract.json', import.meta.url), 'utf8');
const cases = (parseProofJobsJSON(`{"data":${source}}`) as {
  data: ((typeof originalCases)[number] & { proofNumbers: ProofNumbers; expected_streak?: { hours: string; runs: string } | null })[];
}).data;

function row(c: (typeof cases)[number]): CarRow {
  const job = { id: `contract-${c.index}`, kind: 'ship-a-change', title: c.name, status: 'open',
    proofNumbers: c.proofNumbers, owner_id: 'emp-david', opened_on: '2026-10-15', opened_at: c.opened_at, metadata: c.metadata,
    steps: [{ spec_slug: 'proven', title: 'Proven in production', status: c.claim?.status ?? 'ready', assignee_id: c.claim?.assignee ?? null }] };
  return { id: job.id, kind: job.kind, title: job.title, branch: 'fix/contract', tags: [], sim: false,
    skipReason: null, head: 'a'.repeat(40), deliveryChannel: 'software', proof: readCarProof(job) };
}

for (const c of cases) {
  test(`backend proof contract ${c.index}: ${c.name}`, () => {
    const before = JSON.stringify(c);
    const car = row(c);
    if (car.proof !== null) {
      expect(car.proof?.facts?.metadata).toEqual(c.metadata);
      expect(car.proof?.facts?.ownerId).toBe('emp-david');
    }
    const floor = inspectionShed([car], null, c.now);
    expect(floor).toHaveLength(1);
    expect(shedTone(floor[0]!) === 'red').toBe(c.expected.action);
    expect(shedLamp(floor[0]!) === 'err').toBe(c.expected.action);
    const expectedFault = /^Some\((\w+)/.exec(c.expected.fault)?.[1] ?? null;
    expect<string | null | undefined>(floor[0]!.judgment?.fault).toBe(expectedFault);
    if (expectedFault === 'Failing') {
      const token = c.proofNumbers['/proof_attempt/exit'];
      expect(floor[0]!.judgment?.exitText).toBe(token);
      expect(car.proof?.attempt?.exitText).toBe(token);
      if (token !== undefined && !Number.isSafeInteger(Number(token)))
        expect(car.proof?.attempt?.exit).toBeNull();
    }
    expect(floor[0]!.judgment?.wait).toBe(c.expected.wait === 'qualified' ? 'qualified'
      : c.expected.wait === 'none' ? 'none' : 'unqualified');
    if (c.expected.wait !== 'qualified' && c.expected.wait !== 'none')
      expect(floor[0]!.judgment?.reason).toBe(c.expected.wait);
    if ('expected_streak' in c) expect(floor[0]!.judgment?.streak ?? null).toEqual(c.expected_streak ?? null);
    expect(JSON.stringify(c)).toBe(before);
  });
}

test('the shared contract retains every ordinal and the mixed floor population', () => {
  expect(cases.slice(0, 66)).toHaveLength(66);
  expect(cases.map(c => c.index)).toEqual(Array.from({ length: cases.length }, (_, i) => i));
  const commonClock = cases.filter(c => c.now === cases[0]!.now);
  const floor = inspectionShed(cases.map(row), null, cases[0]!.now);
  const commonFloor = inspectionShed(commonClock.map(row), null, cases[0]!.now);
  expect(new Set(floor.map(s => s.car.id)).size).toBe(cases.length);
  expect(commonFloor.filter(s => shedTone(s) === 'red')).toHaveLength(commonClock.filter(c => c.expected.action).length);
  const counts = shedCounts(floor);
  expect(counts.inspecting + counts.onEvent + counts.noProbe).toBe(cases.length);
  expect(shedCounts(commonFloor).needsAction).toBe(commonClock.filter(c => c.expected.action).length);
});
