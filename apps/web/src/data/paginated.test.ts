import { afterEach, describe, it, expect } from 'bun:test';
import { fetchPaged, normalise, isCapped } from './paginated';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: () => Promise<Response>) {
  globalThis.fetch = fn as unknown as typeof fetch;
}

// The false-empty class (packet 3fba9c35): fetchPaged used to return
// `null` for both a non-ok response and a network fault, and every
// caller `?? []`-ed it — so an outage rendered as an empty list. The
// result is now a discriminated union the caller must branch on.
describe('fetchPaged', () => {
  it('a 200 envelope is ready with the page', async () => {
    stubFetch(async () =>
      new Response(JSON.stringify({ data: [1, 2], total: 2, limit: 10, offset: 0 }), {
        status: 200,
      }),
    );
    const res = await fetchPaged<number>('/api/things');
    expect(res).toEqual({
      kind: 'ready',
      page: { data: [1, 2], total: 2, limit: 10, offset: 0 },
    });
  });

  it('a non-ok response is failed with the status — never an empty page', async () => {
    stubFetch(async () => new Response('down', { status: 502 }));
    const res = await fetchPaged<number>('/api/things');
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('502');
  });

  it('a thrown fetch is failed, not an exception and not empty', async () => {
    stubFetch(async () => {
      throw new TypeError('Failed to fetch');
    });
    const res = await fetchPaged<number>('/api/things');
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('Failed to fetch');
  });
});

describe('normalise', () => {
  it('reads the standard envelope shape', () => {
    const p = normalise<number>({ data: [1, 2, 3], total: 50, limit: 3, offset: 6 });
    expect(p.data).toEqual([1, 2, 3]);
    expect(p.total).toBe(50);
    expect(p.limit).toBe(3);
    expect(p.offset).toBe(6);
  });

  it('rejects bare arrays — every list endpoint returns the envelope', () => {
    // A bare-array response is a contract violation; surfacing it as
    // an empty page makes the regression visible instead of silently
    // presenting an uncapped list.
    const p = normalise<number>([1, 2, 3]);
    expect(p).toEqual({ data: [], total: 0, limit: 0, offset: 0 });
  });

  it('treats missing total as data.length so callers do not see fake caps', () => {
    const p = normalise<number>({ data: [1, 2, 3] });
    expect(p.total).toBe(3);
  });

  it('handles non-object bodies as empty', () => {
    expect(normalise<number>(null)).toEqual({ data: [], total: 0, limit: 0, offset: 0 });
    expect(normalise<number>('not-json' as unknown)).toEqual({
      data: [],
      total: 0,
      limit: 0,
      offset: 0,
    });
  });
});

describe('isCapped', () => {
  it('returns true when total exceeds the returned page', () => {
    expect(isCapped({ data: [1, 2, 3], total: 50, limit: 3, offset: 0 })).toBe(true);
  });

  it('returns false when total equals the returned page', () => {
    expect(isCapped({ data: [1, 2, 3], total: 3, limit: 3, offset: 0 })).toBe(false);
  });

  it('returns false for null', () => {
    expect(isCapped(null)).toBe(false);
  });
});
