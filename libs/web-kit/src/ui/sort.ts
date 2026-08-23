// Pure comparator logic for sortable tables — the sort pattern
// WatchlistPage grew by hand (sortKey + sortDir + setSort + arrowFor
// + a switch of comparators), extracted so every table shares one
// implementation. This module is deliberately runes-free so the
// comparators run under plain `bun test`; the reactive shell lives
// in sort-state.svelte.ts, and the header cell in SortHeader.svelte.

export type SortDir = 'asc' | 'desc';

export type SortSpec<K extends string> = Readonly<{
  key: K;
  dir: SortDir;
}>;

/// A value a column can sort by. `null`/`undefined` mean "no data"
/// and sort before every real value (ascending) — the Watchlist
/// convention, where an account with no invoice yet leads the
/// days-since-invoice column.
export type SortValue = string | number | boolean | null | undefined;

export function compareSortValues(a: SortValue, b: SortValue): number {
  const aNull = a === null || a === undefined;
  const bNull = b === null || b === undefined;
  if (aNull && bNull) return 0;
  if (aNull) return -1;
  if (bNull) return 1;
  if (typeof a === 'number' && typeof b === 'number') return a - b;
  if (typeof a === 'boolean' && typeof b === 'boolean') {
    return Number(a) - Number(b);
  }
  return String(a).localeCompare(String(b));
}

/// The click-a-header transition: same key flips direction; a new
/// key takes over with its default direction (`'asc'` unless
/// `defaultDirFor` says otherwise — numeric columns usually want
/// `'desc'`, name columns `'asc'`).
export function nextSort<K extends string>(
  current: SortSpec<K>,
  key: K,
  defaultDirFor?: (key: K) => SortDir,
): SortSpec<K> {
  if (key === current.key) {
    return { key, dir: current.dir === 'asc' ? 'desc' : 'asc' };
  }
  return { key, dir: defaultDirFor ? defaultDirFor(key) : 'asc' };
}

/// Sort a copy of `rows` by the spec'd column. `accessors` maps each
/// sortable key to the value it sorts by — the one table-specific
/// part of the pattern.
export function sortRows<T, K extends string>(
  rows: ReadonlyArray<T>,
  spec: SortSpec<K>,
  accessors: Readonly<Record<K, (row: T) => SortValue>>,
): T[] {
  const accessor = accessors[spec.key];
  const mult = spec.dir === 'asc' ? 1 : -1;
  return [...rows].sort(
    (a, b) => mult * compareSortValues(accessor(a), accessor(b)),
  );
}

/// Header suffix for the active sort column: `' ↑'` / `' ↓'`, or
/// `''` on inactive columns.
export function sortArrow<K extends string>(
  spec: SortSpec<K>,
  key: K,
): string {
  if (spec.key !== key) return '';
  return spec.dir === 'asc' ? ' ↑' : ' ↓';
}
