export type RosterLocationRead = Readonly<
  { kind: 'known'; name: string } | { kind: 'absent' } | { kind: 'failed' }
>;

// The list door returns roots, not every location (b6b50998). Resolve the
// employee's recorded identity through its own door, including child places.
export async function readRosterLocation(
  id: string,
  reader: (path: string) => Promise<Response> = fetch,
): Promise<RosterLocationRead> {
  try {
    const response = await reader(`/api/locations/${encodeURIComponent(id)}`);
    if (response.status === 404) return { kind: 'absent' };
    if (!response.ok) return { kind: 'failed' };
    const row: unknown = await response.json();
    if (typeof row !== 'object' || row === null || Array.isArray(row) ||
        !('id' in row) || row.id !== id || !('name' in row) ||
        typeof row.name !== 'string' || !row.name.trim()) return { kind: 'failed' };
    return { kind: 'known', name: row.name };
  } catch {
    return { kind: 'failed' };
  }
}
