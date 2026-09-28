// What /ux/views says about its sources, read from the server
// (backlog 4a8939b5) — and what it says about whose rows a result is
// (backlog 5392cf23). The fixture below is the shape
// `GET /api/views/sources` serves; the drift this replaced was a page
// keeping its own copy of it.

import { describe, expect, test } from 'bun:test';
import { ceilingLabel, fieldsOf, parseSources, pushableHint, scopeLine } from './sources';
import type { ViewResults } from './types';

const SERVED = {
  scan_ceiling: 5000,
  sources: [
    {
      source: 'jobs',
      fields: ['id', 'kind', 'status', 'created_at', 'partition', 'metadata'],
      pushable: [
        { field: 'kind', type: 'text' },
        { field: 'status', type: 'text' },
        { field: 'created_at', type: 'timestamp' },
        { field: 'partition', type: 'text' },
        { field: 'metadata', type: 'json' },
      ],
    },
    {
      source: 'events',
      fields: ['id', 'event_id', 'kind', 'source', 'timestamp', 'subject_kind', 'subject_id', 'payload'],
      pushable: [
        { field: 'kind', type: 'text' },
        { field: 'source', type: 'text' },
        { field: 'subject_kind', type: 'text' },
        { field: 'subject_id', type: 'text' },
        { field: 'timestamp', type: 'timestamp' },
        { field: 'payload', type: 'json' },
      ],
    },
  ],
};

describe('parseSources', () => {
  test('takes the served document as it is', () => {
    const s = parseSources(SERVED);
    expect(s.scan_ceiling).toBe(5000);
    expect(s.sources.map((x) => x.source)).toEqual(['jobs', 'events']);
  });

  test('refuses a shape it cannot read, rather than offering no fields', () => {
    // A list where the object is due is the wrong-endpoint answer; an
    // empty picker from it would read as a source with nothing in it.
    expect(() => parseSources([])).toThrow(/sources/);
    expect(() => parseSources({ scan_ceiling: '5000', sources: [] })).toThrow(/scan_ceiling/);
    expect(() => parseSources({ scan_ceiling: 5000, sources: [{ source: 'jobs', fields: 'id' }] })).toThrow(
      /jobs/,
    );
  });
});

describe('what the page says from it', () => {
  const s = parseSources(SERVED);

  test('the column picker offers what the row carries, including what the copy dropped', () => {
    expect(fieldsOf(s, 'events')).toContain('subject_kind');
    expect(fieldsOf(s, 'events')).toContain('subject_id');
    expect(fieldsOf(s, 'jobs')).toContain('metadata');
    // A source the server did not describe offers nothing, not a guess.
    expect(fieldsOf(s, 'subjects')).toEqual([]);
  });

  test('the ceiling is the served one, grouped for reading', () => {
    expect(ceilingLabel(s)).toBe('5,000');
    expect(ceilingLabel({ ...s, scan_ceiling: 20000 })).toBe('20,000');
  });

  test('the pushdown hint names every pushable field, a json one as a path', () => {
    expect(pushableHint(s, 'events')).toBe(
      'kind, source, subject_kind, subject_id, payload.<path>, or a timestamp range',
    );
    expect(pushableHint(s, 'jobs')).toBe(
      'kind, status, partition, metadata.<path>, or a created_at range',
    );
    expect(pushableHint(s, 'subjects')).toBe('a different field');
  });
});

describe('scopeLine', () => {
  const res = (scope: 'all' | 'owners'): ViewResults => ({
    view_id: 'v', source: 'jobs', layout: 'table', rows: [], matched: 0,
    pushed_down: 0, truncated: false, scope,
  });

  test('an owner-scoped count says it is only the rows the caller may read', () => {
    expect(scopeLine(res('owners'))).toBe('only rows you may read');
  });

  test('a count over every row says nothing extra', () => {
    expect(scopeLine(res('all'))).toBeNull();
  });
});
