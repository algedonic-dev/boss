import { expect, test } from 'bun:test';
import { fetchRegions } from './regions';
import { fetchBorders } from './borders';

test('a cold map building response stays loading, then publishes the real reading', async () => {
  const original = globalThis.fetch;
  const respond = (body: unknown, status = 200) => {
    globalThis.fetch = Object.assign(async () => Response.json(body, { status }), { preconnect: original.preconnect });
  };
  try {
    respond({ state: 'building', why: 'building first reading', map_as_of: null }, 202);
    expect(await fetchRegions()).toEqual({ kind: 'loading' });
    expect(await fetchBorders()).toEqual({ kind: 'loading' });
    respond({ regions: [], window_hours: 24, now: '2026-10-23T00:00:00Z' });
    expect((await fetchRegions()).kind).toBe('ready');
    respond({ borders: [], window_hours: 24, now: '2026-10-23T00:00:00Z' });
    expect((await fetchBorders()).kind).toBe('ready');
    respond({ why: 'storage refused' }, 503);
    expect((await fetchRegions()).kind).toBe('failed');
    expect((await fetchBorders()).kind).toBe('failed');
    respond({}, 202);
    expect((await fetchRegions()).kind).toBe('failed');
    expect((await fetchBorders()).kind).toBe('failed');
  } finally { globalThis.fetch = original; }
});
