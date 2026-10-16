import { expect, test } from 'bun:test';
import invalidDocuments from './proof-invalid-json.json';
import { parseProofJobsJSON } from './proof-json';
import { readCarProof, type CarFacts } from './yard';
import { inspectionShed, shedTone } from './yard-shed';

for (const [exit, observer, healthy] of [
  ['75', '1', true], ['75.0', '1', false], ['75e0', '1', false], ['75', '1.0', false],
  ['75', '1e0', false], ['75.000000000000000001', '1', false], ['75', '-0', false],
] as const) {
  test(`wire numeric outcomes ${exit}/${observer} preserve native integer kind`, () => {
    const body = `{"data":[{"id":"wire","kind":"ship-a-change","title":"wire","owner_id":"emp-david","metadata":{"proof_probe":"proof","proof_attempt":{"at":"2026-10-15T09:00:00Z","probe":"proof","exit":${exit},"waits_on_exit":${observer},"waits_on_observation":"unseen","waits_on_probe":"observer"},"waits_on":{"on":"condition","seen":"observer","owner":"world","next_check_at":"2026-10-15T11:00:00Z"}}}]}`;
    const job = (parseProofJobsJSON(body) as { data: CarFacts[] }).data[0]!;
    const proof = readCarProof(job);
    const floor = inspectionShed([{ id: job.id, kind: job.kind, title: job.title, branch: 'fix/wire', tags: [], sim: false,
      skipReason: null, head: 'a'.repeat(40), deliveryChannel: 'software', proof }], null, '2026-10-15T10:00:00Z');
    expect(shedTone(floor[0]!) === 'ok').toBe(healthy);
    expect(floor[0]!.judgment?.fault).toBe(healthy ? null : 'Unread');
    expect(proof?.facts?.metadata).toEqual(JSON.parse(body).data[0].metadata);
  });
}

test('all metadata numeric leaves retain escaped JSON-pointer paths and last-key winner', () => {
  const body = String.raw`{"data":[{"metadata":{"proof_attempt":{"exit":75.0,"waits_on_exit":1e0,"raw_exit":0.0,"not_yet_runs":18446744073709551615,"missing_tools":[1.0]},"unknown":{"a/b~c":[-0,{"n":1e100}]},"removed":1,"removed":"string","nested":{"n":7},"nested":null,"waits_on":{"max_wait_hours":1,"max_wait_\u0068ours":2,"clean_window":{"duration_seconds":3}}}}]}`;
  const job = (parseProofJobsJSON(body) as { data: { proofNumbers?: unknown }[] }).data[0]!;
  expect(job.proofNumbers).toEqual({ '/proof_attempt/exit': '75.0', '/proof_attempt/waits_on_exit': '1e0',
    '/proof_attempt/raw_exit': '0.0', '/proof_attempt/not_yet_runs': '18446744073709551615', '/proof_attempt/missing_tools/0': '1.0',
    '/unknown/a~1b~0c/0': '-0', '/unknown/a~1b~0c/1/n': '1e100', '/waits_on/max_wait_hours': '2', '/waits_on/clean_window/duration_seconds': '3' });
});

test('non-finite, overflowing and invalid numeric JSON cannot manufacture a reading', () => {
  for (const source of invalidDocuments) expect(() => parseProofJobsJSON(source)).toThrow();
});


test('absent wire outcome tokens are unread, while an ancillary count stays explicitly unknown', async () => {
  const { judgeProof } = await import('./proof-judgment');
  const metadata = { proof_probe: 'proof', proof_attempt: { at: '2026-10-15T09:00:00Z', probe: 'proof', exit: 75,
    waits_on_exit: 1, waits_on_observation: 'unseen', waits_on_probe: 'observer', not_yet_runs: 2 },
    waits_on: { on: 'condition', seen: 'observer', owner: 'world', next_check_at: '2026-10-15T11:00:00Z' } };
  const facts = { metadata, ownerId: 'emp-david', openedAt: null, steps: [] };
  expect(judgeProof(facts, '2026-10-15T10:00:00Z')).toMatchObject({ kind: 'unread', fault: 'Unread', action: true });
  expect(judgeProof({ ...facts, numbers: { '/proof_attempt/exit': '75', '/proof_attempt/waits_on_exit': '1' } },
    '2026-10-15T10:00:00Z')).toMatchObject({ action: false, wait: 'qualified', streak: { runs: null } });
});

test('a constructed display attempt without numeric provenance cannot establish a wait', () => {
  const floor = inspectionShed([{ id: 'no-facts', kind: 'ship-a-change', title: 'no facts', branch: 'fix/no-facts',
    tags: [], sim: false, skipReason: null, head: 'a'.repeat(40), deliveryChannel: 'software',
    proof: { probe: 'proof', expect: null, event: null, stamped: null,
      attempt: { at: null, exit: 75, host: null, stdout: null, stderr: null, why: null, missingTools: [], notYet: true } } }], null);
  expect(floor[0]?.judgment?.fault).toBe('Unread');
  expect(shedTone(floor[0]!)).toBe('red');
});
