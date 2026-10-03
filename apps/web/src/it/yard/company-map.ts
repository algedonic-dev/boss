import type { Department } from '@boss/web-kit/nav';

/** The map is the registry, not a list of guessed company departments.
 * Refuse a partial roster rather than draw its missing regions as absent. */
export function parseCompanyDepartments(raw: unknown): ReadonlyArray<Department> {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) throw new Error('Department registry is malformed');
  const body = raw as { data?: unknown; total?: unknown };
  if (!Array.isArray(body.data) || !Number.isSafeInteger(body.total) || body.total !== body.data.length) {
    throw new Error('Department registry is incomplete or malformed');
  }
  const seen = new Set<string>();
  return body.data.map((value: unknown) => {
    if (typeof value !== 'object' || value === null) throw new Error('Department registry row is malformed');
    const row = value as { code?: unknown; display_name?: unknown };
    if (typeof row.code !== 'string' || row.code.length === 0 || typeof row.display_name !== 'string' || row.display_name.length === 0 || seen.has(row.code)) {
      throw new Error('Department registry row is malformed or duplicated');
    }
    seen.add(row.code);
    return { code: row.code, label: row.display_name };
  });
}
