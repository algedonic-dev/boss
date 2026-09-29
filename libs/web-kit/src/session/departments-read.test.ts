// Whether the departments registry read failed (backlog 720d6345).
// Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { departmentsReadFailedOf } from './departments-read';

describe('departmentsReadFailedOf — a failed read is not an empty roster', () => {
  test('a read still loading has not failed: nothing has answered yet', () => {
    expect(departmentsReadFailedOf({ kind: 'loading' })).toBe(false);
  });

  test('an answered read has not failed, even with no rows', () => {
    expect(departmentsReadFailedOf({ kind: 'ready', rows: [] })).toBe(false);
  });

  test('an error has failed', () => {
    expect(departmentsReadFailedOf({ kind: 'error' })).toBe(true);
  });
});
