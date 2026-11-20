// A page's URL is its host station's address in the tree plus its slug
// (backlog 0bc2eb68; the routes decision of design deba3000, David
// 2026-11-10). The tree here is the shared fixture's — the server's
// read, standing in — whose hosts carry the bundle's ids, so every
// assertion is the derivation the SPA makes, against the shape the
// server serves.

import { describe, expect, test } from 'bun:test';
import { ROUTE_CATALOG, STATION, unhostedPages, type NavItem } from './nav-catalog';
import {
  ADDRESSES_LOADING,
  addressesOf,
  fillPath,
  matchPath,
  pagePath,
  stationAtPath,
  type StationAddresses,
} from './page-address';
import { readStationAddresses } from './stationAddresses.svelte';
import { parseTree } from '../it/tree/station-tree';
import { TREE_ROWS, stationList, treeRead } from '../../tests/fixtures/station-tree';
import { hostedPatterns, parseRoute, type Route } from '../router';
import { readFinanceView } from '../finance/financeQuery';
import { withRoster } from './sections';

const AT = addressesOf(parseTree(treeRead()));
const entries = Object.entries(ROUTE_CATALOG) as ReadonlyArray<[string, NavItem]>;
const pathOf = (id: keyof typeof ROUTE_CATALOG, at: StationAddresses = AT) => pagePath(ROUTE_CATALOG[id] as NavItem, at);

describe("a station's prefix is its address with the root's own segment dropped", () => {
  test('the root is the top level, a department its name, a station below it both', () => {
    expect(AT.kind).toBe('ready');
    if (AT.kind !== 'ready') return;
    const prefix = new Map(AT.stations.map((s) => [s.name, s.prefix]));
    // The fixture's root is named `company`, not the bundle's name: the
    // prefix is found by the tree's shape, never by a name.
    expect(prefix.get('company')).toBe('');
    expect(prefix.get('engineering')).toBe('/engineering');
    expect(prefix.get('design')).toBe('/engineering/design');
    expect(prefix.get('estate')).toBe('/operations/estate');
  });

  test('a read under any station but the root is refused, never read as the company', () => {
    expect(addressesOf(parseTree(treeRead('engineering'))).kind).toBe('failed');
  });
});

// THE URL SCHEME, as the shipped tree places it. Every hosted page is
// here, so a slug edit is a reviewed line. The person's own pages sit
// at /me (backlog 5cb56bb3); no page is unhosted since the Churn
// watchlist moved to the root at the same address.
const SCHEME: Readonly<Record<string, string>> = {
  finance: '/finance',
  vendors: '/finance/vendors',
  'purchase-order': '/finance/purchase-orders/:poId',
  'vendor-invoice': '/finance/vendor-invoices/:vendorInvoiceId',
  manual: '/engineering/manual',
  'system-yard': '/alarms',
  'system-monitoring': '/operations/audit',
  'system-performance': '/operations/perf',
  workflows: '/engineering/registry',
  'system-step-plugins': '/engineering/registry/step-plugins',
  'system-dispatcher': '/engineering/registry/dispatcher',
  'system-subjects': '/engineering/registry/subjects',
  'system-registry-drift': '/engineering/registry/drift',
  'system-agents': '/engineering/registry/agents',
  'system-credentials': '/operations/vault/credentials',
  'system-dispatcher-rules': '/engineering/registry/rules',
  'system-dispatcher-rule': '/engineering/registry/rules/:ruleName',
  'system-design': '/engineering/design',
  'system-codebase': '/engineering/codebase',
  'system-estate': '/operations/estate',
  'system-kb': '/engineering/kb',
  'auth-admin': '/operations/vault/auth-admin',
  // The signed-in person's own station (design 01fa5bdf step 3, backlog
  // 5cb56bb3): My Day is its page, the inbox and the calendar are below.
  me: '/me',
  schedule: '/me/calendar',
  inbox: '/me/inbox',
  // The Churn watchlist, hosted at the company root (backlog 5cb56bb3):
  // a company-wide accounts list and not the person's watchlist (that
  // one is My Day's, src/me/watchlist.ts). At the root its path is its
  // slug alone, so its address did not change.
  accounts: '/watchlist',
};

describe('every catalogued page has one address, derived from its host', () => {
  test('the scheme, page by page', () => {
    expect(Object.fromEntries(entries.map(([id, e]) => [id, pagePath(e, AT)]))).toEqual(SCHEME);
  });

  test('no hosted page spells a path: the address is its host station plus its slug', () => {
    for (const [id, e] of entries) {
      if (e.host === undefined) continue;
      expect([id, 'path' in e]).toEqual([id, false]);
      // A slug is below the station, never a second absolute path.
      expect([id, e.slug.startsWith('/')]).toEqual([id, false]);
    }
  });

  test('no two pages share an address, and no root page takes a top-level station name', () => {
    const paths = entries.map(([, e]) => pagePath(e, AT));
    expect(new Set(paths).size).toBe(paths.length);
    const tops = new Set(TREE_ROWS.filter((r) => r.parent === 'company').map((r) => r.name));
    for (const [id, e] of entries) {
      if (e.host !== STATION.algedonic) continue;
      expect([id, e.slug !== '' && !tops.has(e.slug.split('/')[0]!)]).toEqual([id, true]);
    }
  });

  test('until the tree answers, a hosted page has no address — never a guessed prefix', () => {
    expect(pathOf('workflows', ADDRESSES_LOADING)).toBeNull();
    expect(pathOf('workflows', { kind: 'failed', why: 'down' })).toBeNull();
    // A root-hosted page too: the root's address is the tree's answer,
    // like any station's (the Churn watchlist spelled /watchlist until
    // backlog 5cb56bb3 hosted it there).
    expect(pathOf('accounts', ADDRESSES_LOADING)).toBeNull();
    expect(parseRoute('/watchlist', '', ADDRESSES_LOADING)).toEqual({ kind: 'resolving', path: '/watchlist' });
    expect(parseRoute('/watchlist', '', AT)).toEqual({ kind: 'watchlist' });
  });

  test('no catalogued page is left unhosted (backlog 5cb56bb3)', () => {
    // The Churn watchlist was the last: the design's table sent
    // "/watchlist" to /me by its name, but it is every account's risk
    // score, company-wide, and David kept it on the company's own
    // instance (design 922f37be), so it is the root's — "anything
    // company-wide goes to the root" (design 01fa5bdf).
    expect(ROUTE_CATALOG.accounts.host).toBe(STATION.algedonic);
    expect(unhostedPages().map((e) => e.id)).toEqual([]);
  });

  test('a host the tree does not hold has no address', () => {
    const without = addressesOf(parseTree(treeRead('company', TREE_ROWS.filter((r) => r.name !== 'vault'))));
    expect(pathOf('system-credentials', without)).toBeNull();
    expect(pathOf('workflows', without)).toBe('/engineering/registry');
  });
});

// THE PERSON'S OWN STATION (design 01fa5bdf step 3, David 2026-11-17:
// "every human executor has a station of their own at /me, resolved from
// the signed-in user"; backlog 5cb56bb3). Its address is the same for
// every person, because whose station it is comes from the session and
// never from the URL. And it is not a position in the company tree, so a
// tree that has not answered, or could not be read, never takes a
// person's own inbox away from them.
describe('the person’s own station is /me, and it needs no tree', () => {
  const personal = entries.filter(([, e]) => e.person === true);

  test('My Day, the inbox and the calendar are the person’s pages', () => {
    expect(personal.map(([id]) => id).sort()).toEqual(['inbox', 'me', 'schedule']);
    // Nothing else spells an address under /me: a page there is the
    // person's, by the address alone.
    for (const [id, e] of entries) {
      const under = e.path !== undefined && (e.path === '/me' || e.path.startsWith('/me/'));
      expect([id, under]).toEqual([id, e.person === true]);
    }
  });

  test('each answers at its address whether the tree is unread, failed or read', () => {
    const want: ReadonlyArray<readonly [string, Route]> = [
      ['/me', { kind: 'me' }],
      ['/me/inbox', { kind: 'inbox' }],
      ['/me/calendar', { kind: 'myCalendar' }],
    ];
    for (const at of [ADDRESSES_LOADING, { kind: 'failed', why: 'down' } as StationAddresses, AT]) {
      for (const [id] of personal) expect([id, pathOf(id as keyof typeof ROUTE_CATALOG, at)]).toEqual([id, SCHEME[id] ?? 'not in the scheme']);
      for (const [path, route] of want) expect([at.kind, path, parseRoute(path, '', at)]).toEqual([at.kind, path, route]);
    }
  });

  test('a person’s page is not an unhosted page: it is hosted at the person', () => {
    const unhosted = unhostedPages().map((e) => e.id);
    for (const [id] of personal) expect([id, unhosted.includes(id)]).toEqual([id, false]);
  });

  // The spellings these pages had before (David, 2026-09-25: no shims
  // before 1.0.0). /inbox is one bare segment, a department's address
  // first, so the registry judges it, as it judges /manual.
  test('the old spellings answer nothing', () => {
    for (const path of ['/ux/me', '/ux/inbox', '/ux/calendar/me', '/calendar/me', '/ux/me/inbox']) {
      expect(parseRoute(path, '', AT)).toEqual({ kind: 'notFound', path });
    }
    const roster = [{ code: 'finance', label: 'Finance' }] as unknown as Parameters<typeof withRoster>[2];
    expect(withRoster(parseRoute('/inbox', '', AT), '/inbox', roster, AT).kind).toBe('notFound');
  });
});

describe('a re-parented station moves its pages, and the old address answers nothing', () => {
  // Engineering moved under Operations: one field on one row
  // (station-tree.md §8 rule 2), and no edit to the catalog.
  const moved = TREE_ROWS.map((r) => (r.name === 'engineering' ? { ...r, parent: 'operations' } : r));
  const AFTER = addressesOf(parseTree(treeRead('company', moved)));

  test('the nav reads the new address', () => {
    expect(pathOf('workflows', AFTER)).toBe('/operations/engineering/registry');
    expect(pathOf('system-design', AFTER)).toBe('/operations/engineering/design');
    // A page hosted elsewhere does not move.
    expect(pathOf('system-estate', AFTER)).toBe('/operations/estate');
  });

  test('the router resolves the new address, and the old one is not an alias', () => {
    expect(parseRoute('/operations/engineering/registry', '', AFTER)).toEqual({ kind: 'workflows' });
    expect(parseRoute('/operations/engineering/registry/rules/r-1', '', AFTER)).toEqual({ kind: 'dispatcherRuleEdit', ruleName: 'r-1' });
    expect(parseRoute('/operations/engineering', '', AFTER)).toEqual({ kind: 'systemStation', station: 'engineering' });
    expect(parseRoute('/engineering/gates', '', AFTER)).toEqual({ kind: 'notFound', path: '/engineering/gates' });
    expect(parseRoute('/engineering/registry', '', AFTER)).toEqual({ kind: 'notFound', path: '/engineering/registry' });
  });
});

describe('every hosted page parses back to itself at its address', () => {
  const sample: Readonly<Record<string, string>> = {
    poId: 'po-1', vendorInvoiceId: 'vi-1', ruleName: 'r-1', jobId: 'j-1', pluginSlug: 'p-1',
    invoiceId: 'inv-1', vendorLookup: 'v-1', slug: 'a/b', kindSlug: 'k-1',
  };
  test('each row of the router table, its parameters filled', () => {
    const seen = hostedPatterns(AT).map(({ page, pattern }) => {
      const path = pattern.replace(/[:*]([A-Za-z]+)/g, (_, n: string) => sample[n] ?? n);
      return [page, path, parseRoute(path, '', AT).kind] as const;
    });
    // None falls to not-found or to another page's route: a row that is
    // eclipsed by an earlier one parses to that row's kind instead.
    for (const [page, path, kind] of seen) {
      expect([page, path, kind === 'notFound' || kind === 'resolving']).toEqual([page, path, false]);
    }
    const kinds = seen.map(([, , k]) => k);
    expect(new Set(kinds).size).toBe(kinds.length);
  });
});

describe('a path only the tree can place says so until the tree answers', () => {
  test('loading: resolving, naming the path', () => {
    expect(parseRoute('/engineering/registry')).toEqual({ kind: 'resolving', path: '/engineering/registry' });
    expect(parseRoute('/engineering/gates')).toEqual({ kind: 'resolving', path: '/engineering/gates' });
  });

  test('failed: not found, saying why; a bare segment is still a department', () => {
    const down: StationAddresses = { kind: 'failed', why: 'GET /api/stations answered 503' };
    expect(parseRoute('/engineering/registry', '', down)).toEqual({
      kind: 'notFound',
      path: '/engineering/registry',
      why: 'the station tree could not be read: GET /api/stations answered 503',
    });
    expect(parseRoute('/engineering', '', down)).toEqual({ kind: 'department', code: 'engineering' });
    expect(parseRoute('/engineering/gates', '', down).kind).toBe('notFound');
  });

  test("a department's landing needs no tree: /finance and /it render with it unread", () => {
    const yard: Route = { kind: 'systemYard', at: 'dock' };
    expect(parseRoute('/finance')).toEqual({ kind: 'finance', view: readFinanceView('') });
    expect(parseRoute('/it', '?at=dock')).toEqual(yard);
    expect(parseRoute('/it', '?at=dock', AT)).toEqual(yard);
  });
});

describe("a station's own address is its page where no page is hosted there", () => {
  test('a room inside a department', () => {
    expect(parseRoute('/engineering/gates', '', AT)).toEqual({ kind: 'systemStation', station: 'gates' });
    expect(parseRoute('/operations/vault', '', AT)).toEqual({ kind: 'systemStation', station: 'vault' });
  });

  // A top-level station's address is one segment, which is a department's
  // address first; the registry, not the router, says which it is.
  test('a top-level station: its page where the registry has no such department, else the department', () => {
    const route = parseRoute('/engineering', '', AT);
    expect(route).toEqual({ kind: 'department', code: 'engineering' });
    const roster = [{ code: 'executive', label: 'Executive' }] as unknown as Parameters<typeof withRoster>[2];
    expect(withRoster(route, '/engineering', roster, AT)).toEqual({ kind: 'systemStation', station: 'engineering' });
    expect(withRoster(parseRoute('/executive', '', AT), '/executive', roster, AT)).toEqual({ kind: 'department', code: 'executive' });
    // Not a station either: the not-found page, naming the department.
    expect(withRoster(parseRoute('/nowhere', '', AT), '/nowhere', roster, AT)).toEqual({
      kind: 'notFound', path: '/nowhere', department: 'nowhere',
    });
    // Its jobs view is the department's alone.
    expect(withRoster(parseRoute('/engineering/jobs', '', AT), '/engineering/jobs', roster, AT).kind).toBe('notFound');
  });
  test('a hosted page at a station address wins: /engineering/design is the design reviews', () => {
    expect(parseRoute('/engineering/design', '', AT)).toEqual({ kind: 'systemDesign' });
    expect(stationAtPath(AT, '/engineering/design')?.name).toBe('design');
  });
});

describe('the path helpers', () => {
  test('a pattern fills one encoded segment per parameter, and refuses a missing one', () => {
    expect(fillPath('/finance/purchase-orders/:poId', { poId: 'a/b c' })).toBe('/finance/purchase-orders/a%2Fb%20c');
    expect(fillPath('/finance/purchase-orders/:poId', {})).toBeNull();
  });
  test('a match decodes its segments, and a trailing star takes the rest', () => {
    expect(matchPath('/x/:id', '/x/a%2Fb')).toEqual({ id: 'a/b' });
    expect(matchPath('/x/:id', '/x/a/b')).toBeNull();
    expect(matchPath('/x/*slug', '/x/a/b')).toEqual({ slug: 'a/b' });
    expect(matchPath('/x/*slug', '/x')).toBeNull();
    expect(matchPath('/x/:id', '/x/%E0%A4%A')).toEqual({ id: '%E0%A4%A' });
  });
});

describe('the SPA reads the tree the way the floor does: the rows, then under the root', () => {
  const answer = (body: unknown, status = 200) => Promise.resolve(new Response(JSON.stringify(body), { status }));

  test('ready, from the two reads', async () => {
    const asked: string[] = [];
    const at = await readStationAddresses((p) => {
      asked.push(p);
      return p === '/api/stations' ? answer(stationList()) : answer(treeRead());
    });
    expect(asked).toEqual(['/api/stations', `/api/stations?under=${encodeURIComponent(STATION.algedonic)}`]);
    expect(at).toEqual(AT);
  });

  test('each failure names itself', async () => {
    expect(await readStationAddresses(() => answer('no', 403))).toEqual({ kind: 'failed', why: 'GET /api/stations answered 403' });
    expect(await readStationAddresses(() => answer({ data: [] }))).toEqual({ kind: 'failed', why: 'the station rows name no single root' });
    expect(
      await readStationAddresses((p) => (p === '/api/stations' ? answer(stationList()) : answer('dark', 503))),
    ).toEqual({ kind: 'failed', why: 'the tree read under the root answered 503' });
    expect((await readStationAddresses(() => Promise.reject(new Error('offline')))).kind).toBe('failed');
  });
});
