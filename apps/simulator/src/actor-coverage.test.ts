// Actor-coverage panel logic — pure functions over the telemetry's
// `actor_coverage` block (see ActorCoveragePanel.svelte for the render).
import { describe, expect, test } from 'bun:test';

import { dormantRoles, sortRoles, statusLabel } from './actor-coverage';
import type { RoleCoverage } from './types';

const role = (r: Partial<RoleCoverage> & Pick<RoleCoverage, 'role' | 'status'>): RoleCoverage => ({
  roster: 0,
  operator_holders: 0,
  acting: 0,
  completions: 0,
  ...r,
});

describe('statusLabel', () => {
  test('operator roles are labeled as by-design exclusions, not gaps', () => {
    expect(statusLabel('operator')).toBe('operator (not simulated)');
  });
  test('acting and dormant pass through', () => {
    expect(statusLabel('acting')).toBe('acting');
    expect(statusLabel('dormant')).toBe('dormant');
  });
});

describe('sortRoles', () => {
  test('dormant first (the point of the panel), then acting busiest-first, operator last', () => {
    const rows = [
      role({ role: 'platform-admin', status: 'operator' }),
      role({ role: 'brewer', status: 'acting', completions: 10 }),
      role({ role: 'cellar-hand', status: 'acting', completions: 40 }),
      role({ role: 'zym-auditor', status: 'dormant' }),
      role({ role: 'auditor', status: 'dormant' }),
    ];
    expect(sortRoles(rows).map((r) => r.role)).toEqual([
      'auditor',
      'zym-auditor',
      'cellar-hand',
      'brewer',
      'platform-admin',
    ]);
  });

  test('does not mutate its input', () => {
    const rows = [
      role({ role: 'b', status: 'acting', completions: 1 }),
      role({ role: 'a', status: 'dormant' }),
    ];
    const before = rows.map((r) => r.role);
    sortRoles(rows);
    expect(rows.map((r) => r.role)).toEqual(before);
  });
});

describe('dormantRoles', () => {
  test('names dormant roles alphabetically, excluding operator roles', () => {
    const rows = [
      role({ role: 'zym-auditor', status: 'dormant' }),
      role({ role: 'brewer', status: 'acting', completions: 3 }),
      role({ role: 'platform-admin', status: 'operator' }),
      role({ role: 'auditor', status: 'dormant' }),
    ];
    expect(dormantRoles(rows)).toEqual(['auditor', 'zym-auditor']);
  });

  test('empty when nothing is dormant', () => {
    expect(dormantRoles([role({ role: 'brewer', status: 'acting', completions: 1 })])).toEqual([]);
  });
});
