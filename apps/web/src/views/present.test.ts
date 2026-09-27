// What /ux/views says, decided by page audit d2e86594 (2026-09-27) and
// held here as pure functions so each sentence is pinned without a
// browser. The mocked spec tests/mocked/views-page.mocked.spec.ts pins
// the same sentences on the rendered page.

import { afterEach, beforeAll, describe, expect, test } from 'bun:test';
import {
  NO_VIEWER_LINE,
  cellHref,
  hasNoViewer,
  refusedLine,
  scannedRows,
  sendWrite,
  sharerName,
  shownLine,
} from './present';
import type { ViewResults } from './types';
import type { Employee } from '@boss/web-kit/session/classify';

// `href` reads window.location.pathname (the /dashboard mount probe);
// stubbed field by field, as entity-href-routes.test.ts does, because
// bun may run another file's window in the same process.
beforeAll(() => {
  const g = globalThis as unknown as { window?: { location?: Record<string, unknown> } };
  if (!g.window) g.window = { location: {} };
  if (!g.window.location) g.window.location = {};
  g.window.location.pathname ??= '/';
});

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

describe('the list with no signed-in viewer (backlog 04754a40)', () => {
  test('a session that resolved to nobody says so, instead of loading forever', () => {
    for (const kind of ['unauthenticated', 'unrecognized', 'unresolved'] as const) {
      expect(hasNoViewer(kind)).toBe(true);
    }
    expect(NO_VIEWER_LINE).toBe('No signed-in viewer, so no Views to list.');
  });

  test('a session still loading, or ready, is not "no viewer"', () => {
    expect(hasNoViewer('loading')).toBe(false);
    expect(hasNoViewer('ready')).toBe(false);
  });
});

describe('a write refusal is attributed to the write (backlog 0148c461)', () => {
  test('a refusal names its status and the server body', async () => {
    const seen: Array<{ url: string; method: string | undefined }> = [];
    const out = await sendWrite('/api/views/v1', { method: 'DELETE' }, async (url, init) => {
      seen.push({ url: String(url), method: init?.method });
      return new Response('not your view', { status: 403 });
    });
    expect(out).toEqual({ kind: 'refused', reason: 'HTTP 403, not your view' });
    expect(seen).toEqual([{ url: '/api/views/v1', method: 'DELETE' }]);
    expect(refusedLine('delete', 'HTTP 403, not your view')).toBe(
      'delete refused: HTTP 403, not your view',
    );
  });

  test('an empty body still names the status', async () => {
    const out = await sendWrite('/api/views/v1', { method: 'DELETE' }, async () => new Response('', { status: 500 }));
    expect(out).toEqual({ kind: 'refused', reason: 'HTTP 500' });
  });

  test('a write that never reached the server is refused with what was thrown', async () => {
    const out = await sendWrite('/api/views', { method: 'POST' }, async () => {
      throw new Error('network down');
    });
    expect(out).toEqual({ kind: 'refused', reason: 'network down' });
    expect(refusedLine('save', 'network down')).toBe('save refused: network down');
  });

  test('a 2xx is done', async () => {
    const out = await sendWrite('/api/views/v1', { method: 'PUT' }, async () => new Response('{}', { status: 200 }));
    expect(out).toEqual({ kind: 'done' });
  });
});

describe('the weak-truncation line names the source (backlog 3d8178e1)', () => {
  test('each source names its own rows, never "events" for all four', () => {
    expect(scannedRows('jobs')).toBe('newest 5,000 jobs');
    expect(scannedRows('steps')).toBe('newest 5,000 steps');
    expect(scannedRows('subjects')).toBe('newest 5,000 subjects');
    expect(scannedRows('events')).toBe('newest 5,000 events');
  });
});

describe('a result says how many rows it shows and when it was read (backlog 22d5bfcd)', () => {
  const res = (rows: number, matched: number): ViewResults => ({
    view_id: 'v1',
    source: 'jobs',
    layout: 'table',
    rows: Array.from({ length: rows }, (_, i) => ({ id: `j-${i}` })),
    matched,
    pushed_down: 1,
    truncated: false,
  });

  test('shown of matched, and the local read time as HH:MM', () => {
    const at = new Date(2026, 8, 27, 9, 5);
    expect(shownLine(res(100, 4000), at)).toBe('showing 100 of 4000 · read 09:05');
    expect(shownLine(res(0, 0), new Date(2026, 8, 27, 23, 59))).toBe('showing 0 of 0 · read 23:59');
  });
});

describe('result cells link to what they name (backlog 00ab7ddd)', () => {
  test('a jobs row links its id to the packet', () => {
    expect(cellHref('jobs', 'id', { id: 'j 1' })).toBe('/ux/jobs/j%201');
  });

  test('a steps row links its id to the step page and its job_id to the packet', () => {
    const row = { id: 's1', job_id: 'j1' };
    expect(cellHref('steps', 'job_id', row)).toBe('/ux/jobs/j1');
    expect(cellHref('steps', 'id', row)).toBe('/ux/jobs/j1/steps/s1?from=%2Fux%2Fviews&from_label=Views');
  });

  test('a step id without its job_id in the row is not a link — there is nowhere to send it', () => {
    expect(cellHref('steps', 'id', { id: 's1' })).toBeNull();
  });

  test('a subjects row links its id to the subject, by its kind', () => {
    expect(cellHref('subjects', 'id', { kind: 'employee', id: 'emp-7' })).toBe('/ux/people/emp-7');
    expect(cellHref('subjects', 'id', { kind: 'custom', id: 'estate' })).toBe('/ux/jobs?subject_id=estate');
    expect(cellHref('subjects', 'id', { id: 'estate' })).toBeNull();
  });

  test('other cells, events rows and empty values stay text', () => {
    expect(cellHref('jobs', 'status', { id: 'j1', status: 'open' })).toBeNull();
    expect(cellHref('events', 'id', { id: 'e1' })).toBeNull();
    expect(cellHref('jobs', 'id', { id: null })).toBeNull();
    expect(cellHref('jobs', 'id', { id: '' })).toBeNull();
  });
});

describe('the sharer is named from their people row (backlog 00ab7ddd)', () => {
  test('a found row names the person', () => {
    const employee = { id: 'emp-2', name: 'Robin' } as unknown as Employee;
    expect(sharerName('emp-2', { kind: 'found', employee })).toBe('Robin');
  });

  test('no row, or a read not made yet, falls back to the id', () => {
    expect(sharerName('agent-x', { kind: 'absent' })).toBe('agent-x');
    expect(sharerName('agent-x', undefined)).toBe('agent-x');
  });

  test('a failed read says it failed rather than passing the id off as the answer', () => {
    expect(sharerName('emp-2', { kind: 'failed', error: '/api/people/emp-2: HTTP 500' })).toBe(
      'emp-2 (name unread: /api/people/emp-2: HTTP 500)',
    );
  });
});
