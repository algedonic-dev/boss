import { describe, it, expect } from 'bun:test';
import { normalise, isCapped } from './paginated';

describe('normalise', () => {
  it('reads the standard envelope shape', () => {
    const p = normalise<number>({ data: [1, 2, 3], total: 50, limit: 3, offset: 6 });
    expect(p.data).toEqual([1, 2, 3]);
    expect(p.total).toBe(50);
    expect(p.limit).toBe(3);
    expect(p.offset).toBe(6);
  });

  it('rejects bare arrays — every list endpoint returns the envelope', () => {
    // A bare-array response is a contract violation; surfacing it as
    // an empty page makes the regression visible instead of silently
    // presenting an uncapped list.
    const p = normalise<number>([1, 2, 3]);
    expect(p).toEqual({ data: [], total: 0, limit: 0, offset: 0 });
  });

  it('treats missing total as data.length so callers do not see fake caps', () => {
    const p = normalise<number>({ data: [1, 2, 3] });
    expect(p.total).toBe(3);
  });

  it('handles non-object bodies as empty', () => {
    expect(normalise<number>(null)).toEqual({ data: [], total: 0, limit: 0, offset: 0 });
    expect(normalise<number>('not-json' as unknown)).toEqual({
      data: [],
      total: 0,
      limit: 0,
      offset: 0,
    });
  });
});

describe('isCapped', () => {
  it('returns true when total exceeds the returned page', () => {
    expect(isCapped({ data: [1, 2, 3], total: 50, limit: 3, offset: 0 })).toBe(true);
  });

  it('returns false when total equals the returned page', () => {
    expect(isCapped({ data: [1, 2, 3], total: 3, limit: 3, offset: 0 })).toBe(false);
  });

  it('returns false for null', () => {
    expect(isCapped(null)).toBe(false);
  });
});
