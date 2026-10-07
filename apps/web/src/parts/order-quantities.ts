import { readRows } from '../data/shape';

// The reader only consumes identity, status and SKU quantities. Vendor
// and date fields may legitimately be null on a draft order.
export function orderQuantities(url: string, body: unknown): ReadonlyMap<string, number> {
  const fail = (detail: string): never => {
    throw new Error(`${url}: HTTP 200, but ${detail}`);
  };
  const record = (value: unknown): value is Record<string, unknown> =>
    typeof value === 'object' && value !== null && !Array.isArray(value);
  const named = (value: unknown): value is string => typeof value === 'string' && value.trim().length > 0;
  const seen = new Set<string>();
  const quantities = new Map<string, number>();
  for (const [index, row] of readRows(url, body).entries()) {
    if (!record(row) || !named(row['id']) || !named(row['status']) || !Array.isArray(row['lines'])) {
      return fail(`purchase order ${index + 1} has invalid identity, status or lines`);
    }
    // The native status taxonomy is open; only these two are terminal.
    const id = row['id'];
    if (seen.has(id)) fail(`purchase order ${index + 1} has a duplicate identity`);
    seen.add(id);
    for (const [lineIndex, line] of row['lines'].entries()) {
      if (!record(line) || !named(line['part_sku']) || typeof line['qty'] !== 'number'
        || !Number.isInteger(line['qty']) || line['qty'] < 0 || line['qty'] > 4294967295) {
        return fail(`purchase order ${index + 1}, line ${lineIndex + 1} has an invalid SKU or quantity`);
      }
      if (row['status'] === 'received' || row['status'] === 'closed') continue;
      const sku = line['part_sku'];
      const sum = (quantities.get(sku) ?? 0) + line['qty'];
      if (!Number.isSafeInteger(sum)) fail(`quantity for ${sku} exceeds the safe integer range`);
      quantities.set(sku, sum);
    }
  }
  return quantities;
}
