// The nav catalog — the single registry of every routable nav entry:
// its label, path, permKey, tenant-module gate, and which top-level
// **app** it belongs to.
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
//     on each said as much. `app` below is now the only place that
//     answers "which app does this surface belong to", and both
//     consumers derive from it.
//
//  2. `sidebar-router-consistency.test.ts` hand-mirrored every sidebar
//     path, because (its header explains) parsing a TypeScript const
//     out of a Svelte `<script>` from a Bun test is fragile. As a
//     plain module the test imports the real thing, so the mirror —
//     and the drift it was there to catch — is gone.
//
// Adding a surface: add one entry here with its `app`, and add a
// matching branch to router.ts. The consistency test enforces the
// second half.

import type { RouteName } from '@boss/web-kit/session/permissions';
import type { AppId } from '@boss/web-kit/nav';

export type { AppId };

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
  /// Which app owns this surface. Required on every catalog entry —
  /// an unassigned surface is how a page ends up rendering under the
  /// wrong tab. Plain sub-page links (Audit Log, Atlas) declared
  /// inline in a nav group carry no `app`; they inherit the group
  /// they sit in.
  app?: AppId;
}>;

export type NavGroup = Readonly<{ label: string; items: ReadonlyArray<NavItem> }>;

export const ROUTE_CATALOG: Readonly<Record<RouteName, NavItem>> = {
  jobs:      { id: 'jobs',      label: 'All jobs',         path: '/ux/jobs',      permKey: 'jobs',      app: 'operations' },
  sales:     { id: 'sales',     label: 'Sales pipeline',   path: '/ux/sales',     permKey: 'sales',     app: 'crm' },
  service:   { id: 'service',   label: 'Service queue',    path: '/ux/service',   permKey: 'service',   module: 'support', app: 'operations' },
  refurb:    { id: 'refurb',    label: 'Refurbishment',    path: '/ux/refurb',    permKey: 'refurb',    module: 'support', app: 'operations' },
  qa:        { id: 'qa',        label: 'QA',               path: '/ux/qa',        permKey: 'qa',        module: 'qa',      app: 'operations' },
  finance:   { id: 'finance',   label: 'Finance',          path: '/ux/finance',   permKey: 'finance',   module: 'finance', app: 'finance' },
  warehouse: { id: 'warehouse', label: 'Inventory',        path: '/ux/warehouse', permKey: 'warehouse', module: 'warehouse', app: 'supply-chain' },
  shipping:  { id: 'shipping',  label: 'Shipments',        path: '/ux/shipping',  permKey: 'shipping',  module: 'shipping', app: 'supply-chain' },
  support:   { id: 'support',   label: 'Support',          path: '/ux/support',   permKey: 'support',   module: 'support', app: 'crm' },
  ops:       { id: 'ops',       label: 'Operations',       path: '/ux/ops',       permKey: 'ops',       app: 'operations' },
  exec:      { id: 'exec',      label: 'Exec',             path: '/ux/exec',      permKey: 'exec',      module: 'exec',    app: 'home' },
  schedule:  { id: 'schedule',  label: 'My schedule',      path: '/ux/calendar/me', permKey: 'schedule', app: 'home' },
  catalog:   { id: 'catalog',   label: 'Equipment',        path: '/ux/catalog',   permKey: 'catalog',   module: 'equipment', app: 'supply-chain' },
  parts:     { id: 'parts',     label: 'Ingredients & parts', path: '/ux/parts',  permKey: 'parts',     module: 'parts',   app: 'supply-chain' },
  products:  { id: 'products',  label: 'Products',         path: '/ux/products',  permKey: 'parts',     module: 'parts',   app: 'supply-chain' },
  accounts:  { id: 'accounts',  label: 'Accounts',         path: '/ux/accounts',  permKey: 'accounts',  app: 'crm' },
  vendors:   { id: 'vendors',   label: 'Vendors',          path: '/ux/vendors',   permKey: 'vendors',   app: 'finance' },
  people:    { id: 'people',    label: 'Employees',        path: '/ux/people',    permKey: 'people',    app: 'people' },
  assets:    { id: 'assets',    label: 'Assets',           path: '/ux/assets',    permKey: 'assets',    module: 'equipment', app: 'supply-chain' },
  shop:      { id: 'shop',      label: 'Shop',             path: '/ux/shop',      permKey: 'shop',      app: 'crm' },
  inbox:     { id: 'inbox',     label: 'Inbox',            path: '/ux/inbox',     permKey: 'inbox',     app: 'home' },
  views:     { id: 'views',     label: 'Views',            path: '/ux/views',     permKey: 'views',     app: 'home' },
  'marketing-assets': { id: 'marketing-assets', label: 'Marketing assets', path: '/ux/marketing-assets', permKey: 'marketing-assets', module: 'marketing-assets', app: 'crm' },
  calendar:  { id: 'calendar',  label: 'Release calendar', path: '/ux/calendar',  permKey: 'calendar',  module: 'calendar', app: 'operations' },

  // Modeling surfaces — operator-tier (no separate /admin tier).
  // policy + job-kinds are dept-head + COO authority (per the
  // "engineers are operators like anyone else" frame). Step
  // plugins are JS bundle authoring → IT engineering work.
  'system-monitoring':       { id: 'system-monitoring',       label: 'Monitoring',          path: '/system/monitoring',   permKey: 'system-monitoring',       app: 'it' },
  policy:                    { id: 'policy',                  label: 'Policy',              path: '/system/policy',       permKey: 'policy',                  app: 'it' },
  'job-kinds':               { id: 'job-kinds',               label: 'Job kinds',           path: '/system/job-kinds',    permKey: 'job-kinds',               app: 'it' },
  'system-step-plugins':     { id: 'system-step-plugins',     label: 'Step plugins',        path: '/system/step-plugins', permKey: 'system-step-plugins',     app: 'it' },
  'system-dispatcher':       { id: 'system-dispatcher',       label: 'Dispatcher rules',    path: '/system/dispatcher',   permKey: 'system-dispatcher',       app: 'it' },
  'system-model':            { id: 'system-model',            label: 'System Model',        path: '/system',              permKey: 'system-model',            app: 'it' },
  'system-subjects':         { id: 'system-subjects',         label: 'Subjects & Classes',  path: '/system/subjects',     permKey: 'system-subjects',         app: 'it' },
  // The rule-authoring list + editor are reached via a link FROM the
  // cascade viz (the system-dispatcher Surface entry), not their own
  // sidebar rows — so these catalog entries exist to satisfy the
  // Record<RouteName,…> type but are intentionally absent from
  // SURFACE_ORDER (no sidebar item ⇒ no sidebar-consistency entry).
  'system-dispatcher-rules': { id: 'system-dispatcher-rules', label: 'Dispatcher rules — authoring', path: '/system/dispatcher/rules', permKey: 'system-dispatcher-rules', app: 'it' },
  'system-dispatcher-rule':  { id: 'system-dispatcher-rule',  label: 'Dispatcher rule — editor',     path: '/system/dispatcher/rules', permKey: 'system-dispatcher-rule',  app: 'it' },
  'system-design':           { id: 'system-design',           label: 'Design review',       path: '/system/design',       permKey: 'system-design',           app: 'it' },
  'system-feedback':         { id: 'system-feedback',         label: 'Feedback triage',     path: '/system/feedback',     permKey: 'system-feedback',         app: 'it' },
  // The "Evolve" surface — controlled, sandboxed model modifications
  // (placeholder for now; visible to every role via canSeeRoute).
  'system-experiments':      { id: 'system-experiments',      label: 'Experiments',         path: '/system/experiments',  permKey: 'system-experiments',      app: 'it' },
  'system-kb':               { id: 'system-kb',               label: 'Knowledge Base',      path: '/system/kb',           permKey: 'system-kb',               app: 'it' },
  'auth-admin':              { id: 'auth-admin',              label: 'Auth admin',          path: '/system/auth-admin',   permKey: 'auth-admin',              app: 'it' },
  // KB view of every active JobKind — read-only catalog, visible to
  // every role via canSeeRoute() short-circuit. Editing lives at
  // /system/job-kinds, reached FROM Workflows.
  workflows:                 { id: 'workflows',               label: 'Workflows',           path: '/system/workflows',    permKey: 'workflows',               app: 'it' },
};

/// Which app each employee **department** is served by.
///
/// Departments are Class-registry data — 19 of them, seeded in
/// `infra/postgres/schema/01-registries.sql` with
/// `member_attribute='department'`. The eight apps are a presentation
/// decision, deliberately not one-per-department: production,
/// packaging, taproom, maintenance, qa, service, support and refurb
/// all work the same surfaces, so they share Operations.
///
/// So this is not a derivation, it is a **mapping that must stay
/// exhaustive**. Deriving tabs from departments would produce
/// nineteen of them and undo the app split the review settled;
/// leaving the relationship implicit is how `audit` ended up with no
/// app at all and nothing failing. The test beside this asserts every
/// registry department lands somewhere, so adding one is a decision
/// somebody makes rather than an omission nobody notices.
export const DEPARTMENT_APP: Readonly<Record<string, AppId>> = {
  // Runs the BOSS deployment itself, and the model that describes it.
  platform: 'it',
  it: 'it',
  admin: 'it',
  // Reads everything, writes nothing — its work is the audit log and
  // the books, both of which live under IT's model surfaces.
  audit: 'it',

  executive: 'home',

  finance: 'finance',

  sales: 'crm',
  marketing: 'crm',
  support: 'crm',

  // The floor: everything that turns inputs into product, plus the
  // service and rework flows that share the Jobs/Steps surfaces.
  production: 'operations',
  packaging: 'operations',
  taproom: 'operations',
  maintenance: 'operations',
  qa: 'operations',
  service: 'operations',
  refurb: 'operations',

  warehouse: 'supply-chain',
  distribution: 'supply-chain',

  people: 'people',
};

/// Which app a surface belongs to, looked up by the `activeSection`
/// id `App.svelte` derives from the current route. Unknown ids (the
/// `me` fallback, plain sub-pages) fall back to `user` — the same
/// answer the previous `MODEL_KINDS.has(route.kind)` check gave for
/// anything it didn't list.
export function appForSection(section: string): AppId {
  const entry = (ROUTE_CATALOG as Record<string, NavItem | undefined>)[section];
  // `me` (App.svelte's terminal fallback) and any plain sub-page id
  // resolve to Home — personal surfaces, which is where the fallback
  // belongs now that there is an app for them.
  return entry?.app ?? 'home';
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
export const APP_SUBJECT_KINDS: Readonly<Record<AppId, ReadonlyArray<string>>> = {
  // Inbox lives here, and messages are what it lists.
  home: ['message'],
  simulator: [],
  // `custom` is the escape hatch for Jobs about things that are not
  // domain Subjects — a design doc is the shipped example, and
  // /system/design is an IT surface.
  it: ['job-kind', 'company', 'custom'],
  crm: ['account', 'customer', 'campaign', 'marketing-asset'],
  finance: ['invoice', 'vendor', 'vendor-invoice', 'purchase_order'],
  operations: ['job-kind', 'calendar', 'location'],
  'supply-chain': ['product', 'asset', 'purchase_order', 'shipment', 'vendor'],
  people: ['employee'],
};
