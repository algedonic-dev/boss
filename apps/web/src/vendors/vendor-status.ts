// These predicates mirror the owning inventory methods. The native contract
// test compares their behavior across its full vocabulary and tenant additions.
export function purchaseOrderIsOpen(status: string): boolean {
  return status !== 'received' && status !== 'closed';
}

export function vendorInvoiceIsPaid(status: string): boolean {
  return status === 'paid';
}
