// Tiny router — every path the app answers, and not-found for the rest.

import { safeSitePath } from '@boss/web-kit/links';
import { readFinanceView, type FinanceView } from './finance/financeQuery';
import { departmentLanding } from './shell/nav-catalog';
import { treeQuery } from './it/tree/station-tree';
import { stationOfPath } from './it/tree/station-page';

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
  | { kind: 'notFound'; path: string; department?: string }
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
  /** The alarms board and the train yard's controls, at /it — all that
   *  stays of the Department Map (design e765b3fc) after step 1 of
   *  design 01fa5bdf: the alarms, and under `at` (`/it?at=dock`) the
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
   *  recorded moves riding it. Additive: the Department Map stays. */
  | { kind: 'systemTree'; under?: string; depth?: number }
  /** The station page (design c4854fd4 E2, backlog 72d58e00): what
   *  opening one station shows — its constraints, its parts, In /
   *  Working / Out — at /it/tree/<name>. */
  | { kind: 'systemStation'; station: string }
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

function underIt(raw: string): boolean {
  return raw === '/it' || raw.startsWith('/it/');
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

/// The one door an unmatched path offers: back to the department it was
/// under (design ee3a3a2f Q3). No search box and no "did you mean" — a
/// suggestion is a guess, which is what the not-found page refuses.
///
/// `department` is set when the path IS a page, owned by a department
/// this instance does not have (sections.ts `withRoster`, backlog
/// 64656a46). Its own department is then the one place the door cannot
/// lead, so it leads Home — "Service is not a department on this
/// instance", with one way back (e543c6fe).
export function notFoundBack(pathname: string, department?: string): { href: string; label: string } {
  if (department !== undefined) return { href: '/', label: 'Back to Home' };
  return underIt(routable(pathname))
    ? { href: '/it', label: 'Back to the alarms board' }
    : { href: '/ux', label: 'Back to My Day' };
}

/// `search` is the query string (`window.location.search` at the SPA's
/// call site, leading `?` and all). It is an ARGUMENT, not a read of
/// `window`: the router read that global until backlog cb211b39, so
/// every caller outside a browser had to plant a window first, and one
/// test file's planted window made another file pass only when bun
/// happened to run it second. Omitted, it is no query — which is what a
/// path with no query means.
export function parseRoute(pathname: string, search = ''): Route {
  const raw = routable(pathname);
  if (raw === '/login') return { kind: 'login' };
  // '/map' (the flat company map, backlog 759da6bc) and
  // '/it/floor/<code>' (the department floor, 858e62aa) retired 2026-11-09:
  // the recursive root station view (/it/tree) is the company index;
  // both addresses now fall through to notFound.

  // ===== The IT department — /it/* =====
  //
  // Six surfaces (the 2026-08-31 consolidation, packet 1f6d55e0):
  // the yard IS the landing, Operate / Registry / Design carry their
  // families as tabs, Estate and the KB stand alone. /system is GONE
  // — David's Q1/Q4 verdicts: department-first, and "we don't need to
  // worry about legacy users. It is just me" — which reverses the
  // "kept permanently" promise the old alias comment made (feedback
  // 0fc8b216 got the /it half; this finishes it). A /system path now
  // falls through to the catch-all like any other unknown route.
  if (underIt(raw)) {
    const p = raw.slice('/it'.length) || '/';
    // 1. The landing is the yard — delivery truth first. Since design
    //    0524fc95 (car 2) the landing is the yard's MAP. Since design
    //    e765b3fc (car N1) it is the DEPARTMENT MAP: the map stays on
    //    top and a selection opens its detail below, named in the query
    //    so a link says what it selects. A query, not a path: the map is
    //    not torn down and rebuilt by a selection. Car N3 retired the
    //    floor pages at /it/yard[/<region>] and every page whose content
    //    moved into a selection's panel — /it/crew, yard status, the
    //    conductor's feed, the feedback and backlog boards — and with
    //    them the four aliases that still answered (/it/yard,
    //    /it/operate/receiving, /it/operate/marshalling,
    //    /it/design/codebase). No alias and no redirect (David,
    //    2026-09-25: no shims before 1.0.0): each falls to not-found
    //    below, whose one door is the Department Map. Step 1 of design
    //    01fa5bdf (2026-11-17) removed the map itself; what answers here
    //    is the alarms board and, under `at`, the board holding the
    //    release and cancel controls, until the station page takes them.
    if (p === '/') {
      const r: Route = { kind: 'systemYard' };
      const at = new URLSearchParams(search).get('at');
      if (at) (r as { at?: string }).at = at;
      return r;
    }
    if (p === '/tree') return { kind: 'systemTree', ...treeQuery(search) };
    if (p.startsWith('/tree/')) {
      const station = stationOfPath(p.slice('/tree/'.length));
      if (station !== null) return { kind: 'systemStation', station };
    }
    // 2. Operate — the audit log and performance tabs. /it/operate
    //    (incidents), Atlas and Bottlenecks went with step 1 of design
    //    01fa5bdf.
    if (p === '/operate/audit') return { kind: 'systemMonitoringEvents' };
    if (p === '/operate/perf') return { kind: 'systemMonitoringPerf' };
    // 3. Registry — one surface over the registry family.
    if (p === '/registry') return { kind: 'workflows' };
    if (p === '/registry/new') return { kind: 'workflowNew' };
    const jkDesignM = p.match(/^\/registry\/authoring\/([^/]+)$/);
    if (jkDesignM) return { kind: 'workflowDesign', jobId: segment(jkDesignM[1]!) };
    if (p === '/registry/step-plugins') return { kind: 'systemStepPlugins' };
    const spM = p.match(/^\/registry\/step-plugins\/([^/]+)$/);
    if (spM) return { kind: 'systemStepPluginDetail', pluginSlug: segment(spM[1]!) };
    if (p === '/registry/rules') return { kind: 'dispatcherRulesList' };
    const drM = p.match(/^\/registry\/rules\/([^/]+)$/);
    if (drM) return { kind: 'dispatcherRuleEdit', ruleName: segment(drM[1]!) };
    if (p === '/registry/dispatcher') return { kind: 'dispatcherRules' };
    if (p === '/registry/subjects') return { kind: 'systemSubjects' };
    if (p === '/registry/drift') return { kind: 'systemRegistryDrift' };
    if (p === '/registry/agents') return { kind: 'systemAgents' };
    if (p === '/registry/credentials') return { kind: 'systemCredentials' };
    // 4. Design — the reviews.
    if (p === '/design') return { kind: 'systemDesign' };
    // /it/codebase is the row (9827c699).
    if (p === '/codebase') return { kind: 'systemCodebase' };
    // 5. Estate. 6. KB. Plus the unlisted auth door.
    if (p === '/estate') return { kind: 'systemEstate' };
    if (p === '/kb') return { kind: 'systemKb' };
    if (p === '/auth-admin') return { kind: 'authAdmin' };
    // 7. The jobs view every department carries (design 8c3e9599 §4,
    //    car 2). IT's block owns every /it path, so the /<code>/jobs
    //    branch below never sees this one; the code is IT's own.
    if (p === '/jobs') return { kind: 'department', code: 'it' };
    // Workflow detail LAST — its wildcard would eclipse the
    // specific /registry/* cases above.
    const jkM = p.match(/^\/registry\/([^/]+)$/);
    if (jkM) return { kind: 'workflowDetail', kindSlug: segment(jkM[1]!) };
    // Unknown /it path: it says so, inside the IT chrome. It returned the
    // yard until design ee3a3a2f (Q4), so a mistyped IT link landed on the
    // map and looked like a working one.
    return { kind: 'notFound', path: pathname };
  }

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
  // ===== Finance — its pages under its own address (car 4 of design
  // 8c3e9599, backlog c7ef4728; design deba3000: a page belongs to the
  // station that hosts it, and the URL is that station's address). Each
  // detail page sits under its slug, never /finance/<id>. Matched on
  // `raw`, never on `p`: the /ux spellings (/ux/finance, /ux/vendors,
  // /ux/purchase-orders/<id>, /ux/vendor-invoices/<id>) and the
  // unprefixed ones they shadowed retired with no alias and no redirect
  // (David, 2026-09-25: no shims before 1.0.0), so each falls to
  // not-found below. /finance/jobs is the department view above.
  if (raw === '/finance') return { kind: 'finance', view: readFinanceView(search) };
  // ===== Engineering — its station page at its own address (backlog
  // 950ad972, David 2026-11-12: "As a test, I would love to see this
  // under /engineering too"; design deba3000: pages live in their
  // station). The same page as /it/tree/engineering, which stays.
  // Matched on `raw`, like /finance: /ux/engineering is not this page.
  if (raw === '/engineering') return { kind: 'systemStation', station: 'engineering' };
  if (raw === '/finance/invoices/new') return { kind: 'newInvoice' };
  if (raw === '/finance/journal-entries/new') return { kind: 'newJournalEntry' };
  // Wildcard after /invoices/new, which it would otherwise eclipse.
  const invM = raw.match(/^\/finance\/invoices\/([^/]+)$/);
  if (invM) return { kind: 'invoice', invoiceId: segment(invM[1]!) };
  if (raw === '/finance/vendors') return { kind: 'vendors' };
  const vm = raw.match(/^\/finance\/vendors\/([^/]+)$/);
  if (vm) return { kind: 'vendor', vendorLookup: segment(vm[1]!) };
  const poM = raw.match(/^\/finance\/purchase-orders\/([^/]+)$/);
  if (poM) return { kind: 'po', poId: segment(poM[1]!) };
  const viM = raw.match(/^\/finance\/vendor-invoices\/([^/]+)$/);
  if (viM) return { kind: 'vendorInvoice', vendorInvoiceId: segment(viM[1]!) };
  // User Experiences lands on My Day by default — the actor's personal
  // work view, not a marketing landing.
  //
  // EVERY SINGLE-ID WILDCARD BELOW TAKES ONE SEGMENT, `([^/]+)`. They
  // were greedy `(.+)` until design ee3a3a2f (Q6), and a deeper path
  // under a list became a convincing "missing X": /ux/accounts/
  // agreements/<id> rendered the ACCOUNT page for accountId
  // "agreements/<id>". An id holding a slash arrives percent-encoded
  // (entityHref encodes every id) and is decoded by `segment`. The one
  // exception is /manual, whose slug may hold a slash by design — the
  // content API routes it as `{*slug}`.
  if (p === '/') return { kind: 'me' };
  if (p === '/me') return { kind: 'me' };
  if (p === '/inbox') return { kind: 'inbox' };
  // The landing page's own door. The catch-all was its ONLY way in until
  // design ee3a3a2f (Q5) — `/` and `/ux` are My Day — so turning the
  // catch-all into a not-found would have orphaned a real page.
  if (p === '/system-model') return { kind: 'home' };
  if (p === '/calendar/me') return { kind: 'myCalendar' };
  // /ux/departments/<code> retired with car 2 of design 8c3e9599 — the
  // jobs view is /<code>/jobs above, with no alias (David, 2026-09-25).
  if (p === '/manual') return { kind: 'manual' };
  // `(.+)` on purpose: a manual slug may hold a slash (see above).
  const mManual = p.match(/^\/manual\/(.+)$/);
  if (mManual) return { kind: 'manualSection', slug: segment(mManual[1]!) };
  if (p === '/watchlist') return { kind: 'watchlist' };

  if (p === '/search') {
    const sp = new URLSearchParams(search);
    return { kind: 'search', q: sp.get('q') ?? '' };
  }

  // /ux/jobs, the jobs list and its new-job form, went with step 1 of
  // design 01fa5bdf too (review df7aeb9c): it falls to not-found. A
  // packet's own page and its step pages stay below — every packet link,
  // search hit and step plugin opens one there.
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

  // A department's landing — LAST of the matches, because every one-
  // segment page above (/inbox, /finance, /watchlist, …) is a more
  // specific answer for its segment. What /<code> renders is catalog
  // data: the entry the department flags `landing`, else its first
  // sidebar entry, else its jobs view (nav-catalog `departmentLanding`).
  // The landing renders in place under the department's own address,
  // the way an entry's own path does. A segment no
  // department owns lands on the jobs view here, and is not found once
  // the registry says it is no row (sections.ts `withRoster`): the
  // router cannot see the registry, so it cannot tell a department
  // nobody built a page for from a typo.
  const landingM = unprefixed ? p.match(/^\/([^/]+)$/) : null;
  if (landingM) {
    const code = segment(landingM[1]!);
    const entry = departmentLanding(code);
    // An entry whose own path is this one would land on itself forever;
    // such a path has no page of its own, so it is the jobs view.
    return entry && routable(entry.path) !== raw ? parseRoute(entry.path) : { kind: 'department', code };
  }

  // Nothing answers this path, and the page says so, naming it as it was
  // asked for (design ee3a3a2f). This returned the landing page until
  // then, so a dead link rendered a real, working, plausible page and
  // the reader concluded they had misremembered (backlog c4f2ae24).
  // Rendered in place, never redirected: a redirect replaces the
  // evidence in the address bar with a working page, which is the defect.
  return { kind: 'notFound', path: pathname };
}

// `href` (honors the /dashboard mount) + `navigate` (pushState SPA nav)
// now live in the shared @boss/web-kit/nav module. Re-exported here so
// the ~55 files importing them from '../router' need no change.
export { href, navigate } from '@boss/web-kit/nav';
