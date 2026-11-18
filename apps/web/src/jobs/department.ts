import type { Department } from '@boss/web-kit/nav';

type Workflow = Readonly<{ kind: string; metadata?: unknown }>;
export type RowDepartment =
  | Readonly<{ kind: 'unknown' }>
  | Readonly<{ kind: 'none' }>
  | Readonly<{ kind: 'declared'; code: string; label: string; registered: boolean }>;

// The server's department::carried rule: a nonempty string names a
// department; other JSON values name none. A packet's word wins over
// its active workflow's (DepartmentFilter::keeps, 481d7939).
function carried(metadata: unknown): string | null {
  if (typeof metadata !== 'object' || metadata === null || Array.isArray(metadata)) return null;
  const word = 'department' in metadata ? metadata.department : undefined;
  return typeof word === 'string' && word.length > 0 ? word : null;
}

export function rowDepartment(
  packet: Readonly<{ kind: string; metadata?: unknown }>,
  workflows: readonly Workflow[] | null,
  departments: readonly Department[] | null,
): RowDepartment {
  if (departments === null || packet.metadata === undefined) return { kind: 'unknown' };
  const own = carried(packet.metadata);
  if (own === null && workflows === null) return { kind: 'unknown' };
  const code = own ?? carried(workflows?.find(row => row.kind === packet.kind)?.metadata);
  if (code === null) return { kind: 'none' };
  const department = departments.find(row => row.code === code);
  return { kind: 'declared', code, label: department?.label ?? code, registered: department !== undefined };
}

/** The department registry (GET /api/departments), read whole. A partial
 *  roster is refused rather than drawn with its missing rows absent. It
 *  was the company map's parser until that page retired (backlog
 *  759da6bc); this list's department filter is its one reader. */
export function parseDepartments(raw: unknown): ReadonlyArray<Department> {
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
