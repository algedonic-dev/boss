import { describe, expect, it } from 'bun:test';
import { TRANSIT_KEY, loadTransit, saveTransit, transitOf } from './transit-choice';
import { TRANSIT_MS } from './live-motion';

/** A Storage that holds its items in a map, and one that refuses every
 *  call the way a blocked or private-mode browser does. */
const memory = (): Pick<Storage, 'getItem' | 'setItem'> & { items: Map<string, string> } => {
  const items = new Map<string, string>();
  return { items, getItem: (k) => items.get(k) ?? null, setItem: (k, v) => void items.set(k, v) };
};
const blocked = (): Storage => {
  throw new DOMException('blocked', 'SecurityError');
};

describe("the viewer's crossing time (David 2026-09-27, backlog 7c581d3d)", () => {
  it('reads only one of the four choices, and anything else is the default', () => {
    expect(transitOf('6000')).toBe(6_000);
    expect(transitOf('60000')).toBe(60_000);
    for (const bad of [null, '', 'x', '5000', '6000.5', '-6000', '1e99']) expect(transitOf(bad)).toBe(TRANSIT_MS);
  });
  it('a choice saved is the choice loaded, under one key', () => {
    const s = memory();
    expect(loadTransit(() => s)).toBe(TRANSIT_MS);
    saveTransit(() => s, 15_000);
    expect(s.items.get(TRANSIT_KEY)).toBe('15000');
    expect(loadTransit(() => s)).toBe(15_000);
  });
  it('blocked storage is the default on read and nothing on write — never a thrown page', () => {
    expect(loadTransit(blocked)).toBe(TRANSIT_MS);
    expect(() => saveTransit(blocked, 6_000)).not.toThrow();
    const refusing = { getItem: () => { throw new Error('no'); }, setItem: () => { throw new Error('no'); } };
    expect(loadTransit(() => refusing)).toBe(TRANSIT_MS);
    expect(() => saveTransit(() => refusing, 6_000)).not.toThrow();
  });
});
