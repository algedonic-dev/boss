import { expect, test } from 'bun:test';
import type { ProofNumbers } from './proof-numbers';
import { parseProofJobsJSON } from './proof-json';

test('the yard boundary preserves exact clock tokens at their actual paths without changing metadata', () => {
  const body = '{"data":[{"id":"a","metadata":{"waits_on":{"max_wait_hours":9223372036854775807,"clean_window":{"duration_seconds":1e3}}}}]}';
  const parsed = parseProofJobsJSON(body) as { data: { metadata: unknown; proofNumbers?: unknown }[] };
  expect(parsed.data[0]?.metadata).toEqual(JSON.parse(body).data[0].metadata);
  expect(parsed.data[0]?.proofNumbers).toEqual({ '/waits_on/max_wait_hours': '9223372036854775807', '/waits_on/clean_window/duration_seconds': '1e3' });
});

test('duplicate keys, escaping, nesting and array positions follow the native JSON winner', () => {
  const body = String.raw`{"data":[{"metadata":{"waits_on":{"max_wait_hours":7}},"metadata":{"waits_on":{"max_wait_hours":8,"max_wait_hours":9007199254740993,"clean_window":{"duration_seconds":-2.5e+2}},"decoy":[{"waits_on":{"max_wait_hours":99}}]}},{"metadata":{"waits_on":{"max_wait_\u0068ours":9223372036854775808,"clean_window":{"duration_seconds":1.25}}}}]}`;
  const parsed = parseProofJobsJSON(body) as { data: { metadata: unknown; proofNumbers?: unknown }[] };
  expect(parsed.data.map(j => j.metadata)).toEqual(JSON.parse(body).data.map((j: { metadata: unknown }) => j.metadata));
  expect(parsed.data.map(j => j.proofNumbers)).toEqual([
    { '/waits_on/max_wait_hours': '9007199254740993', '/waits_on/clean_window/duration_seconds': '-2.5e+2', '/decoy/0/waits_on/max_wait_hours': '99' },
    { '/waits_on/max_wait_hours': '9223372036854775808', '/waits_on/clean_window/duration_seconds': '1.25' },
  ]);
});

test('a final duplicate scalar clears prior nested readings; strings are not numeric tokens', () => {
  const parsed = parseProofJobsJSON('{"data":[{"metadata":{"waits_on":{"max_wait_hours":8},"waits_on":{"max_wait_hours":"99","clean_window":null}}}]}') as { data: { proofNumbers?: unknown }[] };
  expect(parsed.data[0]?.proofNumbers).toEqual({});
});

test('malformed JSON is refused without manufacturing numeric readings', () => {
  for (const body of ['{"data":[', '{"data":[{"metadata":{"waits_on":{"max_wait_hours":01}}}]}'])
    expect(() => parseProofJobsJSON(body)).toThrow();
});

test('the actual yard HTTP reader preserves MAX and MAX+1 through parser to inspection', async () => {
  const { fetchYard } = await import('./yard');
  const { inspectionShed, shedTone } = await import('./yard-shed');
  const rawJob = (id: string, bound: string) => `{"id":"${id}","kind":"ship-a-change","title":"clock","status":"open","owner_id":"emp-david","opened_on":"2026-10-15","opened_at":null,"metadata":{"branch":"fix/${id}","proof_probe":"proof","proof_attempt":{"probe":"proof","at":"2026-10-15T09:00:00Z","exit":75,"waits_on_probe":"observer","waits_on_exit":1,"waits_on_observation":"unseen"},"waits_on":{"on":"condition","owner":"world","seen":"observer","next_check_at":"2026-10-15T11:00:00Z","max_wait_hours":${bound}}},"steps":[{"spec_slug":"proven","title":"Proven","status":"ready"}]}`;
  const source = `{"data":[${rawJob('max', '9223372036854775807')},${rawJob('over', '9223372036854775808')}],"total":2}`;
  const before = JSON.parse(source);
  expect(before.data[0].metadata.waits_on.max_wait_hours).toBe(before.data[1].metadata.waits_on.max_wait_hours);
  const realFetch = globalThis.fetch;
  try {
    globalThis.fetch = (async (input: RequestInfo | URL) => String(input).includes('kind=ship-a-change')
      ? new Response(source) : String(input).includes('/api/stations/')
        ? new Response('unavailable', { status: 404 }) : Response.json({ data: [], total: 0 })) as typeof fetch;
    const yard = await fetchYard();
    expect(yard?.awaitingProof).toHaveLength(2);
    expect(yard?.awaitingProof.map(c => c.proof?.facts?.metadata)).toEqual(before.data.map((j: { metadata: unknown }) => j.metadata));
    const floor = inspectionShed(yard!.awaitingProof, null, '2026-10-15T10:00:00Z');
    expect(floor.map(shedTone)).toEqual(['red', 'ok']);
    expect(floor.map(c => c.judgment?.wait)).toEqual(['unqualified', 'qualified']);
    expect(floor[0]?.judgment?.reason).toBe('bounded wait start clock is unread');
  } finally { globalThis.fetch = realFetch; }
});

test('missing or mismatched exact numeric readings refuse without inventing a runtime fault', async () => {
  const { judgeProof } = await import('./proof-judgment');
  const metadata = { proof_probe: 'proof', proof_attempt: { at: '2026-10-15T09:00:00Z', probe: 'proof', exit: 75,
    waits_on_probe: 'observer', waits_on_exit: 1, waits_on_observation: 'unseen' },
    waits_on: { on: 'condition', owner: 'world', seen: 'observer', next_check_at: '2026-10-15T11:00:00Z', max_wait_hours: 9007199254740992 } };
  for (const extra of [{}, { '/waits_on/max_wait_hours': '1' }] as readonly ProofNumbers[]) {
    const result = judgeProof({ metadata, ownerId: 'emp-david', openedAt: null, steps: [], numbers: { '/proof_attempt/exit': '75', '/proof_attempt/waits_on_exit': '1', ...extra } }, '2026-10-15T10:00:00Z');
    expect(result).toMatchObject({ action: true, fault: null, execution: 'not-yet', wait: 'unqualified' });
    expect(result.reason).toContain('required exact clock-number reading absent');
  }
});

test('escaped strings and nested arrays cannot supply another job’s clock reading', () => {
  const source = String.raw`{"data":[{"metadata":{"text":"\\\"max_wait_hours\\\":9223372036854775807","waits_on":{"max_wait_hours":1,"clean_window":[{"duration_seconds":99}]}},"metadata":{"waits_on":null}},{"metadata":{"waits_on":{"max_wait_hours":1e100,"clean_window":{"duration_seconds":9223372036854775808}}}}]}`;
  const parsed = parseProofJobsJSON(source) as { data: { proofNumbers: unknown }[] };
  expect(parsed.data.map(j => j.proofNumbers)).toEqual([{}, { '/waits_on/max_wait_hours': '1e100', '/waits_on/clean_window/duration_seconds': '9223372036854775808' }]);
});
