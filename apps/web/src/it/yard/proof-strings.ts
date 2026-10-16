/** Rust str::trim uses Unicode White_Space, which differs from JS trim
 * (notably U+0085 and U+FEFF). proof-strings.json pins the complete set
 * against actual Rust char::is_whitespace and this reader. */
export const rustTrim = (value: string): string => value.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, '');
export const proofString = (value: unknown): string | null => typeof value === 'string' ? value : null;
export const nonBlankProofString = (value: unknown): string | null => {
  const text = proofString(value);
  return text === null ? null : rustTrim(text) || null;
};
/** WaitOwner uses eq_ignore_ascii_case; bindings otherwise stay exact. */
export const isWorldOwner = (value: string): boolean => value.replace(/[A-Z]/g, character => character.toLowerCase()) === 'world';
