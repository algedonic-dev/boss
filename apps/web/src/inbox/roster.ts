import type { Remote } from '../data/remote';

export const NAMES_URL = '/api/people/names';
export type EmployeeName = Readonly<{ id: string; name: string | null; role: string | null }>;

/// Parse the identity projection once. Duplicate ids make recipient
/// identity ambiguous; a malformed read never becomes an empty roster.
export function parseNames(raw: unknown): ReadonlyArray<EmployeeName> {
  if (!Array.isArray(raw)) throw new Error('Recipient roster is not a list');
  const ids = new Set<string>();
  return raw.map((value: unknown) => {
    if (typeof value !== 'object' || value === null) throw new Error('Recipient row is not an object');
    const row = value as Record<string, unknown>;
    if (typeof row.id !== 'string' || !row.id.trim() || row.id !== row.id.trim() || ids.has(row.id)
      || !(row.name === null || typeof row.name === 'string')
      || !(row.role === null || typeof row.role === 'string')) {
      throw new Error('Recipient roster has an invalid or duplicate identity');
    }
    ids.add(row.id);
    return { id: row.id, name: row.name, role: row.role };
  });
}

export function recipientReason(read: Remote<ReadonlyArray<EmployeeName>>, selected: string): string | null {
  if (read.kind !== 'ready') return 'Send unavailable until recipients are read';
  if (!read.data.length) return 'No recipients are available in this read';
  if (!selected) return 'Select a recipient';
  return read.data.some((e) => e.id === selected) ? null : 'Selected recipient is not available in this read';
}
