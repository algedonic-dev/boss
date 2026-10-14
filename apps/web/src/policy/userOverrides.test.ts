import { describe, expect, test } from 'bun:test';
import { parseOverride, saveOverride, verifyOverride, type OverrideFetch } from './userOverrides';

const draft = { id: 'prepared-1', user_id: 'emp-person', action: 'update', resource: 'example-control', scope: { department: 'ops' }, reason: 'Approved exact reason', expires_at: '2026-12-01T10:30:00+00:00' };

describe('named-person override wire', () => {
  test('structured import preserves prepared id, reason, scope and expiry', () => {
    expect(parseOverride({ override: draft })).toEqual(draft);
    expect(() => parseOverride({ ...draft, changed_by: 'forged' })).toThrow();
    expect(() => parseOverride({ ...draft, scope: 'invented' })).toThrow();
    expect(() => parseOverride({ ...draft, expires_at: 'tomorrow' })).toThrow();
  });
  test('writes only exact guarded body and checks complete GET effect, including equivalent expiry', async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    const wire: OverrideFetch = async (url, init) => {
      calls.push({ url, init });
      return calls.length === 1 ? Response.json([]) : calls.length === 2 ? Response.json(draft) : Response.json([{ ...draft, expires_at: '2026-12-01T10:30:00Z' }]);
    };
    expect((await saveOverride(draft, wire)).kind).toBe('verified');
    expect(calls.map(c => c.url)).toEqual(['/api/policy/user-overrides/emp-person', '/api/policy/user-overrides', '/api/policy/user-overrides/emp-person']);
    expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({ override: draft });
    expect(calls[1]?.init?.credentials).toBe('same-origin');
    expect(calls[1]?.init?.headers).toEqual({ 'content-type': 'application/json' });
  });
  test('403 retains service refusal and never reads back a refused write', async () => {
    let calls = 0;
    const result = await saveOverride(draft, async () => ++calls === 1 ? Response.json([]) : new Response('last holder would be lost', { status: 403 }));
    expect(result.kind).toBe('refused');
    if (result.kind === 'refused') expect(result.error).toBe('HTTP 403: last holder would be lost');
    expect(calls).toBe(2);
  });
  test('every altered effect field, absent row, duplicate id or unreadable effect remains uncertain', async () => {
    for (const effect of [
      [], [draft, draft], [{ ...draft, reason: 'other' }], [{ ...draft, user_id: 'other' }],
      [{ ...draft, action: 'read' }], [{ ...draft, resource: 'other' }],
      [{ ...draft, scope: 'all' }], [{ ...draft, expires_at: null }],
    ]) {
      let calls = 0;
      expect((await saveOverride(draft, async () => ++calls === 1 ? Response.json([]) : calls === 2 ? Response.json(draft) : Response.json(effect))).kind).toBe('uncertain');
    }
    let calls = 0;
    expect((await saveOverride(draft, async () => ++calls === 1 ? Response.json([]) : new Response('down', { status: 503 }))).kind).toBe('uncertain');
    calls = 0;
    expect((await saveOverride(draft, async () => { if (++calls === 1) return Response.json([]); throw new Error('network lost'); })).kind).toBe('uncertain');
  });
  test('a failed before read sends no write; full before/write/after responses are retained', async () => {
    const methods: Array<string | undefined> = [];
    const failed = await saveOverride(draft, async (_url, init) => { methods.push(init?.method); return new Response('read refused', { status: 403 }); });
    expect(failed.kind).toBe('refused'); expect(methods).toEqual([undefined]);
    const originals = ['[]', JSON.stringify(draft), JSON.stringify([draft, { ...draft, id: 'other-permission' }])];
    let index = 0;
    const saved = await saveOverride(draft, async () => new Response(originals[index++], { status: 200 }));
    expect(saved.kind).toBe('verified');
    expect([saved.receipt.before?.body, saved.receipt.write?.body, saved.receipt.after?.body]).toEqual(originals);
  });
  test('uncertain transport can be rechecked by GET alone while preserving its original before evidence', async () => {
    let calls = 0;
    const lost = await saveOverride(draft, async () => { if (++calls === 1) return Response.json([]); throw new Error('reply lost'); });
    expect(lost.kind).toBe('uncertain');
    const methods: Array<string | undefined> = [];
    const checked = await verifyOverride(draft, async (_url, init) => { methods.push(init?.method); return Response.json([draft]); }, lost.receipt);
    expect(checked.kind).toBe('verified');
    expect(methods).toEqual([undefined]);
    expect(checked.receipt.before).toEqual(lost.receipt.before);
    expect(checked.receipt.write).toBeNull();
    expect(checked.receipt.write_error).toBe('reply lost');
  });
  test('readback cannot hide a fractional-second expiry change below one millisecond', async () => {
    const expected = { ...draft, expires_at: '2026-12-01T10:30:00.000000001Z' };
    const changed = { ...expected, expires_at: '2026-12-01T10:30:00.000000002Z' };
    expect((await verifyOverride(expected, async () => Response.json([changed]))).kind).toBe('uncertain');
  });
  test('GET rechecks append mismatch, transport error and later exact originals without replacing first after', async () => {
    const mismatch = JSON.stringify([{ ...draft, scope: 'none' }]);
    const exact = JSON.stringify([draft]);
    let calls = 0;
    const first = await saveOverride(draft, async () => new Response(++calls === 1 ? '[]' : calls === 2 ? JSON.stringify(draft) : mismatch));
    const lost = await verifyOverride(draft, async () => { throw new Error('recheck connection lost'); }, first.receipt);
    const final = await verifyOverride(draft, async () => new Response(exact), lost.receipt);
    expect(first.kind).toBe('uncertain'); expect(lost.kind).toBe('uncertain'); expect(final.kind).toBe('verified');
    expect(final.receipt.after?.body).toBe(mismatch);
    expect(final.receipt.current_after?.body).toBe(exact);
    expect(final.receipt.readbacks).toEqual([
      { status: 200, response: { status: 200, body: mismatch }, error: null },
      { status: null, response: null, error: 'recheck connection lost' },
      { status: 200, response: { status: 200, body: exact }, error: null },
    ]);
    expect(final.receipt.before).toEqual(first.receipt.before);
    expect(final.receipt.write).toEqual(first.receipt.write);
    expect(first.receipt.after?.body).toBe(mismatch);
  });
  test('an unreadable GET body retains its observed HTTP status and records the read error', async () => {
    const badBody = new ReadableStream({ start(controller) { controller.error(new Error('response body lost')); } });
    const result = await verifyOverride(draft, async () => new Response(badBody, { status: 502 }));
    expect(result.kind).toBe('uncertain');
    expect(result.receipt.readbacks).toEqual([{ status: 502, response: null, error: 'response body lost' }]);
  });
  test('a known 403 with an unreadable body stays refused and cannot become a GET confirmation', async () => {
    let calls = 0;
    const result = await saveOverride(draft, async () => {
      if (++calls === 1) return Response.json([]);
      return new Response(new ReadableStream({ start(controller) { controller.error(new Error('refusal body lost')); } }), { status: 403 });
    });
    expect(result.kind).toBe('refused');
    expect(result.receipt.write).toEqual({ status: 403, body: null });
    expect(result.receipt.write_error).toBe('refusal body lost');
    const checked = await verifyOverride(draft, async () => { calls++; return Response.json([draft]); }, result.receipt);
    expect(checked.kind).toBe('refused');
    expect(calls).toBe(2);
  });
  test('201 and 503 body loss retains known status and original error through a later GET', async () => {
    for (const status of [201, 503]) {
      let calls = 0;
      const first = await saveOverride(draft, async () => ++calls === 1 ? Response.json([]) : new Response(new ReadableStream({ start(controller) { controller.error(new Error('write body lost')); } }), { status }));
      expect(first.kind).toBe('uncertain');
      expect(first.receipt.write).toEqual({ status, body: null });
      expect(first.receipt.write_error).toBe('write body lost');
      const checked = await verifyOverride(draft, async () => Response.json([draft]), first.receipt);
      expect(checked.kind).toBe('verified');
      expect(checked.receipt.write).toEqual(first.receipt.write);
      expect(checked.receipt.write_error).toBe('write body lost');
    }
  });
});
