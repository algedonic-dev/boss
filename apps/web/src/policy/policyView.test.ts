import { describe, expect, test } from 'bun:test';
import { failedRead, loadingRead, okRead } from '../data/readState';
import {
  defaultRole, matrixAxes, policySubtitle, roleList, rulesOf, scopeForDisplay, scopeOptions,
} from './policyView';
import type { PolicyRule, Scope } from './policyTypes';

// The /it/registry/policy audit (page audit b2af346a): what the matrix is
// allowed to draw, given the rules the read carried.

const rule = (role: string, resource: string, action: string, scope: Scope = 'all'): PolicyRule => ({
  id: `${role}:${resource}:${action}`, role, resource, action, scope, active: true,
});

// A resource the page's old hardcoded list of 13 never drew, held by
// break-glass — the one grant a privilege page most needs to show
// (backlog 720d6345, measured live: 43 of 186 rules hidden).
const RULES: ReadonlyArray<PolicyRule> = [
  rule('platform-admin', 'job', 'read'),
  rule('platform-admin', 'ledger', 'update'),
  rule('break-glass', 'step-signoff:platform-admin', 'sign-off'),
  rule('audit-readonly', 'event', 'read'),
];

describe('matrixAxes (720d6345)', () => {
  test('rows are every resource any rule carries, across all roles', () => {
    expect(matrixAxes(RULES).resources).toEqual(['event', 'job', 'ledger', 'step-signoff:platform-admin']);
  });

  test('columns are every action any rule carries — no constant to fall behind the Action enum', () => {
    expect(matrixAxes(RULES).actions).toEqual(['read', 'sign-off', 'update']);
  });

  test('no rules draw no axes', () => {
    expect(matrixAxes([])).toEqual({ resources: [], actions: [] });
  });
});

describe('roleList (47b7f120)', () => {
  test('is the union of role Classes and the roles rules carry, each once', () => {
    expect(roleList(RULES, ['platform-admin', 'engineering-agent', 'owner'])).toEqual([
      'audit-readonly', 'break-glass', 'engineering-agent', 'owner', 'platform-admin',
    ]);
  });
});

describe('rulesOf', () => {
  test('a role holding no rules answers an empty list, not undefined', () => {
    expect(rulesOf(RULES, 'engineering-agent')).toEqual([]);
    expect(rulesOf(RULES, 'break-glass').map((r) => r.resource)).toEqual(['step-signoff:platform-admin']);
  });
});

describe('defaultRole (656dc0de)', () => {
  const roles = roleList(RULES, ['engineering-agent']);

  test('is the signed-in role when it holds rules', () => {
    expect(defaultRole(roles, RULES, 'platform-admin')).toBe('platform-admin');
  });

  test('is the first role listed when the signed-in role holds none', () => {
    expect(defaultRole(roles, RULES, 'engineering-agent')).toBe('audit-readonly');
  });

  test('is the first role listed with no session, never a demo role', () => {
    expect(defaultRole(roles, RULES, null)).toBe('audit-readonly');
    expect(defaultRole(roles, RULES, 'ceo')).toBe('audit-readonly');
  });

  test('is null when there is no role to show', () => {
    expect(defaultRole([], [], 'platform-admin')).toBeNull();
  });
});

describe('policySubtitle (5a7ab4b1, 5358ad72)', () => {
  const roles = roleList(RULES, ['engineering-agent']);

  test('a pending read is not drawn as a zero', () => {
    expect(policySubtitle(loadingRead, RULES, roles)).toBe('Loading rules…');
  });

  test('a failed read claims no count', () => {
    expect(policySubtitle(failedRead('HTTP 500'), RULES, roles)).toBe(
      'Rule count unknown — the policy read failed',
    );
  });

  test('a read that landed counts the rules and the roles that hold them', () => {
    expect(policySubtitle(okRead, RULES, roles)).toBe('4 active rules · 3 of 4 roles hold rules');
  });
});

describe('scopeOptions (ba8411da)', () => {
  test('a department scope comes from the departments registry', () => {
    const values = scopeOptions(['it', 'finance'], 'all').map((o) => o.value);
    expect(values).toEqual(['none', 'self', 'team', 'territory', 'all', 'department:it', 'department:finance']);
  });

  test('the rule’s current scope is always an option, even when the registry does not list it', () => {
    const opts = scopeOptions([], 'department:archive');
    expect(opts.map((o) => o.value)).toContain('department:archive');
    expect(opts.find((o) => o.value === 'department:archive')?.label).toBe(
      'Department: archive (current; not in the departments registry)',
    );
  });

  test('a current scope already listed is not listed twice', () => {
    const values = scopeOptions(['it'], 'department:it').map((o) => o.value);
    expect(values.filter((v) => v === 'department:it')).toHaveLength(1);
  });
});

describe('scopeForDisplay', () => {
  test('a department scope reads as department:<code>', () => {
    expect(scopeForDisplay({ department: 'it' })).toBe('department:it');
    expect(scopeForDisplay('self')).toBe('self');
  });
});
