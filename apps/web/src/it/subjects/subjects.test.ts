import { describe, expect, test } from 'bun:test';
import {
  buildKindTree,
  groupClassesByAttribute,
  kindModule,
  workflowCountsByKind,
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

  test('a null member_attribute with neither shape falls under "(unclassified)"', () => {
    const groups = groupClassesByAttribute([cls({ code: 'mystery', member_attribute: null })]);
    expect(groups.map(([k]) => k)).toEqual(['(unclassified)']);
  });

  // Backlog 2c7a2d5c (page audit 9f7ba57d, gap 3): the six `node`
  // Classes carry a NULL member_attribute BY DESIGN — their membership
  // is the node_roles junction table, named in metadata.membership
  // (202609120300-a-node-declares-its-roles.sql). A declared shape is
  // titled by what it declares, never called unclassified.
  test('a null member_attribute is titled by its declared metadata.membership', () => {
    const groups = groupClassesByAttribute([
      cls({ code: 'gate', member_attribute: null, metadata: { membership: 'node_roles' }, sort_order: 2 }),
      cls({ code: 'build', member_attribute: null, metadata: { membership: 'node_roles' }, sort_order: 1 }),
      cls({ code: 'mystery', member_attribute: null }),
    ]);
    expect(groups.map(([k]) => k)).toEqual(['(unclassified)', 'node_roles']);
    expect(groups[1]![1].map((c) => c.code)).toEqual(['build', 'gate']);
  });

  test('a membership that is not a non-empty string is no declaration', () => {
    const groups = groupClassesByAttribute([
      cls({ code: 'a', member_attribute: null, metadata: { membership: '' } }),
      cls({ code: 'b', member_attribute: null, metadata: { membership: 7 } }),
    ]);
    expect(groups.map(([k]) => k)).toEqual(['(unclassified)']);
  });

  test('a member_attribute wins over a membership on the same row', () => {
    const groups = groupClassesByAttribute([
      cls({ code: 'ceo', member_attribute: 'role', metadata: { membership: 'node_roles' } }),
    ]);
    expect(groups.map(([k]) => k)).toEqual(['role']);
  });
});

// Backlog 92ea2e00 (page audit 9f7ba57d, gap 5): `owner platform` was
// the same on 24 of 24 kinds. A kind now names the manifest module its
// surfaces live behind, in metadata.module; a platform kind names none.
describe('kindModule', () => {
  test('reads metadata.module', () => {
    expect(kindModule(sk({ kind: 'asset', metadata: { module: 'equipment' } }))).toBe('equipment');
  });

  test('a platform kind has none', () => {
    expect(kindModule(sk({ kind: 'employee' }))).toBeNull();
  });

  test('a module that is not a non-empty string is none', () => {
    expect(kindModule(sk({ kind: 'x', metadata: { module: '' } }))).toBeNull();
    expect(kindModule(sk({ kind: 'y', metadata: { module: true } }))).toBeNull();
  });
});

describe('workflowCountsByKind', () => {
  const wf = (kind: string, status: string, subject_kinds: ReadonlyArray<string>) => ({
    kind,
    status,
    subject_kinds,
  });

  test('counts the ACTIVE workflows naming each kind in subject_kinds', () => {
    const counts = workflowCountsByKind([
      wf('sale', 'active', ['account', 'customer']),
      wf('renewal', 'active', ['account']),
      wf('old-sale', 'retired', ['account']),
      wf('idea', 'draft', ['asset']),
    ]);
    expect(counts.get('account')).toBe(2);
    expect(counts.get('customer')).toBe(1);
    expect(counts.get('asset')).toBeUndefined();
  });

  test('a workflow naming a kind twice counts once', () => {
    expect(workflowCountsByKind([wf('dup', 'active', ['asset', 'asset'])]).get('asset')).toBe(1);
  });
});
