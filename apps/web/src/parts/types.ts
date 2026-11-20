// Purchase-order types — the last of the parts domain's web types,
// kept because po/PoPage.svelte reads PurchaseOrder. The parts pages
// were deleted in train #1838 (design 01fa5bdf, UI step 1), and with
// them every reader of the rest of this file: kindFromSku (one
// tenant's ING-/PKG- SKU-prefix taxonomy hardcoded in core web,
// backlog c58bd4e8), the stock-status helpers, CatalogPart and the
// device-model part collector went with no caller left.

export type PoStatus =
  | 'draft'
  | 'submitted'
  | 'acknowledged'
  | 'in-transit'
  | 'received'
  | 'closed';

export type PurchaseOrderLine = {
  part_sku: string;
  qty: number;
  unit_cost_cents: number;
  currency: string;
};

export type PurchaseOrder = {
  id: string;
  vendor: string;
  status: PoStatus;
  placed_on: string;
  expected_on: string;
  received_on: string | null;
  lines: ReadonlyArray<PurchaseOrderLine>;
};
