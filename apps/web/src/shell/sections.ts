// Which sidebar section highlights for each route kind — and, through
// `appForSection`, which app tab the page renders under.
//
// This was a 60-line ternary inside App.svelte. It moved out for the
// same two reasons the nav catalog itself moved out of AppShell:
//
//  1. Exhaustiveness is now the type system's job. The ternary's
//     fall-through shipped 21 of 74 kinds rendering inside the Home
//     chrome, and the test guarding it had to scrape the Svelte source
//     with regexes. `Record<Route['kind'], string>` makes a missing
//     kind a typecheck failure instead.
//
//  2. The section ids on the right-hand side are ROUTE_CATALOG keys —
//     the same string must exist there for the sidebar row to
//     highlight and for `appForSection` to find the owning app. That
//     agreement was unpinned, and it drifted: 'systemMonitoring' and
//     'systemStepPlugins' (camelCase route kinds used as section ids)
//     miss their kebab-case catalog keys, so /it/operate/audit — the
//     IT tab's own landing page — rendered under Home chrome.
//     `sections.test.ts` pins every value now (CLAUDE.md §9a).

import { notFoundBack, parseRoute, routable, type Route } from '../router';
import { ADDRESSES_LOADING, stationAtPath, type StationAddresses } from './page-address';
import type { Department } from '@boss/web-kit/nav';
import { moduleOn, type ManifestState } from '@boss/web-kit/session/manifest-inline';
import { ROUTE_CATALOG, appForSection, type AppId, type NavItem } from './nav-catalog';

/// Sections that deliberately resolve to the Home app instead of a
/// catalog entry, each with the reason. The bar for adding one is
/// "this route renders outside AppShell or has no sidebar row" — not
/// "I could not find where it goes".
export const HOME_CHROME_SECTIONS: ReadonlyMap<string, string> = new Map([
  [
    'me',
    'the personal fallback: login/stepFocus/home render outside AppShell ' +
      'entirely, and search and a packet\'s own page are cross-cutting ' +
      'with no sidebar row',
  ],
  // 'manual' graduated out of this map on 2026-09-02 (CAR-6): Home
  // claimed it, so it is a real catalog entry — exactly the departure
  // its row here said it was waiting for.
]);

/// Sections whose APP is carried by the route rather than by a catalog
/// entry, each with the reason. The catalog's `owner` field is static —
/// one surface, one department — and that is right for every surface
/// built for a department by name. A department's jobs view is one
/// surface for EVERY department the registry declares (cc76f755,
/// 2026-09-18): the app it renders under is the route's `code`, so no
/// catalog row can answer for it, and `appForRoute` reads the route
/// instead. The sidebar row that highlights for it is the permKey-less
/// "Jobs" row every department's sidebar ended on until backlog 474e2c1c.
export const DYNAMIC_APP_SECTIONS: ReadonlyMap<string, string> = new Map([
  [
    'department-jobs',
    'the department jobs view renders under the department named in the ' +
      'route (/<code>/jobs), which is registry data',
  ],
]);

/// Which app a route renders under. The catalog answers for every
/// surface with a static owner (`appForSection`); a route whose app is
/// its own data answers for itself. This is the ONE derivation
/// App.svelte's tab highlight reads — before it existed the
/// department view would have highlighted Home, the exact defect the
/// packet was filed on ("lands on All jobs with Home highlighted").
export function appForRoute(route: Route): AppId {
  if (route.kind === 'department') return route.code;
  return appForSection(sectionForRoute(route));
}

/// The route as THIS instance can render it: unchanged, unless the
/// department that owns it is not a live row in the registry — then the
/// not-found page, naming the path and the department, with one door
/// Home (backlog 64656a46, car 2 of design 8c3e9599; e543c6fe asked for
/// exactly this). A department the company does not have is never a
/// module-off notice, and never its own chrome: the tab bar has no tab
/// for it, so the page would render under a tab nobody can see.
///
/// `roster` is `null` until the registry has ANSWERED, and while its
/// read is failing — never read as an empty roster, which would turn
/// every department page into not-found because a request was slow or
/// dark (a wrong target answers instead of erroring). Unanswered, the
/// route renders as it always did.
///
/// Home and the Simulator are not departments and pass untouched, as
/// does a path the router already could not match. IT's pages answer to
/// the registry like any department's: IT is a row (design 8c3e9599 §2).
///
/// A bare /<code> that is no department here but IS a top-level station's
/// address in the tree is that station's page (backlog 0bc2eb68: a
/// station's page lives at its address — /engineering, which answered by
/// a literal path until then). A department's address stays the
/// department's: the registry decides, and only it can.
export function withRoster(
  route: Route,
  pathname: string,
  roster: ReadonlyArray<Department> | null,
  at: StationAddresses = ADDRESSES_LOADING,
): Route {
  if (roster === null || route.kind === 'notFound' || route.kind === 'resolving') return route;
  const owner = appForRoute(route);
  if (owner === 'home' || owner === 'simulator') return route;
  if (roster.some((d) => d.code === owner)) return route;
  const station = route.kind === 'department' ? stationAtPath(at, routable(pathname)) : null;
  if (station !== null) return { kind: 'systemStation', station: station.name };
  return { kind: 'notFound', path: pathname, department: owner };
}

/// Which tenant module a route needs, and the label to say it with —
/// or null when the surface is always-on.
///
/// DERIVED from the catalog entry the route lights, because the module
/// that HIDES a sidebar row and the module that gates the ROUTE behind
/// it are one fact. App.svelte carried the second copy as a
/// hand-written `routeRequiredModule` switch, and it drifted exactly
/// the way §9a says a fact that lives twice does: /ux/support was
/// gated on 'shipping' while its nav row was gated on 'support', so a
/// direct visit answered "Shipments is not enabled" for a page with
/// nothing to do with shipments (backlog f9b43965, found by the
/// /ux/support page audit 9876ef0d, 2026-09-19). The switch also
/// listed no module for finance, warehouse, parts or products, whose
/// nav rows the catalog hides — the same disagreement pointing the
/// other way.
export function moduleForRoute(route: Route): { id: string; label: string } | null {
  const entry = (ROUTE_CATALOG as Record<string, NavItem | undefined>)[sectionForRoute(route)];
  if (!entry?.module) return null;
  return { id: entry.module, label: entry.label };
}

/// Clean absence is catalog data, not a tenant or route-name branch.
/// An unread manifest decides nothing; App keeps its loading/error gate.
export function withManifest(route: Route, pathname: string, manifest: ManifestState): Route {
  if (manifest.kind !== 'ready' || route.kind === 'notFound') return route;
  const entry = (ROUTE_CATALOG as Record<string, NavItem | undefined>)[sectionForRoute(route)];
  if (entry?.whenDisabled === 'absent' && entry.module && !moduleOn(manifest, entry.module)) {
    return { kind: 'notFound', path: pathname };
  }
  return route;
}

/// Which sidebar row a route lights — the ONE answer, and the only
/// one this module exports. Most rows are named by the route's kind
/// (SECTION_FOR_KIND); a row that depends on a route PARAMETER, which
/// a kind-keyed map cannot express, gets its branch HERE, never a
/// second exported lookup. A yard floor's region was one until car N1
/// of design e765b3fc (2026-09-25): the two yards with rows of their
/// own lost them to the one Department Map row, so every region lights
/// that row and the region map that answered for them is gone.
export function sectionForRoute(route: Route): string {
  // An unmatched path lights the row of the page its one back link
  // opens, so it renders where that door leads (design ee3a3a2f Q4):
  // Home, since backlog 92bfcb04 retired the /it prefix whose typos
  // rendered in the IT chrome.
  // A path owned by a department this instance does not have lights
  // Home, where its one door leads (`withRoster`).
  if (route.kind === 'notFound' && route.path) {
    return sectionForRoute(parseRoute(notFoundBack(route.path, route.department).href));
  }
  return SECTION_FOR_KIND[route.kind];
}

/// The kind half of `sectionForRoute`: NOT authoritative alone, and so
/// not exported. It was the public answer (as SECTION_FOR_ROUTE) until
/// car 4 of design d2154293 retired /it/operate/receiving and
/// /it/operate/marshalling as pages: both are yard floors now, and
/// read off this map both light the Train Yard's row — a plausible
/// wrong answer with no error, which is the failure a reader cannot
/// see (backlog c6f91515, 2026-09-20). Its reader is
/// `sectionForRoute`, which answers first for any route whose row a
/// parameter decides.
const SECTION_FOR_KIND: Readonly<Record<Route['kind'], string>> = {
  // Renders outside AppShell (or has no sidebar row) — see
  // HOME_CHROME_SECTIONS for the reasons.
  login: 'me',
  stepFocus: 'me',
  home: 'me',
  // The kind half only: `sectionForRoute` answers by the path's
  // department, the way it answers a yard floor by its region.
  notFound: 'me',
  // A path the station tree has not placed yet (backlog 0bc2eb68): no
  // page is open, so no row lights, until the read lands and it re-parses.
  resolving: 'me',
  search: 'me',
  me: 'me',
  manual: 'manual',
  manualSection: 'manual',

  inbox: 'inbox',
  // A packet's own page has no sidebar row: it lit All jobs until that
  // list went with step 1 of design 01fa5bdf (review df7aeb9c), and is
  // cross-cutting like search, so it takes the personal fallback.
  jobDetail: 'me',
  // The Churn watchlist is the `accounts` entry since design 01fa5bdf
  // removed the Accounts page it sat beside.
  watchlist: 'accounts',
  vendors: 'vendors',
  vendor: 'vendors',
  // Financial detail availability is separate from the vendor register;
  // neither has a sidebar row of its own (design45fad3bb).
  po: 'purchase-order',
  vendorInvoice: 'vendor-invoice',
  finance: 'finance',
  invoice: 'finance',
  newInvoice: 'finance',
  newJournalEntry: 'finance',
  // /me/calendar is the `schedule` row's own path (Home, "My
  // schedule"). It lit `calendar` until 2026-09-24, so it highlighted
  // Release calendar and was gated on the calendar module — off on the
  // live instance, so the page was ModuleDisabled (eff0c5e5).
  myCalendar: 'schedule',
  // The department jobs view: its app is the route's own code, not a
  // catalog field — see DYNAMIC_APP_SECTIONS and `appForRoute`.
  department: 'department-jobs',

  // Operate-family kinds highlight the Operate row, the audit log
  // (2026-08-31 consolidation; the incidents surface led it until
  // design 01fa5bdf); registry- and design-family kinds highlight
  // theirs the same way, via their own catalog ids whose paths now
  // live under the family surface.
  systemMonitoringPerf: 'system-monitoring',
  systemMonitoringEvents: 'system-monitoring',
  systemStepPlugins: 'system-step-plugins',
  systemStepPluginDetail: 'system-step-plugins',
  systemSubjects: 'system-subjects',
  systemRegistryDrift: 'system-registry-drift',
  systemAgents: 'system-agents',
  systemCredentials: 'system-credentials',
  // The alarms board at /alarms — what stays of the Department Map
  // (design e765b3fc) after design 01fa5bdf — and every selection on it
  // light its one row.
  systemYard: 'system-yard',
  // The station tree floor (T7, backlog 94a9dad4): the root's page since
  // backlog 92bfcb04, Home's, so it outlives IT's departments row. It lit
  // the alarms board's row while both sat under /it.
  systemTree: 'system-tree',
  // The station page (design c4854fd4 E2) is a station of that floor, opened.
  systemStation: 'system-tree',
  // Its Resources view (backlog 87575d69) is the same station, opened on its pools.
  systemStationResources: 'system-tree',
  systemEstate: 'system-estate',
  systemKb: 'system-kb',
  systemDesign: 'system-design',
  // The codebase has its own row since 2026-09-14 (feedback 9827c699).
  systemCodebase: 'system-codebase',
  authAdmin: 'auth-admin',
  dispatcherRules: 'system-dispatcher',
  dispatcherRulesList: 'system-dispatcher',
  dispatcherRuleEdit: 'system-dispatcher',
  workflows: 'workflows',
  workflowNew: 'workflows',
  workflowDesign: 'workflows',
  workflowDetail: 'workflows',
};

/// Every route kind — the map's keys, which the `Record` type holds
/// complete. Exported so a pin can walk every kind through
/// `sectionForRoute` without reaching for the map itself.
export const ROUTE_KINDS: ReadonlyArray<Route['kind']> = Object.keys(
  SECTION_FOR_KIND,
) as ReadonlyArray<Route['kind']>;

/// Every section a route can light. Derived here, beside the map, so no
/// caller has to know how many answers there are (backlog c6f91515);
/// a parameter-decided row, when one returns, is added here too.
export const SECTIONS_PRODUCED: ReadonlySet<string> = new Set(Object.values(SECTION_FOR_KIND));
