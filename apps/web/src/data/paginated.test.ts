import { afterEach, describe, it, expect } from 'bun:test';
import { fetchPaged, normalise, isCapped, fetchEvery, wholeOrThrow } from './paginated';

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

  // Backlog 0ef5e008. A 200 that is not the envelope is not a read that
  // said "no rows": `normalise` turned it into an empty page, and the
  // page painted it as an empty source — or, before that car, as "No X
  // match those filters." It is a failed read naming what came back.
  it.each([
    ['a bare list', [1, 2], 'the body is a list'],
    ['an object with no data list', { rows: [] }, 'the body is an object with no data list'],
    ['null', null, 'the body is null'],
  ])('a 200 body that is %s is failed, never an empty page', async (_, body, what) => {
    stubFetch(async () => new Response(JSON.stringify(body), { status: 200 }));
    const res = await fetchPaged<number>('/api/things');
    expect(res).toEqual({
      kind: 'failed',
      error: `/api/things: HTTP 200, but ${what}, not a {data: [...]} envelope`,
    });
  });

  it('a 200 envelope with no rows is the one empty page', async () => {
    stubFetch(async () => new Response(JSON.stringify({ data: [], total: 0 }), { status: 200 }));
    const res = await fetchPaged<number>('/api/things');
    expect(res).toEqual({ kind: 'ready', page: { data: [], total: 0, limit: 0, offset: 0 } });
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

// EVERY ROW, OR A STATEMENT THAT IT STOPPED (backlog b68a9dde). Seven
// pages read `/api/jobs?…&limit=200` once and kept the page; two of
// them lost rows on 2026-09-27 (ship-a-change had 978 packets, open
// backlog-items 361). `fetchEvery` pages by offset until the rows it
// holds reach the `total` the server reports, or answers `truncated`
// naming how far it got — never a smaller list dressed as the whole.
type Row = { id: string };

/** A server holding `n` rows, answering `limit`/`offset` like the
 *  jobs API does, and recording every URL it was asked for. */
function serverOf(n: number, asked: string[] = []): () => Promise<Response> {
  const rows: Row[] = Array.from({ length: n }, (_, i) => ({ id: `r${i}` }));
  return (async (input: RequestInfo | URL) => {
    const url = String(input);
    asked.push(url);
    const q = new URL(url, 'http://x').searchParams;
    const limit = Number(q.get('limit'));
    const offset = Number(q.get('offset'));
    return new Response(
      JSON.stringify({ data: rows.slice(offset, offset + limit), total: n, limit, offset }),
      { status: 200 },
    );
  }) as unknown as () => Promise<Response>;
}

describe('fetchEvery', () => {
  it('reads page after page until the rows held reach the total', async () => {
    const asked: string[] = [];
    stubFetch(serverOf(5, asked));
    const res = await fetchEvery<Row>('/api/jobs?kind=k&status=open', { page: 2 });
    expect(res.kind).toBe('ready');
    if (res.kind === 'ready') {
      expect(res.data.map((r) => r.id)).toEqual(['r0', 'r1', 'r2', 'r3', 'r4']);
      expect(res.total).toBe(5);
    }
    // The caller's filter survives in its own order; the helper owns
    // limit and offset.
    expect(asked).toEqual([
      '/api/jobs?kind=k&status=open&limit=2&offset=0',
      '/api/jobs?kind=k&status=open&limit=2&offset=2',
      '/api/jobs?kind=k&status=open&limit=2&offset=4',
    ]);
  });

  it('an answer inside one page is one read, at the jobs API page size', async () => {
    const asked: string[] = [];
    stubFetch(serverOf(3, asked));
    const res = await fetchEvery<Row>('/api/jobs?kind=k');
    expect(res.kind).toBe('ready');
    expect(asked).toEqual(['/api/jobs?kind=k&limit=1000&offset=0']);
  });

  it('a failed page fails the read — a part is never answered as the whole', async () => {
    let n = 0;
    stubFetch(async () => {
      n += 1;
      return n === 1
        ? new Response(JSON.stringify({ data: [{ id: 'a' }], total: 2 }), { status: 200 })
        : new Response('down', { status: 503 });
    });
    const res = await fetchEvery<Row>('/api/jobs?kind=k', { page: 1 });
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('503');
  });

  it('stops at its ceiling and says so, with how many it holds of how many', async () => {
    stubFetch(serverOf(10));
    const res = await fetchEvery<Row>('/api/jobs?kind=k', { page: 2, ceiling: 4 });
    expect(res.kind).toBe('truncated');
    if (res.kind === 'truncated') {
      expect(res.data.length).toBe(4);
      expect(res.total).toBe(10);
    }
    expect(() => wholeOrThrow(res)).toThrow(/4 of 10/);
  });

  it('a server that ignores offset is truncated, not a loop', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      return new Response(JSON.stringify({ data: [{ id: 'a' }, { id: 'b' }], total: 9 }), {
        status: 200,
      });
    });
    const res = await fetchEvery<Row>('/api/jobs?kind=k', { page: 2 });
    expect(res.kind).toBe('truncated');
    expect(calls).toBe(2);
  });

  it('refuses a URL that already names a limit or an offset', async () => {
    stubFetch(serverOf(1));
    const res = await fetchEvery<Row>('/api/jobs?kind=k&limit=200');
    expect(res.kind).toBe('failed');
  });

  it('an envelope under a wrong key fails rather than reading as empty', async () => {
    stubFetch(async () => new Response(JSON.stringify({ jobs: [{ id: 'a' }] }), { status: 200 }));
    const res = await fetchEvery<Row>('/api/jobs?kind=k');
    expect(res.kind).toBe('failed');
  });

  it('wholeOrThrow hands back the rows of a whole read', async () => {
    stubFetch(serverOf(3));
    const rows = wholeOrThrow(await fetchEvery<Row>('/api/jobs?kind=k'));
    expect(rows.length).toBe(3);
  });
});
