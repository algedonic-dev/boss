// The SPA half of the surface-open record (backlog 628f182b): the
// pattern is the router's vocabulary with ids replaced, the recorder
// posts once per change of pattern, and a failed post is nobody's
// problem.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { QUERY_FIELDS, makeSurfaceOpenRecorder, openedPattern, routePattern, type SurfaceOpen } from './surface-opens';
import { ADDRESSES_LOADING, pagePath } from './page-address';
import { ROUTE_CATALOG, type NavItem } from './nav-catalog';
import { catalogPaths } from '../it/metrics/surfaces';
import { parseRoute } from '../router';
import { ADDRESSES } from '../../tests/fixtures/station-tree';

describe('routePattern', () => {
  test('a catalogued surface is its own path', () => {
    expect(routePattern({ kind: 'systemCodebase' }, '/it/codebase')).toBe('/it/codebase');
    expect(routePattern({ kind: 'systemYard' }, '/it')).toBe('/it');
    expect(routePattern({ kind: 'me' }, '/')).toBe('/');
  });

  test('the /dashboard mount and a trailing slash are not part of the pattern', () => {
    expect(routePattern({ kind: 'systemCodebase' }, '/dashboard/it/codebase/')).toBe('/it/codebase');
  });

  test('a detail page carries the field name, never the id', () => {
    const uuid = '0123abcd-4567-89ef-0123-456789abcdef';
    expect(routePattern({ kind: 'jobDetail', jobId: uuid }, `/ux/jobs/${uuid}`)).toBe('/ux/jobs/:jobId');
    expect(routePattern({ kind: 'invoice', invoiceId: 'inv-00001' }, '/finance/invoices/inv-00001')).toBe(
      '/finance/invoices/:invoiceId',
    );
    expect(
      routePattern({ kind: 'workflowDetail', kindSlug: 'seasonal-release' }, '/it/registry/seasonal-release'),
    ).toBe('/it/registry/:kindSlug');
  });

  test('two ids on one path are both replaced', () => {
    expect(
      routePattern({ kind: 'stepFocus', jobId: 'job-1', stepId: 'step-9' }, '/ux/jobs/job-1/steps/step-9'),
    ).toBe('/ux/jobs/:jobId/steps/:stepId');
  });

  test('an encoded id in the pathname still matches its decoded field', () => {
    expect(routePattern({ kind: 'vendor', vendorLookup: 'HOP 01' }, '/finance/vendors/HOP%2001')).toBe(
      '/finance/vendors/:vendorLookup',
    );
  });

  test('a query-string field never rewrites a static segment', () => {
    // `fromLabel: 'jobs'` is free text that happens to equal a segment.
    expect(
      routePattern(
        { kind: 'stepFocus', jobId: 'j', stepId: 's', from: '/it', fromLabel: 'jobs' },
        '/ux/jobs/j/steps/s',
      ),
    ).toBe('/ux/jobs/:jobId/steps/:stepId');
    expect(routePattern({ kind: 'search', q: 'search' }, '/ux/search')).toBe('/ux/search');  });

  test('an id that is a substring of a static segment is left alone', () => {
    expect(routePattern({ kind: 'invoice', invoiceId: 'invoice' }, '/finance/invoices/invoice')).toBe(
      '/finance/invoices/:invoiceId',
    );
  });

  test('the router and the pattern agree on every parsed route (CLAUDE.md 9a)', () => {
    // Round trip through the real parser: the pattern of a parsed
    // route never carries the id the parser extracted.
    for (const [path, expected] of [
      ['/ux/jobs/abc', '/ux/jobs/:jobId'],
      ['/finance/vendors/acme%20co', '/finance/vendors/:vendorLookup'],
      ['/finance/invoices/inv-1', '/finance/invoices/:invoiceId'],
      ['/finance/purchase-orders/po-7', '/finance/purchase-orders/:poId'],
      // Hosted pages at their stations' addresses (backlog 0bc2eb68).
      ['/engineering/registry/rules/some-rule', '/engineering/registry/rules/:ruleName'],
      ['/engineering/registry/step-plugins/review-design', '/engineering/registry/step-plugins/:pluginSlug'],
      ['/engineering/manual/intro', '/engineering/manual/:slug'],
      ['/operations/audit', '/operations/audit'],
    ] as const) {
      expect(routePattern(parseRoute(path, '', ADDRESSES), path)).toBe(expected);
    }
  });
});

// Review 7379181c F1 (backlog 0bc2eb68): the IT tab links to /it, which
// renders the alarms board in place, and the board's catalog address is
// /alarms. An open through the tab must count as an open of the board,
// or the never-opened list (catalog paths minus opened patterns) names
// the board a deletion candidate on every day it was reached by the tab.
describe('openedPattern — what an open is recorded under', () => {
  const board = pagePath(ROUTE_CATALOG['system-yard'] as NavItem, ADDRESSES);

  test('an open of /it is an open of the alarms board at its own address', () => {
    expect(board).toBe('/alarms');
    for (const [path, search] of [['/it', ''], ['/it', '?at=dock'], ['/dashboard/it/', '']] as const) {
      const recorded = openedPattern(parseRoute(path, search, ADDRESSES), path, ADDRESSES);
      expect({ path, recorded }).toEqual({ path, recorded: board });
    }
    // So the roster the never-opened list subtracts from holds it.
    expect(catalogPaths(ADDRESSES)).toContain(board!);
  });

  // The site's root and /ux render My Day in place, as /it renders the
  // board; My Day is the person's station's own page at /me (backlog
  // 5cb56bb3), so an open of either is an open of /me — and needs no
  // tree, because /me is the person's and not a position in it.
  test('an open of / or /ux is an open of the person’s station at /me', () => {
    const me = pagePath(ROUTE_CATALOG.me as NavItem, ADDRESSES_LOADING);
    expect(me).toBe('/me');
    for (const path of ['/', '/ux', '/dashboard/', '/me']) {
      for (const at of [ADDRESSES_LOADING, ADDRESSES]) {
        expect({ path, recorded: openedPattern(parseRoute(path, '', at), path, at) }).toEqual({ path, recorded: me });
      }
    }
    expect(catalogPaths(ADDRESSES_LOADING)).toContain(me!);
  });

  test('until the tree places the board, the open waits rather than record a guess', () => {
    expect(openedPattern(parseRoute('/it', ''), '/it', ADDRESSES_LOADING)).toBeNull();
  });

  test('every other address records its own pattern', () => {
    for (const [path, expected] of [
      ['/alarms', '/alarms'],
      ['/finance', '/finance'],
      // A department with no landing page: its jobs view, as before.
      ['/sales', '/:code'],
      ['/ux/jobs/abc', '/ux/jobs/:jobId'],
      ['/engineering/registry/rules/r-1', '/engineering/registry/rules/:ruleName'],
    ] as const) {
      expect({ path, recorded: openedPattern(parseRoute(path, '', ADDRESSES), path, ADDRESSES) }).toEqual({ path, recorded: expected });
    }
  });
});

describe('QUERY_FIELDS', () => {
  test('names every field router.ts reads off the query string', () => {
    // Every `(r as { NAME?: string }).NAME = …` assignment in router.ts is a
    // query-string field, plus search's `q`. A new one that is not in
    // QUERY_FIELDS would be treated as a path id, so this names it.
    const src = readFileSync(new URL('../router.ts', import.meta.url).pathname, 'utf8');
    const fromSource = new Set<string>(['q']);
    for (const m of src.matchAll(/\(r as \{ (\w+)\?: string \}\)/g)) fromSource.add(m[1]!);
    expect([...fromSource].sort()).toEqual([...QUERY_FIELDS].sort());
  });
});

describe('makeSurfaceOpenRecorder', () => {
  test('posts once per change of pattern, with the client clock', async () => {
    const posted: SurfaceOpen[] = [];
    const clock = new Date('2026-09-16T12:00:00Z');
    const record = makeSurfaceOpenRecorder(
      async (o) => {
        posted.push(o);
      },
      () => clock,
    );
    record('/it');
    record('/it'); // the same surface re-parsed: not a second open
    record('/it/codebase');
    record('/it'); // away and back: a new open
    expect(posted).toEqual([
      { route: '/it', at: '2026-09-16T12:00:00.000Z' },
      { route: '/it/codebase', at: '2026-09-16T12:00:00.000Z' },
      { route: '/it', at: '2026-09-16T12:00:00.000Z' },
    ]);
  });

  test('a failed post is swallowed and the next navigation still records', async () => {
    let calls = 0;
    const record = makeSurfaceOpenRecorder(async () => {
      calls += 1;
      throw new Error('502 upstream unavailable');
    });
    expect(() => record('/it')).not.toThrow();
    expect(() => record('/it/codebase')).not.toThrow();
    await Promise.resolve();
    expect(calls).toBe(2);
  });
});
