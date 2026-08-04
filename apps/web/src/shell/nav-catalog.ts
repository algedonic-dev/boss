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

/// Top-level apps. Each owns a tab in the chrome bar and its own
/// sidebar below it; every routable surface belongs to exactly one.
///
/// Apps partition PRESENTATION, never data — CRM's "account" and
/// Finance's "account" are the same Subject read through two lenses,
/// not two records kept in step by convention. Adding an app must
/// never mean adding a store.
export type AppId = 'simulator' | 'model' | 'user';

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
  jobs:      { id: 'jobs',      label: 'All jobs',         path: '/ux/jobs',      permKey: 'jobs',      app: 'user' },
  sales:     { id: 'sales',     label: 'Sales pipeline',   path: '/ux/sales',     permKey: 'sales',     app: 'user' },
  service:   { id: 'service',   label: 'Service queue',    path: '/ux/service',   permKey: 'service',   module: 'support', app: 'user' },
  refurb:    { id: 'refurb',    label: 'Refurbishment',    path: '/ux/refurb',    permKey: 'refurb',    module: 'support', app: 'user' },
  qa:        { id: 'qa',        label: 'QA',               path: '/ux/qa',        permKey: 'qa',        module: 'qa',      app: 'user' },
  finance:   { id: 'finance',   label: 'Finance',          path: '/ux/finance',   permKey: 'finance',   module: 'finance', app: 'user' },
  warehouse: { id: 'warehouse', label: 'Inventory',        path: '/ux/warehouse', permKey: 'warehouse', module: 'warehouse', app: 'user' },
  shipping:  { id: 'shipping',  label: 'Shipments',        path: '/ux/shipping',  permKey: 'shipping',  module: 'shipping', app: 'user' },
  support:   { id: 'support',   label: 'Support',          path: '/ux/support',   permKey: 'support',   module: 'support', app: 'user' },
  ops:       { id: 'ops',       label: 'Operations',       path: '/ux/ops',       permKey: 'ops',       app: 'user' },
  exec:      { id: 'exec',      label: 'Exec',             path: '/ux/exec',      permKey: 'exec',      module: 'exec',    app: 'user' },
  schedule:  { id: 'schedule',  label: 'My schedule',      path: '/ux/calendar/me', permKey: 'schedule', app: 'user' },
  catalog:   { id: 'catalog',   label: 'Equipment',        path: '/ux/catalog',   permKey: 'catalog',   module: 'equipment', app: 'user' },
  parts:     { id: 'parts',     label: 'Ingredients & parts', path: '/ux/parts',  permKey: 'parts',     module: 'parts',   app: 'user' },
  products:  { id: 'products',  label: 'Products',         path: '/ux/products',  permKey: 'parts',     module: 'parts',   app: 'user' },
  accounts:  { id: 'accounts',  label: 'Accounts',         path: '/ux/accounts',  permKey: 'accounts',  app: 'user' },
  vendors:   { id: 'vendors',   label: 'Vendors',          path: '/ux/vendors',   permKey: 'vendors',   app: 'user' },
  people:    { id: 'people',    label: 'Employees',        path: '/ux/people',    permKey: 'people',    app: 'user' },
  assets:    { id: 'assets',    label: 'Assets',           path: '/ux/assets',    permKey: 'assets',    module: 'equipment', app: 'user' },
  shop:      { id: 'shop',      label: 'Shop',             path: '/ux/shop',      permKey: 'shop',      app: 'user' },
  inbox:     { id: 'inbox',     label: 'Inbox',            path: '/ux/inbox',     permKey: 'inbox',     app: 'user' },
  'marketing-assets': { id: 'marketing-assets', label: 'Marketing assets', path: '/ux/marketing-assets', permKey: 'marketing-assets', module: 'marketing-assets', app: 'user' },
  calendar:  { id: 'calendar',  label: 'Release calendar', path: '/ux/calendar',  permKey: 'calendar',  module: 'calendar', app: 'user' },

  // Modeling surfaces — operator-tier (no separate /admin tier).
  // policy + job-kinds are dept-head + COO authority (per the
  // "engineers are operators like anyone else" frame). Step
  // plugins are JS bundle authoring → IT engineering work.
  'system-monitoring':       { id: 'system-monitoring',       label: 'Monitoring',          path: '/system/monitoring',   permKey: 'system-monitoring',       app: 'model' },
  policy:                    { id: 'policy',                  label: 'Policy',              path: '/system/policy',       permKey: 'policy',                  app: 'model' },
  'job-kinds':               { id: 'job-kinds',               label: 'Job kinds',           path: '/system/job-kinds',    permKey: 'job-kinds',               app: 'model' },
  'system-step-plugins':     { id: 'system-step-plugins',     label: 'Step plugins',        path: '/system/step-plugins', permKey: 'system-step-plugins',     app: 'model' },
  'system-dispatcher':       { id: 'system-dispatcher',       label: 'Dispatcher rules',    path: '/system/dispatcher',   permKey: 'system-dispatcher',       app: 'model' },
  'system-model':            { id: 'system-model',            label: 'System Model',        path: '/system',              permKey: 'system-model',            app: 'model' },
  'system-subjects':         { id: 'system-subjects',         label: 'Subjects & Classes',  path: '/system/subjects',     permKey: 'system-subjects',         app: 'model' },
  // The rule-authoring list + editor are reached via a link FROM the
  // cascade viz (the system-dispatcher Surface entry), not their own
  // sidebar rows — so these catalog entries exist to satisfy the
  // Record<RouteName,…> type but are intentionally absent from
  // SURFACE_ORDER (no sidebar item ⇒ no sidebar-consistency entry).
  'system-dispatcher-rules': { id: 'system-dispatcher-rules', label: 'Dispatcher rules — authoring', path: '/system/dispatcher/rules', permKey: 'system-dispatcher-rules', app: 'model' },
  'system-dispatcher-rule':  { id: 'system-dispatcher-rule',  label: 'Dispatcher rule — editor',     path: '/system/dispatcher/rules', permKey: 'system-dispatcher-rule',  app: 'model' },
  'system-design':           { id: 'system-design',           label: 'Design review',       path: '/system/design',       permKey: 'system-design',           app: 'model' },
  // The "Evolve" surface — controlled, sandboxed model modifications
  // (placeholder for now; visible to every role via canSeeRoute).
  'system-experiments':      { id: 'system-experiments',      label: 'Experiments',         path: '/system/experiments',  permKey: 'system-experiments',      app: 'model' },
  'system-kb':               { id: 'system-kb',               label: 'Knowledge Base',      path: '/system/kb',           permKey: 'system-kb',               app: 'model' },
  'auth-admin':              { id: 'auth-admin',              label: 'Auth admin',          path: '/system/auth-admin',   permKey: 'auth-admin',              app: 'model' },
  // KB view of every active JobKind — read-only catalog, visible to
  // every role via canSeeRoute() short-circuit. Editing lives at
  // /system/job-kinds, reached FROM Workflows.
  workflows:                 { id: 'workflows',               label: 'Workflows',           path: '/system/workflows',    permKey: 'workflows',               app: 'model' },
};

/// Which app a surface belongs to, looked up by the `activeSection`
/// id `App.svelte` derives from the current route. Unknown ids (the
/// `me` fallback, plain sub-pages) fall back to `user` — the same
/// answer the previous `MODEL_KINDS.has(route.kind)` check gave for
/// anything it didn't list.
export function appForSection(section: string): AppId {
  const entry = (ROUTE_CATALOG as Record<string, NavItem | undefined>)[section];
  return entry?.app ?? 'user';
}
