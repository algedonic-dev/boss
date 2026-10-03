import { describe, expect, test } from 'bun:test';
import { parseViews, parseViewResults } from './schemas';
import type { View, ViewResults } from './types';

const view: View = {
  id: 'view-one', owner_id: 'emp-one', title: 'Question', source: 'jobs', filter: '',
  columns: [], layout: 'table', visibility: 'private',
  created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-01T00:00:00+00:00',
};
const result: ViewResults = {
  view_id: view.id, source: view.source, layout: view.layout, rows: [], matched: 0,
  pushed_down: 0, truncated: false, scope: 'all',
};

describe('the saved View list contract', () => {
  test('real empty and every supported enum remain valid', () => {
    expect(parseViews([])).toEqual([]);
    for (const source of ['subjects', 'jobs', 'steps', 'events'] as const) {
      for (const layout of ['table', 'list', 'count'] as const) {
        for (const visibility of ['private', 'shared'] as const) {
          const input = { ...view, source, layout, visibility };
          expect(parseViews([input])).toEqual([input]);
        }
      }
    }
  });
  test('the backend defaults omitted filter and columns, never explicit null', () => {
    const { filter, columns, ...wire } = view;
    expect(parseViews([wire])).toEqual([view]);
    expect(() => parseViews([{ ...view, filter: null }])).toThrow('body');
    expect(() => parseViews([{ ...view, columns: null }])).toThrow('body');
  });
  for (const body of [{}, null, { data: [] }, [null], [view, {}]]) {
    test(`refuses the entire malformed list ${JSON.stringify(body)}`, () => {
      expect(() => parseViews(body)).toThrow('body');
    });
  }
  for (const field of ['id', 'owner_id', 'title', 'source', 'layout', 'visibility', 'created_at', 'updated_at']) {
    test(`requires ${field} with its native type`, () => {
      const missing: Record<string, unknown> = { ...view };
      delete missing[field];
      expect(() => parseViews([missing])).toThrow('body');
      expect(() => parseViews([{ ...view, [field]: 4 }])).toThrow('body');
    });
  }
  for (const patch of [
    { source: 'assets' }, { layout: 'graph' }, { visibility: 'public' },
    { columns: ['id', null] }, { filter: false }, { created_at: 'yesterday' },
  ]) {
    test(`refuses invalid field ${JSON.stringify(patch)}`, () => {
      expect(() => parseViews([{ ...view, ...patch }])).toThrow('body');
    });
  }
});

describe('one View result contract', () => {
  test('preserves empty, capped rows, projected arbitrary fields, scoped floors and nested JSON', () => {
    expect(parseViewResults(result, view)).toEqual(result);
    const rows = [{ custom: null, nested: { list: [1, 'a', null] } }, {}];
    const body = { ...result, rows, matched: 5000, pushed_down: 2, truncated: true, scope: 'owners' as const };
    expect(parseViewResults(body, view)).toEqual(body);
    // Count layouts can still carry rows: the native resolver caps rows by request limit.
    const count = { ...view, layout: 'count' as const };
    expect(parseViewResults({ ...body, layout: 'count' }, count).rows).toEqual(rows);
  });
  for (const body of [{}, null, [], { ...result, rows: [null] }, { ...result, rows: [[]] }, { ...result, rows: ['row'] }]) {
    test(`refuses malformed result ${JSON.stringify(body)}`, () => {
      expect(() => parseViewResults(body, view)).toThrow('body');
    });
  }
  for (const field of ['view_id', 'source', 'layout', 'rows', 'matched', 'pushed_down', 'truncated', 'scope']) {
    test(`requires result ${field}`, () => {
      const missing: Record<string, unknown> = { ...result };
      delete missing[field];
      expect(() => parseViewResults(missing, view)).toThrow('body');
    });
  }
  for (const field of ['matched', 'pushed_down']) {
    for (const value of [-1, 0.5, null, '0', Number.MAX_SAFE_INTEGER + 1, Infinity, NaN]) {
      test(`refuses unsafe ${field} ${String(value)}`, () => {
        expect(() => parseViewResults({ ...result, [field]: value }, view)).toThrow('body');
      });
    }
  }
  for (const patch of [
    { view_id: 'other' }, { source: 'events' }, { layout: 'list' },
    { scope: 'private' }, { truncated: 'false' }, { rows: [{}], matched: 0 },
  ]) {
    test(`refuses misattributed or contradictory result ${JSON.stringify(patch)}`, () => {
      expect(() => parseViewResults({ ...result, ...patch }, view)).toThrow('body');
    });
  }
});
