// The false-empty class (packet 3fba9c35): a failed fetch must come
// back as a FAILED state carrying the error — never as data that
// happens to be empty. Remote<T> is the house discriminated union;
// fetchRemote can only resolve ready or failed, so a surface that
// stores its result cannot render "loaded and truly empty" for an
// outage without choosing to.

import { afterEach, describe, expect, test } from 'bun:test';
import { fetchRemote, readFailed, type Remote } from './remote';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: (url: string) => Promise<Response>) {
  globalThis.fetch = fn as unknown as typeof fetch;
}

describe('fetchRemote', () => {
  test('a 200 parses into ready data', async () => {
    stubFetch(async () => new Response(JSON.stringify([1, 2, 3]), { status: 200 }));
    const res = await fetchRemote('/api/things', (raw) =>
      Array.isArray(raw) ? (raw as number[]) : [],
    );
    expect(res).toEqual({ kind: 'ready', data: [1, 2, 3] });
  });

  test('a non-ok response is failed with the status — not empty data', async () => {
    stubFetch(async () => new Response('nope', { status: 503 }));
    const res = await fetchRemote('/api/things', (raw) => raw);
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') {
      expect(res.error).toContain('503');
      expect(res.error).toContain('/api/things');
    }
  });

  // A REFUSAL IS NOT AN OUTAGE (backlog 7abb184f). /api/stations/load
  // answers a caller who may read no packets 403 with a sentence naming
  // the caller and why; this reader used to keep only the status, so
  // every page said the read failed and the reason was lost.
  test('a 403 is a refusal, and keeps the reason the server gave', async () => {
    const why = 'emp-x (role intern) may read no packets, so no station can be evaluated for them';
    stubFetch(async () => new Response(`${why}\n`, { status: 403 }));
    const res = await fetchRemote('/api/stations/load', (raw) => raw);
    expect(res).toEqual({ kind: 'failed', error: `/api/stations/load: HTTP 403: ${why}`, refused: true });
  });

  test('a 401 is a refusal too; one with no body says only its status', async () => {
    stubFetch(async () => new Response('', { status: 401 }));
    expect(await fetchRemote('/api/things', (raw) => raw)).toEqual({
      kind: 'failed', error: '/api/things: HTTP 401', refused: true,
    });
  });

  test('a long refusal body is cut, and says it was cut', async () => {
    stubFetch(async () => new Response('x'.repeat(1000), { status: 403 }));
    const res = await fetchRemote('/api/things', (raw) => raw);
    if (res.kind !== 'failed') throw new Error('expected failed');
    expect(res.error).toBe(`/api/things: HTTP 403: ${'x'.repeat(400)}… (600 more characters)`);
  });

  test('a 5xx is not a refusal', async () => {
    stubFetch(async () => new Response('upstream down', { status: 502 }));
    const res = await fetchRemote('/api/things', (raw) => raw);
    expect(res).toEqual({ kind: 'failed', error: '/api/things: HTTP 502' });
    if (res.kind !== 'failed') throw new Error('expected failed');
    expect(readFailed(res)).toBe('failed');
    expect(readFailed({ kind: 'failed', error: 'x', refused: true })).toBe('was refused');
  });

  test('a thrown fetch (network down) is failed, not an exception', async () => {
    stubFetch(async () => {
      throw new TypeError('Failed to fetch');
    });
    const res = await fetchRemote('/api/things', (raw) => raw);
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('Failed to fetch');
  });

  test('a parse that throws is failed — malformed data is not empty data', async () => {
    stubFetch(async () => new Response('"not-an-array"', { status: 200 }));
    const res = await fetchRemote('/api/things', (raw) => {
      if (!Array.isArray(raw)) throw new Error('expected an array');
      return raw;
    });
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('expected an array');
  });

  test('the result is assignable to Remote<T> state', async () => {
    stubFetch(async () => new Response('[]', { status: 200 }));
    // Compile-time shape check: a page stores the result directly.
    const state: Remote<unknown[]> = await fetchRemote('/api/things', (raw) =>
      Array.isArray(raw) ? raw : [],
    );
    expect(state.kind).toBe('ready');
  });
});
