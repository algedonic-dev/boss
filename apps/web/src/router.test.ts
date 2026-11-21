// Router resolution snapshot. Pin every canonical path to its
// expected `Route` so a future router edit can't silently let a
// wildcard eclipse a specific case (a wildcard-before-specific
// ordering bug is the recurring failure mode; the `/finance`
// wildcard catches everything tail-shaped as `invoice`).
//
// Add an entry here every time `router.ts` learns a new path.
// Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { notFoundBack, parseRoute } from './router';
import { RETIRED_ROUTES } from '../tests/mocked/_routes';
import { ADDRESSES } from '../tests/fixtures/station-tree';

// Every path is read against the shared fixture's station tree, the
// server's read standing in: a hosted page's address is its host
// station's in the tree plus its slug (backlog 0bc2eb68), so the router
// places it only with a tree in hand. src/shell/page-address.test.ts
// pins the unread and failed trees, and a re-parented one.
const parse = (path: string, search = '') => parseRoute(path, search, ADDRESSES);

// STEP 1 OF DESIGN 01fa5bdf (David, 2026-11-17): the /ux business pages,
// HR, /it/operate, Atlas, Bottlenecks, Experiments and Policy answer
// nothing. Each is held to not-found by RETIRED_ROUTES at the end of this
// file; these are the spellings that would otherwise fall to a broader
// branch and look served.
test('the removed pages answer nothing, under any spelling a broader branch could catch', () => {
  for (const path of ['/people/hr', '/ux/people/hr', '/people/emp-1', '/ux/service/abc', '/ux/sales/abc', '/service/abc', '/sales/abc', '/accounts/a-1', '/shop/sku-1']) {
    expect({ path, route: parse(path) }).toEqual({ path, route: { kind: 'notFound', path } });
  }
  // A bare segment is a department's address, never a removed page: the
  // departments that owned one land on their jobs view now.
  expect(parse('/hr')).toEqual({ kind: 'department', code: 'hr' });
  expect(parse('/executive')).toEqual({ kind: 'department', code: 'executive' });
  // <registry>/<kind> is a workflow's page, so registry/policy asks the
  // registry for a workflow named `policy` — not the Policy page.
  expect(parse('/engineering/registry/policy')).toEqual({ kind: 'workflowDetail', kindSlug: 'policy' });
});

test('the flat company map is gone; the root station view is the company index', () => {
  // '/map' is now one more unclaimed segment: the department catch-all
  // takes it, and the registry check (sections.ts withRoster) finds no
  // department `map`, so the page renders not-found.
  expect(parse('/map')).toEqual({ kind: 'department', code: 'map' });
  expect(parse('/tree').kind).toBe('systemTree');
});

// No window is planted here. parseRoute takes the query string as its
// second argument; it read `window.location.search` until backlog
// cb211b39, and this file planted a window for it that it never took
// down — so every LATER file in bun's one process that called
// parseRoute passed on a window it never set up. nav-catalog.test.ts
// was one: it failed alone, and failed PR #244's gate on GitHub, whose
// runner ordered the files differently.
describe('parseRoute reads no global', () => {
  test('every query-reading branch takes the query from its argument, never from window', () => {
    // A window whose location THROWS, so a read of it fails this test
    // whatever order the files run in — an absent window would pass
    // here whenever an earlier file happened to plant one.
    const g = globalThis as { window?: unknown };
    const had = 'window' in g;
    const prev = g.window;
    g.window = {
      get location(): never {
        throw new Error('parseRoute read window.location');
      },
    };
    try {
      expect(parseRoute('/finance', '?tab=invoices')).toEqual({
        kind: 'finance',
        view: { tab: 'invoices', entry: '', fact: '' },
      });
      expect(parseRoute('/ux/search', '?q=cascade')).toEqual({ kind: 'search', q: 'cascade' });
      expect(parseRoute('/ux/jobs/j-1/steps/s-1', '?from=%2Fit')).toEqual({
        kind: 'stepFocus',
        jobId: 'j-1',
        stepId: 's-1',
        from: '/it',
      });
    } finally {
      if (had) g.window = prev;
      else delete g.window;
    }
  });

  test('an omitted query is no query', () => {
    expect(parseRoute('/finance')).toEqual({
      kind: 'finance',
      view: { tab: 'overview', entry: '', fact: '' },
    });
    expect(parseRoute('/ux/search')).toEqual({ kind: 'search', q: '' });
  });
});

describe('parseRoute — every specific path matches its specific case', () => {
  // Each entry: [path, expected partial-Route shape].
  // We don't check fields that come from URL-decoding (ids); only
  // that the discriminator `kind` is right + any positional param
  // is captured.
  const cases: Array<[string, Record<string, unknown>]> = [
    // User Experiences home — bare / and /ux both land on My Day (the
    // actor's personal work view) by default, rendered in place: its own
    // address is /me, the person's station (backlog 5cb56bb3), with the
    // inbox and the calendar below it.
    ['/', { kind: 'me' }],
    ['/ux', { kind: 'me' }],
    ['/me', { kind: 'me' }],
    ['/me/inbox', { kind: 'inbox' }],
    ['/me/calendar', { kind: 'myCalendar' }],
    // Browse / domain dashboards — User Experiences perspective (/ux/*).
    // A packet's own page; the /ux/jobs list went with step 1 of design
    // 01fa5bdf (review df7aeb9c) and is held to not-found below.
    ['/ux/jobs/abc-123', { kind: 'jobDetail', jobId: 'abc-123' }],
    // Finance — its own address since car 4 of design 8c3e9599 (backlog
    // c7ef4728). The regression nest: /invoices/new MUST win over the
    // `/finance/invoices/:id` wildcard.
    ['/finance', { kind: 'finance' }],
    ['/finance/invoices/new', { kind: 'newInvoice' }],
    ['/finance/journal-entries/new', { kind: 'newJournalEntry' }],
    ['/finance/invoices/inv-step-12345678', { kind: 'invoice', invoiceId: 'inv-step-12345678' }],
    ['/finance/vendors', { kind: 'vendors' }],
    ['/finance/vendors/v-1', { kind: 'vendor', vendorLookup: 'v-1' }],
    // Engineering's station page at its own top-level address (backlog
    // 950ad972, David 2026-11-12: "As a test, I would love to see this
    // under /engineering too"; design deba3000). One segment is a
    // department's address first, so the router answers the department
    // and the registry check makes it the station page where there is no
    // such department (page-address.test.ts pins that); a room inside a
    // department is its station page here. By name, a station's page sits
    // below the root's tree floor, /tree/<name>, since backlog 92bfcb04.
    ['/engineering', { kind: 'department', code: 'engineering' }],
    ['/engineering/gates', { kind: 'systemStation', station: 'gates' }],
    ['/tree', { kind: 'systemTree' }],
    ['/tree/engineering', { kind: 'systemStation', station: 'engineering' }],
    ['/tree/engineering/resources', { kind: 'systemStationResources', station: 'engineering' }],
    // Calendar / scheduling
    // The launch calendar retired with the second example tenant
    // (design 2ea444f5, backlog a8991c86): /ux/calendar is an unknown
    // path now and takes the catch-all; the person's calendar answers at
    // /me/calendar since backlog 5cb56bb3, and its /ux spelling is gone.
    ['/ux/calendar', { kind: 'notFound', path: '/ux/calendar' }],
    // The landing page (the System Model live view) was reachable ONLY
    // through the catch-all until design ee3a3a2f gave it a door.
    ['/ux/system-model', { kind: 'home' }],
    ['/ux/calendar/me', { kind: 'notFound', path: '/ux/calendar/me' }],
    // A department's own jobs view (cc76f755) — at /<code>/jobs since car
    // 2 of design 8c3e9599. The code is registry data, so the route
    // carries it rather than the router knowing it; the `department`
    // describe below pins the rest.
    ['/sales/jobs', { kind: 'department', code: 'sales' }],
    ['/operations/jobs', { kind: 'department', code: 'operations' }],
    // The former IT family pages, each at its host station's address in
    // the tree plus its slug since backlog 0bc2eb68 (the placement table
    // of design 01fa5bdf): Operate and the estate and vault pages under
    // Operations, the registry family, the codebase, the KB and the
    // manual under Engineering, Design at the design station's own
    // address, the alarms board at the root.
    ['/operations/perf', { kind: 'systemMonitoringPerf' }],
    ['/operations/audit', { kind: 'systemMonitoringEvents' }],
    ['/engineering/kb', { kind: 'systemKb' }],
    ['/engineering/registry/subjects', { kind: 'systemSubjects' }],
    // The Drift tab (4ae9969e): declared before the workflow-detail
    // wildcard, which would otherwise read 'drift' as a kind slug.
    ['/engineering/registry/drift', { kind: 'systemRegistryDrift' }],
    // The Agents tab (62988516), before the same wildcard for the same
    // reason.
    ['/engineering/registry/agents', { kind: 'systemAgents' }],
    // The Credentials tab (851259b9, design 76155676), hosted at the vault.
    ['/operations/vault/credentials', { kind: 'systemCredentials' }],
    ['/engineering/design', { kind: 'systemDesign' }],
    ['/engineering/codebase', { kind: 'systemCodebase' }],
    ['/alarms', { kind: 'systemYard' }],
    // IT's own address renders its landing in place: the IT tab links here.
    ['/it', { kind: 'systemYard' }],
    ['/operations/estate', { kind: 'systemEstate' }],
    ['/engineering/registry/step-plugins', { kind: 'systemStepPlugins' }],
    ['/engineering/registry/step-plugins/pour-quality-check', { kind: 'systemStepPluginDetail', pluginSlug: 'pour-quality-check' }],
    ['/engineering/registry/dispatcher', { kind: 'dispatcherRules' }],
    ['/engineering/registry/rules', { kind: 'dispatcherRulesList' }],
    ['/engineering/registry/rules/restock-on-low', { kind: 'dispatcherRuleEdit', ruleName: 'restock-on-low' }],
    // Manual + workflows + watchlist
    ['/engineering/manual', { kind: 'manual' }],
    ['/engineering/registry', { kind: 'workflows' }],
    ['/engineering/manual/intro', { kind: 'manualSection', slug: 'intro' }],
    // The Churn watchlist, hosted at the root (backlog 5cb56bb3).
    ['/watchlist', { kind: 'watchlist' }],
    // PO + vendor invoices — Finance's, under its address.
    ['/finance/purchase-orders/po-1', { kind: 'po', poId: 'po-1' }],
    ['/finance/vendor-invoices/vi-1', { kind: 'vendorInvoice', vendorInvoiceId: 'vi-1' }],
    // Workflow authoring (System Model). The workflows
    // `/authoring/<jobId>` route is the wildcard-precedence trap: it MUST
    // resolve before the catch-all `/workflows/(.+)` detail route.
    ['/operations/vault/auth-admin', { kind: 'authAdmin' }],
    ['/engineering/registry/new', { kind: 'workflowNew' }],
    ['/engineering/registry/authoring/job-abc-123', { kind: 'workflowDesign', jobId: 'job-abc-123' }],
    ['/engineering/registry/seasonal-release', { kind: 'workflowDetail', kindSlug: 'seasonal-release' }],
  ];

  for (const [path, expected] of cases) {
    test(`${path}`, () => {
      const actual = parse(path);
      for (const [key, value] of Object.entries(expected)) {
        expect((actual as Record<string, unknown>)[key]).toBe(value);
      }
    });
  }
});

// An unmatched path says so, and names the path (design ee3a3a2f,
// backlog c4f2ae24). The catch-all used to return the landing page for
// /ux and the yard for /it, so a dead link rendered a real, working,
// plausible page and the reader concluded they had misremembered.
describe('an unmatched path is notFound, naming the path', () => {
  test('an unknown path is notFound, not the landing page', () => {
    expect(parse('/no-such/route')).toEqual({ kind: 'notFound', path: '/no-such/route' });
    expect(parse('/ux/no-such-route')).toEqual({ kind: 'notFound', path: '/ux/no-such-route' });
    // One bare segment is a department's address (/<code>) since car 2
    // of design 8c3e9599, so the router cannot call it unknown: the
    // registry decides, in sections.ts `withRoster`, and a code the
    // instance does not have is not found there.
    expect(parse('/no-such-route')).toEqual({ kind: 'department', code: 'no-such-route' });
  });

  test('an unknown /it path is notFound, not the yard', () => {
    expect(parse('/it/no-such')).toEqual({ kind: 'notFound', path: '/it/no-such' });
    // /system retired with the IT consolidation; it is unknown like any other.
    expect(parse('/it/system/yard').kind).toBe('notFound');
  });

  test('the path named is the one asked for, mount and all', () => {
    expect(parse('/dashboard/ux/nope')).toEqual({ kind: 'notFound', path: '/dashboard/ux/nope' });
  });

  test('the one back link goes to the department the path was under', () => {
    // /it is no department's chrome to go back to since backlog 92bfcb04
    // (IT retires as a department, design df0ecd8b): a path under it is a
    // path like any other.
    expect(notFoundBack('/it/no-such')).toEqual({ href: '/me', label: 'Back to My Day' });
    expect(notFoundBack('/dashboard/it/no-such/')).toEqual({ href: '/me', label: 'Back to My Day' });
    // My Day's own address: the person's station (backlog 5cb56bb3).
    expect(notFoundBack('/ux/no-such')).toEqual({ href: '/me', label: 'Back to My Day' });
    expect(notFoundBack('/items')).toEqual({ href: '/me', label: 'Back to My Day' });
  });

  test('a department this instance does not have sends the reader Home, even from /it', () => {
    // Its own chrome is the one thing it cannot go back to (e543c6fe).
    expect(notFoundBack('/service', 'service')).toEqual({ href: '/', label: 'Back to Home' });
    expect(notFoundBack('/it/estate', 'it')).toEqual({ href: '/', label: 'Back to Home' });
  });
});

// DEPARTMENT ROUTING FROM THE REGISTRY (backlog 64656a46, car 2 of design
// 8c3e9599). A department's address is its registry code: /<code> is its
// landing and /<code>/jobs its in / working / out, for any code — the
// router knows no department by name, and whether the code is a live row
// on this instance is the registry's answer (sections.ts `withRoster`).
// /ux/departments/<code> retired with no alias (David, 2026-09-25).
describe('a department is addressed by its code', () => {
  test('/<code>/jobs is the jobs view, for a code the catalog has never heard of', () => {
    expect(parse('/hosting/jobs')).toEqual({ kind: 'department', code: 'hosting' });
    expect(parse('/dashboard/sales/jobs/')).toEqual({ kind: 'department', code: 'sales' });
    expect(parse('/front%20of%20house/jobs')).toEqual({ kind: 'department', code: 'front of house' });
  });

  test('/<code>/jobs wins over a family wildcard that would read "jobs" as an id', () => {
    // /sales/:jobId, /service/:jobId and /people/:empId each took one
    // segment until design 01fa5bdf removed them, and "jobs" is not an
    // id. (/finance/:invoiceId was the fourth until its invoices moved
    // under /finance/invoices, c7ef4728.)
    expect(parse('/finance/jobs')).toEqual({ kind: 'department', code: 'finance' });
    expect(parse('/sales/jobs')).toEqual({ kind: 'department', code: 'sales' });
    expect(parse('/service/jobs')).toEqual({ kind: 'department', code: 'service' });
    expect(parse('/people/jobs')).toEqual({ kind: 'department', code: 'people' });
  });

  test('IT carries the jobs view too, and keeps every path it had', () => {
    expect(parse('/it/jobs')).toEqual({ kind: 'department', code: 'it' });
    expect(parse('/it')).toEqual({ kind: 'systemYard' });
  });

  test('/<code> renders the landing: the flagged entry, else the first in catalog order, else the jobs view', () => {
    // IT's entry is flagged `landing`; Finance's is its first entry; a
    // code that owns nothing lands on its jobs. Executive, Distribution
    // and Maintenance landed on their first entries (Exec, Shipments,
    // Equipment) until those went with step 1 of design 01fa5bdf, and
    // Sales owns only the unlisted watchlist now.
    expect(parse('/it')).toEqual({ kind: 'systemYard' });
    expect(parse('/executive')).toEqual({ kind: 'department', code: 'executive' });
    expect(parse('/distribution')).toEqual({ kind: 'department', code: 'distribution' });
    expect(parse('/maintenance')).toEqual({ kind: 'department', code: 'maintenance' });
    expect(parse('/sales')).toEqual({ kind: 'department', code: 'sales' });
    expect(parse('/hosting')).toEqual({ kind: 'department', code: 'hosting' });
    expect(parse('/dashboard/hosting/')).toEqual({ kind: 'department', code: 'hosting' });
  });

  test('the retired /ux/departments/<code> answers nothing, and /ux/<code> is no department', () => {
    expect(parse('/ux/departments/sales')).toEqual({ kind: 'notFound', path: '/ux/departments/sales' });
    expect(parse('/ux/hosting')).toEqual({ kind: 'notFound', path: '/ux/hosting' });
    expect(parse('/ux/hosting/jobs')).toEqual({ kind: 'notFound', path: '/ux/hosting/jobs' });
  });
});

// FINANCE'S PAGES LIVE AT FINANCE'S ADDRESS (backlog c7ef4728, car 4 of
// design 8c3e9599; design deba3000: the URL is the address of the station
// that hosts the page). Every old spelling — the /ux one and the
// unprefixed one the router also answered — retired with no alias and no
// redirect (David, 2026-09-25): each is a not-found naming the path as it
// was asked for, never a page that quietly works.
describe("Finance's pages are under /finance, and their old spellings answer nothing", () => {
  const retired = [
    '/ux/finance',
    '/ux/finance/new',
    '/ux/finance/journal-entries/new',
    '/ux/finance/inv-1',
    '/ux/vendors',
    '/ux/vendors/v-1',
    '/ux/purchase-orders/po-1',
    '/ux/vendor-invoices/vi-1',
    // The unprefixed forms the /ux strip let through.
    '/finance/new',
    '/finance/inv-1',
    '/purchase-orders/po-1',
    '/vendor-invoices/vi-1',
    '/vendors/v-1',
  ];
  for (const path of retired) {
    test(`${path} is not found`, () => {
      expect(parse(path)).toEqual({ kind: 'notFound', path });
    });
  }

  test('/finance is the landing and /finance/jobs its jobs view', () => {
    expect(parse('/finance').kind).toBe('finance');
    expect(parse('/dashboard/finance/')).toEqual({
      kind: 'finance',
      view: { tab: 'overview', entry: '', fact: '' },
    });
    expect(parse('/finance/jobs')).toEqual({ kind: 'department', code: 'finance' });
  });

  test('an id holding a slash is one encoded segment under its slug', () => {
    expect(parse('/finance/invoices/a%2Fb')).toEqual({ kind: 'invoice', invoiceId: 'a/b' });
    expect(parse('/finance/vendors/acme%20co')).toEqual({ kind: 'vendor', vendorLookup: 'acme co' });
  });
});

// The single-id wildcards were greedy `(.+)`, so a deeper path under a
// list became a convincing "missing X": /ux/accounts/agreements/<id>
// rendered the ACCOUNT page for accountId "agreements/<id>", and
// /ux/sales/opportunities/<id> the JOB page. Narrowed to one segment
// (design ee3a3a2f Q6); an id holding a slash arrives percent-encoded.
describe('a single-id wildcard takes one segment', () => {
  test('the two motivating dead links are notFound, not a missing entity', () => {
    expect(parse('/ux/accounts/agreements/x').kind).toBe('notFound');
    expect(parse('/ux/sales/opportunities/x').kind).toBe('notFound');
  });

  // The /ux business families went with step 1 of design 01fa5bdf; the
  // families that stay are each held to one segment.
  const families = [
    '/finance/vendors', '/finance/invoices', '/finance/purchase-orders',
    '/finance/vendor-invoices', '/ux/jobs',
    '/engineering/registry', '/engineering/registry/authoring', '/engineering/registry/step-plugins',
    '/engineering/registry/rules',
  ];
  for (const f of families) {
    test(`${f}/a/b is notFound`, () => {
      expect(parse(`${f}/a/b`)).toEqual({ kind: 'notFound', path: `${f}/a/b` });
    });
  }

  test('an encoded slash is one segment, and arrives decoded', () => {
    expect(parse('/ux/jobs/a%2Fb')).toEqual({ kind: 'jobDetail', jobId: 'a/b' });
    expect(parse('/finance/vendors/a%2Fb')).toEqual({ kind: 'vendor', vendorLookup: 'a/b' });
  });

  test('a malformed escape does not throw; the segment arrives as typed', () => {
    expect(parse('/ux/jobs/%E0')).toEqual({ kind: 'jobDetail', jobId: '%E0' });
  });

  test('a manual section slug may hold a slash — the content API routes {*slug}', () => {
    expect(parse('/engineering/manual/ops/brewing')).toEqual({ kind: 'manualSection', slug: 'ops/brewing' });
  });

  test('a job id followed by a lone steps segment is not a job page', () => {
    expect(parse('/ux/jobs/job-1/steps').kind).toBe('notFound');
  });
});

describe('parseRoute — wildcard does not shadow specific cases', () => {
  // Pins the canonical fix for the invoice wildcard's precedence bug:
  // specific cases MUST be declared before the wildcard or they resolve
  // as `{ kind: 'invoice', invoiceId: ... }`. Any new
  // `/finance/invoices/X` case the SPA introduces should be added to the
  // table above + a regression assertion here.
  test('/finance/invoices/new → newInvoice, NOT invoice', () => {
    const r = parse('/finance/invoices/new');
    expect(r.kind).toBe('newInvoice');
  });
  test('/finance/journal-entries/new → newJournalEntry, NOT invoice', () => {
    const r = parse('/finance/journal-entries/new');
    expect(r.kind).toBe('newJournalEntry');
  });
  test('/engineering/registry/authoring/X → workflowDesign, NOT workflowDetail', () => {
    const r = parse('/engineering/registry/authoring/job-abc-123');
    expect(r.kind).toBe('workflowDesign');
  });
  // The bare /it/registry/authoring rendered a second list over the same
  // GET /api/workflows (WorkflowsAdminPage) that no link reached; deleted
  // with page audit 9da74410 (d854fe9c). The path is one more kind slug
  // now, like any other, and only its /<jobId> child is authoring.
  test('/engineering/registry/authoring is a kind slug, not a second workflow list', () => {
    expect(parse('/engineering/registry/authoring')).toEqual({ kind: 'workflowDetail', kindSlug: 'authoring' });
  });
});

// The /ux/products list's `q` (backlog 1c2db4c2) was read here until
// the page went with step 1 of design 01fa5bdf.

// /finance's tab, entry and fact ride in the query (backlog
// 2ab44d55): NewJournalEntryPage lands on ?entry=<id> after a post, and
// the route must hand that to the page rather than drop it.
describe('finance view from the query string', () => {
  const at = (search: string) => parse('/finance', search);

  test('a posted entry is carried through, onto the Trial Balance', () => {
    expect(at('?entry=ent-1')).toEqual({
      kind: 'finance',
      view: { tab: 'trial-balance', entry: 'ent-1', fact: '' },
    });
  });

  test('a fact and a tab are carried through', () => {
    expect(at('?fact=f-1')).toEqual({
      kind: 'finance',
      view: { tab: 'trial-balance', entry: '', fact: 'f-1' },
    });
    expect(at('?tab=invoices')).toEqual({
      kind: 'finance',
      view: { tab: 'invoices', entry: '', fact: '' },
    });
  });

  test('a bare /finance is the Overview', () => {
    expect(at('')).toEqual({ kind: 'finance', view: { tab: 'overview', entry: '', fact: '' } });
  });

  test('an invoice page does not read the finance query', () => {
    expect(parse('/finance/invoices/inv-1', '?entry=ent-1')).toEqual({
      kind: 'invoice',
      invoiceId: 'inv-1',
    });
  });
});

describe('global search results route', () => {
  test('/search carries the query through', () => {
    const r = parse('/ux/search', '?q=cascade');
    expect(r.kind).toBe('search');
    expect((r as { q: string }).q).toBe('cascade');
  });

  test('/search with no query is still the search route, not the catch-all', () => {
    const r = parse('/ux/search', '');
    expect(r.kind).toBe('search');
    expect((r as { q: string }).q).toBe('');
  });
});

// The jobs list's query-string filters (status, owner, subject, the
// new-job seed) were pinned here until the list went with step 1 of
// design 01fa5bdf (review df7aeb9c); /ux/jobs with any query is not
// found (src/jobs/the-jobs-list-is-retired.test.ts).

describe('standalone Views retirement', () => {
  for (const path of ['/ux/views', '/views']) {
    test(`${path} has no page and retains the requested address`, () => {
      // Bare one-segment addresses are resolved against the department
      // registry by withRoster; this parser must no longer select Views.
      if (path.startsWith('/ux/')) expect(parse(path)).toEqual({ kind: 'notFound', path });
      else expect(parse(path)).toEqual({ kind: 'department', code: 'views' });
    });
  }
  test('remaining Home routes keep their meanings, at the person’s station', () => {
    expect(parse('/').kind).toBe('me');
    expect(parse('/me/inbox').kind).toBe('inbox');
    expect(parse('/me/calendar').kind).toBe('myCalendar');
  });
});

describe('full-page step route', () => {
  test('parses /jobs/{job}/steps/{step} to stepFocus', () => {
    const r = parse('/ux/jobs/job-123/steps/step-456');
    expect(r.kind).toBe('stepFocus');
    expect((r as { jobId: string }).jobId).toBe('job-123');
    expect((r as { stepId: string }).stepId).toBe('step-456');
  });

  test('carries the lens Back target through, with its label', () => {
    // David, 40fe7291: Back from a design review landed on the job
    // page instead of the queue he came from. Only the lens knows
    // where back is, so it says so on the URL.
    const r = parse(
      '/ux/jobs/job-123/steps/step-456',
      '?from=%2Fit%2Fdesign&from_label=Design%20Review',
    );
    expect(r.kind).toBe('stepFocus');
    expect((r as { from?: string }).from).toBe('/it/design');
    expect((r as { fromLabel?: string }).fromLabel).toBe('Design Review');
  });

  test('refuses a Back target that leaves the app', () => {
    // A `from` naming another origin would turn the Back button into
    // an open redirect. Protocol-relative `//host` is the one that
    // looks in-app at a glance, which is why it is tested by name.
    // `/\host` and `/<tab>/host` are `//host` to a browser too (4f1f7698).
    for (const hostile of [
      '//evil.example',
      'https://evil.example',
      'evil',
      '/\\evil.example',
      '/\t/evil.example',
      'javascript:alert(1)',
    ]) {
      const r = parse(
        '/ux/jobs/job-123/steps/step-456',
        `?from=${encodeURIComponent(hostile)}`,
      );
      expect(r.kind).toBe('stepFocus');
      expect((r as { from?: string }).from).toBeUndefined();
    }
  });

  test('does not steal the plain job-detail route', () => {
    // The greedy /jobs/(.+) branch sits right after this one; if the
    // step pattern were looser (or ordered later) one of these two
    // would swallow the other.
    expect(parse('/ux/jobs/job-123').kind).toBe('jobDetail');
    expect((parse('/ux/jobs/job-123') as { jobId: string }).jobId).toBe('job-123');
  });

  test('a job id containing "steps" still resolves to jobDetail', () => {
    const r = parse('/ux/jobs/steps');
    expect(r.kind).toBe('jobDetail');
  });
});

// THE DEPARTMENT MAP'S SELECTION (design e765b3fc, car N1). /it is the
// map on top and the selection's detail below it, and the selection is
// kept in the query — `/it?at=gates` — so a link names what it selects.
// State on the page, not a second route: the map stays mounted across a
// selection, which is the whole point of putting it on top.
describe('/it?at= — the Department Map carries its selection in the query', () => {
  test('a station named in `at` is the selection, and the bare landing selects nothing', () => {
    expect(parse('/it', '?at=gates')).toEqual({ kind: 'systemYard', at: 'gates' });
    expect(parse('/dashboard/it/', '?at=shop-floor')).toEqual({ kind: 'systemYard', at: 'shop-floor' });
    expect(parse('/it')).toEqual({ kind: 'systemYard' });
    // An empty `at` selects nothing rather than a station named "".
    expect(parse('/it', '?at=')).toEqual({ kind: 'systemYard' });
  });

  test('`at` is read on the landing only — another IT page ignores it', () => {
    expect(parse('/operations/estate', '?at=gates')).toEqual({ kind: 'systemEstate' });
    // The board's own address reads it as the landing does.
    expect(parse('/alarms', '?at=gates')).toEqual({ kind: 'systemYard', at: 'gates' });
  });
});

// THE PAGES THE DEPARTMENT MAP REPLACED (design e765b3fc, car N3). Each
// one's content is a selection's panel now — a region's floor is its
// station's panel, the Crew Board the shop floor's, yard status the
// track's, dock's and garage's, the conductor's feed the track's, and
// the feedback and backlog boards receiving's and marshalling's — so the
// path is not found, like any other. No alias and no redirect (David,
// 2026-09-25: no shims before 1.0.0): the four aliases that still
// answered go with them.
// The list is the crawl roster's (_routes.ts `RETIRED_ROUTES`), so the
// mocked spec that opens each page and this pin read one definition.
// Step 1 of design 01fa5bdf added the map's own leftovers and the /ux
// business pages. Every door is My Day: an IT path's was the alarms
// board until backlog 92bfcb04 retired the /it prefix.
describe('the retired pages are not found', () => {
  for (const path of RETIRED_ROUTES) {
    const door = { href: '/me', label: 'Back to My Day' };
    test(`${path} is notFound, and its back link is ${door.href}`, () => {
      expect(parse(path)).toEqual({ kind: 'notFound', path });
      expect(notFoundBack(path)).toEqual(door);
    });
  }
});

// THE ADDRESS A HOSTED PAGE HAD BEFORE ITS HOST (backlog 0bc2eb68). Each
// IT family page and the manual answered at a hand-typed /it or /ux path;
// they answer at their host stations' addresses now, and the old spelling
// is a not-found naming the path, never an alias (David, 2026-09-25: no
// shims before 1.0.0). /it itself is not on this list: it is IT's
// department address, which the IT tab links to.
describe('the spellings a hosted page had before its host answer nothing', () => {
  const moved = [
    '/it/operate/audit', '/it/operate/perf', '/it/registry', '/it/registry/new',
    '/it/registry/authoring/j-1', '/it/registry/seasonal-release', '/it/registry/step-plugins',
    '/it/registry/step-plugins/p-1', '/it/registry/dispatcher', '/it/registry/subjects',
    '/it/registry/drift', '/it/registry/agents', '/it/registry/credentials', '/it/registry/rules',
    '/it/registry/rules/r-1', '/it/design', '/it/codebase', '/it/estate', '/it/kb', '/it/auth-admin',
    '/ux/manual', '/ux/manual/intro', '/manual/intro',
    // The person's pages, at /me since backlog 5cb56bb3.
    '/ux/me', '/ux/inbox', '/ux/calendar/me', '/calendar/me',
    // The Churn watchlist's /ux spelling, until backlog 5cb56bb3 hosted
    // it at the root.
    '/ux/watchlist',
  ];
  for (const path of moved) {
    test(`${path} is not found`, () => {
      expect(parse(path)).toEqual({ kind: 'notFound', path });
    });
  }
  // One bare segment is a department's address, which the registry
  // judges: /manual is no department, so withRoster renders it not found.
  test('/manual is one more unclaimed segment', () => {
    expect(parse('/manual')).toEqual({ kind: 'department', code: 'manual' });
  });
});
