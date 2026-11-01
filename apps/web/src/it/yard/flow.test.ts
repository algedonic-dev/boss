import { afterEach, expect, test } from 'bun:test';
import { fetchFlow, parseFlow } from './flow';

const pending = { state: 'pending', as_of: null, window_hours: 24, why: 'flow counts pending' } as const;
const ready = { state: 'ready', as_of: '2026-10-23T12:00:00Z', window_hours: 24, why: null } as const;

test('a completed cold refresh failure stays unread with its original cause', () => {
  const unread = { ...pending, state: 'unread', why: 'storage failure: native membership history unavailable' } as const;
  expect(parseFlow(unread)).toEqual(unread);
  expect(() => parseFlow({ ...unread, as_of: ready.as_of })).toThrow();
  expect(() => parseFlow({ ...unread, why: '' })).toThrow();
});

test('flow parses pending, a named ready instant, and a policy refusal separately', () => {
  expect(parseFlow(pending)).toEqual(pending);
  expect(parseFlow(ready)).toEqual(ready);
  const withheld = { ...pending, state: 'withheld', why: 'history is outside your policy scope' } as const;
  expect(parseFlow(withheld)).toEqual(withheld);
});

test('a failed refresh keeps the successful reading instant and exact cause', () => {
  const stale = { ...ready, why: 'storage failure: history statement timed out' };
  expect(parseFlow(stale)).toEqual(stale);
  expect(parseFlow(stale).as_of).toBe(ready.as_of);
});

test('malformed states and invented reading instants are refused', () => {
  for (const raw of [null, [], {}, { ...pending, window_hours: 0 },
    { ...pending, as_of: ready.as_of }, { ...ready, as_of: null },
    { ...ready, as_of: 'yesterday' }, { ...ready, why: '' }, { ...ready, why: '   ' }, { ...ready, why: 42 },
    { ...pending, state: 'withheld', why: null }]) {
    expect(() => parseFlow(raw)).toThrow();
  }
});

const realFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = realFetch; });
test('HTTP202 is domain pending, not a failed read or fabricated ready counts', async () => {
  globalThis.fetch = (async (url: string) => {
    expect(url).toBe('/api/yard/flow');
    return new Response(JSON.stringify(pending), { status: 202 });
  }) as unknown as typeof fetch;
  expect(await fetchFlow()).toEqual({ kind: 'ready', data: pending });
});
test('native HTTP failures remain unread', async () => {
  globalThis.fetch = (async () => new Response('gateway timeout', { status: 524 })) as unknown as typeof fetch;
  expect(await fetchFlow()).toEqual({ kind: 'failed', error: '/api/yard/flow: HTTP 524' });
});
