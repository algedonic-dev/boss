import { describe, expect, test } from 'bun:test';
import { fmtCount, SURFACE_CARDS } from './systemModel';

describe('fmtCount', () => {
  test('passes small counts through unchanged', () => {
    expect(fmtCount(0)).toBe('0');
    expect(fmtCount(27)).toBe('27');
    expect(fmtCount(714)).toBe('714');
    expect(fmtCount(999)).toBe('999');
  });

  test('compacts thousands with one decimal under 10k, none above', () => {
    expect(fmtCount(1000)).toBe('1k');
    expect(fmtCount(1200)).toBe('1.2k');
    expect(fmtCount(12000)).toBe('12k');
  });

  test('compacts millions', () => {
    expect(fmtCount(2_500_000)).toBe('2.5M');
  });
});

describe('SURFACE_CARDS', () => {
  test('every card has a unique id and an absolute path', () => {
    const ids = SURFACE_CARDS.map((c) => c.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(SURFACE_CARDS.every((c) => c.path.startsWith('/'))).toBe(true);
  });
});
