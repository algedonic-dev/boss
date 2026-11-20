// The nav catalog is the single source of truth for which app owns a
// surface. These tests hold two lines.
//
// First, the de-duplication that made the app split safe: app
// membership used to live in two sets, in two vocabularies, in two
// files (AppShell's `MODEL_ROUTES: Set<RouteName>` for the sidebar,
// App.svelte's `MODEL_KINDS: Set<Route['kind']>` for the active tab),
// which had to agree for every routed surface or a page rendered under
// the wrong tab with nothing failing. That membership set is still
// pinned verbatim against the deleted list — it now belongs to the IT
// app rather than a top-level System Model tab, but which surfaces
// travel together has not changed.
//
// Second, the split itself: every surface belongs to exactly one app,
// every app the tab bar advertises can actually be reached, and no
// surface is stranded in an app with no tab.

import { describe, it, expect } from 'bun:test';
import type { AppId, Department } from '@boss/web-kit/nav';
import {
  APP_SUBJECT_KINDS,
  ROUTE_CATALOG,
  STATION,
  appForSection,
  appsFor,
  departmentJobsPath,
  departmentLanding,
  departmentPath,
  departmentRows,
  departmentsWithoutSurfaces,
  inPerspective,
  unhostedPages,
  type NavItem,
} from './nav-catalog';
import { appForRoute, withRoster } from './sections';
import { parseRoute, type Route } from '../router';
import { pagePath } from './page-address';
import { ADDRESSES, pageAt } from '../../tests/fixtures/station-tree';
import { readdirSync, readFileSync } from 'node:fs';

/// The departments registry's rows, read from the files that seed them
/// rather than restated here — restating is the drift this test exists
/// to catch.
///
/// They come from TWO places, which is itself worth knowing: the
/// migration seeds thirteen on every instance
/// (`20260919181324-a-department-is-a-subject.sql`, `it` among them —
/// the platform's), and the playground tenant declares its roster
/// (`examples/brewery/seeds/departments.toml`). The SPA reads them from
/// `GET /api/departments` at boot, so these tests hand the seeded set
/// to `appsFor` the way the shell hands it the fetched one. They were
/// read from the `(employee, department)` Classes until backlog
/// c87e3d6d retired those (2026-09-27).
function registryDepartments(): ReadonlyArray<string> {
  const core = readFileSync(
    new URL(
      '../../../../infra/postgres/schema/20260919181324-a-department-is-a-subject.sql',
      import.meta.url,
    ),
    'utf8',
  );
  const coreCodes = [
    ...core.matchAll(
      /\(\s*'([a-z-]+)',\s*'[^']*',\s*'(?:operations|revenue|support|governance)',\s*\d+\s*\)/g,
    ),
  ].map((m) => m[1]!);

  const tenant = readFileSync(
    new URL('../../../../examples/brewery/seeds/departments.toml', import.meta.url),
    'utf8',
  );
  const tenantCodes = [...tenant.matchAll(/^code = "([a-z-]+)"$/gm)].map((m) => m[1]!);

  const all = [...new Set([...coreCodes, ...tenantCodes])];
  // A parser that silently matched nothing would make every
  // assertion below vacuous.
  expect(coreCodes.length).toBeGreaterThan(10);
  expect(tenantCodes.length).toBeGreaterThan(0);
  return all;
}

const SEEDED: ReadonlyArray<Department> = registryDepartments().map((code) => ({
  code,
  label: code,
}));

/// The bar the playground tenant gets: every seeded department, and
/// the Simulator because its manifest lists `sim = true`.
const APPS = appsFor(SEEDED, { simulator: true });

/// Verbatim copy of AppShell.svelte's deleted `MODEL_ROUTES`. These
/// surfaces have now moved wholesale from the retired `model` app
/// into `it` — the review resolved that IT is the department and
/// System Model lives inside it, rather than the two being separate
/// tabs (architecture-decisions.md §Step UX & frontend). The MEMBERSHIP
/// is still pinned verbatim: the app they belong to changed, which
/// surfaces belong together did not. If a future change moves a
/// surface into or out of the set, this list is the thing to update,
/// deliberately. 'system-experiments' and 'policy' left with their
/// pages in step 1 of design 01fa5bdf (2026-11-17).
const LEGACY_MODEL_ROUTES: ReadonlyArray<string> = [
  'system-monitoring', 'system-step-plugins', 'system-dispatcher',
  'system-subjects', 'system-dispatcher-rules', 'system-dispatcher-rule',
  'system-kb', 'system-design',
  // One entry, not two. `workflows` (the authoring row, already
  // dropped from the sidebar) and `workflows` (the catalog) were
  // separate keys pointing at the same path; the rename collapsed
  // them, which is what surfaced the redundancy.
  'workflows', 'auth-admin',
];

const entries = Object.entries(ROUTE_CATALOG) as ReadonlyArray<[string, NavItem]>;

describe('nav catalog — app assignment', () => {
  it('every catalog entry declares an owner', () => {
    const missing = entries.filter(([, v]) => v.owner === undefined).map(([k]) => k);
    expect(
      missing,
      `these surfaces declare no owner and would render under whichever tab ` +
        `they happened to fall back to: ${missing.join(', ')}`,
    ).toEqual([]);
  });

  it('ONE owner field: the app and department fields it replaced are gone', () => {
    // Car 2 of design 8c3e9599 (backlog 64656a46). Which department owns
    // a surface was written as `app` (its tab) and as `department` (what
    // it lists), and the two disagreed on the Service queue — app
    // `service`, department `support`. One field cannot disagree with
    // itself (CLAUDE.md §9a).
    const stale = entries
      .filter(([, v]) => 'app' in v || 'department' in v)
      .map(([k]) => k);
    expect(stale).toEqual([]);
  });

  /// IT surfaces added SINCE the app split, listed explicitly.
  ///
  /// The pin below is about surfaces not silently CHANGING app; it was
  /// never meant to freeze IT at its 2026-08-05 size. Growth goes here
  /// deliberately, one line per surface, so the two properties stay
  /// separable: nothing drifted, and this is what we added.
  const IT_SURFACES_ADDED_SINCE: ReadonlyArray<string> = [
    // The feedback triage board and the IT backlog board were rows here
    // ('system-feedback', 'system-backlog') until car N3 of design
    // e765b3fc (2026-09-25) moved each board into a station's panel on
    // the Department Map — receiving for feedback, receiving and
    // marshalling for the backlog — and retired their pages.
    // The Operating System map — the executor network. Sits beside
    // the dispatcher cascade: same IT audience, different question
    // (job traffic, not rule wiring).
    // Flow — the team's own throughput, in wall-clock time. Distinct
    // from System Monitoring on purpose: monitoring answers what the
    // machine is doing, Flow answers what the people got through.
    // Fleet (Bottlenecks), Incidents and Atlas were rows here until
    // step 1 of design 01fa5bdf (2026-11-17) took them with the map.
    // The Crew Board, the Receiving Yard and the Marshalling Yard were
    // rows here (04c5bbc0, 92921c2f) and LEFT the catalog with car N1
    // of design e765b3fc (2026-09-25): each is a station on the
    // Department Map, selected there — `/it?at=shop-floor`,
    // `?at=receiving`, `?at=marshalling` — and a catalog row existed
    // only to be a sidebar row.
    // The train yard — the departure board over the pipeline's queues
    // and the IT app's guest-visible landing (departure-board.md Q1).
    // Its car landed without this line; added when the map arrived. The
    // alarms board since design 01fa5bdf step 1.
    'system-yard',
    'system-performance',
    // The hardware registry page — declared beside observed, plus the
    // dev-workspace ssh door (59ef456a).
    'system-estate',
    // The codebase — the tree's own numbers and trend; a row since
    // David's feedback 9827c699 (2026-09-14), formerly a Design tab.
    'system-codebase',
    // The Drift tab on Registry — the newest maintenance-protocol-drift
    // packet rendered (4ae9969e, car 2 of 8f4e9cc0). A TAB, not a row:
    // it gates under `workflows` like the registry it is a view of.
    'system-registry-drift',
    // The Agents tab on Registry — the agents registry as a directory
    // (backlog 62988516; David 2026-09-26: agents get a page in IT,
    // never the People roster). A TAB, not a row, gated like Drift.
    'system-agents',
    // The Credentials tab on Registry — the credentials registry drawn,
    // with rotate / retire / declare filed as packets (backlog 851259b9;
    // design 76155676 step 3). A TAB, not a row, gated like Agents.
    'system-credentials',
  ];

  it('the IT app contains the System Model set plus what we added deliberately', () => {
    const derived = entries
      .filter(([, v]) => v.owner === 'it')
      .map(([k]) => k)
      .sort();
    const expected = [...LEGACY_MODEL_ROUTES, ...IT_SURFACES_ADDED_SINCE].sort();
    expect(derived).toEqual(expected);
  });

  // A route can sit in the catalog under IT and still be unreachable. It
  // was a SOURCE-level check against AppShell's own IT_GROUPS list until
  // car 2 of design 8c3e9599 (backlog 64656a46) deleted that list: the
  // sidebar is `departmentRows`, the department's catalog entries in
  // catalog order minus the `unlisted` ones, so the check reads the rows
  // the shell renders instead of scraping a component.
  it('the IT sidebar holds exactly seven surface rows, and every other IT surface is a tab or a documented door', () => {
    // The 2026-08-31 consolidation (packet 1f6d55e0): David — "we do
    // have too many IT pages though. We should consolidate." The
    // sidebar is EXACTLY seven surface rows; every remaining IT catalog
    // entry must be reachable as a tab on one of them (ItTabs.svelte) or
    // be on the short documented list of parent-reached doors. This test
    // is also the executable "17 pages became 6, plus one decided since"
    // claim.
    const SIDEBAR_ROWS: ReadonlyArray<string> = [
      'system-yard',        // /it — the alarms board, the landing
      'system-monitoring',  // Operate — the audit log since design 01fa5bdf
      'workflows',          // Registry
      'system-design',      // Design
      'system-codebase',    // Codebase — see below
      'system-estate',      // Estate
      'system-kb',          // Knowledge Base
    ];
    // No EIGHTH surface row.
    //
    // The count still exists and still bites — the consolidation's point
    // was that a family belongs behind one row — so a new row needs a
    // decision of David's, not an edit to this line. Each change so far
    // had one: the Crew Board (04c5bbc0, 2026-09-11), the Codebase
    // (9827c699, 2026-09-14), and the Receiving and Marshalling Yards
    // (92921c2f, 2026-09-18) took it from six to ten; and then the map
    // took it back to seven (design e765b3fc, car N1, decided
    // 2026-09-25): "remove the left nav bar items associated with
    // navigating to different areas on the map and consolidate to maybe
    // just Department Map" — the three yards and the Crew Board are
    // stations on the one map, selected there. The Jobs row after them
    // is not a surface of IT's: every department ends on it (design
    // 8c3e9599 §9, approved 2026-09-25), IT included.
    const rows = departmentRows('it').map((r) => r.id);
    expect(rows).toEqual([...SIDEBAR_ROWS, 'department-jobs']);

    const tabs = readFileSync(
      new URL('../it/ItTabs.svelte', import.meta.url),
      'utf8',
    );
    // Doors reached from a parent page rather than sidebar or tabs.
    const DOCUMENTED_DOORS: ReadonlyArray<string> = [
      'system-dispatcher-rules', // reached from the cascade
      'system-dispatcher-rule',
      'auth-admin',              // unlisted by design (1f6d55e0)
    ];
    const unreachable = entries
      .filter(([, v]) => v.owner === 'it')
      // ItTabs names each tab's page by catalog id (backlog 0bc2eb68).
      .map(([k]) => [k, k] as const)
      .filter(([k]) => !SIDEBAR_ROWS.includes(k) && !DOCUMENTED_DOORS.includes(k))
      .filter(([, path]) => !tabs.includes(`'${path}'`));
    expect(
      unreachable.map(([k]) => k),
      `IT surfaces neither sidebar, tab, nor documented door: ${unreachable.map(([k]) => k).join(', ')}`,
    ).toEqual([]);
    // And every one of them is marked `unlisted`, which is what keeps it
    // out of the rows above — a tab left unmarked becomes an eighth row.
    const listed = entries
      .filter(([k, v]) => v.owner === 'it' && !SIDEBAR_ROWS.includes(k) && v.unlisted !== true)
      .map(([k]) => k);
    expect(listed).toEqual([]);
  });

  it('the IT sidebar leads with the alarms board, which is also where the IT tab lands', () => {
    // Design e765b3fc, car N1 (David, 2026-09-25): ONE sidebar row for
    // the map where Receiving Yard, Marshalling Yard, Train Yard and Crew
    // Board stood (design 55417146's flow-ordered four). Step 1 of design
    // 01fa5bdf pulled the map out and kept the row for what it left: the
    // alarms board. The landing is the entry flagged `landing` (design
    // 8c3e9599 §5), so it does not rest on coming first in catalog order.
    expect(departmentRows('it')[0]?.id).toBe('system-yard');
    expect(departmentLanding('it')?.id).toBe('system-yard');
    expect(ROUTE_CATALOG['system-yard'].landing).toBe(true);
    expect(ROUTE_CATALOG['system-yard'].label).toBe('Alarms');
    // Hosted at the root, so its own address is its slug (backlog
    // 0bc2eb68); /it, the IT tab's address, renders it in place.
    expect(pageAt('system-yard')).toBe('/alarms');
    expect(APPS.find((a) => a.id === 'it')?.href).toBe('/it');
    expect(parseRoute('/it')).toEqual({ kind: 'systemYard' });
    expect(parseRoute('/alarms', '', ADDRESSES)).toEqual({ kind: 'systemYard' });
    // One landing per department, or the flag answers nothing.
    const flagged = entries.filter(([, v]) => v.landing === true).map(([, v]) => v.owner);
    expect(flagged.length).toBe(new Set(flagged).size);
    // The four rows the map replaced are gone from the catalog too — no
    // row is left pointing at a floor page (no shims before 1.0.0).
    const catalog = ROUTE_CATALOG as Readonly<Record<string, NavItem | undefined>>;
    for (const gone of [
      'system-receiving', 'system-marshalling', 'system-crew',
      // design 01fa5bdf step 1
      'system-incidents', 'system-atlas', 'system-fleet', 'system-experiments', 'policy',
    ]) {
      expect(catalog[gone], gone).toBeUndefined();
    }
  });

  it('the /ux business pages and HR are gone from the catalog (design 01fa5bdf step 1)', () => {
    const catalog = ROUTE_CATALOG as Readonly<Record<string, NavItem | undefined>>;
    for (const gone of [
      'sales', 'service', 'qa', 'warehouse', 'shipping', 'support', 'exec', 'catalog', 'parts',
      'products', 'people', 'assets', 'shop', 'marketing-assets', 'hr', 'watchlist',
    ]) {
      expect({ gone, entry: catalog[gone] }).toEqual({ gone, entry: undefined });
    }
    // And no entry left points under a removed /ux address.
    const removed = /^\/ux\/(accounts|assets|catalog|exec|marketing-assets|parts|people|products|qa|sales|service|shipping|shipments|shop|support|warehouse)(\/|$)|^\/people\/hr$/;
    expect(entries.filter(([, v]) => removed.test(pagePath(v, ADDRESSES) ?? '')).map(([k]) => k)).toEqual([]);
    // The watchlist kept its gate: the `accounts` entry is it now, at
    // the root since backlog 5cb56bb3, where its address is its slug.
    expect(pagePath(ROUTE_CATALOG.accounts, ADDRESSES)).toBe('/watchlist');
    expect(ROUTE_CATALOG.accounts.permKey).toBe('accounts');
  });

  it('the shell keeps no list of its own: APP_SURFACES and IT_GROUPS are gone', () => {
    // Both were a second answer to "what does this department's sidebar
    // hold" beside the catalog (design 8c3e9599 §9). A department added
    // to the registry now gets its sidebar with no edit here.
    const shell = readFileSync(new URL('./AppShell.svelte', import.meta.url), 'utf8');
    expect(shell).not.toContain('const APP_SURFACES');
    expect(shell).not.toContain('const IT_GROUPS');
    expect(shell).toContain('departmentRows(');
  });

  // A row a fixed-perspective group lists must be one that perspective
  // can render. AppShell's visible() runs inPerspective on every row,
  // which drops any catalog row whose owner is not the app being
  // rendered — so a row listed under the wrong app is dead text: no role
  // ever sees it, and nothing says so. Home's Mine group carried `exec`
  // (owner executive) that way until backlog e8fe5e5a (2026-09-24). Exec
  // was never reachable through Home; it is the Executive app's row, and
  // every department tab is offered to every role (appsFor takes the
  // departments alone), so removing the dead row takes no route away
  // from anyone.
  //
  // The department groups ride the same check (backlog 72a88031,
  // 2026-09-24): Production's Products row carries permKey `parts`, the
  // gate it shares with Warehouse's Ingredients & parts, and the rule
  // then looked its owner up THROUGH that permKey — warehouse — so
  // Production dropped the row for every role. The rule is the shell's
  // own function, imported here rather than restated, so the pin judges
  // the rows the way the sidebar does. Home's groups are still a list in
  // the shell; every department's are `departmentRows` (design 8c3e9599).
  it('every row the Home, IT and department sidebars list is one that app renders', () => {
    const shell = readFileSync(new URL('./AppShell.svelte', import.meta.url), 'utf8');
    const between = (from: string, to: string): string => {
      const start = shell.indexOf(from);
      const end = shell.indexOf(to, start);
      // A marker that moved would make the slice empty and the check
      // vacuous; refuse that rather than pass it.
      expect(start, `marker not found: ${from}`).toBeGreaterThanOrEqual(0);
      expect(end, `marker not found: ${to}`).toBeGreaterThan(start);
      return shell.slice(start, end);
    };
    const rowsOf = (src: string): ReadonlyArray<string> =>
      [...src.matchAll(/ROUTE_CATALOG(?:\.(\w[\w-]*)|\['([^']+)'\])/g)].map((m) => (m[1] ?? m[2])!);
    const catalog = ROUTE_CATALOG as Readonly<Record<string, NavItem | undefined>>;
    const homeGroups: ReadonlyArray<readonly [AppId, ReadonlyArray<NavItem | undefined>]> = [
      // Home's Work group (All jobs) went with the jobs list, design 01fa5bdf.
      ['home', rowsOf(between('const HOME_GROUPS', 'let MAIN')).map((k) => catalog[k])],
    ];
    const departmentGroups = SEEDED.map((d) => [d.code, departmentRows(d.code)] as const);
    const allGroups = [...homeGroups, ...departmentGroups];
    for (const [app, rows] of allGroups) expect(rows.length, app).toBeGreaterThan(0);
    const dead = allGroups.flatMap(([app, rows]) =>
      rows
        .filter((item) => item === undefined || !inPerspective(item, app))
        .map((item) => `${item?.id ?? '(no catalog entry)'} (listed under ${app}, owner ${item?.owner ?? 'none'})`),
    );
    expect(dead, `sidebar rows no role can ever see: ${dead.join(', ')}`).toEqual([]);
    const listed = new Set(allGroups.flatMap(([, rows]) => rows.map((item) => item?.id)));
    const missing = entries
      .filter(([, item]) => !item.unlisted)
      .filter(([, item]) => item.owner === 'home' || SEEDED.some((d) => d.code === item.owner))
      .filter(([, item]) => !listed.has(item.id))
      .map(([name]) => name);
    expect(missing, `listed catalog surfaces without a sidebar entrance: ${missing.join(', ')}`).toEqual([]);
  });

  it("a department's sidebar is its listed surfaces in catalog order, then Jobs", () => {
    // Design 8c3e9599 §9 (car 2): its own surfaces in catalog order,
    // then its child units (none nest yet), then Jobs. Sales lists its
    // pipeline first now — its landing, and first in the catalog — where
    // the deleted APP_SURFACES listed Accounts first against the tab that
    // opened on the pipeline. Since step 1 of design 01fa5bdf took the
    // pipeline, Accounts and the Shop, Sales owns only the churn
    // watchlist — a door, not a row, as it was — so its sidebar is Jobs.
    expect(departmentRows('sales').map((r) => r.id)).toEqual(['department-jobs']);
    expect(departmentRows('finance').map((r) => r.id)).toEqual(['finance', 'vendors', 'department-jobs']);
    expect(departmentRows('people').map((r) => r.id)).toEqual(['department-jobs']);
    const jobs = departmentRows('sales').at(-1)!;
    expect(jobs).toEqual({ id: 'department-jobs', label: 'Jobs', path: '/sales/jobs' });
    // No owner of its own: it belongs to the group it sits in.
    expect(inPerspective(jobs, 'sales')).toBe(true);
  });

  it('nothing from the original System Model set has left the IT app', () => {
    // The half of the pin that matters most: a surface silently
    // changing app is the failure this list was written for.
    const inIt = new Set(entries.filter(([, v]) => v.owner === 'it').map(([k]) => k));
    const missing = LEGACY_MODEL_ROUTES.filter((r) => !inIt.has(r));
    expect(missing, `these left the IT app: ${missing.join(', ')}`).toEqual([]);
  });

  it('every surface lands in an app the chrome bar actually offers', () => {
    const tabbed = new Set<AppId>(APPS.map((a) => a.id));
    for (const [name, item] of entries) {
      expect(
        tabbed.has(item.owner as AppId),
        `${name} is owned by "${item.owner}", which has no tab in APPS — ` +
          `it would be unreachable.`,
      ).toBe(true);
    }
  });

  it('no surface is left in the retired catch-all "user" app', () => {
    // `/ux` was one tab holding 24 surfaces. The split exists to end
    // that; a straggler here means a surface nobody re-homed.
    const stragglers = entries
      .filter(([, v]) => (v.owner as string) === 'user')
      .map(([k]) => k);
    expect(stragglers).toEqual([]);
  });

  it('no surface is left in the retired "model" app', () => {
    // System Model stopped being a top-level app when the review made
    // IT the department that owns it. A straggler here would render
    // under a tab that no longer exists.
    const stragglers = entries
      .filter(([, v]) => (v.owner as string) === 'model')
      .map(([k]) => k);
    expect(stragglers).toEqual([]);
  });

  it('IT is a department app, not a second model-facing tab', () => {
    // The decision (Q2) was that IT is a department like Finance or
    // People. Pinning its presence and Simulator's separateness keeps
    // a later reshuffle from quietly recreating the two-model-tabs
    // shape the review rejected.
    const ids = APPS.map((a) => a.id);
    expect(ids).toContain('it');
    expect(ids).not.toContain('model');
    expect(ids.indexOf('it')).toBeGreaterThan(ids.indexOf('simulator'));
  });

  it('a department app that owns no surface lands on its own jobs view, not All jobs', () => {
    // A tab that renders an empty sidebar is a dead end. Since the tabs
    // are the registry's (ce68f137), a department can own nothing here;
    // its tab then opens the department's jobs view — in / working /
    // out over the packets whose workflow declares it (cc76f755). It
    // used to open All jobs with Home highlighted, which read as "this
    // department has no work". Simulator is exempt: it is a separate
    // SPA with no surfaces in this catalog.
    const owned = new Set(entries.map(([, v]) => v.owner));
    let checked = 0;
    for (const app of APPS) {
      if (app.id === 'simulator' || owned.has(app.id)) continue;
      checked += 1;
      expect(app.href, `app "${app.id}" owns no surface`).toBe(departmentPath(app.id));
      // And the router answers the landing with the department itself.
      expect(parseRoute(app.href, '', ADDRESSES)).toEqual({ kind: 'department', code: app.id });
    }
    expect(checked, 'the seeded registry has surface-less departments to check').toBeGreaterThan(0);
  });
});

describe('appForSection — the App.svelte tab derivation', () => {
  it('resolves surfaces to their app', () => {
    expect(appForSection('system-yard')).toBe('it');
    expect(appForSection('accounts')).toBe('sales');
    expect(appForSection('finance')).toBe('finance');
    // 'All jobs' sat on Home deliberately until the list went with step
    // 1 of design 01fa5bdf; a packet's own page takes Home's fallback
    // section for the same reason (feedback 4b454768: "I shouldn't be
    // jerked around through apps as I work").
    expect(appForSection('vendors')).toBe('finance');
    expect(appForSection('system-monitoring')).toBe('it');
    expect(appForSection('inbox')).toBe('home');
  });

  it('falls back to Home for unknown sections', () => {
    // `me` is App.svelte's terminal fallback in the activeSection
    // ternary and has no catalog entry. Home is where personal
    // surfaces live, so that is the right landing for the fallback.
    expect(appForSection('me')).toBe('home');
    expect(appForSection('definitely-not-a-section')).toBe('home');
  });
});

describe('departments map to apps', () => {
  it('every catalog owner names a seeded department', () => {
    // The other half of "apps are departments": the tabs are the
    // registry's now, so a catalog entry owned by a department the
    // registry does not declare would render under no tab at all.
    const codes = new Set(SEEDED.map((d) => d.code));
    const invented = entries
      .map(([, v]) => v.owner)
      .filter((a): a is string => a !== undefined && a !== 'home' && a !== 'simulator')
      .filter((a) => !codes.has(a));
    expect(
      [...new Set(invented)],
      `these catalog owners name no department in 01-registries.sql or the playground seed: ${invented.join(', ')}`,
    ).toEqual([]);
  });

  it('every app is a department, except Home and Simulator', () => {
    // The whole point of the change: "CRM is not a department for
    // example. The only exception to the department-based apps is the
    // Simulator." Home is the second exception — personal work belongs
    // to whoever is doing it, not to a department.
    const codes = new Set(SEEDED.map((d) => d.code));
    const invented = APPS.map((a) => a.id).filter(
      (id) => id !== 'home' && id !== 'simulator' && !codes.has(id),
    );
    expect(
      invented,
      `these apps name no department: ${invented.join(', ')}`,
    ).toEqual([]);
  });

  it('every seeded department has a tab, in registry order', () => {
    // A tab per department the tenant declares (ce68f137). `audit`
    // once mapped to no app at all and nothing failed; a second
    // tenant's `operations` had no tab because a hardcoded list did
    // not know it.
    const tabbed = APPS.filter((a) => a.id !== 'home' && a.id !== 'simulator').map((a) => a.id);
    expect(tabbed).toEqual(SEEDED.map((d) => d.code));
  });

  it('every department tab links to /<code>, which renders its landing', () => {
    // Design 8c3e9599 §5/§9 (car 2): the tab's address is the
    // department's own, and the landing is the entry flagged `landing`,
    // else the first in catalog order, else the jobs view — never an
    // empty page, and never the catch-all.
    for (const app of APPS) {
      if (app.id === 'home' || app.id === 'simulator') continue;
      expect(app.href).toBe(departmentPath(app.id));
      const landing = departmentLanding(app.id);
      const own = landing ? pagePath(landing, ADDRESSES) : null;
      const expected: Route = own !== null ? parseRoute(own, '', ADDRESSES) : { kind: 'department', code: app.id };
      expect(parseRoute(app.href, '', ADDRESSES), `app "${app.id}"`).toEqual(expected);
      expect(parseRoute(app.href, '', ADDRESSES).kind, `app "${app.id}" lands on the catch-all`).not.toBe('notFound');
      // And the page it lands on renders under its own tab.
      expect(appForRoute(parseRoute(app.href, '', ADDRESSES)), `app "${app.id}"`).toBe(app.id);
    }
  });

  it('the department jobs path round-trips through the router, code included', () => {
    // The path is the one spelling both halves share: the sidebar row
    // builds it here, the router parses it. A code with a character the
    // URL would eat must survive the trip.
    for (const code of ['sales', 'operations', 'front of house', 'it']) {
      expect(parseRoute(departmentJobsPath(code))).toEqual({ kind: 'department', code });
    }
    expect(departmentJobsPath('sales')).toBe('/sales/jobs');
  });

  it('a tenant with no departments gets Home alone, not a crash', () => {
    expect(appsFor([], { simulator: false }).map((a) => a.id)).toEqual(['home']);
  });

  it('the Simulator tab is the sim module, not a fixture of the bar', () => {
    expect(appsFor(SEEDED, { simulator: false }).map((a) => a.id)).not.toContain('simulator');
    expect(appsFor(SEEDED, { simulator: true }).map((a) => a.id)).toContain('simulator');
  });

  it('reports the departments with no surface of their own', () => {
    // Not a failure — a report, so the gap is visible rather than
    // reading as covered. These are real departments with real people
    // and no screen built for them yet; their tab lands on their jobs.
    const bare = [...departmentsWithoutSurfaces(SEEDED)].sort();
    // `refurb` joined this list on 2026-08-28 when the /ux/refurb route
    // was removed from the shared shell (feedback 96c37dbe): it was a
    // device-shop surface every other tenant saw as an empty list, and
    // David's reason was that tenants connect at the boundary through
    // agreed protocols rather than sharing one multi-tenant shell. The
    // DEPARTMENT still exists and its people still exist.
    // Since c87e3d6d the roster is the departments registry, not the
    // employee Classes: `refurb` and `audit` were Classes and never
    // departments rows, and packaging and taproom are the brewery's
    // declared departments with no screen yet. Step 1 of design 01fa5bdf
    // (2026-11-17) removed the /ux business pages, so every department
    // but Finance, Sales (its watchlist) and IT has none now: each tab
    // lands on its jobs view until a station's page is built for it.
    expect(bare).toEqual([
      'distribution', 'executive', 'maintenance', 'marketing', 'packaging', 'people',
      'production', 'qa', 'service', 'support', 'taproom', 'warehouse',
    ]);
  });

  // THE PIN the packet names (backlog 64656a46, car 2 of design
  // 8c3e9599): a registry fixture with an extra department renders its
  // tab and its jobs view without touching apps/web. `hosting` is a row
  // on the live instance and owns nothing in this catalog, so it is the
  // shape every department a company adds will have on day one.
  it('a department the registry adds gets a tab, a landing, a sidebar and a jobs view with no code', () => {
    const extra = { code: 'hosting', label: 'Hosting' };
    expect(entries.some(([, v]) => v.owner === extra.code)).toBe(false);
    const roster = [...SEEDED, extra];
    const tab = appsFor(roster, { simulator: false }).find((a) => a.id === extra.code);
    expect(tab).toEqual({ id: 'hosting', label: 'Hosting', href: '/hosting' });
    // The tab lands on the jobs view, which renders under its own tab…
    const landing = parseRoute(tab!.href, '', ADDRESSES);
    expect(landing).toEqual({ kind: 'department', code: 'hosting' });
    expect(parseRoute('/hosting/jobs')).toEqual(landing);
    expect(appForRoute(landing)).toBe('hosting');
    // …with a sidebar of one row, Jobs…
    expect(departmentRows('hosting')).toEqual([{ id: 'department-jobs', label: 'Jobs', path: '/hosting/jobs' }]);
    // …and it is a page only where the registry has the row.
    expect(withRoster(landing, '/hosting', roster)).toEqual(landing);
    expect(withRoster(landing, '/hosting', SEEDED)).toEqual({
      kind: 'notFound',
      path: '/hosting',
      department: 'hosting',
    });
  });
});

describe('every concrete Subject kind is claimed by an app', () => {
  /// The subject-kind registry is a taxonomy, not a flat list: rows
  /// carry a `parent_kind`, and the roots with children (`person`,
  /// `object`, `intangible`) are abstract — `account` specializes
  /// `person`, and nothing is ever of kind `person` itself. So this
  /// exempts roots-with-children structurally rather than naming them,
  /// which means a future abstract root is exempt automatically and a
  /// future concrete kind is not.
  ///
  /// Rows look like:
  ///   ('account', 'Account', 'desc…', 'platform', 10, 'person'),
  /// with the kind first and parent_kind last. Descriptions contain
  /// commas and parentheses, so this anchors on those two positions
  /// rather than splitting fields.
  function taxonomy(): ReadonlyArray<{ kind: string; parent: string | null }> {
    const sql = readFileSync(
      new URL('../../../../infra/postgres/schema/01-registries.sql', import.meta.url),
      'utf8',
    );
    // Walk lines from the INSERT to the statement terminator. Slicing
    // on the first `;` truncated the block at a semicolon INSIDE a
    // description ("one row per tenant; the subject…"), which silently
    // yielded only the six root rows.
    const lines = sql.slice(sql.indexOf('INSERT INTO subject_kinds')).split('\n');
    const rows: Array<{ kind: string; parent: string | null }> = [];
    for (const line of lines) {
      const isLast = line.trimEnd().endsWith(';');
      // Kind is the first quoted token; parent_kind is the last field.
      // Parsed positionally rather than by one big regex — the
      // descriptions carry commas, parens, quotes and em-dashes, and a
      // regex threading past all of them matched only the rows ending
      // in NULL.
      const kind = /^\s*\('([a-z_-]+)'/.exec(line)?.[1];
      if (!kind) {
        if (isLast) break;
        continue;
      }
      // Strip the row's closing `),` FIRST — otherwise the last comma
      // in the line is the trailing one and the field comes back empty.
      const inner = line.trim().replace(/\),?$/, '');
      const last = inner.slice(inner.lastIndexOf(',') + 1).trim();
      const parent = last.startsWith('NULL')
        ? null
        : (/^'([a-z_-]+)'/.exec(last)?.[1] ?? null);
      rows.push({ kind, parent });
      // Terminator checked AFTER parsing: the final row ends `NULL);`,
      // so breaking first silently dropped it — and it was `custom`,
      // one of the two kinds this whole test exists to catch.
      if (isLast) break;
    }
    // A parser that silently matched nothing — or only some rows —
    // would make every assertion below vacuous. It did exactly that
    // once.
    expect(rows.length).toBeGreaterThan(15);
    expect(rows.filter((r) => r.parent !== null).length).toBeGreaterThan(5);
    return rows;
  }

  it('claims every kind that is not an abstract root', () => {
    const rows = taxonomy();
    const hasChildren = new Set(rows.map((r) => r.parent).filter(Boolean) as string[]);
    const claimed = new Set(Object.values(APP_SUBJECT_KINDS).flat());

    const unclaimed = rows
      .filter((r) => !(r.parent === null && hasChildren.has(r.kind)))
      .map((r) => r.kind)
      .filter((k) => !claimed.has(k));

    expect(
      unclaimed,
      `concrete Subject kinds no app claims, so search never floats them: ` +
        unclaimed.join(', '),
    ).toEqual([]);
  });

  it('claims nothing the registry does not define', () => {
    const known = new Set(taxonomy().map((r) => r.kind));
    const stale = [...new Set(Object.values(APP_SUBJECT_KINDS).flat())].filter(
      (k): k is string => k !== undefined && !known.has(k),
    );
    expect(stale, `claimed but not a registered kind: ${stale.join(', ')}`).toEqual([]);
  });

  it('leaves the abstract roots unclaimed', () => {
    // The other direction: claiming `person` would rank a kind that
    // has no instances, which is noise in every result set.
    const claimed = new Set(Object.values(APP_SUBJECT_KINDS).flat());
    for (const root of ['person', 'object', 'intangible']) {
      expect(claimed.has(root), `${root} is abstract and should not be claimed`).toBe(
        false,
      );
    }
  });
});

describe('a surface that lists a department names it by its owner', () => {
  // Backlog 423a531d (2026-09-22). The Service queue and the Sales
  // pipeline were mounted with a hardcoded workflow kind —
  // `field-service` and `sale`, both authored only in a tenant's seed
  // bundle and published by no instance running another tenant — so
  // both rendered a title and a permanent "No jobs match", with
  // nothing holding the literal to anything. What a surface FILTERS on
  // is a DEPARTMENT, because a department's work is several protocols
  // (Sales runs receive-a-sponsorship AND receive-an-inquiry), so no
  // single kind could have expressed it even once corrected.
  //
  // Since car 2 of design 8c3e9599 that department is the entry's
  // OWNER, the one field that also places it under a tab. It was a
  // second `department` field until then, and on the Service queue the
  // two disagreed: tab `service`, filter `support`. The queue lists the
  // Service department's packets now, under the Service tab.
  const SEEDED_CODES = new Set(registryDepartments());

  it('every department owner is one the Class registry seeds', () => {
    for (const [key, e] of entries) {
      if (e.owner === 'home') continue;
      expect([key, SEEDED_CODES.has(e.owner!)]).toEqual([key, true]);
    }
  });

  // The two jobs-queue surfaces (Service queue, Sales pipeline) and the
  // parts surface (backlog 044dffa1) were pinned here until they went
  // with step 1 of design 01fa5bdf.

  // Backlog 4d4dc204 (2026-09-23, page audit 3f964c57 gap 2): /ux/finance
  // made no jobs read and linked nowhere that did, so a receive-a-payout
  // packet waiting at its post step for 2.6 days was on no finance
  // surface. Same field, same reason as the warehouse's above.
  it('the finance surface lists the finance department it sits under', () => {
    expect(ROUTE_CATALOG.finance.owner).toBe('finance');
  });
});

describe('standalone Views retirement', () => {
  it('removes Views without removing the remaining Home catalog entries', () => {
    for (const path of ['/ux/views', '/views']) {
      expect(withRoster(parseRoute(path, '', ADDRESSES), path, SEEDED, ADDRESSES).kind).toBe('notFound');
    }
    const rows = Object.values(ROUTE_CATALOG);
    expect(rows.some((row) => row.path === '/ux/views' || row.label === 'Views')).toBe(false);
    // 'jobs' (All jobs) went with step 1 of design 01fa5bdf.
    for (const id of ['inbox', 'schedule']) {
      expect(rows.some((row) => row.id === id)).toBe(true);
    }
  });
});

describe('every page names the station that hosts it', () => {
  // Car 1 of design deba3000 (backlog 03fb75c8, David 2026-11-10): a
  // page has one host station, declared beside its registration here,
  // by the station's ID — the one field of a station row that never
  // changes (its name, and with it its address, can). These tests read
  // the rows from infra/platform/stations/, the files the platform seed
  // publishes, so the ids the catalog spells are held to the rows they
  // name (CLAUDE.md §9a) rather than restated.
  type StationRow = Readonly<{ id: string; name: string; status: string; parent?: string }>;
  const STATIONS_DIR = new URL('../../../../infra/platform/stations/', import.meta.url);
  const rows: ReadonlyArray<StationRow> = readdirSync(STATIONS_DIR)
    .filter((f) => f.endsWith('.toml'))
    .flatMap((f) => {
      const parsed = Bun.TOML.parse(readFileSync(new URL(f, STATIONS_DIR), 'utf8')) as {
        station?: ReadonlyArray<StationRow>;
      };
      return parsed.station ?? [];
    });
  const byId = new Map(rows.map((r) => [r.id, r]));
  // A reader that silently matched nothing would make every assertion
  // below vacuous.
  expect(rows.length).toBeGreaterThan(10);

  /// The root: the one row naming no parent (validate_station_tree
  /// refuses a tree with any other number of roots).
  const roots = rows.filter((r) => r.parent === undefined);
  expect(roots.length).toBe(1);
  const ROOT = roots[0]!;

  /// The station and every station above it, nearest first.
  function lineage(id: string): ReadonlyArray<StationRow> {
    const row = byId.get(id);
    return row === undefined ? [] : [row, ...(row.parent ? lineage(row.parent) : [])];
  }

  it('every station the catalog names is an active row, under the name it is given here', () => {
    for (const [name, id] of Object.entries(STATION)) {
      const row = byId.get(id);
      expect([name, row?.name, row?.status]).toEqual([name, name, 'active']);
    }
  });

  it('every host is one of those stations, spelled by id', () => {
    const named = new Set<string>(Object.values(STATION));
    for (const [key, e] of entries) {
      if (e.host === undefined) continue;
      expect([key, named.has(e.host)]).toEqual([key, true]);
    }
  });

  // Design deba3000 §1: company-level pages sit at the root. Since step
  // 1 of design 01fa5bdf the /it landing is only the alarms board (and
  // the train yard's release and cancel board under `at`): every
  // station's alarms, which is the company's view, not one station's.
  it('the company-level pages are hosted at the root', () => {
    expect<string | undefined>(ROUTE_CATALOG['system-yard'].host).toBe(ROOT.id);
  });

  // Design 01fa5bdf (David 2026-11-17): Executive dissolves — its
  // personal parts go to /me, the rest to the root (878fd1e7). A page
  // hosted there would be one more thing that retirement has to move.
  it('no page is hosted at Executive or beneath it', () => {
    for (const [key, e] of entries) {
      if (e.host === undefined) continue;
      expect([key, lineage(e.host).some((s) => s.name === 'executive')]).toEqual([key, false]);
    }
  });

  // The host is placement; the owner is the tab and the packets the
  // page lists (design 8c3e9599). Where the owner is itself a station of
  // the tree (Finance), the host must not take the page somewhere else.
  it('a page whose owner is a station is hosted at that station or beneath it', () => {
    const stationNames = new Set(rows.map((r) => r.name));
    for (const [key, e] of entries) {
      if (e.owner === undefined || !stationNames.has(e.owner)) continue;
      const above = e.host === undefined ? [] : lineage(e.host).map((s) => s.name);
      expect([key, above.includes(e.owner)]).toEqual([key, true]);
    }
  });

  // The report (design deba3000 §Shape): the pages with no host yet.
  // It fails nothing — the migration is incremental, a few pages a car —
  // and prints `unhosted-pages:<n>`, a number that falls, the way
  // infra/lint/rows-still-in-v1-words.sh prints `v1-rows:<n>`. When it
  // reaches zero, car 3 of the design makes `host` required and this
  // becomes a pin.
  // A person's page (`person`, at /me) is hosted at the signed-in
  // person's own station, which is no row of the company tree (backlog
  // 5cb56bb3), so it is not on the report.
  it('the unhosted report lists exactly the catalog entries with no host', () => {
    const report = unhostedPages();
    expect(report.map((e) => e.id)).toEqual(
      Object.values(ROUTE_CATALOG)
        .filter((e) => e.host === undefined && e.person !== true)
        .map((e) => e.id),
    );
    console.info(`unhosted-pages:${report.length} ${report.map((e) => e.id).join(' ')}`);
  });
});
