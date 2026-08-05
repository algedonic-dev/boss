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

export type AppId =
  | 'home'
  | 'simulator'
  | 'it'
  | 'crm'
  | 'finance'
  | 'operations'
  | 'supply-chain'
  | 'people';

export type AppTab = Readonly<{
  id: AppId;
  label: string;
  /// Where the tab lands. Apps served by a different piece
  /// (simulator) are a full navigation; the rest are same-SPA
  /// routes that happen to be plain anchors, which is fine —
  /// the router picks them up on popstate.
  href: string;
}>;

/// Tab order, left to right. Home first: it is where sign-in lands
/// and where personal work lives regardless of which domain it
/// belongs to. Simulator second — it is the one app that is not a
/// department, because it drives the model rather than doing work
/// inside it. Then the **department apps**.
///
/// IT is a department like the rest, and System Model lives inside it
/// (home-workspace-and-department-apps.md, Q2). The earlier shape had
/// a top-level "System Model" tab sitting beside Simulator as a
/// second model-facing app; the review rejected splitting platform
/// work away from the department that does it. It sits last so the
/// five domain tabs keep the positions operators already know.
export const APPS: ReadonlyArray<AppTab> = [
  { id: 'home', label: 'Home', href: '/' },
  { id: 'simulator', label: 'Simulator', href: '/simulator' },
  { id: 'crm', label: 'CRM', href: '/ux/accounts' },
  { id: 'finance', label: 'Finance', href: '/ux/finance' },
  { id: 'operations', label: 'Operations', href: '/ux/ops' },
  { id: 'supply-chain', label: 'Supply Chain', href: '/ux/warehouse' },
  { id: 'people', label: 'People', href: '/ux/people' },
  { id: 'it', label: 'IT', href: '/system' },
];
