// Pure helpers for EntityLink — extracted so non-Svelte callers can
// import without pulling a component. Same contract as
// apps/web/src/ui/EntityLink.tsx.

import { href } from '../nav';

export type EntityKind =
  | 'account'
  | 'employee'
  | 'job'
  | 'invoice'
  | 'asset'
  | 'part'
  | 'product'
  | 'vendor'
  | 'agreement'
  | 'po'
  | 'vendor-invoice'
  | 'shipment'
  | 'opportunity'
  | 'ticket'
  | 'fact'
  | 'ledger-entry'
  | 'marketing-asset';

export function entityHref(kind: EntityKind, id: string): string {
  const encoded = encodeURIComponent(id);
  switch (kind) {
    case 'account': return href(`/accounts/${encoded}`);
    case 'employee': return href(`/people/${encoded}`);
    case 'job': return href(`/jobs/${encoded}`);
    case 'invoice': return href(`/finance/${encoded}`);
    case 'asset': return href(`/assets/${encoded}`);
    case 'part': return href(`/parts/${encoded}`);
    case 'product': return href(`/products/${encoded}`);
    case 'vendor': return href(`/vendors/${encoded}`);
    case 'agreement': return href(`/accounts/agreements/${encoded}`);
    case 'po': return href(`/purchase-orders/${encoded}`);
    case 'vendor-invoice': return href(`/vendor-invoices/${encoded}`);
    case 'shipment': return href(`/shipments/${encoded}`);
    case 'opportunity': return href(`/sales/opportunities/${encoded}`);
    case 'ticket': return href(`/support/${encoded}`);
    case 'fact': return href(`/finance?fact=${encoded}`);
    case 'ledger-entry': return href(`/finance?entry=${encoded}`);
    case 'marketing-asset': return href(`/assets/${encoded}`);
  }
}

export const ID_IS_LABEL: ReadonlySet<EntityKind> = new Set<EntityKind>([
  'asset', 'product', 'invoice', 'po', 'vendor-invoice', 'shipment', 'ticket', 'fact',
  'ledger-entry', 'marketing-asset',
]);
