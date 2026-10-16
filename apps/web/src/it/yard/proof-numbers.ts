/** Exact JSON number spellings, keyed by metadata-relative JSON pointer.
 *  Parsed metadata remains unchanged; absent evidence is not an integer. */
export type ProofNumbers = Readonly<Record<string, string>>;
type IntegerReading = bigint | null | 'unread';

function serdeInteger(value: unknown, token: string | undefined, min: bigint, max: bigint): IntegerReading {
  if (typeof value !== 'number') return null;
  if (token === undefined || !Object.is(Number(token), value)) return 'unread';
  // serde_json preserves -0 as floating point, as well as decimal/exponent tokens.
  if (token === '-0' || !/^-?(?:0|[1-9]\d*)$/.test(token)) return null;
  const integer = BigInt(token);
  return integer >= min && integer <= max ? integer : null;
}
export const serdeI64 = (value: unknown, token: string | undefined): IntegerReading =>
  serdeInteger(value, token, -9_223_372_036_854_775_808n, 9_223_372_036_854_775_807n);
export const serdeU64 = (value: unknown, token: string | undefined): IntegerReading =>
  serdeInteger(value, token, 0n, 18_446_744_073_709_551_615n);
