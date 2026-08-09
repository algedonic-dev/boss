// The position key must match the server's fleet grouping —
// COALESCE(NULLIF(spec_slug,''), title) — or item lists sit under
// the wrong node badge.
import { describe, expect, it } from 'bun:test';
import { groupByPosition, positionOf } from './position';
import type { Job } from './types';

function job(id: string, priority: string, opened: string, steps: unknown[]): Job {
  return { id, priority, opened_on: opened, steps } as unknown as Job;
}

describe('positionOf', () => {
  it('uses spec_slug of the first in-flight step', () => {
    const j = job('a', 'standard', '2026-08-01', [
      { status: 'completed', title: 'Old', spec_slug: 'old' },
      { status: 'ready', title: 'Triage feedback', spec_slug: 'triage' },
      { status: 'pending', title: 'Later', spec_slug: 'later' },
    ]);
    expect(positionOf(j)).toBe('triage');
  });

  it('falls back to the title when the slug is empty or absent — the pre-migration shape', () => {
    const empty = job('a', 'standard', '2026-08-01', [
      { status: 'active', title: 'Deliver', spec_slug: '' },
    ]);
    const absent = job('b', 'standard', '2026-08-01', [{ status: 'ready', title: 'Deliver' }]);
    expect(positionOf(empty)).toBe('Deliver');
    expect(positionOf(absent)).toBe('Deliver');
  });

  it('is null for a Job with nothing in flight', () => {
    expect(positionOf(job('a', 'standard', '2026-08-01', [{ status: 'completed', title: 'X' }]))).toBeNull();
  });
});

describe('groupByPosition', () => {
  it('orders each queue priority-first then oldest-first', () => {
    const grouped = groupByPosition([
      job('old-standard', 'standard', '2026-08-01', [{ status: 'ready', spec_slug: 't', title: 'T' }]),
      job('new-urgent', 'urgent', '2026-08-08', [{ status: 'ready', spec_slug: 't', title: 'T' }]),
      job('new-standard', 'standard', '2026-08-08', [{ status: 'ready', spec_slug: 't', title: 'T' }]),
    ]);
    expect(grouped.get('t')!.map((j) => j.id)).toEqual([
      'new-urgent',
      'old-standard',
      'new-standard',
    ]);
  });
});
