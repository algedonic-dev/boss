// The native contract test supplies its discovered vocabulary and computes
// its own expected answers with the compiled Rust methods.
import { purchaseOrderIsOpen, vendorInvoiceIsPaid } from '../../src/vendors/vendor-status';

const input: unknown = JSON.parse(await Bun.stdin.text());
if (input === null || typeof input !== 'object') throw new Error('status contract input missing');
const fields = input as Record<string, unknown>;
const strings = (value: unknown): ReadonlyArray<string> => {
  if (!Array.isArray(value) || value.length === 0 || value.some((entry) => typeof entry !== 'string')) {
    throw new Error('status vocabulary must be a nonempty string array');
  }
  return value as string[];
};
console.log(JSON.stringify({
  pos: strings(fields['pos']).map(purchaseOrderIsOpen),
  bills: strings(fields['bills']).map(vendorInvoiceIsPaid),
}));
