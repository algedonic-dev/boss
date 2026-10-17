import type { ProofNumbers } from './proof-numbers';

type Tokens = Readonly<{ number?: string; fields?: ReadonlyMap<string, Tokens>; items?: readonly Tokens[] }>;
const object = (v: unknown): Record<string, unknown> | null => typeof v === 'object' && v !== null && !Array.isArray(v)
  ? v as Record<string, unknown> : null;

/** Native JSON.parse validates the whole document first. This structural
 *  token walk then retains number spellings: string escapes, duplicate keys
 *  (last wins) and array positions follow that same validated document. */
function numberTokens(source: string): Tokens {
  let at = 0;
  const whitespace = () => { while (/\s/.test(source[at] ?? '') && at < source.length) at++; };
  const string = (): string => {
    const begin = at++;
    while (source[at] !== '"') {
      if (source[at] === '\\') at++;
      at++;
    }
    at++;
    return JSON.parse(source.slice(begin, at)) as string;
  };
  const value = (): Tokens => {
    whitespace();
    if (source[at] === '"') { string(); return {}; }
    if (source[at] === '{') {
      at++; whitespace();
      const fields = new Map<string, Tokens>();
      while (source[at] !== '}') {
        whitespace();
        const key = string(); whitespace(); at++;
        fields.set(key, value()); whitespace();
        if (source[at] === ',') at++;
      }
      at++;
      return { fields };
    }
    if (source[at] === '[') {
      at++; whitespace();
      const items: Tokens[] = [];
      while (source[at] !== ']') {
        items.push(value()); whitespace();
        if (source[at] === ',') at++;
      }
      at++;
      return { items };
    }
    const begin = at;
    while (at < source.length && !/[\s,}\]]/.test(source[at]!)) at++;
    const token = source.slice(begin, at);
    if (token[0] === '-' || /[0-9]/.test(token[0] ?? '')) {
      if (!Number.isFinite(Number(token))) throw new SyntaxError('JSON number is outside the finite range');
      return { number: token };
    }
    return {};
  };
  return value();
}

/** Keep every numeric metadata leaf, not a second roster of classifier fields. */
function numericLeaves(node: Tokens | undefined, path = ''): readonly (readonly [string, string])[] {
  if (node?.number !== undefined) return [[path, node.number]];
  if (node?.fields !== undefined) return Array.from(node.fields).flatMap(([key, value]) =>
    numericLeaves(value, `${path}/${key.replaceAll('~', '~0').replaceAll('/', '~1')}`));
  return node?.items?.flatMap((value, index) => numericLeaves(value, `${path}/${index}`)) ?? [];
}

/** Only the yard's full-job envelope uses this boundary. Other HTTP readers
 *  retain their existing contract. Missing side facts are never guessed. */
export function parseProofJobsJSON(source: string): unknown {
  const parsed: unknown = JSON.parse(source);
  const document = numberTokens(source);
  const envelope = object(parsed);
  if (envelope === null || !Array.isArray(envelope.data)) return parsed;
  const tokens = document.fields?.get('data')?.items;
  return { ...envelope, data: envelope.data.map((value, index) => {
    const job = object(value);
    if (job === null) return value;
    const proofNumbers: ProofNumbers = Object.fromEntries(numericLeaves(tokens?.[index]?.fields?.get('metadata')));
    return { ...job, proofNumbers };
  }) };
}

export async function readProofJobsEnvelope(url: string): Promise<unknown> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  return parseProofJobsJSON(await response.text());
}
