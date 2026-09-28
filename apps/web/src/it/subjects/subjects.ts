// Types + read client + pure shaping for the Subjects & Classes surface
// (/it/registry/subjects) — the model's vocabulary, read-only: the
// SubjectKind taxonomy (boss-subject-kinds, GET /api/subject-kinds) + the
// Class registry (boss-classes, GET /api/classes?subject_kind=…) + the
// Workflow registry's subject_kinds (GET /api/workflows), which says how
// many live protocols name each kind (backlog 92ea2e00). Deserialized at
// the call site per the repo's no-shared-types convention.

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

/** The three fields of a Workflow row this surface reads. */
export type WorkflowRef = Readonly<{
  kind: string;
  status: string;
  subject_kinds: ReadonlyArray<string>;
}>;

export async function listWorkflows(): Promise<ReadonlyArray<WorkflowRef>> {
  const r = await ok(await fetch('/api/workflows'));
  return (await r.json()) as WorkflowRef[];
}

const nonEmptyString = (v: unknown): string | null =>
  typeof v === 'string' && v.length > 0 ? v : null;

/** The tenant-manifest module a kind's surfaces live behind, from the
 *  row's `metadata.module` (backlog 92ea2e00). A platform kind — one
 *  every instance speaks — carries none, and so answers null. The rows
 *  gain the key through the subject-kinds registry's own evented write,
 *  a separate core car — never a migration's silent UPDATE — so until
 *  it lands every kind reads as a platform kind here. */
export function kindModule(kind: SubjectKind): string | null {
  return nonEmptyString(kind.metadata['module']);
}

/** How many ACTIVE workflows name each subject kind in `subject_kinds`.
 *  A kind no active workflow names is absent from the map, and the page
 *  reads that as zero only when the read itself succeeded. */
export function workflowCountsByKind(
  workflows: ReadonlyArray<WorkflowRef>,
): ReadonlyMap<string, number> {
  return workflows
    .filter((w) => w.status === 'active')
    .flatMap((w) => [...new Set(w.subject_kinds)])
    .reduce((m, k) => m.set(k, (m.get(k) ?? 0) + 1), new Map<string, number>());
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
 *  alphabetically. A null member_attribute is titled by the row's
 *  declared `metadata.membership` when it names one — the six `node`
 *  role Classes are that shape by design, their members held in the
 *  node_roles junction table (202609120300-a-node-declares-its-roles.sql),
 *  and filing them under "(unclassified)" read as missing data (backlog
 *  2c7a2d5c). Only a row with neither falls under "(unclassified)". */
export function groupClassesByAttribute(
  classes: ReadonlyArray<ClassRow>,
): ReadonlyArray<readonly [string, ReadonlyArray<ClassRow>]> {
  const groups = new Map<string, ClassRow[]>();
  for (const c of classes) {
    if (c.retired_at !== null) continue;
    const key =
      c.member_attribute ?? nonEmptyString(c.metadata['membership']) ?? '(unclassified)';
    (groups.get(key) ?? groups.set(key, []).get(key)!).push(c);
  }
  const sorter = bySort<ClassRow>((c) => c.code);
  for (const [, arr] of groups) arr.sort(sorter);
  return [...groups.entries()].sort((a, b) => a[0].localeCompare(b[0]));
}
