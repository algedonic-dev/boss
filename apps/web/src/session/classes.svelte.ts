// Class registry client — read-only loader for tenant-extensible
// taxonomies (departments, roles, account tiers, marketing-asset
// kinds, …), keyed by subject_kind.
//
// The Class registry (boss-classes-api) is the canonical source for
// per-(subject_kind, member_attribute) enumerations. Hardcoding any
// of these in SPA code defeats the whole "data over code" point of
// the registry — a brewery operator should be able to add a new
// department (or carrier, or note kind) by inserting one row, not by
// patching a Svelte file.
//
// Each subject_kind is fetched once, on demand, and cached. The boot
// path loads `employee` (departments/roles); other surfaces call
// `loadClasses('<subject_kind>')` from their mount and read via
// `classesFor('<subject_kind>', '<member_attribute>')`.

type ClassRow = Readonly<{
  subject_kind: string;
  code: string;
  display_name: string;
  parent_code: string | null;
  member_attribute: string;
  metadata: Readonly<Record<string, unknown>>;
  sort_order: number;
  retired_at: string | null;
}>;

type ClassesState =
  | { kind: 'loading' }
  | { kind: 'ready'; rows: ReadonlyArray<ClassRow> }
  | { kind: 'error' };

// Cache of loaded class sets, keyed by subject_kind. Reassigned (not
// mutated in place) on each transition so the $derived reads in callers
// re-run.
const classes = $state<{ value: Readonly<Record<string, ClassesState>> }>({
  value: {},
});

/// Load (once) the Class rows for a subject_kind. Idempotent — a kind
/// already loaded is a no-op, so multiple surfaces can call it freely.
export async function loadClasses(subject_kind: string): Promise<void> {
  if (classes.value[subject_kind]?.kind === 'ready') return;
  classes.value = { ...classes.value, [subject_kind]: { kind: 'loading' } };
  try {
    const r = await fetch(
      `/api/classes?subject_kind=${encodeURIComponent(subject_kind)}`,
    );
    const next: ClassesState = r.ok
      ? { kind: 'ready', rows: (await r.json()) as ClassRow[] }
      : { kind: 'error' };
    classes.value = { ...classes.value, [subject_kind]: next };
  } catch {
    classes.value = { ...classes.value, [subject_kind]: { kind: 'error' } };
  }
}

/// Active (non-retired) Class rows for a (subject_kind, member_attribute),
/// sorted by sort_order. Returns an empty array while loading, on error,
/// or before the subject_kind has been loaded — callers fall back to a
/// sensible default if the list is empty.
export function classesFor(
  subject_kind: string,
  member_attribute: string,
): ReadonlyArray<ClassRow> {
  const st = classes.value[subject_kind];
  if (!st || st.kind !== 'ready') return [];
  return st.rows
    .filter(
      (r) => r.member_attribute === member_attribute && r.retired_at === null,
    )
    .slice()
    .sort((a, b) => a.sort_order - b.sort_order);
}
