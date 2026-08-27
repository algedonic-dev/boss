// Reactive shell over the pure comparators in sort.ts — the
// sortKey/sortDir/setSort/arrowFor state Watchlist-style tables
// hand-roll, packaged once. Runes only compile in .svelte.ts, which
// is why this thin layer is a separate file from the tested logic.
//
// Usage:
//   const sort = createSortState<'name' | 'score'>(
//     { key: 'score', dir: 'desc' },
//     (k) => (k === 'name' ? 'asc' : 'desc'),
//   );
//   let sorted = $derived(sort.sorted(rows, { name: r => r.name, ... }));
//   <SortHeader {sort} key="name">Account</SortHeader>

import {
  nextSort,
  sortArrow,
  sortRows,
  type SortDir,
  type SortSpec,
  type SortValue,
} from './sort';

export type SortState<K extends string> = {
  readonly key: K;
  readonly dir: SortDir;
  /// Header-click transition: flip direction on the active column,
  /// switch columns (at that column's default direction) otherwise.
  toggle(key: K): void;
  /// `' ↑'` / `' ↓'` on the active column, `''` elsewhere.
  arrow(key: K): string;
  /// Sort a copy of `rows` by the current spec.
  sorted<T>(
    rows: ReadonlyArray<T>,
    accessors: Readonly<Record<K, (row: T) => SortValue>>,
  ): T[];
};

export function createSortState<K extends string>(
  initial: SortSpec<K>,
  defaultDirFor?: (key: K) => SortDir,
): SortState<K> {
  let spec = $state<SortSpec<K>>(initial);
  return {
    get key(): K {
      return spec.key;
    },
    get dir(): SortDir {
      return spec.dir;
    },
    toggle(key: K): void {
      spec = nextSort(spec, key, defaultDirFor);
    },
    arrow(key: K): string {
      return sortArrow(spec, key);
    },
    sorted<T>(
      rows: ReadonlyArray<T>,
      accessors: Readonly<Record<K, (row: T) => SortValue>>,
    ): T[] {
      return sortRows(rows, spec, accessors);
    },
  };
}
