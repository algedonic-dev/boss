import { describe, expect, it } from 'bun:test';
import { parseNames, recipientReason } from './roster';

describe('the scoped identity roster', () => {
  it('retains identity-first nulls and discards unrelated fields', () => {
    expect(parseNames([{ id: 'emp-a', name: null, role: null, annual_salary_cents: 100 }]))
      .toEqual([{ id: 'emp-a', name: null, role: null }]);
    expect(parseNames([])).toEqual([]);
  });
  it('refuses ambiguous or malformed identities and descriptions', () => {
    for (const raw of [null, {}, [null], [{ id: '' }], [{ id: 'emp-a' }],
      [{ id: 'emp-a', name: 2, role: null }], [{ id: 'emp-a', name: null, role: false }],
      [{ id: 'emp-a', name: null, role: null }, { id: 'emp-a', name: 'Other', role: null }]]) {
      expect(() => parseNames(raw)).toThrow();
    }
  });
  it('distinguishes loading, failure, read-empty and an out-of-scope recipient', () => {
    expect(recipientReason({ kind: 'loading' }, '')).toBe('Send unavailable until recipients are read');
    expect(recipientReason({ kind: 'failed', error: 'HTTP 500' }, 'emp-a')).toBe('Send unavailable until recipients are read');
    expect(recipientReason({ kind: 'ready', data: [] }, '')).toBe('No recipients are available in this read');
    const read = { kind: 'ready' as const, data: [{ id: 'emp-a', name: null, role: null }] };
    expect(recipientReason(read, '')).toBe('Select a recipient');
    expect(recipientReason(read, 'emp-elsewhere')).toBe('Selected recipient is not available in this read');
    expect(recipientReason(read, 'emp-a')).toBeNull();
  });
});
