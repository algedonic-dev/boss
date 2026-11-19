import { expect, test } from 'bun:test';
import { mapRead, startMapPolling } from './map-polling';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function harness() {
  let hidden = false;
  let tick = () => {};
  let visibility = () => {};
  let stopped = false;
  let removed = false;
  return {
    host: {
      hidden: () => hidden,
      every: (callback: () => void, ms: number) => {
        expect(ms).toBe(10_000);
        tick = callback;
        return () => { stopped = true; };
      },
      onVisibility: (callback: () => void) => {
        visibility = callback;
        return () => { removed = true; };
      },
    },
    tick: () => tick(),
    visible: (value: boolean) => { hidden = !value; visibility(); },
    cleaned: () => stopped && removed,
  };
}

// Flush actual promise continuations, without assuming elapsed host time.
const settle = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

test('a slow endpoint stays single-flight while independent results publish immediately', async () => {
  const h = harness();
  const slow = deferred<string>();
  let slowCalls = 0;
  let fastCalls = 0;
  const seen: unknown[] = [];
  const stop = startMapPolling([
    mapRead(() => { slowCalls++; return slow.promise; }, (value) => { seen.push(value); }),
    mapRead(async () => String(++fastCalls), (value) => { seen.push(value); }),
  ], h.host);
  await settle();
  expect(seen).toEqual(['1']);
  h.tick(); await settle();
  h.tick(); await settle();
  expect(slowCalls).toBe(1);
  expect(fastCalls).toBe(3);
  expect(seen).toEqual(['1', '2', '3']);
  slow.resolve('slow'); await settle();
  expect(seen).toEqual(['1', '2', '3', 'slow']);
  h.tick(); await settle();
  expect(slowCalls).toBe(2);
  stop();
});

test('failure is published and releases the endpoint for recovery', async () => {
  const h = harness();
  const pending = deferred<string>();
  const seen: unknown[] = [];
  let calls = 0;
  const stop = startMapPolling([
    mapRead(() => ++calls === 1 ? pending.promise : Promise.resolve('recovered'), (x) => { seen.push(x); }),
  ], h.host);
  pending.reject(new Error('server rolled')); await settle();
  expect(seen).toEqual([{ kind: 'failed', error: 'server rolled' }]);
  h.tick(); await settle();
  expect(seen).toEqual([{ kind: 'failed', error: 'server rolled' }, 'recovered']);
  stop();
});

test('three elapsed polling intervals keep exactly one unresolved request', async () => {
  const h = harness();
  const pending = deferred<string>();
  let calls = 0;
  const stop = startMapPolling([
    mapRead(() => { calls++; return pending.promise; }, () => {}),
  ], h.host);
  h.tick(); h.tick(); h.tick();
  expect(calls).toBe(1);
  pending.resolve('done'); await settle();
  h.tick();
  expect(calls).toBe(2);
  stop();
});

// A page opened in a background tab still reads ONCE, on mount, so it
// has a map to draw (the it-map-live hidden-tab spec opens /it hidden
// and counts moves on that map; gate 2b1594ef went red when the mount
// read waited for visibility). Only the background ticks skip.
test('a hidden page reads once on mount, skips background ticks, reads on return, and disposal refuses late publication', async () => {
  const h = harness();
  h.visible(false);
  const first = deferred<string>();
  const second = deferred<string>();
  let calls = 0;
  const seen: unknown[] = [];
  const stop = startMapPolling([
    mapRead(() => (++calls === 1 ? first.promise : second.promise), (x) => { seen.push(x); }),
  ], h.host);
  expect(calls).toBe(1);
  first.resolve('drawn'); await settle();
  expect(seen).toEqual(['drawn']);
  h.tick(); h.tick();
  expect(calls).toBe(1);
  h.visible(true);
  expect(calls).toBe(2);
  h.visible(false); h.tick(); h.visible(true); h.tick();
  expect(calls).toBe(2);
  stop();
  expect(h.cleaned()).toBe(true);
  second.resolve('obsolete'); await settle();
  expect(seen).toEqual(['drawn']);
  h.tick(); h.visible(true);
  expect(calls).toBe(2);
});

test('each result carries its own completion time, including failure', async () => {
  const h = harness();
  const slow = deferred<string>();
  const fast = deferred<string>();
  const seen: unknown[] = [];
  let now = 12;
  const stop = startMapPolling([
    mapRead(() => slow.promise, (value, at) => { seen.push([value, at]); }),
    mapRead(() => fast.promise, (value, at) => { seen.push([value, at]); }),
  ], h.host, () => now);
  fast.resolve('fast'); await settle();
  now = 32;
  slow.reject(new Error('down')); await settle();
  expect(seen).toEqual([['fast', 12], [{ kind: 'failed', error: 'down' }, 32]]);
  stop();
});

// A slow read keeps its own slot: the flow read that first pinned this
// retired with the transit map (R3, backlog 23a4b4d7); the rule holds for
// any read the page polls.
test('a blocked read and a pending answer never delay or replace another read', async () => {
  const h = harness();
  type Answer = Readonly<{ state: 'pending' | 'ready' }>;
  const first = deferred<Answer>();
  const pending: Answer = { state: 'pending' };
  const ready: Answer = { state: 'ready' };
  let flowCalls = 0, baseCalls = 0;
  const statuses: unknown[] = [], counts: unknown[] = [];
  const stop = startMapPolling([
    mapRead(() => ++flowCalls === 1 ? first.promise : Promise.resolve(ready), (value) => { statuses.push(value); }),
    mapRead(async () => { baseCalls++; return { count: 7 }; }, (value) => { counts.push(value); }),
  ], h.host);
  await settle(); h.tick(); await settle();
  expect(flowCalls).toBe(1); expect(baseCalls).toBe(2);
  expect(statuses).toEqual([]); expect(counts).toEqual([{ count: 7 }, { count: 7 }]);
  first.resolve(pending); await settle();
  expect(statuses).toEqual([pending]);
  h.tick(); await settle();
  expect(statuses).toEqual([pending, ready]);
  expect(counts).toEqual([{ count: 7 }, { count: 7 }, { count: 7 }]);
  stop();
});
