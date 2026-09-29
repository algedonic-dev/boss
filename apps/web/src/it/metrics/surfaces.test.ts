// The reading half of the surface-open record (backlog 628f182b):
// every number on the Surfaces section is a pure function of the
// roll-up rows, and the never-opened list is the catalog minus them.

import { afterEach, describe, expect, test } from 'bun:test';
import {
  catalogPaths,
  loadSurfaceRollup,
  neverOpened,
  parseRollup,
  perActor,
  uncatalogued,
  windowFor,
  type RouteCount,
} from './surfaces';
import { ROUTE_CATALOG } from '../../shell/nav-catalog';

const row = (actor_id: string, route: string, opens: number, last_at = '2026-09-16T12:00:00Z'): RouteCount => ({
  actor_id,
  route,
  opens,
  last_at,
});

describe('parseRollup', () => {
  test('reads the envelope and drops a row that is not a count', () => {
    const r = parseRollup({
      since: '2026-09-09T00:00:00Z',
      until: '2026-09-16T00:00:00Z',
      rows: [
        { actor_id: 'emp-david', route: '/it', opens: 4, last_at: '2026-09-16T11:00:00Z' },
        { actor_id: 'emp-david', route: '/it/codebase', opens: 'four' },
        { route: '/it/kb', opens: 1 },
      ],
    });
    expect(r.since).toBe('2026-09-09T00:00:00Z');
    expect(r.rows).toEqual([row('emp-david', '/it', 4, '2026-09-16T11:00:00Z')]);
  });

  test("a mock's bare array is an empty roll-up with no window, not a crash", () => {
    expect(parseRollup([])).toEqual({ since: null, until: null, rows: [] });
  });

  // b64b3c04 (page audit f82b05a9, gap 3): an object with no `rows` used
  // to read as zero rows — "No surface open is recorded" — for a body
  // the page did not understand. It throws, so fetchRemote says failed.
  test('an unrecognised envelope throws, naming what it got, rather than reading as a quiet week', () => {
    for (const body of [null, 'x', 7, {}, { data: [] }, { rows: 'x' }]) {
      expect(() => parseRollup(body), JSON.stringify(body)).toThrow(/unrecognised .* envelope/);
    }
  });
});

describe('loadSurfaceRollup', () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  test('a 200 with an envelope it does not know is a failed read, with the reason', async () => {
    globalThis.fetch = (async () => new Response(JSON.stringify({ data: [] }), { status: 200 })) as unknown as typeof fetch;
    const r = await loadSurfaceRollup(7, new Date('2026-09-27T12:00:00Z'));
    expect(r.kind).toBe('failed');
    if (r.kind === 'failed') expect(r.error).toMatch(/unrecognised .* envelope/);
  });
});

describe('windowFor', () => {
  test('the last N days ending now', () => {
    const w = windowFor(7, new Date('2026-09-16T12:00:00Z'));
    expect(w).toEqual({ since: '2026-09-09T12:00:00.000Z', until: '2026-09-16T12:00:00.000Z' });
  });
});

describe('perActor', () => {
  test('folds opens and distinct routes per actor, busiest first, routes most-opened first', () => {
    const usage = perActor([
      row('emp-032', '/ux/me', 2),
      row('emp-david', '/it/codebase', 3),
      row('emp-david', '/it', 5),
      row('emp-david', '/ux/jobs/:jobId', 3),
    ]);
    expect(usage.map((u) => u.actor_id)).toEqual(['emp-david', 'emp-032']);
    expect(usage[0]).toEqual({
      actor_id: 'emp-david',
      opens: 11,
      distinct: 3,
      routes: [
        { route: '/it', opens: 5, last_at: '2026-09-16T12:00:00Z' },
        { route: '/it/codebase', opens: 3, last_at: '2026-09-16T12:00:00Z' },
        { route: '/ux/jobs/:jobId', opens: 3, last_at: '2026-09-16T12:00:00Z' },
      ],
    });
    expect(usage[1]!.distinct).toBe(1);
  });

  test('no rows is no actors', () => {
    expect(perActor([])).toEqual([]);
  });
});

describe('neverOpened', () => {
  test('the roster minus what any actor opened, in roster order', () => {
    const roster = ['/it', '/it/codebase', '/it/kb', '/ux/me'];
    expect(neverOpened([row('emp-david', '/it', 1), row('emp-032', '/ux/me', 1)], roster)).toEqual([
      '/it/codebase',
      '/it/kb',
    ]);
  });

  test('with no rows every catalogued surface is a candidate — and the page says why', () => {
    expect(neverOpened([], catalogPaths())).toEqual(catalogPaths());
  });

  // 13ded76c part (a): the roster is the nav catalog, so a route the
  // router serves and the catalog omits can never be a candidate. The
  // section lists the ones opened this week on their own rather than
  // letting the scope pass for the whole app.
  test('the opened routes the roster does not hold, once each, sorted', () => {
    const roster = ['/it', '/it/codebase'];
    const rows = [
      row('emp-david', '/it', 3),
      row('emp-david', '/ux/jobs/:jobId', 2),
      row('emp-032', '/ux/jobs/:jobId', 1),
      row('emp-032', '/audit', 1),
    ];
    expect(uncatalogued(rows, roster)).toEqual(['/audit', '/ux/jobs/:jobId']);
    expect(uncatalogued([], roster)).toEqual([]);
  });

  test('the roster is every catalog path once (CLAUDE.md 9a: one catalog)', () => {
    const paths = catalogPaths();
    expect(new Set(paths).size).toBe(paths.length);
    for (const e of Object.values(ROUTE_CATALOG)) expect(paths).toContain(e.path);
  });
});
