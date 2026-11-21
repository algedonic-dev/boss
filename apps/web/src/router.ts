// Tiny router — every path the app answers, and not-found for the rest.

import { safeSitePath } from '@boss/web-kit/links';
import { readFinanceView, type FinanceView } from './finance/financeQuery';
import { ROUTE_CATALOG, departmentLanding, type NavItem } from './shell/nav-catalog';
import { ADDRESSES_LOADING, matchPath, pagePath, rootPagePath, stationAtPath, type StationAddresses } from './shell/page-address';
import { treeQuery } from './it/tree/station-tree';

// Removed with step 1 of design 01fa5bdf (David, 2026-11-17: "IT
// disappears, the old map is pulled out, and the /ux business pages are
// removed; when unsure, remove"): the /ux business pages — accounts, assets,
// catalog, exec, marketing-assets, parts, people, products, qa, sales,
// service (and its schedule), shipping, shop, support, warehouse, and
// the jobs list (review df7aeb9c; a packet's own page stays) —
// /people/hr, /it/operate (incidents), Atlas, Bottlenecks,
// /it/design/experiments and /it/registry/policy. Each path now falls
// to not-found like any other; a page comes back as a station's page
// when a protocol runs there.
export type Route =
  /// The landing page (the System Model live view), at /ux/system-model.
  | { kind: 'home' }
  /// A path nothing in the app answers — rendered in place, naming the
  /// path as it was asked for (design ee3a3a2f). `department` is set
  /// only by the registry check (sections.ts `withRoster`): the path is
  /// a page, and the department that owns it is not a row on this
  /// instance. The router never sets it; it cannot see the registry.
  ///
  /// `why` is set when the station tree could not be read (backlog
  /// 0bc2eb68): the path may be a page some station hosts, and nothing
  /// could place it — said, rather than read as a typo.
  | { kind: 'notFound'; path: string; department?: string; why?: string }
  /// A path only the station tree can place — a hosted page or a
  /// station's own address — asked before the tree has answered. Never a
  /// guessed prefix; it re-parses when the read lands (backlog 0bc2eb68).
  | { kind: 'resolving'; path: string }
  | { kind: 'login' }
  | { kind: 'authAdmin' }
  | { kind: 'me' }
  | { kind: 'jobDetail'; jobId: string }
  /// Full results for a global-search query. Lives in Home because
  /// Home is the cross-app surface — the chrome dropdown is scoped to
  /// the app you are in, this is the unscoped view it escalates to.
  | { kind: 'search'; q: string }
  /// The codebase trend — the daily metrics packets, rendered.
  | { kind: 'systemCodebase' }
  /// Full-page step surface. A step whose UX is a plugin gets the
  /// whole viewport instead of a panel inside the job page — review
  /// and authoring steps are reading tasks, and reading competes
  /// badly with a sidebar and a step list.
  | {
      kind: 'stepFocus';
      jobId: string;
      stepId: string;
      /// In-app path the lens that opened this step wants Back to
      /// return to, with a label for it. Absent for a deep link.
      from?: string;
      fromLabel?: string;
    }
  | { kind: 'vendors' }
  | { kind: 'vendor'; vendorLookup: string }
  /// The tab, and the entry or fact a link opened, read back off the
  /// query so a posted entry is shown and a tab survives back and
  /// reload (2ab44d55).
  | { kind: 'finance'; view: FinanceView }
  | { kind: 'newInvoice' }
  | { kind: 'newJournalEntry' }
  | { kind: 'invoice'; invoiceId: string }
  | { kind: 'vendorInvoice'; vendorInvoiceId: string }
  // 'itSim' retired 2026-05-03 with boss-sim-api (HumanWorker step 9b).
  // 'systemMonitoring' (the index page), 'systemModel', 'systemMap' and
  // 'systemFlow' retired 2026-08-31 with the IT consolidation
  // (1f6d55e0): the monitoring index became the Operate tab strip, and
  // the map/flow renderings folded into Atlas.
  | { kind: 'systemKb' }
  | { kind: 'systemMonitoringPerf' }
  | { kind: 'systemMonitoringEvents' }
  | { kind: 'workflows' }
  | { kind: 'workflowNew' }
  | { kind: 'workflowDesign'; jobId: string }
  | { kind: 'workflowDetail'; kindSlug: string }
  | { kind: 'systemStepPlugins' }
  | { kind: 'systemStepPluginDetail'; pluginSlug: string }
  | { kind: 'systemDesign' }
  /** The alarms board and the train yard's controls, at /alarms (and at
   *  /it while the registry keeps IT's row, as IT's landing) — all that
   *  stays of the Department Map (design e765b3fc) after step 1 of
   *  design 01fa5bdf: the alarms, and under `at` (`/alarms?at=dock`) the
   *  one board that holds the release and cancel controls, until the
   *  station page takes them (that design's step 4). */
  | { kind: 'systemYard'; at?: string }
  // 'systemConsole' (/it/console) retired with the transit map (R3,
  // backlog 23a4b4d7): its review screen read the floor at a past
  // sequence, and the visuals show real, current activity only (David,
  // 2026-11-08 00:50Z).
  /** The station tree floor (T7 of design 4b27a65a, backlog 94a9dad4):
   *  one card drawn recursively from the tree read under `under` (an id
   *  or a name; absent, the root), `depth` levels deep, with the
   *  recorded moves riding it. The root's page, at /tree, since backlog
   *  92bfcb04 (it was /it/tree). */
  | { kind: 'systemTree'; under?: string; depth?: number }
  /** The station page (design c4854fd4 E2, backlog 72d58e00): what
   *  opening one station shows — its constraints, its parts, In /
   *  Working / Out — at /tree/<name> (/it/tree/<name> until backlog
   *  92bfcb04), and at the station's own
   *  address in the tree where no page is hosted there (backlog
   *  0bc2eb68: /engineering, /engineering/gates). */
  | { kind: 'systemStation'; station: string }
  /** A station's Resources view (backlog 87575d69, car C of the station
   *  visuals): its pools as a rail of units drawn where the claims hold
   *  them, at /tree/<name>/resources — the Work / Resources toggle on
   *  the station page. */
  | { kind: 'systemStationResources'; station: string }
  // 'systemYardFloor' (/it/yard/<region>), 'systemCrew' (/it/crew),
  // 'systemYardStatus', 'systemMonitoringConductor', 'systemFeedback'
  // and 'systemBacklog' retired 2026-09-25 with car N3 of design
  // e765b3fc: each page's content is a selection's panel on the
  // Department Map now (MapPage.svelte says which), and the paths are
  // not found like any other.
  /// The hardware registry, declared beside observed (59ef456a).
  | { kind: 'systemEstate' }
  | { kind: 'systemSubjects' }
  /// The Drift tab on Registry — the newest maintenance-protocol-drift
  /// packet rendered (4ae9969e, car 2 of 8f4e9cc0).
  | { kind: 'systemRegistryDrift' }
  /// The Agents tab on Registry — the agents registry as a directory
  /// (backlog 62988516; David 2026-09-26: agents get a page in IT,
  /// never the People roster).
  | { kind: 'systemAgents' }
  /// The Credentials tab on Registry — the credentials registry drawn,
  /// its lifecycle filed as packets (backlog 851259b9; design 76155676
  /// step 3). It holds no value and reads none.
  | { kind: 'systemCredentials' }
  | { kind: 'dispatcherRules' }
  | { kind: 'dispatcherRulesList' }
  | { kind: 'dispatcherRuleEdit'; ruleName: string }
  | { kind: 'inbox' }
  | { kind: 'myCalendar' }
  /// A department's own jobs — its in / working / out over the
  /// packets whose workflow declares it (cc76f755), at /<code>/jobs,
  /// and at /<code> for a department whose catalog owns no landing. The
  /// code is registry data, so the route carries it; the router knows
  /// no department by name, and whether the code is a row on this
  /// instance is the registry's answer, not the router's.
  | { kind: 'department'; code: string }
  | { kind: 'manual' }
  | { kind: 'manualSection'; slug: string }
  | { kind: 'po'; poId: string }
  | { kind: 'watchlist' };

/// The path as the router matches it: the /dashboard mount and a
/// trailing slash dropped.
/// Exported for the interaction crawl, which asks "does this link land
/// on /?" of the router's own normalisation rather than a copy of it
/// (backlog 7c69a45f).
export function routable(pathname: string): string {
  return pathname.replace(/^\/dashboard/, '').replace(/\/$/, '') || '/';
}

/// One path segment, decoded — or as it was typed when its escape is
/// malformed, so a mistyped URL renders a page instead of throwing out
/// of the router.
function segment(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/// The one door an unmatched path offers (design ee3a3a2f Q3). No search
/// box and no "did you mean" — a suggestion is a guess, which is what the
/// not-found page refuses. A path under /it led back to the alarms board
/// until backlog 92bfcb04 retired the prefix with IT (design df0ecd8b):
/// it is a path like any other now, and leads to My Day.
///
/// `department` is set when the path IS a page, owned by a department
/// this instance does not have (sections.ts `withRoster`, backlog
/// 64656a46). Its own department is then the one place the door cannot
/// lead, so it leads Home — "Service is not a department on this
/// instance", with one way back (e543c6fe).
export function notFoundBack(pathname: string, department?: string): { href: string; label: string } {
  if (department !== undefined) return { href: '/', label: 'Back to Home' };
  return { href: myDayPath(), label: 'Back to My Day' };
}

/// The person's own station's pages (design 01fa5bdf step 3, backlog
/// 5cb56bb3), each by the catalog entry that places it under /me — the
/// catalog spells the path, so this table never does. Matched on the
/// path as asked, before anything reads the station tree: /me is the
/// signed-in person's, never read off the company's tree (whose `me`
/// row is one station for every person, infra/platform/stations/me.toml), so an
/// unread or unreadable tree never takes a person's own pages away.
/// Whose page it is comes from the session (MePage, InboxPage and
/// MyCalendarPage each read the signed-in user), never from the URL.
const PERSONAL: ReadonlyArray<Readonly<{ page: keyof typeof ROUTE_CATALOG; route: () => Route }>> = [
  { page: 'me', route: () => ({ kind: 'me' }) },
  { page: 'inbox', route: () => ({ kind: 'inbox' }) },
  { page: 'schedule', route: () => ({ kind: 'myCalendar' }) },
];

/// My Day's own address, the person's station: /me.
function myDayPath(): string {
  return ROUTE_CATALOG.me.path ?? '/';
}

/// Whether a path is at or under the person's station.
function underMe(raw: string): boolean {
  const me = myDayPath();
  return raw === me || raw.startsWith(`${me}/`);
}

/// A catalogued page's own route, or one of its sub-pages, keyed by the
/// catalog entry that HOSTS it. The router never spells a hosted page's
/// prefix: the full pattern is the host station's address in the tree
/// plus the entry's slug (shell/page-address.ts `pagePath`) plus `rest`,
/// so a re-parented station moves every row here with it (backlog
/// 0bc2eb68, design deba3000). `:name` takes one decoded segment, a
/// trailing `*name` the rest of the path.
type HostedRoute = Readonly<{
  page: keyof typeof ROUTE_CATALOG;
  rest: string;
  route: (params: Readonly<Record<string, string>>, search: string) => Route;
}>;

/// Tried in order, and the order is the old branch order's reason kept:
/// every specific row before a wildcard that could eclipse it — the
/// workflow detail's `/:kindSlug` under Registry is LAST, because
/// `registry/step-plugins`, `registry/rules` and the rest are segments
/// it would read as a workflow kind. Every single-id wildcard takes ONE
/// segment (design ee3a3a2f Q6: a greedy one turned a deeper path into a
/// convincing "missing X"); the manual's `*slug` is the one exception,
/// because a manual slug may hold a slash (the content API routes it as
/// `{*slug}`).
const HOSTED: ReadonlyArray<HostedRoute> = [
  // The alarms board — what stays of the Department Map (design e765b3fc)
  // after step 1 of design 01fa5bdf — and, under `at` (`?at=dock`), the
  // one board holding the release and cancel controls, until the station
  // page takes them.
  { page: 'system-yard', rest: '', route: (_, search) => yardRoute(search) },
  // The station tree floor, a station's page by name and its Resources
  // view: the root's pages since backlog 92bfcb04 (car 3 of design
  // df0ecd8b), at /it/tree until then. Each takes ONE segment, so a
  // deeper path is not found rather than a convincing missing station.
  { page: 'system-tree', rest: '', route: (_, search) => ({ kind: 'systemTree', ...treeQuery(search) }) },
  { page: 'system-tree', rest: '/:station', route: (p) => ({ kind: 'systemStation', station: p.station! }) },
  { page: 'system-tree', rest: '/:station/resources', route: (p) => ({ kind: 'systemStationResources', station: p.station! }) },
  // The Churn watchlist, the root's company-wide list (backlog 5cb56bb3).
  { page: 'accounts', rest: '', route: () => ({ kind: 'watchlist' }) },
  // Operate — the audit log and its performance tab.
  { page: 'system-monitoring', rest: '', route: () => ({ kind: 'systemMonitoringEvents' }) },
  { page: 'system-performance', rest: '', route: () => ({ kind: 'systemMonitoringPerf' }) },
  // Registry — one surface over the registry family.
  { page: 'workflows', rest: '', route: () => ({ kind: 'workflows' }) },
  { page: 'workflows', rest: '/new', route: () => ({ kind: 'workflowNew' }) },
  { page: 'workflows', rest: '/authoring/:jobId', route: (p) => ({ kind: 'workflowDesign', jobId: p.jobId! }) },
  { page: 'system-step-plugins', rest: '', route: () => ({ kind: 'systemStepPlugins' }) },
  { page: 'system-step-plugins', rest: '/:pluginSlug', route: (p) => ({ kind: 'systemStepPluginDetail', pluginSlug: p.pluginSlug! }) },
  { page: 'system-dispatcher-rules', rest: '', route: () => ({ kind: 'dispatcherRulesList' }) },
  { page: 'system-dispatcher-rule', rest: '', route: (p) => ({ kind: 'dispatcherRuleEdit', ruleName: p.ruleName! }) },
  { page: 'system-dispatcher', rest: '', route: () => ({ kind: 'dispatcherRules' }) },
  { page: 'system-subjects', rest: '', route: () => ({ kind: 'systemSubjects' }) },
  { page: 'system-registry-drift', rest: '', route: () => ({ kind: 'systemRegistryDrift' }) },
  { page: 'system-agents', rest: '', route: () => ({ kind: 'systemAgents' }) },
  { page: 'system-credentials', rest: '', route: () => ({ kind: 'systemCredentials' }) },
  // Design — the reviews. The Codebase is its own row (9827c699).
  { page: 'system-design', rest: '', route: () => ({ kind: 'systemDesign' }) },
  { page: 'system-codebase', rest: '', route: () => ({ kind: 'systemCodebase' }) },
  { page: 'system-estate', rest: '', route: () => ({ kind: 'systemEstate' }) },
  { page: 'system-kb', rest: '', route: () => ({ kind: 'systemKb' }) },
  { page: 'auth-admin', rest: '', route: () => ({ kind: 'authAdmin' }) },
  // Finance — the tab, and the entry or fact a link opened, read back
  // off the query (2ab44d55). /invoices/new before the invoice wildcard,
  // which would otherwise read "new" as an id. Each detail page sits
  // under its slug, never <finance>/<id>.
  { page: 'finance', rest: '', route: (_, search) => ({ kind: 'finance', view: readFinanceView(search) }) },
  { page: 'finance', rest: '/invoices/new', route: () => ({ kind: 'newInvoice' }) },
  { page: 'finance', rest: '/journal-entries/new', route: () => ({ kind: 'newJournalEntry' }) },
  { page: 'finance', rest: '/invoices/:invoiceId', route: (p) => ({ kind: 'invoice', invoiceId: p.invoiceId! }) },
  { page: 'vendors', rest: '', route: () => ({ kind: 'vendors' }) },
  { page: 'vendors', rest: '/:vendorLookup', route: (p) => ({ kind: 'vendor', vendorLookup: p.vendorLookup! }) },
  { page: 'purchase-order', rest: '', route: (p) => ({ kind: 'po', poId: p.poId! }) },
  { page: 'vendor-invoice', rest: '', route: (p) => ({ kind: 'vendorInvoice', vendorInvoiceId: p.vendorInvoiceId! }) },
  { page: 'manual', rest: '', route: () => ({ kind: 'manual' }) },
  { page: 'manual', rest: '/*slug', route: (p) => ({ kind: 'manualSection', slug: p.slug! }) },
  // Workflow detail LAST — its wildcard would eclipse every specific
  // registry row above.
  { page: 'workflows', rest: '/:kindSlug', route: (p) => ({ kind: 'workflowDetail', kindSlug: p.kindSlug! }) },
];

/// Every hosted page's own pattern and sub-page patterns, as the tree
/// places them now — exported for the pins that walk every row.
export function hostedPatterns(at: StationAddresses): ReadonlyArray<Readonly<{ page: string; pattern: string }>> {
  return HOSTED.flatMap((h) => {
    const base = pagePath(ROUTE_CATALOG[h.page] as NavItem, at);
    return base === null ? [] : [{ page: h.page, pattern: base === '/' ? h.rest || '/' : `${base}${h.rest}` }];
  });
}

/// The yard's route: the board, and the station a link selected.
function yardRoute(search: string): Route {
  const r: Route = { kind: 'systemYard' };
  const at = new URLSearchParams(search).get('at');
  if (at) (r as { at?: string }).at = at;
  return r;
}

/// What a catalog entry renders, found without the tree: a hosted page
/// by its own row above, an unhosted one by the path it spells. The
/// department landing renders through this, so `/it` is IT's landing
/// whether or not the tree has answered.
function routeOfPage(entry: NavItem, search: string, at: StationAddresses): Route | null {
  if (entry.host === undefined) return parseRoute(entry.path, search, at);
  const own = HOSTED.find((h) => h.page === entry.id && h.rest === '');
  return own === undefined ? null : own.route({}, search);
}

/// `search` is the query string (`window.location.search` at the SPA's
/// call site, leading `?` and all). It is an ARGUMENT, not a read of
/// `window`: the router read that global until backlog cb211b39, so
/// every caller outside a browser had to plant a window first, and one
/// test file's planted window made another file pass only when bun
/// happened to run it second. Omitted, it is no query — which is what a
/// path with no query means.
///
/// `at` is the station tree's addresses (shell/page-address.ts), also an
/// argument and for the same reason: the SPA passes its one read of the
/// tree (shell/stationAddresses.svelte.ts). Omitted, the tree has not
/// answered, and a path only the tree could place is `resolving`.
export function parseRoute(pathname: string, search = '', at: StationAddresses = ADDRESSES_LOADING): Route {
  const raw = routable(pathname);
  if (raw === '/login') return { kind: 'login' };

  // ===== The person's own station — /me (backlog 5cb56bb3) =====
  //
  // My Day, the inbox and the calendar, at the catalog's addresses and
  // needing no tree. Any other path under /me is no page: said at once,
  // rather than `resolving` while a tree that cannot place it loads.
  if (underMe(raw)) {
    const mine = PERSONAL.find((r) => ROUTE_CATALOG[r.page].path === raw);
    return mine === undefined ? { kind: 'notFound', path: pathname } : mine.route();
  }
  // '/map' (the flat company map, backlog 759da6bc) and
  // '/it/floor/<code>' (the department floor, 858e62aa) retired 2026-11-09:
  // the recursive root station view (/tree) is the company index;
  // both addresses now fall through to notFound.

  // ===== /it/* — RETIRED with IT as a department =====
  //
  // IT retires as a department (design df0ecd8b, David 2026-11-10): its
  // work lives in Engineering, Operations and the company root, and each
  // page moved to its host station. The family pages went with backlog
  // 0bc2eb68; the station tree floor, a station's page by name and its
  // Resources view, the last pages /it held, are the root's at /tree
  // since backlog 92bfcb04. No path under /it is special any more, and
  // none has an alias (David, 2026-09-25: no shims before 1.0.0). /it and
  // /it/jobs are a department's address and jobs view like any other,
  // answered below for as long as the registry keeps IT's row (car 2 of
  // that design retires it).

  // ===== User Experiences perspective — /ux/* (canonical); bare / is the public alias for the UX home.
  // Unprefixed legacy paths still resolve here (defensive). =====
  const p = raw === '/' || raw === '/ux' ? '/' : raw.startsWith('/ux/') ? raw.slice('/ux'.length) : raw;
  // A department's address (design 8c3e9599 §5, car 2): /<code> and
  // /<code>/jobs, for any code — never under /ux, which is retiring and
  // was never a department's (a /ux/<code> path is not found).
  const unprefixed = !raw.startsWith('/ux/');
  // /<code>/jobs FIRST: a one-segment family wildcard below (as
  // /sales/:jobId and /people/:empId were until design 01fa5bdf) would
  // read "jobs" as an id and render a missing record.
  const deptJobsM = unprefixed ? p.match(/^\/([^/]+)\/jobs$/) : null;
  if (deptJobsM) return { kind: 'department', code: segment(deptJobsM[1]!) };
  // User Experiences lands on My Day by default — the actor's personal
  // work view, not a marketing landing.
  //
  // EVERY SINGLE-ID WILDCARD BELOW TAKES ONE SEGMENT, `([^/]+)`. They
  // were greedy `(.+)` until design ee3a3a2f (Q6), and a deeper path
  // under a list became a convincing "missing X": /ux/accounts/
  // agreements/<id> rendered the ACCOUNT page for accountId
  // "agreements/<id>". An id holding a slash arrives percent-encoded
  // (entityHref encodes every id) and is decoded by `segment`.
  //
  // `/` and `/ux` render My Day in place; its own address is /me, above.
  // /ux/me, /ux/inbox, /inbox, /ux/calendar/me and /calendar/me answered
  // here until backlog 5cb56bb3 moved the person's pages to /me, and
  // they retired with no alias (David, 2026-09-25: no shims before
  // 1.0.0).
  if (p === '/') return { kind: 'me' };
  // The landing page's own door. The catch-all was its ONLY way in until
  // design ee3a3a2f (Q5) — `/` and `/ux` are My Day — so turning the
  // catch-all into a not-found would have orphaned a real page.
  if (p === '/system-model') return { kind: 'home' };
  // /ux/departments/<code> retired with car 2 of design 8c3e9599 — the
  // jobs view is /<code>/jobs above, with no alias (David, 2026-09-25).
  // /manual is hosted at Engineering since backlog 0bc2eb68, and answers
  // at its address below, as the Churn watchlist does at the root since
  // backlog 5cb56bb3 (it was spelled here, as /watchlist and
  // /ux/watchlist, until then).

  if (p === '/search') {
    const sp = new URLSearchParams(search);
    return { kind: 'search', q: sp.get('q') ?? '' };
  }

  // /ux/jobs, the jobs list and its new-job form, went with step 1 of
  // design 01fa5bdf too (review df7aeb9c): it falls to not-found. A
  // packet's own page and its step pages stay below — every packet link,
  // search hit and step plugin opens one there. A record view belongs to
  // no one station, so it is the company root's short record path
  // (design deba3000: /jobs/:id).
  //
  // Before /jobs/([^/]+) below — which, while it was the greedy
  // /jobs/(.+), swallowed the whole `{id}/steps/{stepId}` tail as a job id.
  const sfm = p.match(/^\/jobs\/([^/]+)\/steps\/([^/]+)$/);
  if (sfm) {
    const sp = new URLSearchParams(search);
    const r: Route = { kind: 'stepFocus', jobId: sfm[1]!, stepId: sfm[2]! };
    // Where "back" goes, and what to call it. Only the lens that sent
    // the operator here knows — the step surface cannot infer it, and
    // guessing from the Job's kind would put a per-workflow branch in
    // core routing (CLAUDE.md 9). Leading-slash check keeps this an
    // in-app path: a `from` naming another origin would turn a Back
    // button into an open redirect.
    const from = sp.get('from');
    const fromLabel = sp.get('from_label');
    // safeSitePath also refuses `/\host` and a control character, each
    // of which a browser reads as `//host` (4f1f7698).
    const back = safeSitePath(from);
    if (back !== null) {
      (r as { from?: string }).from = back;
      if (fromLabel) (r as { fromLabel?: string }).fromLabel = fromLabel;
    }
    return r;
  }

  const jm = p.match(/^\/jobs\/([^/]+)$/);
  if (jm) return { kind: 'jobDetail', jobId: segment(jm[1]!) };

  // A department's landing, for a department whose catalog names one:
  // what /<code> renders is catalog data — the entry the department
  // flags `landing`, else its first sidebar entry (nav-catalog
  // `departmentLanding`) — rendered in place under the department's own
  // address, the way an entry's own path does. Found without the tree,
  // so /finance is Finance's page whether or not the tree has answered.
  const landingM = unprefixed ? p.match(/^\/([^/]+)$/) : null;
  const landing = landingM ? departmentLanding(segment(landingM[1]!)) : undefined;
  if (landing !== undefined) {
    const r = routeOfPage(landing, search, at);
    // An unhosted entry whose own path is this one would land on itself
    // forever; such a path has no page of its own, so it is the jobs view.
    if (r !== null && !(landing.host === undefined && routable(landing.path) === raw)) return r;
  }

  // ===== The pages stations host (backlog 0bc2eb68) =====
  //
  // A hosted page answers at its host station's address in the tree plus
  // its slug — /finance/vendors, /engineering/registry, /operations/estate
  // — and a station's own address with no page of its own is that
  // station's page (/engineering/gates; /engineering through `withRoster`,
  // below). Only the tree can
  // say which station an address is, so until it answers such a path is
  // `resolving`, and when its read failed the not-found page says why
  // rather than guessing a prefix.
  if (unprefixed && at.kind === 'ready') {
    for (const h of HOSTED) {
      const base = pagePath(ROUTE_CATALOG[h.page] as NavItem, at);
      if (base === null) continue;
      const params = matchPath(base === '/' ? h.rest : `${base}${h.rest}`, raw);
      if (params !== null) return h.route(params, search);
    }
    // A station inside a department. A top-level station's address is one
    // segment, which is a department's address first: it is the station's
    // page only where the registry has no such department (sections.ts
    // `withRoster`), because the router cannot see the registry.
    const station = stationAtPath(at, raw);
    if (station !== null && station.prefix.lastIndexOf('/') > 0) return { kind: 'systemStation', station: station.name };
  }
  if (unprefixed && at.kind === 'loading') return { kind: 'resolving', path: pathname };

  // A department nobody built a landing for lands on its jobs view, and
  // is not found once the registry says it is no row (sections.ts
  // `withRoster`): the router cannot see the registry, so it cannot tell
  // a department nobody built a page for from a typo. A root page's
  // address is one bare segment too (/tree, /alarms), and the catalog
  // names it without the tree (page-address.ts `rootPagePath`): with the
  // tree unread it is the page nothing could place, said below, never a
  // department the registry then denies (backlog 92bfcb04).
  const rootPage = Object.values(ROUTE_CATALOG).some((e) => rootPagePath(e as NavItem) === raw);
  if (landingM && !(at.kind === 'failed' && rootPage)) return { kind: 'department', code: segment(landingM[1]!) };

  // Nothing answers this path, and the page says so, naming it as it was
  // asked for (design ee3a3a2f). This returned the landing page until
  // then, so a dead link rendered a real, working, plausible page and
  // the reader concluded they had misremembered (backlog c4f2ae24).
  // Rendered in place, never redirected: a redirect replaces the
  // evidence in the address bar with a working page, which is the defect.
  // When the tree could not be read, the page says that too: the path
  // may be a station's page nobody could place.
  return unprefixed && at.kind === 'failed'
    ? { kind: 'notFound', path: pathname, why: `the station tree could not be read: ${at.why}` }
    : { kind: 'notFound', path: pathname };
}

// `href` (honors the /dashboard mount) + `navigate` (pushState SPA nav)
// now live in the shared @boss/web-kit/nav module. Re-exported here so
// the ~55 files importing them from '../router' need no change.
export { href, navigate } from '@boss/web-kit/nav';
