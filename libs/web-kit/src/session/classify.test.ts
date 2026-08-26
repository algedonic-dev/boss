import { describe, expect, test } from 'bun:test';
import { classifyProbe, type Employee } from './classify';

const emp: Employee = {
  id: 'emp-1', name: 'Ada', email: 'ada@x', role: 'platform-admin',
  department: 'platform', hire_date: '2020-01-01', status: 'active',
  location: 'hq', employment_type: 'ft', skills: [], certifications: [],
};
const byId = new Map([[emp.id, emp]]);

describe('classifyProbe', () => {
  test('a resolved employee is ready and writable', () => {
    const c = classifyProbe({ username: 'ada@x', employee_id: 'emp-1' }, byId)!;
    expect(c.value.kind).toBe('ready');
    expect(c.readonly).toBe(false);
  });

  test('audit-readonly with no employee is the guest — first-class and read-only', () => {
    const c = classifyProbe(
      { username: 'guest@algedonic.dev', role: 'audit-readonly' },
      byId,
    )!;
    expect(c.value.kind).toBe('ready');
    expect(c.readonly).toBe(true);
    if (c.value.kind === 'ready') {
      expect(c.value.user.name).toBe('Guest');
      expect(c.value.user.role).toBe('audit-readonly');
    }
  });

  test('any other unmatched session stays unrecognized', () => {
    const c = classifyProbe({ username: 'who@x', role: 'service-tech' }, byId)!;
    expect(c.value.kind).toBe('unrecognized');
  });

  test('no session at all falls through', () => {
    expect(classifyProbe({}, byId)).toBeNull();
  });
});
