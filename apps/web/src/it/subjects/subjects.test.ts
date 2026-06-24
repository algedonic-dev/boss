import { describe, expect, test } from 'bun:test';
import {
  buildKindTree,
  groupClassesByAttribute,
  type ClassRow,
  type SubjectKind,
} from './subjects';

const sk = (over: Partial<SubjectKind> & Pick<SubjectKind, 'kind'>): SubjectKind => ({
  label: over.kind,
  parent_kind: null,
  description: null,
  owning_team: 'platform',
  metadata: {},
  sort_order: 0,
  retired_at: null,
  ...over,
});

const cls = (over: Partial<ClassRow> & Pick<ClassRow, 'code'>): ClassRow => ({
  subject_kind: 'employee',
  display_name: over.code,
  parent_code: null,
  member_attribute: 'role',
  metadata: {},
  sort_order: 0,
  retired_at: null,
  ...over,
});

describe('buildKindTree', () => {
  test('roots nest their children, both sorted by sort_order then kind', () => {
    const tree = buildKindTree([
      sk({ kind: 'account', parent_kind: 'person', sort_order: 2 }),
      sk({ kind: 'person', sort_order: 1 }),
      sk({ kind: 'object', sort_order: 3 }),
      sk({ kind: 'employee', parent_kind: 'person', sort_order: 1 }),
    ]);
    expect(tree.map((n) => n.kind.kind)).toEqual(['person', 'object']);
    expect(tree[0]!.children.map((c) => c.kind)).toEqual(['employee', 'account']);
    expect(tree[1]!.children).toEqual([]);
  });

  test('retired kinds are dropped from roots and children', () => {
    const tree = buildKindTree([
      sk({ kind: 'person', sort_order: 1 }),
      sk({ kind: 'employee', parent_kind: 'person', sort_order: 1 }),
      sk({ kind: 'ghost', parent_kind: 'person', retired_at: '2026-01-01T00:00:00Z' }),
      sk({ kind: 'old-root', sort_order: 9, retired_at: '2026-01-01T00:00:00Z' }),
    ]);
    expect(tree.map((n) => n.kind.kind)).toEqual(['person']);
    expect(tree[0]!.children.map((c) => c.kind)).toEqual(['employee']);
  });

  test('an orphan (parent absent) surfaces as its own top-level node', () => {
    const tree = buildKindTree([
      sk({ kind: 'person', sort_order: 1 }),
      sk({ kind: 'asset', parent_kind: 'object', sort_order: 2 }), // object not present
    ]);
    expect(tree.map((n) => n.kind.kind)).toEqual(['person', 'asset']);
  });
});

describe('groupClassesByAttribute', () => {
  test('groups by member_attribute, sorts within group and across keys', () => {
    const groups = groupClassesByAttribute([
      cls({ code: 'sales', member_attribute: 'department', sort_order: 2 }),
      cls({ code: 'cto', member_attribute: 'role', sort_order: 2 }),
      cls({ code: 'ceo', member_attribute: 'role', sort_order: 1 }),
      cls({ code: 'exec', member_attribute: 'department', sort_order: 1 }),
    ]);
    expect(groups.map(([k]) => k)).toEqual(['department', 'role']);
    expect(groups[0]![1].map((c) => c.code)).toEqual(['exec', 'sales']);
    expect(groups[1]![1].map((c) => c.code)).toEqual(['ceo', 'cto']);
  });

  test('retired classes are excluded', () => {
    const groups = groupClassesByAttribute([
      cls({ code: 'ceo' }),
      cls({ code: 'retired-role', retired_at: '2026-01-01T00:00:00Z' }),
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0]![1].map((c) => c.code)).toEqual(['ceo']);
  });

  test('a null member_attribute falls under "(unclassified)"', () => {
    const groups = groupClassesByAttribute([cls({ code: 'mystery', member_attribute: null })]);
    expect(groups.map(([k]) => k)).toEqual(['(unclassified)']);
  });
});
