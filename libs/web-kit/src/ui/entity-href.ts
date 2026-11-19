// Pure helpers for EntityLink — extracted so non-Svelte callers can
// import without pulling a component. Same contract as
// apps/web/src/ui/EntityLink.tsx.

import { href } from '../nav';

// The kinds, as DATA — the union is derived from these arrays so the
// list a test can iterate and the type the compiler checks cannot
// disagree (CLAUDE.md 9a). Every kind with a page must resolve to that
// page's route: apps/web/src/entity-href-routes.test.ts is the pin.
//
// 'ticket', 'agreement' and 'opportunity' were RETIRED here on
// 2026-09-20 (backlog 38d4e458, from the /ux/support page audit
// 9876ef0d): each produced a path the router never matched, so every
// link they made was a dead end. A page is a page car; producing a link
// to a page that does not exist is not.

/// Kinds with a page of their own: entityHref answers each with that
/// page's path.
export const PAGE_KINDS = [
  'job',
  'invoice',
  'vendor',
  'po',
  'vendor-invoice',
  'fact',
  'ledger-entry',
] as const;

/// Kinds an id is still recognised as — a manual names an employee, a
/// step surface its account or asset — but with NO page: their /ux pages
/// went with step 1 of design 01fa5bdf (David, 2026-11-17). An id of
/// one of these is drawn as text, never as a link to a page that would
/// answer not-found (the same rule that retired 'ticket' above). A kind
/// moves back to PAGE_KINDS when a station's page draws it.
export const PAGELESS_KINDS = [
  'account',
  'employee',
  'asset',
  'part',
  'product',
  'shipment',
  'marketing-asset',
] as const;

export const ENTITY_KINDS = [...PAGE_KINDS, ...PAGELESS_KINDS] as const;

export type PageKind = (typeof PAGE_KINDS)[number];
export type EntityKind = (typeof ENTITY_KINDS)[number];

/// Whether `kind` has a page to link to.
export function hasPage(kind: EntityKind): kind is PageKind {
  return (PAGE_KINDS as ReadonlyArray<string>).includes(kind);
}

export function entityHref(kind: PageKind, id: string): string {
  const encoded = encodeURIComponent(id);
  switch (kind) {
    case 'job': return href(`/ux/jobs/${encoded}`);
    case 'invoice': return href(`/finance/invoices/${encoded}`);
    case 'vendor': return href(`/finance/vendors/${encoded}`);
    case 'po': return href(`/finance/purchase-orders/${encoded}`);
    case 'vendor-invoice': return href(`/finance/vendor-invoices/${encoded}`);
    case 'fact': return href(`/finance?fact=${encoded}`);
    case 'ledger-entry': return href(`/finance?entry=${encoded}`);
  }
}

export const ID_IS_LABEL: ReadonlySet<EntityKind> = new Set<EntityKind>([
  'asset', 'product', 'invoice', 'po', 'vendor-invoice', 'shipment', 'fact',
  'ledger-entry', 'marketing-asset',
]);
