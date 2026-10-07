type Row = Readonly<{ kind: string; metadata?: Record<string, unknown> | null }>;
type Group = Readonly<{ value: string; label: string; kinds: readonly string[] }>;

/** No kind names or prefixes carry classification: the registry declaration does. */
export function listGroups(rows: readonly Row[]): readonly Group[] {
  return rows.reduce<readonly Group[]>((groups, row) => {
    const declaration = row.metadata?.list_group;
    if (declaration === undefined || declaration === null) return groups;
    if (typeof declaration !== 'object' || Array.isArray(declaration)) throw new Error('Invalid workflow list group');
    const record = declaration as Record<string, unknown>;
    const { code, label } = record;
    if (typeof code !== 'string' || !code.trim() || code.trim() !== code ||
        typeof label !== 'string' || !label.trim() || label.trim() !== label) throw new Error('Invalid workflow list group');
    const value = `group:${code}`;
    const previous = groups.find(group => group.value === value);
    if (previous && previous.label !== label) throw new Error('Conflicting workflow list group labels');
    return previous
      ? groups.map(group => group.value === value
        ? { ...group, kinds: [...new Set([...group.kinds, row.kind])].sort() } : group)
      : [...groups, { value, label, kinds: [row.kind] }];
  }, []).slice().sort((a, b) => a.value.localeCompare(b.value));
}

export function groupKindQuery(rows: readonly Row[], selection: string): Readonly<{
  kinds?: readonly string[]; exclude_kinds?: readonly string[];
}> {
  if (!selection) return {};
  const groups = listGroups(rows);
  if (selection === 'other') return { exclude_kinds: [...new Set(groups.flatMap(group => group.kinds))].sort() };
  if (selection.startsWith('group:')) return { kinds: groups.find(group => group.value === selection)?.kinds ?? [] };
  throw new Error('Invalid workflow list group selection');
}
