import { describe, expect, it } from 'bun:test';
import { PLATFORM_REGIONS, hasPlatforms } from './world-interior';

describe('which regions have platforms', () => {
  it('is receiving, marshalling and the shop floor, and nothing else', () => {
    expect([...PLATFORM_REGIONS]).toEqual(['receiving', 'marshalling', 'shop-floor']);
    expect(hasPlatforms('receiving')).toBe(true);
    expect(hasPlatforms('marshalling')).toBe(true);
    expect(hasPlatforms('shop-floor')).toBe(true);
    expect(hasPlatforms('dock')).toBe(false);
    expect(hasPlatforms('nowhere')).toBe(false);
  });
});
