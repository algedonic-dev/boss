import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { kindsForDepartment } from './newJobKinds';

// "Start a new Job" on /ux/sales offered every live kind — 64 on
// 2026-09-27, two of them Sales's — because the picker read
// GET /api/workflows unfiltered (backlog dc06c0fc, page audit
// 1e9283fc gap 4). A department page now offers the kinds whose row
// declares its department, by the rule the server's department join
// reads (boss-jobs `department::carried`).
describe('kindsForDepartment', () => {
  const row = (kind: string, metadata?: Record<string, unknown> | null) => ({
    kind, label: kind, subject_kinds: ['custom'], metadata,
  });
  const REGISTRY = [
    row('receive-a-sponsorship', { department: 'sales' }),
    row('page-audit', {}),
    row('receive-an-inquiry', { department: 'sales' }),
    row('receive-a-payout', { department: 'finance' }),
    row('backlog-item'),
    row('ad-hoc', null),
  ];
  const kinds = (rows: ReadonlyArray<{ kind: string }>) => rows.map((r) => r.kind);

  test('with no department the picker is the whole registry, in its order', () => {
    expect(kinds(kindsForDepartment(REGISTRY, ''))).toEqual(kinds(REGISTRY));
  });

  test("a department offers the kinds declaring it, in the registry's order, and the registered ad-hoc row", () => {
    expect(kinds(kindsForDepartment(REGISTRY, 'sales'))).toEqual([
      'receive-a-sponsorship', 'receive-an-inquiry', 'ad-hoc',
    ]);
  });

  test('a department nothing declares offers only ad hoc — never the whole registry', () => {
    expect(kinds(kindsForDepartment(REGISTRY, 'support'))).toEqual(['ad-hoc']);
    expect(kinds(kindsForDepartment(REGISTRY.filter((r) => r.kind !== 'ad-hoc'), 'support'))).toEqual([]);
  });

  test('an empty or non-string declaration names no department', () => {
    const odd = [row('blank', { department: '' }), row('numeric', { department: 7 })];
    expect(kindsForDepartment(odd, 'sales')).toEqual([]);
  });

  test('the jobs list scopes its picker by the page department and defaults a subject kind only from a chosen kind', () => {
    const src = readFileSync(join(import.meta.dir, 'JobsListPage.svelte'), 'utf8').replace(/\s+/g, ' ');
    expect(src).toContain('kindsForDepartment(kinds, initialDepartment)');
    // backlog 685f43aa: the opening default took the first subject kind
    // of the union, then narrowed the picker to it — custom hid
    // "Receive an inquiry". The first allowed of the WHOLE union is no
    // longer read anywhere.
    expect(src).not.toContain('allowedSubjectKinds[0]');
  });
});
