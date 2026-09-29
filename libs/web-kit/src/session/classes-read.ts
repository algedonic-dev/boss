// The Class registry read, reduced to what a page may say about it —
// pure, deliberately free of runes, so `bun test` can load it (the
// classify.ts precedent: a module-level $state makes a module
// unloadable outside the Svelte compiler).
//
// WHY (backlog e520c794, page audit 7cdb095b, 2026-09-28). The loader
// in classes.svelte.ts recorded `{ kind: 'error' }` per subject_kind
// and exported only `classesFor`, which answers [] "while loading, on
// error". So a refused read emptied the marketing-asset Kind rail to
// "All" alone and every kind printed as its raw code, and no line on
// either page said the read had failed — the false-empty class the
// page's own list read was fixed for (3fba9c35). The error now keeps
// its reason, and `classesLoadOf` hands a page the load state beside
// the rows, so the page can say it.

export type ClassRow = Readonly<{
  subject_kind: string;
  code: string;
  display_name: string;
  parent_code: string | null;
  member_attribute: string;
  metadata: Readonly<Record<string, unknown>>;
  sort_order: number;
  retired_at: string | null;
}>;

/// What the loader holds per subject_kind.
export type ClassesState =
  | { kind: 'loading' }
  | { kind: 'ready'; rows: ReadonlyArray<ClassRow> }
  | { kind: 'error'; error: string };

/// What a page renders beside `classesFor`: whether the read answered,
/// and if it failed, why — in the words of the page read lines
/// (`<url>: HTTP n`, data/readState.ts in apps/web). No rows: those
/// have one reader, `classesFor`.
export type ClassesLoad =
  | { kind: 'loading' }
  | { kind: 'ready' }
  | { kind: 'error'; error: string };

export function classesUrl(subject_kind: string): string {
  return `/api/classes?subject_kind=${encodeURIComponent(subject_kind)}`;
}

/// One answer, judged. An answer that is not a list is an error, not a
/// registry: since the chrome bar reads departments from the registry on
/// every page (ce68f137), a `{data: [], total: 0}` envelope from a wrong
/// endpoint used to throw inside a derived and take the shell down.
export function classesStateOfAnswer(
  url: string,
  resp: Readonly<{ ok: boolean; status: number }>,
  body: unknown,
): ClassesState {
  if (!resp.ok) return { kind: 'error', error: `${url}: HTTP ${resp.status}` };
  if (!Array.isArray(body)) return { kind: 'error', error: `${url}: the answer was not a list` };
  return { kind: 'ready', rows: body as ClassRow[] };
}

/// A subject_kind nobody has asked for yet reads as loading: nothing
/// has answered, so nothing may be claimed about it.
export function classesLoadOf(st: ClassesState | undefined): ClassesLoad {
  if (!st || st.kind === 'loading') return { kind: 'loading' };
  if (st.kind === 'error') return { kind: 'error', error: st.error };
  return { kind: 'ready' };
}
