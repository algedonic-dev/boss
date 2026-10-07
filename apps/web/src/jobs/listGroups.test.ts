import { expect, test } from 'bun:test';
import { listGroups, groupKindQuery } from './listGroups';

const rows = [
  { kind: 'ordinary-looking', metadata: { list_group: { code: 'machines', label: 'Machine activity' } } },
  { kind: 'maintenance-human', metadata: {} },
  { kind: 'second', metadata: { list_group: { code: 'machines', label: 'Machine activity' } } },
  { kind: 'customer-batch', metadata: { list_group: { code: 'batch', label: 'Batch work' } } },
];

test('group membership and arbitrary labels come only from registry data', () => {
  expect(listGroups(rows)).toEqual([
    { value: 'group:batch', label: 'Batch work', kinds: ['customer-batch'] },
    { value: 'group:machines', label: 'Machine activity', kinds: ['ordinary-looking', 'second'] },
  ]);
  expect(groupKindQuery(rows, 'group:machines')).toEqual({ kinds: ['ordinary-looking', 'second'] });
  expect(groupKindQuery(rows, 'group:unknown')).toEqual({ kinds: [] });
  expect(groupKindQuery(rows, '')).toEqual({});
  expect(groupKindQuery(rows, 'other')).toEqual({ exclude_kinds: ['customer-batch', 'ordinary-looking', 'second'] });
});

test('unreadable declarations cannot silently select everything', () => {
  expect(() => listGroups([{ kind: 'bad', metadata: { list_group: { code: 'machine' } } }])).toThrow();
  expect(() => listGroups([...rows, { kind: 'bad', metadata: { list_group: { code: 'machines', label: 'Contradictory' } } }])).toThrow();
  expect(() => groupKindQuery(rows, 'invalid')).toThrow();
});
