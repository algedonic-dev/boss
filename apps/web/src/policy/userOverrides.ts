import type { Scope } from './policyTypes';

export type UserOverride = Readonly<{
  id: string; user_id: string; resource: string; action: string;
  scope: Scope; reason: string; expires_at: string | null;
}>;
export type OverrideFetch = (url: string, init?: RequestInit) => Promise<Response>;
export type WireResponse = Readonly<{ status: number; body: string | null }>;
export type Readback = Readonly<{ status: number | null; response: WireResponse | null; error: string | null }>;
export type OverrideReceipt = Readonly<{
  expected: UserOverride; before: WireResponse | null; before_error: Readback | null;
  write: WireResponse | null; write_error: string | null;
  after: WireResponse | null; current_after: WireResponse | null; readbacks: ReadonlyArray<Readback>;
}>;
export type OverrideResult =
  | Readonly<{ kind: 'verified'; override: UserOverride; receipt: OverrideReceipt }>
  | Readonly<{ kind: 'refused' | 'uncertain'; error: string; receipt: OverrideReceipt }>;
const fields = ['id', 'user_id', 'resource', 'action', 'scope', 'reason', 'expires_at'];
const object = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value);

export function parseOverride(value: unknown): UserOverride {
  if (object(value) && Object.keys(value).length === 1 && 'override' in value) value = value.override;
  if (!object(value)) throw new Error('Provide a permission object.');
  const row = value;
  if (Object.keys(row).length !== fields.length || fields.some(k => !(k in row))) throw new Error('Provide exactly id, user_id, resource, action, scope, reason and expires_at.');
  for (const key of ['id', 'user_id', 'resource', 'action', 'reason']) {
    if (typeof value[key] !== 'string' || !(value[key] as string).trim()) throw new Error(`${key} is required.`);
  }
  const scope = value.scope;
  if (!(typeof scope === 'string' && ['none', 'self', 'territory', 'team', 'all'].includes(scope)) &&
      !(object(scope) && Object.keys(scope).length === 1 && typeof scope.department === 'string' && scope.department.trim())) throw new Error('Choose a valid scope or a named department.');
  const expiry = value.expires_at;
  if (expiry !== null && (typeof expiry !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/.test(expiry) || !Number.isFinite(Date.parse(expiry)))) throw new Error('Expiry must include a date, time and timezone, or be empty.');
  return { id: value.id as string, user_id: value.user_id as string, resource: value.resource as string, action: value.action as string, scope: scope as Scope, reason: value.reason as string, expires_at: expiry as string | null };
}

export async function listOverrides(user: string, wire: OverrideFetch = fetch): Promise<ReadonlyArray<UserOverride>> {
  const response = await wire(`/api/policy/user-overrides/${encodeURIComponent(user)}`, { credentials: 'same-origin' });
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${await response.text()}`);
  const body: unknown = await response.json();
  if (!Array.isArray(body)) throw new Error('The override list did not return a complete list.');
  return body.map(parseOverride);
}

function sameOverride(actual: UserOverride, expected: UserOverride): boolean {
  const expiry = (value: string | null): string | null => {
    if (value === null) return null;
    const fraction = /\.(\d+)(?:Z|[+-])/.exec(value)?.[1] ?? '';
    return `${Date.parse(value.replace(/\.\d+(?=Z|[+-])/, ''))}:${fraction.padEnd(9, '0')}`;
  };
  const scope = (value: Scope): string => typeof value === 'string' ? value : `department:${value.department}`;
  return ['id', 'user_id', 'resource', 'action', 'reason'].every(k => actual[k as keyof UserOverride] === expected[k as keyof UserOverride]) && scope(actual.scope) === scope(expected.scope) && expiry(actual.expires_at) === expiry(expected.expires_at);
}

function rowsFrom(response: WireResponse): ReadonlyArray<UserOverride> {
  if (response.status < 200 || response.status >= 300) throw new Error(`HTTP ${response.status}: ${response.body}`);
  if (response.body === null) throw new Error('The response body could not be read.');
  const body: unknown = JSON.parse(response.body);
  if (!Array.isArray(body)) throw new Error('The override list did not return a complete list.');
  return body.map(parseOverride);
}
async function readEffect(user: string, wire: OverrideFetch): Promise<Readback> {
  let status: number | null = null;
  try {
    const response = await wire(`/api/policy/user-overrides/${encodeURIComponent(user)}`, { credentials: 'same-origin' });
    status = response.status;
    return { status, response: { status, body: await response.text() }, error: null };
  } catch (error) {
    return { status, response: null, error: error instanceof Error ? error.message : String(error) };
  }
}

const emptyReceipt = (expected: UserOverride): OverrideReceipt => ({ expected, before: null, before_error: null, write: null, write_error: null, after: null, current_after: null, readbacks: [] });
function appendReadback(prior: OverrideReceipt, read: Readback): OverrideReceipt {
  return { ...prior, after: prior.after ?? read.response, current_after: read.response, readbacks: [...prior.readbacks, read] };
}

export async function verifyOverride(expected: UserOverride, wire: OverrideFetch = fetch, prior?: OverrideReceipt): Promise<OverrideResult> {
  const receipt = prior ?? emptyReceipt(expected);
  if (receipt.write && receipt.write.status >= 300 && receipt.write.status < 500) {
    return { kind: 'refused', error: `HTTP ${receipt.write.status}: ${receipt.write_error ?? receipt.write.body ?? 'The write was refused.'}`, receipt };
  }
  const read = await readEffect(expected.user_id, wire);
  try {
    if (!read.response) throw new Error(read.error ?? 'The read returned no response.');
    const rows = rowsFrom(read.response);
    const matching = rows.filter(row => row.id === expected.id);
    const actual = matching[0];
    if (matching.length !== 1 || !actual || !sameOverride(actual, expected)) return { kind: 'uncertain', error: 'The saved permission could not be confirmed exactly. Review the current permissions before submitting again.', receipt: appendReadback(receipt, read) };
    return { kind: 'verified', override: actual, receipt: appendReadback(receipt, read) };
  } catch (error) {
    const cause = error instanceof Error ? error.message : String(error);
    return { kind: 'uncertain', error: `The saved permission could not be confirmed: ${cause}`, receipt: appendReadback(receipt, { ...read, error: cause }) };
  }
}

export async function saveOverride(expected: UserOverride, wire: OverrideFetch = fetch): Promise<OverrideResult> {
  let receipt = emptyReceipt(expected);
  const read = await readEffect(expected.user_id, wire);
  receipt = { ...receipt, before: read.response };
  try {
    if (!read.response) throw new Error(read.error ?? 'The read returned no response.');
    rowsFrom(read.response);
  } catch (error) {
    const cause = error instanceof Error ? error.message : String(error);
    return { kind: 'refused', error: `No permission was submitted: the current permissions could not be read: ${cause}`, receipt: { ...receipt, before_error: { ...read, error: cause } } };
  }
  try {
    const response = await wire('/api/policy/user-overrides', { method: 'POST', credentials: 'same-origin', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ override: expected }) });
    receipt = { ...receipt, write: { status: response.status, body: null } };
    const write = { status: response.status, body: await response.text() };
    receipt = { ...receipt, write };
    if (!response.ok) {
      const error = `HTTP ${write.status}: ${write.body}`;
      return { kind: response.status >= 500 ? 'uncertain' : 'refused', error, receipt };
    }
    return verifyOverride(expected, wire, receipt);
  } catch (error) {
    const cause = error instanceof Error ? error.message : String(error);
    const status = receipt.write?.status;
    const refused = status !== undefined && status >= 300 && status < 500;
    return { kind: refused ? 'refused' : 'uncertain', error: refused ? `HTTP ${status}: the response body could not be read: ${cause}` : `The request may have been saved. Check the saved permission before submitting again: ${cause}`, receipt: { ...receipt, write_error: cause } };
  }
}
