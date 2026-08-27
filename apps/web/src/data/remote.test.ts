// The false-empty class (packet 3fba9c35): a failed fetch must come
// back as a FAILED state carrying the error — never as data that
// happens to be empty. Remote<T> is the house discriminated union;
// fetchRemote can only resolve ready or failed, so a surface that
// stores its result cannot render "loaded and truly empty" for an
// outage without choosing to.

import { afterEach, describe, expect, test } from 'bun:test';
import { fetchRemote, type Remote } from './remote';

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
