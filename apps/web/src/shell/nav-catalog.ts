// The nav catalog — the single registry of every routable nav entry:
// its label, path, permKey, tenant-module gate, and its **owner** —
// Home, or the department whose tab it renders under and whose packets
// it lists. Each department's sidebar and landing are derived from it
// (car 2 of design 8c3e9599); the departments themselves are the
// registry's.
//
// This lived inside AppShell.svelte's `<script>` block. It moved out
// for two reasons, both about drift:
//
//  1. App membership was duplicated. `AppShell.svelte` carried a
//     `MODEL_ROUTES: Set<RouteName>` (driving which sidebar entries
//     show) and `App.svelte` carried a `MODEL_KINDS: Set<Route['kind']>`
//     (driving which tab highlights) — two sets, keyed off two
//     different vocabularies, that had to agree for every routed
//     surface or a page would render under the wrong tab. The comment
//     on each said as much. `owner` below (it was `app` until car 2 of
//     design 8c3e9599) is now the only place that answers "which app
//     does this surface belong to", and both consumers derive from it.
//
//  2. `sidebar-router-consistency.test.ts` hand-mirrored every sidebar
//     path, because (its header explains) parsing a TypeScript const
//     out of a Svelte `<script>` from a Bun test is fragile. As a
//     plain module the test imports the real thing, so the mirror —
//     and the drift it was there to catch — is gone.
//
// Adding a surface: add one entry here with its `owner` (and
// `unlisted` if it is a tab or a door rather than a sidebar row), and
// add a matching branch to router.ts. The consistency test enforces the
// second half. Adding a DEPARTMENT touches neither: it is a registry
// row, and its tab, landing, sidebar and jobs view follow from it.

import type { RouteName } from '@boss/web-kit/session/permissions';
import type { AppId, AppTab, Department } from '@boss/web-kit/nav';
import { HOME_APP, SIMULATOR_APP } from '@boss/web-kit/nav';

export type { AppId, AppTab };

// `AppId` and the tab list live in @boss/web-kit/nav (the bar is
// rendered by apps/web AND apps/simulator). THIS file answers the
// other half — which surface belongs to which app — because web-kit
// has no business knowing about /ux/warehouse.

export type NavItem = Readonly<{
  id: string;
  label: string;
  path: string;
  permKey?: RouteName;
  /// Tenant module that this nav entry belongs to. When the
  /// manifest disables the module (e.g. brewery turns off
  /// `equipment` and `shipping`), the entry is hidden. Items
  /// without a module field are always-on (e.g. /jobs).
  module?: string;
  /// Who owns this surface: `home`, or a department's registry code.
  /// Required on every catalog entry — an unowned surface is how a page
  /// ends up rendering under the wrong tab. Plain rows declared inline
  /// in a sidebar group (My Day, a department's Jobs) carry none; they
  /// belong to the group they sit in.
  ///
  /// ONE FIELD, since car 2 of design 8c3e9599 (backlog 64656a46). It
  /// replaced two: `app`, the tab the surface renders under, and
  /// `department`, the department whose packets it lists — which is
  /// part of what a surface IS (backlog 423a531d: two queues carried a
  /// workflow kind no instance published, and rendered a permanent "No
  /// jobs match"). A department rather than a kind, because a
  /// department's work is several protocols, and the server resolves it
  /// to the kinds whose workflow row declares it
  /// (`/api/jobs?department=<code>`). The two fields disagreed exactly
  /// once — the Service queue sat under Service and listed Support — and
  /// one field cannot disagree with itself (CLAUDE.md §9a). Practice
  /// owners, which mount one surface under several departments, arrive
  /// with car 9.
  owner?: AppId;
  /// The department's landing: what /<code> renders (design 8c3e9599
  /// §5). Absent everywhere, the landing is the department's first
  /// sidebar entry in catalog order, else its jobs view. At most one per
  /// owner — the catalog's test holds that.
  landing?: true;
  /// Reachable, never a sidebar row: a tab on a row's page (ItTabs), or
  /// a door reached from one. The sidebar is every OTHER entry its
  /// department owns, in catalog order — the job AppShell's
  /// APP_SURFACES and IT_GROUPS did as two hand lists until car 2.
  unlisted?: true;
}>;

export type NavGroup = Readonly<{ label: string; items: ReadonlyArray<NavItem> }>;

/// Catalog keys that are surfaces without a permission key of their
/// own. `RouteName` (libs/web-kit) is the role-gating vocabulary;
/// these entries are visible to every role by construction (like the
/// permKey-less Audit Log / Atlas rows), so they extend the CATALOG
/// without widening the PERMISSION vocabulary — the catalog still
/// answers "which app owns this surface" and "which sidebar row
/// highlights" for them.
// 'system-fleet' left the permission vocabulary with the 2026-08-31
// consolidation (its tab gates under system-monitoring), but the
// catalog still answers "which app / which row" for its route kind.
// 'hr', 'watchlist', 'manual' (CAR-6, 6edb1b77): routable since their
// pages shipped but never listed here, so a 574-line HR page and the
// app's best list were typed-URL-only. Like system-fleet they borrow
// gate parity from the surface they sit beside (hr → 'people',
// watchlist → 'accounts') rather than widening the vocabulary; the
// manual is permKey-less like the docs it renders.
// 'system-crew', 'system-receiving' and 'system-marshalling' were ids
// here until car N1 of design e765b3fc (2026-09-25): each was a sidebar
// row onto a station of the map, and the map's one row replaced them.
export type UngatedSurfaceId =
  | 'system-incidents'
  // 'system-codebase' (the Codebase row): the department's own numbers,
  // readable by any operator — permKey-less like the Operate row above,
  // because a permKey would widen the RouteName vocabulary in
  // libs/web-kit and every tenant's declared `surfaces` lists
  // (feedback 9827c699, 2026-09-14).
  | 'system-codebase'
  // 'system-registry-drift' (the Drift tab on Registry): a view of the
  // workflow registry against its authored bundle, so it borrows the
  // `workflows` gate of the family it sits in rather than widening the
  // RouteName vocabulary (4ae9969e, 2026-09-15).
  | 'system-registry-drift'
  // 'system-agents' (the Agents tab on Registry): the agents registry
  // as a directory (backlog 62988516). It borrows the `workflows` gate
  // of the family it sits in, as Drift does, rather than widening the
  // RouteName vocabulary.
  | 'system-agents'
  | 'system-fleet'
  | 'hr'
  | 'watchlist'
  | 'manual';

export const ROUTE_CATALOG: Readonly<Record<RouteName | UngatedSurfaceId, NavItem>> = {
  jobs:      { id: 'jobs',      label: 'All jobs',         path: '/ux/jobs',      permKey: 'jobs',      owner: 'home' },
  sales:     { id: 'sales',     label: 'Sales pipeline',   path: '/ux/sales',     permKey: 'sales',     owner: 'sales' },
  service:   { id: 'service',   label: 'Service queue',    path: '/ux/service',   permKey: 'service',   module: 'support', owner: 'service' },
  qa:        { id: 'qa',        label: 'QA',               path: '/ux/qa',        permKey: 'qa',        module: 'qa',      owner: 'qa' },
  finance:   { id: 'finance',   label: 'Finance',          path: '/ux/finance',   permKey: 'finance',   module: 'finance', owner: 'finance' },
  warehouse: { id: 'warehouse', label: 'Inventory',        path: '/ux/warehouse', permKey: 'warehouse', module: 'warehouse', owner: 'warehouse' },
  shipping:  { id: 'shipping',  label: 'Shipments',        path: '/ux/shipping',  permKey: 'shipping',  module: 'shipping', owner: 'distribution' },
  support:   { id: 'support',   label: 'Support',          path: '/ux/support',   permKey: 'support',   module: 'support', owner: 'support' },
  exec:      { id: 'exec',      label: 'Exec',             path: '/ux/exec',      permKey: 'exec',      module: 'exec',    owner: 'executive' },
  schedule:  { id: 'schedule',  label: 'My schedule',      path: '/ux/calendar/me', permKey: 'schedule', owner: 'home' },
  catalog:   { id: 'catalog',   label: 'Equipment',        path: '/ux/catalog',   permKey: 'catalog',   module: 'equipment', owner: 'maintenance' },
  parts:     { id: 'parts',     label: 'Ingredients & parts', path: '/ux/parts',  permKey: 'parts',     module: 'parts',   owner: 'warehouse' },
  products:  { id: 'products',  label: 'Products',         path: '/ux/products',  permKey: 'parts',     module: 'parts',   owner: 'production' },
  accounts:  { id: 'accounts',  label: 'Accounts',         path: '/ux/accounts',  permKey: 'accounts',  owner: 'sales' },
  vendors:   { id: 'vendors',   label: 'Vendors',          path: '/ux/vendors',   permKey: 'vendors',   owner: 'finance' },
  people:    { id: 'people',    label: 'Employees',        path: '/ux/people',    permKey: 'people',    owner: 'people' },
  assets:    { id: 'assets',    label: 'Assets',           path: '/ux/assets',    permKey: 'assets',    module: 'equipment', owner: 'maintenance' },
  shop:      { id: 'shop',      label: 'Shop',             path: '/ux/shop',      permKey: 'shop',      module: 'shop',    owner: 'sales' },
  inbox:     { id: 'inbox',     label: 'Inbox',            path: '/ux/inbox',     permKey: 'inbox',     owner: 'home' },
  views:     { id: 'views',     label: 'Views',            path: '/ux/views',     permKey: 'views',     owner: 'home' },
  'marketing-assets': { id: 'marketing-assets', label: 'Marketing assets', path: '/ux/marketing-assets', permKey: 'marketing-assets', module: 'marketing-assets', owner: 'marketing' },
  hr:        { id: 'hr',        label: 'HR',               path: '/hr',           permKey: 'people',    owner: 'people', unlisted: true },
  watchlist: { id: 'watchlist', label: 'Churn watchlist',  path: '/watchlist',    permKey: 'accounts',  owner: 'sales', unlisted: true },
  manual:    { id: 'manual',    label: 'Manual',           path: '/manual',       owner: 'home' },

  // The IT department — SIX surfaces (the 2026-08-31 consolidation,
  // packet 1f6d55e0; was 17 pages, four of them dual-routed). The
  // catalog keeps non-sidebar entries only where a RouteName still
  // exists (tab pages, unlisted doors) so appForSection() can answer
  // for them; map/flow/model died outright and fleet became Operate's
  // Bottlenecks tab.
  // The IT app's landing (departure-board.md Q1), AT /it itself, and
  // flagged `landing` since car 2 of design 8c3e9599 rather than resting
  // on coming first in catalog order. Since car N1 of design
  // e765b3fc (David, 2026-09-25) it is the DEPARTMENT MAP: the map on
  // top, the selection's detail below it, and the one sidebar row where
  // Receiving Yard, Marshalling Yard, Train Yard and Crew Board stood —
  // each of those is a station on the map, selected there.
  'system-yard':              { id: 'system-yard',              label: 'Department Map',      path: '/it',              permKey: 'system-yard',             owner: 'it', landing: true },
  // The Operate row is permKey-less like the incidents surface it
  // leads with — readable by any operator; the tabs behind it keep
  // their own gates.
  'system-incidents':        { id: 'system-incidents',        label: 'Operate',             path: '/it/operate',      owner: 'it' },
  'system-monitoring':       { id: 'system-monitoring',       label: 'Monitoring',          path: '/it/operate/audit', permKey: 'system-monitoring',      owner: 'it', unlisted: true },
  'system-fleet':            { id: 'system-fleet',            label: 'Bottlenecks',         path: '/it/operate/bottlenecks', permKey: 'system-monitoring', owner: 'it', unlisted: true },
  workflows:                 { id: 'workflows',               label: 'Registry',            path: '/it/registry',     permKey: 'workflows',               owner: 'it' },
  policy:                    { id: 'policy',                  label: 'Policy',              path: '/it/registry/policy', permKey: 'policy',               owner: 'it', unlisted: true },
  'system-step-plugins':     { id: 'system-step-plugins',     label: 'Step plugins',        path: '/it/registry/step-plugins', permKey: 'system-step-plugins', owner: 'it', unlisted: true },
  'system-dispatcher':       { id: 'system-dispatcher',       label: 'Dispatcher rules',    path: '/it/registry/dispatcher', permKey: 'system-dispatcher', owner: 'it', unlisted: true },
  'system-subjects':         { id: 'system-subjects',         label: 'Subjects & Classes',  path: '/it/registry/subjects', permKey: 'system-subjects',    owner: 'it', unlisted: true },
  'system-registry-drift':   { id: 'system-registry-drift',   label: 'Protocol drift',      path: '/it/registry/drift', permKey: 'workflows',             owner: 'it', unlisted: true },
  // Agents live here and never on the People roster (David, 2026-09-26,
  // answering 6a123f1f; backlog 62988516).
  'system-agents':           { id: 'system-agents',           label: 'Agents',              path: '/it/registry/agents', permKey: 'workflows',            owner: 'it', unlisted: true },
  'system-dispatcher-rules': { id: 'system-dispatcher-rules', label: 'Dispatcher rules — authoring', path: '/it/registry/rules', permKey: 'system-dispatcher-rules', owner: 'it', unlisted: true },
  // The editor's path is a PATTERN, spelled the way surface-opens records
  // every open of it (routePattern). It shared the list's path until
  // backlog 3071e235 (2026-09-24), so the page march — one audit per
  // catalog path — never reached the page carrying all four rule writes.
  'system-dispatcher-rule':  { id: 'system-dispatcher-rule',  label: 'Dispatcher rule — editor',     path: '/it/registry/rules/:ruleName', permKey: 'system-dispatcher-rule',  owner: 'it', unlisted: true },
  'system-design':           { id: 'system-design',           label: 'Design',              path: '/it/design',       permKey: 'system-design',           owner: 'it' },
  'system-experiments':      { id: 'system-experiments',      label: 'Experiments',         path: '/it/design/experiments', permKey: 'system-experiments', owner: 'it', unlisted: true },
  // 'system-feedback' (Feedback triage, /it/design/feedback) and
  // 'system-backlog' (IT backlog, /it/design/backlog) were Design tabs
  // until car N3 of design e765b3fc (2026-09-25): the feedback board is
  // the receiving station's panel on the Department Map, and the backlog
  // board the receiving and marshalling stations' — a packet of either
  // kind is inbound until triaged, then stands at a station.
  // The Codebase — a SIDEBAR ROW, not the Design tab it was: David's
  // feedback 9827c699 (2026-09-14) asked for "a page to the IT department
  // showing the Code base stats" while the trend sat one tab in. permKey-
  // less: the department's own numbers, readable by any operator.
  'system-codebase':         { id: 'system-codebase',         label: 'Codebase',            path: '/it/codebase',     owner: 'it' },
  // The hardware registry — declared beside observed beside the
  // difference, plus the dev-workspace door (59ef456a).
  'system-estate':          { id: 'system-estate',           label: 'Estate',              path: '/it/estate',       permKey: 'system-estate',           owner: 'it' },
  'system-kb':               { id: 'system-kb',               label: 'Knowledge Base',      path: '/it/kb',           permKey: 'system-kb',               owner: 'it' },
  // Unlisted door: reachable, never a sidebar row.
  'auth-admin':              { id: 'auth-admin',              label: 'Auth admin',          path: '/it/auth-admin',   permKey: 'auth-admin',              owner: 'it', unlisted: true },
};

/// The apps this host offers: Home, Simulator when the tenant has one,
/// and one per department the tenant's registry declares.
///
/// DERIVED, not listed, and since ce68f137 derived from the REGISTRY:
/// the departments are the rows `GET /api/departments` answers at boot
/// (libs/web-kit session/departments.svelte.ts), in their sort order,
/// so a tenant adds a tab by adding a row (CLAUDE.md §9). The previous
/// version filtered a hardcoded list down to the departments owning a
/// catalog surface, which read one tenant's org chart and left
/// Algedonic's `operations` with no tab at all.
///
/// A department with no surface of its own still gets its tab — that
/// is the org chart, and the tenant declared it — and lands on its
/// own jobs view: the packets whose workflow declares the department,
/// as in / working / out (cc76f755, 2026-09-18). Until then it landed
/// on All jobs with Home highlighted, which read as "this department
/// has no work" for Algedonic's operations, sales, finance and
/// marketing. `departmentsWithoutSurfaces()` still reports which
/// departments have no built surface, so the gap stays visible
/// instead of reading as covered.
const OWNED = new Set<string>(
  Object.values(ROUTE_CATALOG)
    .map((e) => e.owner)
    .filter((a): a is AppId => a !== undefined && a !== 'home' && a !== 'simulator'),
);

/// A department's address: `/<code>`, its registry code and nothing
/// else (design 8c3e9599 §5, car 2). Only the unit's own code — never
/// its parents' — so a re-org changes no URL. The router's
/// single-segment branch is the other half of this spelling, and what
/// it renders there is `departmentLanding`.
export function departmentPath(code: string): string {
  return `/${encodeURIComponent(code)}`;
}

/// The department jobs view's path — in / working / out, the one
/// surface every department carries, keyed by its registry code. It was
/// /ux/departments/<code> until car 2 of design 8c3e9599 (backlog
/// 64656a46), which retired that spelling with no alias (David,
/// 2026-09-25). The router's `department` route is the other half.
export function departmentJobsPath(code: string): string {
  return `${departmentPath(code)}/jobs`;
}

/// Every catalog entry a department owns, in catalog order.
function ownedBy(code: string): ReadonlyArray<NavItem> {
  return Object.values(ROUTE_CATALOG).filter((e) => e.owner === code);
}

/// What `/<code>` renders (design 8c3e9599 §5): the entry the
/// department flags `landing`, else its first sidebar entry in catalog
/// order — so the tab opens on the row the sidebar shows first — else
/// nothing, and the caller renders the jobs view. IT is the flagged
/// one: its landing is the Department Map, which until car 2 rested on
/// the map happening to come first in the catalog, beside a sidebar
/// order that was AppShell's own IT_GROUPS list.
export function departmentLanding(code: string): NavItem | undefined {
  const owned = ownedBy(code);
  return owned.find((e) => e.landing === true) ?? owned.find((e) => e.unlisted !== true);
}

/// A department's sidebar, before policy and module filter it (design
/// 8c3e9599 §9): its own surfaces in catalog order, then Jobs. Child
/// units go between the two when the registry first nests one — the
/// departments row has no parent yet, and the approved roster nests
/// nothing.
///
/// The Jobs row is permKey-less like the Audit Log link: the listing
/// behind it is policy-scoped by the server, and a permKey would widen
/// the RouteName vocabulary for a row every department has. For a
/// department that owns no surface it is the whole group, which is the
/// point: the tab used to open All jobs under Home with an empty
/// sidebar here (cc76f755). Here rather than in AppShell so the test
/// that pins every sidebar reads the rows the shell renders.
export function departmentRows(code: string): ReadonlyArray<NavItem> {
  return [
    ...ownedBy(code).filter((e) => e.unlisted !== true),
    { id: 'department-jobs', label: 'Jobs', path: departmentJobsPath(code) },
  ];
}

/// One tab per declared department, in registry order, each linking to
/// the department's own address.
export function departmentApps(departments: ReadonlyArray<Department>): ReadonlyArray<AppTab> {
  return departments.map((d) => ({ id: d.code, label: d.label, href: departmentPath(d.code) }));
}

/// The full tab list, left to right. Simulator is a tab only for a
/// tenant whose manifest lists the `sim` module (ce68f137): it drives
/// the playground's model, and a company running on BOSS has no
/// simulation to drive.
export function appsFor(
  departments: ReadonlyArray<Department>,
  opts: Readonly<{ simulator: boolean }>,
): ReadonlyArray<AppTab> {
  return [HOME_APP, ...(opts.simulator ? [SIMULATOR_APP] : []), ...departmentApps(departments)];
}

/// Departments with no surface of their own. Not an error — a report.
export function departmentsWithoutSurfaces(
  departments: ReadonlyArray<Department>,
): ReadonlyArray<string> {
  return departments.filter((d) => !OWNED.has(d.code)).map((d) => d.code);
}

/// Which app a surface belongs to, looked up by the `activeSection`
/// id `App.svelte` derives from the current route: the entry's owner.
/// Unknown ids (the `me` fallback, plain sub-pages) fall back to Home.
export function appForSection(section: string): AppId {
  const entry = (ROUTE_CATALOG as Record<string, NavItem | undefined>)[section];
  // `me` (App.svelte's terminal fallback) and any plain sub-page id
  // resolve to Home — personal surfaces, which is where the fallback
  // belongs now that there is an app for them.
  return entry?.owner ?? 'home';
}

/// Whether a sidebar row renders in the app being shown: judged by the
/// row's OWN owner, and a row with none (an inline link like My Day or
/// a department's Jobs row) belongs to the group it sits in.
///
/// It judged by the owner of the catalog entry the row's PERMKEY names
/// until backlog 72a88031 (2026-09-24). That is the same answer for
/// every row but one: Production's Products gates on `parts`, whose
/// entry is Warehouse's, so Production dropped Products for every
/// role. The permKey decides policy (canSeeRoute); the owner decides
/// placement. Here, not in AppShell, so the test that pins every
/// sidebar list imports the rule instead of restating it.
export function inPerspective(item: NavItem, app: AppId): boolean {
  return item.owner === undefined || item.owner === app;
}

/// Subject kinds each app is "about".
///
/// Feeds `app_kinds` on `/api/search`, which floats these to the top of
/// the dropdown — Q4's "prioritise results from the immediate app".
/// It is a ranking hint, never a filter: the whole value of a global
/// box is that it still finds the thing when you are looking in the
/// wrong app, so a CRM search for a part number must still surface the
/// part, just below the accounts.
///
/// Kinds may repeat across apps. An invoice is Finance's to reconcile
/// and CRM's to chase, and both are right — this maps attention, not
/// ownership. Home lists nothing: it is the cross-app surface, so it
/// prioritises nothing and shows the unweighted ranking.
/// Subject kinds each app claims, for search's app-scoped ranking.
///
/// Must cover every **concrete** kind. The subject-kind registry is a
/// taxonomy: `person`, `object` and `intangible` are abstract roots
/// that nothing is ever an instance of — `account` and `employee`
/// specialize `person` — so they are deliberately unclaimed, and the
/// test beside this exempts roots-with-children on that basis rather
/// than by name.
///
/// Everything else must land somewhere or search silently never
/// floats it for the app whose surface shows it. That is what happened
/// to `message`: Inbox is a Home surface listing 13,483 message
/// Subjects, and no app claimed the kind.
export const APP_SUBJECT_KINDS: Readonly<Partial<Record<AppId, ReadonlyArray<string>>>> = {
  // Inbox lives here, and messages are what it lists.
  home: ['message'],
  simulator: [],
  // `custom` is the escape hatch for Jobs about things that are not
  // domain Subjects — a design doc is the shipped example, and
  // /it/design is an IT surface.
  it: ['workflow', 'company', 'custom'],
  // Was one `crm` bucket. Split along the departments that actually do
  // the work: Sales owns the accounts and the shop, Marketing owns the
  // campaigns and their assets.
  sales: ['account', 'customer'],
  marketing: ['campaign', 'marketing-asset'],
  finance: ['invoice', 'vendor', 'vendor-invoice', 'purchase_order'],
  // Was `supply-chain`, which spanned four departments. A purchase
  // order is Warehouse's to raise and Finance's to pay, and both claim
  // it — this maps attention, not ownership.
  warehouse: ['purchase_order', 'vendor'],
  production: ['product', 'calendar'],
  distribution: ['shipment'],
  maintenance: ['asset', 'location'],
  people: ['employee'],
};
