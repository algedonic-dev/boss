import { describe, expect, test } from 'bun:test';
import { presentationHref } from './map-navigation';

describe('map presentation navigation preserves the existing selection identity', () => {
  test('direct detailed links remain unchanged', () => {
    expect(presentationHref('/it?at=gates', false)).toBe('/it?at=gates');
  });
  test('overview selections stay in the same presentation and retain encoded section identity', () => {
    expect(presentationHref('/it?at=gates-%3Edock', true)).toBe('/it?at=gates-%3Edock&view=overview');
    expect(presentationHref('/it', true)).toBe('/it?view=overview');
  });
});
