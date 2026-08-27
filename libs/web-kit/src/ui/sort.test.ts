// Unit tests for the pure table-sort comparators. Run via `bun test`.
//
// The reactive wrapper (`sort-state.svelte.ts`) is a thin runes shell
// over these functions; the comparator logic under test here is the
// part that carries the behavior.

import { describe, expect, test } from 'bun:test';
import {
  compareSortValues,
  nextSort,
  sortArrow,
  sortRows,
  type SortSpec,
} from './sort';

describe('compareSortValues', () => {
  test('numbers compare numerically', () => {
    expect(compareSortValues(1, 2)).toBeLessThan(0);
    expect(compareSortValues(10, 2)).toBeGreaterThan(0);
    expect(compareSortValues(3, 3)).toBe(0);
  });

  test('strings compare with localeCompare', () => {
    expect(compareSortValues('alpha', 'beta')).toBeLessThan(0);
    expect(compareSortValues('gamma', 'beta')).toBeGreaterThan(0);
    expect(compareSortValues('same', 'same')).toBe(0);
  });

  test('booleans compare false < true', () => {
    expect(compareSortValues(false, true)).toBeLessThan(0);
    expect(compareSortValues(true, false)).toBeGreaterThan(0);
    expect(compareSortValues(true, true)).toBe(0);
  });

  test('null/undefined sort before any value (WatchlistPage semantics)', () => {
    expect(compareSortValues(null, 5)).toBeLessThan(0);
    expect(compareSortValues(5, null)).toBeGreaterThan(0);
    expect(compareSortValues(null, null)).toBe(0);
    expect(compareSortValues(undefined, 'x')).toBeLessThan(0);
    expect(compareSortValues(null, undefined)).toBe(0);
  });

  test('mixed number/string falls back to string comparison', () => {
    // Degenerate case (a column should yield one type): the number
    // is stringified, so '2' sorts after '10' lexicographically.
    expect(compareSortValues(2, '10')).toBeGreaterThan(0);
  });
});

describe('nextSort', () => {
  const current: SortSpec<'name' | 'score'> = { key: 'score', dir: 'desc' };

  test('same key flips direction', () => {
    expect(nextSort(current, 'score')).toEqual({ key: 'score', dir: 'asc' });
    expect(nextSort({ key: 'score', dir: 'asc' }, 'score')).toEqual({
      key: 'score',
      dir: 'desc',
    });
  });

  test('new key starts ascending by default', () => {
    expect(nextSort(current, 'name')).toEqual({ key: 'name', dir: 'asc' });
  });

  test('new key honors a per-key default direction', () => {
    const numericDesc = (k: 'name' | 'score') =>
      k === 'score' ? ('desc' as const) : ('asc' as const);
    expect(nextSort({ key: 'name', dir: 'asc' }, 'score', numericDesc)).toEqual(
      { key: 'score', dir: 'desc' },
    );
  });
});

describe('sortRows', () => {
  type Row = { name: string; score: number; days: number | null };
  const rows: ReadonlyArray<Row> = [
    { name: 'Beta', score: 10, days: 3 },
    { name: 'Alpha', score: 30, days: null },
    { name: 'Gamma', score: 20, days: 1 },
  ];
  const accessors = {
    name: (r: Row) => r.name,
    score: (r: Row) => r.score,
    days: (r: Row) => r.days,
  };

  test('sorts ascending by the spec key', () => {
    const out = sortRows(rows, { key: 'score', dir: 'asc' }, accessors);
    expect(out.map((r) => r.score)).toEqual([10, 20, 30]);
  });

  test('desc reverses the comparator', () => {
    const out = sortRows(rows, { key: 'name', dir: 'desc' }, accessors);
    expect(out.map((r) => r.name)).toEqual(['Gamma', 'Beta', 'Alpha']);
  });

  test('nulls sort first ascending, last descending', () => {
    const asc = sortRows(rows, { key: 'days', dir: 'asc' }, accessors);
    expect(asc.map((r) => r.days)).toEqual([null, 1, 3]);
    const desc = sortRows(rows, { key: 'days', dir: 'desc' }, accessors);
    expect(desc.map((r) => r.days)).toEqual([3, 1, null]);
  });

  test('returns a new array and leaves the input untouched', () => {
    const before = rows.map((r) => r.name);
    const out = sortRows(rows, { key: 'name', dir: 'asc' }, accessors);
    expect(out).not.toBe(rows);
    expect(rows.map((r) => r.name)).toEqual(before);
  });
});

describe('sortArrow', () => {
  test('marks only the active key, by direction', () => {
    expect(sortArrow({ key: 'score', dir: 'desc' }, 'score')).toBe(' ↓');
    expect(sortArrow({ key: 'score', dir: 'asc' }, 'score')).toBe(' ↑');
    expect(sortArrow({ key: 'score', dir: 'desc' }, 'name')).toBe('');
  });
});
