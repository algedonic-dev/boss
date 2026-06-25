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
    case 'account': return href(`/ux/accounts/${encoded}`);
    case 'employee': return href(`/ux/people/${encoded}`);
    case 'job': return href(`/ux/jobs/${encoded}`);
    case 'invoice': return href(`/ux/finance/${encoded}`);
    case 'asset': return href(`/ux/assets/${encoded}`);
    case 'part': return href(`/ux/parts/${encoded}`);
    case 'product': return href(`/ux/products/${encoded}`);
    case 'vendor': return href(`/ux/vendors/${encoded}`);
    case 'agreement': return href(`/ux/accounts/agreements/${encoded}`);
    case 'po': return href(`/ux/purchase-orders/${encoded}`);
    case 'vendor-invoice': return href(`/ux/vendor-invoices/${encoded}`);
    case 'shipment': return href(`/ux/shipments/${encoded}`);
    case 'opportunity': return href(`/ux/sales/opportunities/${encoded}`);
    case 'ticket': return href(`/ux/support/${encoded}`);
    case 'fact': return href(`/ux/finance?fact=${encoded}`);
    case 'ledger-entry': return href(`/ux/finance?entry=${encoded}`);
    case 'marketing-asset': return href(`/ux/assets/${encoded}`);
  }
}

export const ID_IS_LABEL: ReadonlySet<EntityKind> = new Set<EntityKind>([
  'asset', 'product', 'invoice', 'po', 'vendor-invoice', 'shipment', 'ticket', 'fact',
  'ledger-entry', 'marketing-asset',
]);
