// The nav catalog — the single registry of every routable nav entry:
// its label, path, permKey, tenant-module gate, and its **owner** —
// Home, or the department whose tab it renders under and whose packets
// it lists — and the station that HOSTS it. A station's sidebar is the
// entries it hosts (shell/station-trail.ts, backlog 474e2c1c); the
// department rows that were each department's sidebar retired with the
// department tabs, which drew departments the company no longer has.
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
// Adding a surface: add one entry here with its `owner`, the station
// that hosts it (`host`, design deba3000) and its `slug` below that
// station — never a path: the path is the host's address plus the slug
// (backlog 0bc2eb68, shell/page-address.ts) — plus `unlisted` if it is
// a tab or a door rather than a sidebar row, and add its row to
// router.ts's HOSTED table. The consistency test enforces the
// second half. The row lands in its host station's sidebar by itself.

import type { RouteName } from '@boss/web-kit/session/permissions';
import type { AppId } from '@boss/web-kit/nav';

export type { AppId };

// `AppId` and the tab list live in @boss/web-kit/nav (the bar is
// rendered by apps/web AND apps/simulator). THIS file answers the
// other half — which surface belongs to which app — because web-kit
// has no business knowing about /finance/vendors.

/// The stations that host a catalog page, each by the `id` of its row
/// under infra/platform/stations/ (car 1 of design deba3000, backlog
/// 03fb75c8). The id is the one field of a station row that never
/// changes: its name can, and its address is derived from the name
/// (station-tree.md: code never names an address). The key is only a
/// label for the reader here. Both are held to the rows by
/// nav-catalog.test.ts, which reads the files the platform seed
/// publishes, so neither can drift from the tree (CLAUDE.md §9a).
export const STATION = {
  algedonic: '00b04483-90c6-46b9-b911-8e19d2a0b410',
  engineering: '4c5ddf6c-b8ed-4776-98d9-2f99553317c5',
  design: 'cc856b32-3667-4e70-b6cd-667f6fe89007',
  operations: '9660c60f-eb47-4405-ae08-fcbb30c71e5f',
  estate: '134464a5-0b56-4386-86cb-d354c7448fdf',
  vault: 'fa8b479f-1573-4153-9359-0ee123c3295c',
  finance: '9b024f00-a8d4-43da-b2de-b648772c062b',
} as const;

/// A host: one of the stations above, by id.
export type HostStation = (typeof STATION)[keyof typeof STATION];

/// Where a page is: hosted (a station and the slug below it, so its path
/// is DERIVED — `pagePath` in ./page-address) or, until it moves, the
/// path it still spells. Never both: a hosted entry that also spelled a
/// path would be the hand-typed prefix the host replaced (backlog
/// 0bc2eb68), and the type refuses it.
type Placement =
  | Readonly<{
      /// The one station in the tree that hosts this page, by station id
      /// (design deba3000, David 2026-11-10): the station whose
      /// executors the page serves, and whose queue its maintenance is.
      /// The owner below still decides the tab and the packets the page
      /// lists. Optional while pages move a few at a time;
      /// `unhostedPages()` counts the rest, and when that reaches zero
      /// the field becomes required (car 3 of the design).
      host: HostStation;
      /// The page's path below its host's address: '' for the station's
      /// own page (/finance, /operations/estate), `vendors` for
      /// /finance/vendors. A `:name` segment is a parameter, spelled the
      /// way surface-opens records the page.
      slug: string;
      path?: never;
      person?: never;
    }>
  | Readonly<{ host?: never; slug?: never; path: string; person?: never }>
  | Readonly<{
      /// Hosted at the signed-in person's own station (design 01fa5bdf
      /// step 3, David 2026-11-17: "every human executor has a station
      /// of their own at /me, resolved from the signed-in user"; backlog
      /// 5cb56bb3). One address for every person, because WHOSE station
      /// it is comes from the session, never from the URL — so a link to
      /// /me/inbox is the reader's own inbox, and no address names
      /// another person's. Its row in the company tree, `me`
      /// (infra/platform/stations/me.toml), is one row for every person,
      /// so the path is spelled rather than read off the tree's address,
      /// and needs no tree read: a tree that has not answered
      /// or could not be read never takes a person's own pages away.
      /// The type holds it under /me.
      person: true;
      path: '/me' | `/me/${string}`;
      host?: never;
      slug?: never;
    }>;

export type NavItem = Readonly<{
  id: string;
  label: string;
  permKey?: RouteName;
  /// Tenant module that this nav entry belongs to. When the
  /// manifest disables the module (e.g. brewery turns off
  /// `equipment` and `shipping`), the entry is hidden. Items
  /// without a module field are always-on (e.g. /jobs).
  module?: string;
  /// A disabled module normally has a notice. A surface declared absent
  /// instead resolves to NotFound, only after the manifest answers.
  whenDisabled?: 'absent';
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
  /// a door reached from one. A station's sidebar is every OTHER entry
  /// it hosts, in catalog order (shell/station-trail.ts `sidebarOf`).
  unlisted?: true;
}> &
  Placement;

/// Catalog keys that are surfaces without a permission key of their
/// own. `RouteName` (libs/web-kit) is the role-gating vocabulary;
/// these entries are visible to every role by construction (like the
/// permKey-less Codebase row), so they extend the CATALOG without
/// widening the PERMISSION vocabulary — the catalog still answers
/// "which app owns this surface" and "which sidebar row highlights" for
/// them.
// 'manual' (CAR-6, 6edb1b77) is permKey-less like the docs it renders.
// 'system-crew', 'system-receiving' and 'system-marshalling' were ids
// here until car N1 of design e765b3fc (2026-09-25): each was a sidebar
// row onto a station of the map, and the map's one row replaced them.
// 'system-incidents' (/it/operate), 'system-atlas', 'system-fleet'
// (Bottlenecks) and 'hr' (/people/hr) went with their pages in step 1
// of design 01fa5bdf (2026-11-17). 'watchlist' went with them as an id:
// the Churn watchlist is the `accounts` entry now, the gate it always
// borrowed, since the Accounts page it sat beside is gone.
export type UngatedSurfaceId =
  // 'me' (My Day, the person's own station's page, backlog 5cb56bb3):
  // everyone's, so the session is its only gate.
  | 'me'
  | 'system-performance'
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
  // 'system-credentials' (the Credentials tab on Registry): the
  // credentials registry drawn, its lifecycle filed as packets (backlog
  // 851259b9). It borrows the `workflows` gate of the family it sits in,
  // as Agents does; the registry read itself is operator-tier upstream.
  | 'system-credentials'
  // 'system-tree' (the station tree floor, backlog 92bfcb04): the
  // company's view of itself, gated like the alarms board it sat behind.
  | 'system-tree'
  | 'purchase-order'
  | 'vendor-invoice'
  | 'manual';

export const ROUTE_CATALOG: Readonly<Record<RouteName | UngatedSurfaceId, NavItem>> = {
  // The /ux business pages (sales, service, qa, warehouse, shipping,
  // support, exec, catalog, parts, products, accounts, people, assets,
  // shop, marketing-assets) and their entries went with step 1 of design
  // 01fa5bdf (David, 2026-11-17); a page comes back as a station's page
  // when a protocol runs there. All jobs (/ux/jobs) went with them
  // (review df7aeb9c); a packet's own page has no row, as it never had.
  //
  // HOSTS (car 1 of design deba3000, backlog 03fb75c8) follow the
  // placement table of design 01fa5bdf (David, 2026-11-17). The pages
  // that table sends to /me — My Day, the Inbox and My schedule — are
  // the person's own station's (`person`, backlog 5cb56bb3). My Day is
  // where the person's queue and watchlist already render (the packets
  // they filed, the my-watchlist station, src/me/watchlist.ts). The
  // table's "/watchlist" is NOT that list: it is the Churn watchlist,
  // every account's risk score, a company-wide list under the accounts
  // gate, which David kept on the company's own instance to dogfood it
  // (design 922f37be). Company-wide parts go to the root (design
  // 01fa5bdf), so the root hosts it, at the address it always had.
  //
  // ADDRESSES (backlog 0bc2eb68): a hosted page's path is its host's
  // address in the tree plus the `slug` here (shell/page-address.ts), so
  // no entry below spells a prefix, and the /it and /ux spellings of a
  // hosted page retired with no alias (David, 2026-09-25: no shims
  // before 1.0.0). The one address kept is a department's own: /it still
  // renders IT's landing, because the IT tab links there.
  finance:   { id: 'finance',   label: 'Finance',          slug: '',              permKey: 'finance',   module: 'finance', owner: 'finance', host: STATION.finance },
  // THE PERSON'S OWN STATION, /me (backlog 5cb56bb3): My Day is its page
  // — the person's queue and watchlist — and the calendar and the inbox
  // sit below it. Each read is the signed-in person's own, as it was at
  // /ux/me, /ux/calendar/me and /ux/inbox, whose spellings retired with
  // no alias (David, 2026-09-25: no shims before 1.0.0). / and /ux still
  // render My Day in place, recorded as an open of /me (surface-opens).
  me:        { id: 'me',        label: 'My Day',           path: '/me',           owner: 'home', person: true },
  schedule:  { id: 'schedule',  label: 'My schedule',      path: '/me/calendar',  permKey: 'schedule', owner: 'home', person: true },
  // The Churn watchlist, under the `accounts` gate it always borrowed
  // (6edb1b77); it was the `watchlist` entry beside an Accounts page
  // until that page went (design 01fa5bdf). Unlisted, as it was. Hosted
  // at the ROOT since backlog 5cb56bb3, the last page to name a host:
  // at the root its path is its slug alone, /watchlist, as before.
  accounts:  { id: 'accounts',  label: 'Churn watchlist',  slug: 'watchlist',     permKey: 'accounts',  owner: 'sales', unlisted: true, host: STATION.algedonic },
  vendors:   { id: 'vendors',   label: 'Vendors',          slug: 'vendors',       permKey: 'vendors',   owner: 'finance', module: 'warehouse', whenDisabled: 'absent', host: STATION.finance },
  // Design45fad3bb preserves financial details when the vendor register
  // is absent. Separate unlisted declarations keep their existing gate
  // and Finance ownership without adding sidebar rows (6d6f88bd).
  'purchase-order': { id: 'purchase-order', label: 'Purchase order', slug: 'purchase-orders/:poId', permKey: 'vendors', owner: 'finance', unlisted: true, host: STATION.finance },
  'vendor-invoice': { id: 'vendor-invoice', label: 'Vendor invoice', slug: 'vendor-invoices/:vendorInvoiceId', permKey: 'vendors', owner: 'finance', unlisted: true, host: STATION.finance },
  inbox:     { id: 'inbox',     label: 'Inbox',            path: '/me/inbox',     permKey: 'inbox',     owner: 'home', person: true },
  manual:    { id: 'manual',    label: 'Manual',           slug: 'manual',        owner: 'home', host: STATION.engineering },
  // The station tree floor, and below it each station's page and its
  // Resources view (router.ts HOSTED). It answered at /it/tree until
  // backlog 92bfcb04 (car 3 of design df0ecd8b: "the whole tree is the
  // company's view of itself"), so the root hosts it and Home owns it.
  'system-tree':             { id: 'system-tree',             label: 'Station tree',        slug: 'tree',             permKey: 'system-yard',             owner: 'home', unlisted: true, host: STATION.algedonic },

  // The IT department (the 2026-08-31 consolidation, packet 1f6d55e0).
  // The catalog keeps non-sidebar entries only where a RouteName still
  // exists (tab pages, unlisted doors) so appForSection() can answer
  // for them. Its pages move into stations in steps 2-4 of design
  // 01fa5bdf; step 1 removed the map, /it/operate (incidents), Atlas,
  // Bottlenecks, Experiments and Policy.
  // The IT app's landing, AT /it itself, flagged `landing` since car 2
  // of design 8c3e9599. It was the DEPARTMENT MAP (design e765b3fc);
  // since step 1 of design 01fa5bdf it is only what the map held that
  // nothing else does yet: the alarms board, and the board with the
  // release and cancel controls, until the station page takes them.
  // Hosted at the ROOT: every station's alarms on one board is the
  // company's view, not any one station's (design deba3000 §1:
  // company-level pages sit at the root). At the root its path is its
  // slug alone, /alarms; /it renders it too, as IT's landing.
  'system-yard':              { id: 'system-yard',              label: 'Alarms',              slug: 'alarms',           permKey: 'system-yard',             owner: 'it', landing: true, host: STATION.algedonic },
  // The Operate row: the audit log leads, its performance tab behind it.
  // It was the incidents surface's (/it/operate) until design 01fa5bdf.
  'system-monitoring':       { id: 'system-monitoring',       label: 'Audit Log',          slug: 'audit',            permKey: 'system-monitoring',      owner: 'it', host: STATION.operations },
  // This served Operate tab must reach the page-audit march too
  // (e52e3017). Catalog membership adds no permission or sidebar row.
  'system-performance':     { id: 'system-performance',      label: 'Performance',         slug: 'perf',             owner: 'it', unlisted: true, host: STATION.operations },
  workflows:                 { id: 'workflows',               label: 'Registry',            slug: 'registry',         permKey: 'workflows',               owner: 'it', host: STATION.engineering },
  'system-step-plugins':     { id: 'system-step-plugins',     label: 'Step plugins',        slug: 'registry/step-plugins', permKey: 'system-step-plugins', owner: 'it', unlisted: true, host: STATION.engineering },
  'system-dispatcher':       { id: 'system-dispatcher',       label: 'Dispatcher rules',    slug: 'registry/dispatcher', permKey: 'system-dispatcher', owner: 'it', unlisted: true, host: STATION.engineering },
  'system-subjects':         { id: 'system-subjects',         label: 'Subjects & Classes',  slug: 'registry/subjects', permKey: 'system-subjects',    owner: 'it', unlisted: true, host: STATION.engineering },
  'system-registry-drift':   { id: 'system-registry-drift',   label: 'Protocol drift',      slug: 'registry/drift',   permKey: 'workflows',             owner: 'it', unlisted: true, host: STATION.engineering },
  // Agents live here and never on the People roster (David, 2026-09-26,
  // answering 6a123f1f; backlog 62988516).
  'system-agents':           { id: 'system-agents',           label: 'Agents',              slug: 'registry/agents',  permKey: 'workflows',            owner: 'it', unlisted: true, host: STATION.engineering },
  // What the estate holds access with — declared, rotated and retired as
  // packets, never a value on the page (David 2026-09-27, design
  // 76155676 step 3; backlog 851259b9). Hosted at the vault, the
  // station whose machines are the credential automations.
  'system-credentials':      { id: 'system-credentials',      label: 'Credentials',         slug: 'credentials',      permKey: 'workflows',       owner: 'it', unlisted: true, host: STATION.vault },
  'system-dispatcher-rules': { id: 'system-dispatcher-rules', label: 'Rule authoring', slug: 'registry/rules', permKey: 'system-dispatcher-rules', owner: 'it', unlisted: true, host: STATION.engineering },
  // The editor's path is a PATTERN, spelled the way surface-opens records
  // every open of it (routePattern). It shared the list's path until
  // backlog 3071e235 (2026-09-24), so the page march — one audit per
  // catalog path — never reached the page carrying all four rule writes.
  'system-dispatcher-rule':  { id: 'system-dispatcher-rule',  label: 'Dispatcher rule — editor',     slug: 'registry/rules/:ruleName', permKey: 'system-dispatcher-rule',  owner: 'it', unlisted: true, host: STATION.engineering },
  // The design station's own page: its address, /engineering/design.
  'system-design':           { id: 'system-design',           label: 'Design',              slug: '',                 permKey: 'system-design',           owner: 'it', host: STATION.design },
  // 'system-feedback' (Feedback triage, /it/design/feedback) and
  // 'system-backlog' (IT backlog, /it/design/backlog) were Design tabs
  // until car N3 of design e765b3fc (2026-09-25) made each a station's
  // panel on the Department Map; both panels went with the map in step 1
  // of design 01fa5bdf.
  // The Codebase — a SIDEBAR ROW, not the Design tab it was: David's
  // feedback 9827c699 (2026-09-14) asked for "a page to the IT department
  // showing the Code base stats" while the trend sat one tab in. permKey-
  // less: the department's own numbers, readable by any operator.
  'system-codebase':         { id: 'system-codebase',         label: 'Codebase',            slug: 'codebase',         owner: 'it', host: STATION.engineering },
  // The hardware registry — declared beside observed beside the
  // difference, plus the dev-workspace door (59ef456a). The estate
  // station's own page: its address, /operations/estate.
  'system-estate':          { id: 'system-estate',           label: 'Estate',              slug: '',                 permKey: 'system-estate',           owner: 'it', host: STATION.estate },
  'system-kb':               { id: 'system-kb',               label: 'Knowledge Base',      slug: 'kb',               permKey: 'system-kb',               owner: 'it', host: STATION.engineering },
  // Unlisted door: reachable, never a sidebar row. It onboards users and
  // issues reset tokens on the credential store, so it is hosted at the
  // vault beside Credentials.
  'auth-admin':              { id: 'auth-admin',              label: 'Auth admin',          slug: 'auth-admin',       permKey: 'auth-admin',              owner: 'it', unlisted: true, host: STATION.vault },
};

/// The migration's progress report (design deba3000 §Shape): every
/// catalog entry that names no host station yet, in catalog order. It
/// fails nothing — pages move a few at a time, usually when a car
/// touches them — and nav-catalog.test.ts prints its count as
/// `unhosted-pages:<n>`, a number that falls. When it reaches zero the
/// `host` field becomes required and the report becomes a pin (car 3).
/// A person's page is hosted — at the signed-in person's own station,
/// /me, whose page the session decides rather than the tree (backlog
/// 5cb56bb3) — so it is not on the report.
export function unhostedPages(): ReadonlyArray<NavItem> {
  return Object.values(ROUTE_CATALOG).filter((e) => e.host === undefined && e.person !== true);
}

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
/// one: its landing is the alarms board at /it, which until car 2 rested
/// on happening to come first in the catalog, beside a sidebar
/// order that was AppShell's own IT_GROUPS list.
export function departmentLanding(code: string): NavItem | undefined {
  const owned = ownedBy(code);
  return owned.find((e) => e.landing === true) ?? owned.find((e) => e.unlisted !== true);
}

/// Which app a surface belongs to, looked up by the `activeSection`
/// id `App.svelte` derives from the current route: the entry's owner.
/// Unknown ids (plain sub-pages) fall back to Home.
export function appForSection(section: string): AppId {
  const entry = (ROUTE_CATALOG as Record<string, NavItem | undefined>)[section];
  // `me` (App.svelte's terminal fallback) is My Day's own entry, owned
  // by Home since backlog 5cb56bb3, and any plain sub-page id resolves
  // to Home too — personal surfaces, which is where the fallback
  // belongs now that there is an app for them.
  return entry?.owner ?? 'home';
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
