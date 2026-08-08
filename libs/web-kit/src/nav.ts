// Shared client-side navigation. Each app owns its own Route union +
// parseRoute; only these origin-relative helpers are shared.
export function navigate(path: string): void {
  window.history.pushState({}, '', path);
  window.dispatchEvent(new PopStateEvent('popstate'));
}
/** href factory for a mount prefix, e.g. makeHref('/simulator'). */
export function makeHref(basePrefix: string): (relative: string) => string {
  return (relative: string): string =>
    basePrefix + (relative.startsWith('/') ? relative : `/${relative}`);
}
/** Default href — auto-detects the /dashboard mount (apps/web behavior). */
export function href(relative: string): string {
  const base = window.location.pathname.startsWith('/dashboard') ? '/dashboard' : '';
  return base + (relative.startsWith('/') ? relative : `/${relative}`);
}

// ---------------------------------------------------------------------------
// Top-level apps — the tabs in the chrome bar.
//
// Each app owns a tab and the whole surface below it: its own sidebar,
// its own layout, its own idea of what a landing page is. The chrome
// bar itself (wordmark, tabs, system time, sign-in) is the only thing
// every app shares.
//
// Apps partition PRESENTATION, never data. CRM's "account", Finance's
// "account" and the Job the Operations app lists against it are one
// Subject read through three lenses — not three records federated at
// the UI and kept in step by convention. That is the whole claim: an
// enterprise-suite shape on top of one coherent information layer,
// rather than the usual suite of separate systems with an integration
// budget. Adding an app must never mean adding a store.
//
// Lives in web-kit because two apps render the bar: apps/web (which
// serves home/model and the domain apps) and apps/simulator (its own
// service). Which SURFACES belong to which app is a separate question,
// answered by apps/web's nav-catalog `app` field — web-kit has no
// business knowing about /ux/warehouse.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Apps ARE departments.
//
// They used to be invented groupings — `crm`, `supply-chain`,
// `operations` — and none of those is a department Algedonic Ales has.
// That was reported plainly: "CRM is not a department for example. The
// only exception to the department-based apps is the Simulator."
//
// The earlier shape had 7 apps against 15 departments: 3 of the apps
// were real departments, 11 departments had no app at all, and
// `operations` represented nobody, so nobody could reclaim it. An app
// that names no part of the org is a grouping the reader has to learn
// instead of one they already know.
//
// Two things this does NOT mean. Apps partition PRESENTATION, never
// data — one Subject read through several lenses, not several records
// federated at the UI. And an app is not a permission boundary: "Apps
// don't have any relationship to the data, so there is no actual or
// technical silo behind the app grouping. It is really for helping the
// humans navigate."
//
// Home and Simulator are the two exceptions, for opposite reasons.
// Home is cross-cutting: personal work belongs to whoever is doing it
// regardless of which department the Job sits in, and it is the answer
// to "I shouldn't be jerked around through apps as I work" — following
// your own queue must never bounce you between tabs. Simulator drives
// the model rather than doing work inside it.
// ---------------------------------------------------------------------------

/// The department vocabulary, in registry sort order.
///
/// This is the Class registry's `(employee, *, department)` rows —
/// core's in infra/postgres/schema/01-registries.sql plus the tenant's
/// in the seed. It is duplicated here because the chrome bar cannot
/// wait on a fetch to know what tabs exist, so it is pinned by an
/// equality test against both sources (CLAUDE.md §9a): add a
/// department to either registry and the test names it until it
/// appears here.
export const DEPARTMENTS = [
  { code: 'it', label: 'IT' },
  { code: 'executive', label: 'Executive' },
  { code: 'sales', label: 'Sales' },
  { code: 'service', label: 'Service' },
  { code: 'refurb', label: 'Refurb' },
  { code: 'qa', label: 'QA' },
  { code: 'warehouse', label: 'Warehouse' },
  { code: 'finance', label: 'Finance' },
  { code: 'people', label: 'People' },
  { code: 'support', label: 'Support' },
  { code: 'marketing', label: 'Marketing' },
  { code: 'production', label: 'Production' },
  { code: 'packaging', label: 'Packaging' },
  { code: 'distribution', label: 'Distribution' },
  { code: 'maintenance', label: 'Maintenance' },
  { code: 'taproom', label: 'Taproom' },
  { code: 'audit', label: 'Audit' },
] as const;

export type DepartmentCode = (typeof DEPARTMENTS)[number]['code'];

/// Every app: the two that are not departments, plus one per
/// department. `AppId` stays a closed union so a typo in a catalog
/// entry is a compile error rather than a surface that renders under
/// no tab.
export type AppId = 'home' | 'simulator' | DepartmentCode;

export type AppTab = Readonly<{
  id: AppId;
  label: string;
  /// Where the tab lands. Apps served by a different piece
  /// (simulator) are a full navigation; the rest are same-SPA
  /// routes that happen to be plain anchors, which is fine —
  /// the router picks them up on popstate.
  href: string;
}>;

/// The two apps that are not departments. Home first — it is where
/// sign-in lands and where personal work lives whichever department
/// the work belongs to.
export const HOME_APP: AppTab = { id: 'home', label: 'Home', href: '/' };
export const SIMULATOR_APP: AppTab = {
  id: 'simulator',
  label: 'Simulator',
  href: '/simulator',
};

/// The default tab list, for a host with no catalog of its own
/// (apps/simulator). apps/web passes its own list, built from the nav
/// catalog, because which department a SURFACE belongs to is the
/// host's question — web-kit has no business knowing about
/// /ux/warehouse.
export const APPS: ReadonlyArray<AppTab> = [HOME_APP, SIMULATOR_APP];

/// The label for a department code, or the code itself if unknown.
export function departmentLabel(code: string): string {
  return DEPARTMENTS.find((d) => d.code === code)?.label ?? code;
}
