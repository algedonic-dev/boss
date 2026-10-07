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
