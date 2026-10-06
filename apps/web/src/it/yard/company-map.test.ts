import { describe, expect, test } from 'bun:test';
import { parseCompanyDepartments } from './company-map';

describe('a company map reads the complete department registry', () => {
  const row = { code: 'hosting', display_name: 'Cloud Operations' };
  test('new departments and empty complete registries are real readings', () => {
    expect(parseCompanyDepartments({ data: [row], total: 1 })).toEqual([{ code: 'hosting', label: 'Cloud Operations' }]);
    expect(parseCompanyDepartments({ data: [], total: 0 })).toEqual([]);
  });
  test('partial, malformed or duplicate rows cannot pose as a complete map', () => {
    for (const body of [null, [], { data: [row], total: 2 }, { data: [row] },
      { data: [row, row], total: 2 }, { data: [{ code: '', display_name: 'IT' }], total: 1 },
      { data: [{ code: 'it', display_name: null }], total: 1 }]) {
      expect(() => parseCompanyDepartments(body)).toThrow();
    }
  });
});
