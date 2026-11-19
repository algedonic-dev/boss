// The route catalog pin — every entityHref kind must land somewhere.
//
// `entityHref` and `parseRoute` are two statements of one fact: the set
// of entity paths this app can produce and the set it can resolve
// (CLAUDE.md 9a). They drifted. `entityHref('ticket', id)` produced
// /ux/support/<id> and the router had only `p === '/support'`, so every
// ticket link anywhere in the app fell through to the catch-all — a
// link that cannot land, which reads to the operator as bad data rather
// than a missing route (backlog 38d4e458, from the /ux/support page
// audit 9876ef0d). Two siblings failed the same way, and worse, because
// a greedy wildcard ATE them: 'agreement' resolved to the account page
// with accountId 'agreements/<id>', and 'opportunity' to the job page
// with jobId 'opportunities/<id>'. A fourth, 'marketing-asset', resolved
// to the device-asset page instead of its own.
//
// So the pin asserts the mapping, not merely non-emptiness: a kind whose
// href resolves to the WRONG page is the failure that hides.
//
// Run via `bun test`.

import { readFileSync } from 'node:fs';
import { beforeAll, describe, expect, test } from 'bun:test';
import {
  ENTITY_KINDS,
  PAGE_KINDS,
  PAGELESS_KINDS,
  entityHref,
  hasPage,
  type EntityKind,
} from '@boss/web-kit/ui/entity-href';
import { parseRoute, type Route } from './router';

// `href` reads window.location.pathname (the /dashboard mount probe),
// so stub that one field for bun's non-DOM context. parseRoute takes
// its query as an argument (backlog cb211b39) and needs no window.
// Patched field by field rather than assigned whole: bun runs every
// test file in one process, so another file's window may already be
// there.
beforeAll(() => {
  const g = globalThis as unknown as {
    window?: { location?: Record<string, unknown> };
  };
  if (!g.window) g.window = { location: {} };
  if (!g.window.location) g.window.location = {};
  g.window.location.pathname ??= '/';
});

// One row per kind — the route its href must resolve to, or `null` for a
// kind with no page (PAGELESS_KINDS): its /ux page went with step 1 of
// design 01fa5bdf, and a link to it would land on not-found, the defect
// this pin exists for. `Record` over the union makes a new kind a TYPE
// error here, and the runtime loop below makes it a test failure even
// under bun's type stripping.
const EXPECTED: Readonly<Record<EntityKind, Route['kind'] | null>> = {
  job: 'jobDetail',
  invoice: 'invoice',
  vendor: 'vendor',
  po: 'po',
  'vendor-invoice': 'vendorInvoice',
  // Both are a query on the Finance page, not a path of their own.
  fact: 'finance',
  'ledger-entry': 'finance',
  account: null,
  employee: null,
  asset: null,
  part: null,
  product: null,
  shipment: null,
  'marketing-asset': null,
};

/// Where each pageless kind's page was, which must answer nothing now —
/// so a kind cannot be moved back to PAGE_KINDS without its page.
const FORMER_PAGES: Readonly<Record<(typeof PAGELESS_KINDS)[number], string>> = {
  account: '/ux/accounts/id-1',
  employee: '/ux/people/id-1',
  asset: '/ux/assets/id-1',
  part: '/ux/parts/id-1',
  product: '/ux/products/id-1',
  shipment: '/ux/shipments/id-1',
  'marketing-asset': '/ux/marketing-assets/id-1',
};

describe('entityHref — every kind resolves to its own page, or is drawn as text', () => {
  test('every kind in ENTITY_KINDS has a declared route or a declared absence', () => {
    const undeclared = ENTITY_KINDS.filter((k) => !(k in EXPECTED));
    expect(undeclared).toEqual([]);
  });

  test('a kind has a page exactly when its expected route is not null', () => {
    for (const kind of ENTITY_KINDS) {
      expect({ kind, page: hasPage(kind) }).toEqual({ kind, page: EXPECTED[kind] !== null });
    }
    expect([...PAGE_KINDS, ...PAGELESS_KINDS].sort()).toEqual([...ENTITY_KINDS].sort());
  });

  for (const kind of PAGE_KINDS) {
    test(`${kind}`, () => {
      // parseRoute takes a pathname; the query (fact/ledger-entry) is
      // read off window.location by the page, not by the router.
      const path = entityHref(kind, 'id-1').split('?')[0]!;
      expect<{ kind: string; route: string | null }>({ kind, route: parseRoute(path).kind })
        .toEqual({ kind, route: EXPECTED[kind] });
    });
  }

  for (const kind of PAGELESS_KINDS) {
    test(`${kind} has no page: its former address is not found`, () => {
      expect({ kind, route: parseRoute(FORMER_PAGES[kind]).kind }).toEqual({ kind, route: 'notFound' });
    });
  }
});

// The interaction crawl is the check a reader trusts for "links land",
// and it cannot see this pin's failures: it judges only the hrefs the
// mocked backend renders, on the routes it crawls, and it counts a
// wildcard-eaten path as served. All four defects above passed it green
// for as long as they existed (backlog d063c290). So its header must
// say so and send the reader here, and its title must not promise more
// than it covers. Pinned because a pointer in prose goes stale the day
// this file is renamed, and a title is the one line everyone reads.
describe('the interaction crawl names this pin as the cover for its blind spot', () => {
  const crawl = readFileSync(
    new URL('../tests/mocked/interaction-crawl.mocked.spec.ts', import.meta.url),
    'utf8',
  );

  // Asserted as named booleans, not toContain on the file: a failure
  // then says which claim broke instead of printing the whole spec.
  test('its header points at this file', () => {
    expect({ pointer: crawl.includes('apps/web/src/entity-href-routes.test.ts') }).toEqual({ pointer: true });
  });

  test('its title claims rendered links, not every link', () => {
    expect({
      overclaims: crawl.includes('every link lands'),
      scoped: crawl.includes('every rendered link lands'),
    }).toEqual({ overclaims: false, scoped: true });
  });
});
