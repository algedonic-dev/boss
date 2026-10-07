import { expect, test } from 'bun:test';
import { rowDepartment } from './department';

const roster = [{ code: 'finance', label: 'Finance' }, { code: 'it', label: 'IT' }];
const workflows = [{ kind: 'ordinary', metadata: { department: 'finance' } }, { kind: 'shared', metadata: {} }];

test('packet department overrides active workflow and labels come from registry data', () => {
  expect(rowDepartment({ kind: 'ordinary', metadata: { department: 'it' } }, workflows, roster)).toEqual({ kind: 'declared', code: 'it', label: 'IT', registered: true });
  expect(rowDepartment({ kind: 'ordinary', metadata: {} }, workflows, roster)).toEqual({ kind: 'declared', code: 'finance', label: 'Finance', registered: true });
});

test('native non-string and empty carried values fall back, unclassified and historical kinds remain blank', () => {
  for (const department of ['', null, 7, {}]) {
    expect(rowDepartment({ kind: 'ordinary', metadata: { department } }, workflows, roster)).toEqual({ kind: 'declared', code: 'finance', label: 'Finance', registered: true });
  }
  expect(rowDepartment({ kind: 'shared', metadata: {} }, workflows, roster)).toEqual({ kind: 'none' });
  expect(rowDepartment({ kind: 'historic', metadata: null }, workflows, roster)).toEqual({ kind: 'none' });
});

test('unread registries and omitted packet metadata never assert no department', () => {
  expect(rowDepartment({ kind: 'ordinary', metadata: {} }, null, roster)).toEqual({ kind: 'unknown' });
  expect(rowDepartment({ kind: 'ordinary', metadata: {} }, workflows, null)).toEqual({ kind: 'unknown' });
  expect(rowDepartment({ kind: 'ordinary' }, workflows, roster)).toEqual({ kind: 'unknown' });
  expect(rowDepartment({ kind: 'ordinary', metadata: { department: 'it' } }, null, roster)).toEqual({ kind: 'declared', code: 'it', label: 'IT', registered: true });
});

test('an unregistered declared code is retained without inventing a valid destination', () => {
  expect(rowDepartment({ kind: 'ordinary', metadata: { department: 'unknown' } }, workflows, roster)).toEqual({ kind: 'declared', code: 'unknown', label: 'unknown', registered: false });
});
