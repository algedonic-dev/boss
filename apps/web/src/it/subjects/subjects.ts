// Types + read client + pure shaping for the Subjects & Classes surface
// (/it/subjects) — the model's vocabulary, read-only: the SubjectKind
// taxonomy (boss-subject-kinds, GET /api/subject-kinds) + the Class
// registry (boss-classes, GET /api/classes?subject_kind=…). Deserialized
// at the call site per the repo's no-shared-types convention.

/** One row of the SubjectKind taxonomy (GET /api/subject-kinds). */
export type SubjectKind = Readonly<{
  kind: string;
  label: string;
  parent_kind: string | null;
  description: string | null;
  owning_team: string;
  metadata: Readonly<Record<string, unknown>>;
  sort_order: number;
  retired_at: string | null;
}>;

/** One Class row (GET /api/classes?subject_kind=…). Keyed (subject_kind,
 *  code); `member_attribute` names the Subject column whose value the code
 *  matches (e.g. role / department / type). */
export type ClassRow = Readonly<{
  subject_kind: string;
  code: string;
  display_name: string;
  parent_code: string | null;
  member_attribute: string | null;
  metadata: Readonly<Record<string, unknown>>;
  sort_order: number;
  retired_at: string | null;
}>;

/** A SubjectKind plus its direct child kinds — one node of the taxonomy. */
export type KindTreeNode = Readonly<{
  kind: SubjectKind;
  children: ReadonlyArray<SubjectKind>;
}>;

async function ok(r: Response): Promise<Response> {
  if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
  return r;
}

export async function listSubjectKinds(): Promise<ReadonlyArray<SubjectKind>> {
  const r = await ok(await fetch('/api/subject-kinds'));
  return (await r.json()) as SubjectKind[];
}

export async function listClasses(subjectKind: string): Promise<ReadonlyArray<ClassRow>> {
  const r = await ok(await fetch(`/api/classes?subject_kind=${encodeURIComponent(subjectKind)}`));
  return (await r.json()) as ClassRow[];
}

const bySort = <T extends { sort_order: number }>(key: (t: T) => string) => (a: T, b: T): number =>
  a.sort_order - b.sort_order || key(a).localeCompare(key(b));

/** Shape the flat SubjectKind list into roots (parent_kind === null) each
 *  with their direct children, both active-only and sorted by sort_order
 *  then kind. Defensive: any active kind not placed under a root (orphan
 *  parent, or deeper nesting than the seeded 2 levels) surfaces as its own
 *  top-level node, so nothing is silently hidden. */
export function buildKindTree(kinds: ReadonlyArray<SubjectKind>): ReadonlyArray<KindTreeNode> {
  const sorter = bySort<SubjectKind>((k) => k.kind);
  const active = kinds.filter((k) => k.retired_at === null).slice().sort(sorter);
  const childrenOf = (parent: string): SubjectKind[] =>
    active.filter((k) => k.parent_kind === parent);
  const nodes: KindTreeNode[] = active
    .filter((k) => k.parent_kind === null)
    .map((k) => ({ kind: k, children: childrenOf(k.kind) }));
  const shown = new Set(nodes.flatMap((n) => [n.kind.kind, ...n.children.map((c) => c.kind)]));
  for (const k of active) {
    if (!shown.has(k.kind)) {
      nodes.push({ kind: k, children: childrenOf(k.kind) });
      shown.add(k.kind);
      for (const c of childrenOf(k.kind)) shown.add(c.kind);
    }
  }
  return nodes;
}

/** Group active classes by `member_attribute` (role / department / type /
 *  …), each group sorted by sort_order then code; group keys sorted
 *  alphabetically. A null member_attribute falls under "(unclassified)". */
export function groupClassesByAttribute(
  classes: ReadonlyArray<ClassRow>,
): ReadonlyArray<readonly [string, ReadonlyArray<ClassRow>]> {
  const groups = new Map<string, ClassRow[]>();
  for (const c of classes) {
    if (c.retired_at !== null) continue;
    const key = c.member_attribute ?? '(unclassified)';
    (groups.get(key) ?? groups.set(key, []).get(key)!).push(c);
  }
  const sorter = bySort<ClassRow>((c) => c.code);
  for (const [, arr] of groups) arr.sort(sorter);
  return [...groups.entries()].sort((a, b) => a[0].localeCompare(b[0]));
}
