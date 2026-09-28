// /ux/people counted an unread roster as an empty one (backlog 47eadca3;
// page audit 0c0265a3 GAP 3, 2026-09-23). The header and every filter
// button derived their numbers from the `[]` the roster starts as, so
// while the read was in flight — and above the "Couldn't load the
// roster" line when it failed — the page stated "0 active employees",
// "0 certifications expiring in 90 days", Active (0) and All (0). The
// body was honest; the counts were the false-empty class. These cases
// hold the rule: a count is printed only for a roster that was read.
// The page wiring is pinned by people-unread-roster-counts.mocked.spec.ts.
// The read state and the filter labels are data/readState.ts's
// (readStateOfLoad, countLabel), tested there (backlog a97d4cf2).

import { describe, expect, it } from 'bun:test';
import { failedRead, loadingRead, okRead } from '../data/readState';
import { countedRoster, headcount, rosterHeader } from './roster-counts';

describe('rosterHeader', () => {
  it('counts a read roster, zero included — an empty roster that was read is empty', () => {
    expect(rosterHeader(okRead, 12, 3)).toEqual({
      title: '12 active employees',
      subtitle: '3 certifications expiring in 90 days',
    });
    // One is one employee (backlog 6a123f1f made one the live count).
    expect(rosterHeader(okRead, 1, 1)).toEqual({
      title: '1 active employee',
      subtitle: '1 certification expiring in 90 days',
    });
    expect(rosterHeader(okRead, 0, 0)).toEqual({
      title: '0 active employees',
      subtitle: '0 certifications expiring in 90 days',
    });
  });

  it('states no count while the roster is loading', () => {
    const h = rosterHeader(loadingRead, 0, 0);
    expect(h.title).toBe('Active employees');
    expect(h.subtitle).toBe('Loading the roster…');
  });

  it('says the counts are unknown when the read failed', () => {
    const h = rosterHeader(failedRead('people HTTP 500'), 0, 0);
    expect(h.title).toBe('Active employees');
    expect(h.subtitle).toBe('Counts unknown: the roster did not load');
  });

  it('never prints a number for an unread roster, whatever the arrays hold', () => {
    for (const read of [loadingRead, failedRead('people HTTP 500')]) {
      const h = rosterHeader(read, 7, 2);
      expect(`${h.title} ${h.subtitle}`).not.toMatch(/\d/);
    }
  });
});

// Backlog 6a123f1f (page audit 0c0265a3 GAP 12): the live header read
// "2 active employees" — David and emp-audit, the System Audit Account
// the operator baseline ships. Who counts is the role's Class, not an
// id: a role whose metadata says `counts_in_headcount: false` is worn
// by accounts, not people.
describe('headcount', () => {
  const row = (id: string, role: string | null, status: string | null) => ({ id, role, status });
  const ROLES = [
    { code: 'platform-admin', metadata: { is_system_role: true } },
    { code: 'audit-readonly', metadata: { is_system_role: true, counts_in_headcount: false } },
    { code: 'brewer', metadata: {} },
  ];

  it('counts the active rows whose role Class does not exclude them', () => {
    const roster = [
      row('emp-david', 'platform-admin', 'active'),
      row('emp-audit', 'audit-readonly', 'active'),
      row('emp-001', 'brewer', 'active'),
      row('emp-002', 'brewer', 'on-leave'),
    ];
    expect(headcount(roster, ROLES)).toBe(2);
  });

  it('is_system_role alone does not exclude — the platform admin is a person', () => {
    expect(headcount([row('emp-david', 'platform-admin', 'active')], ROLES)).toBe(1);
  });

  it('counts a role the registry does not know, and a row with no role', () => {
    expect(headcount([row('a', 'unknown-role', 'active'), row('b', null, 'active')], ROLES)).toBe(2);
  });
});

// Backlog ad2da739 (page audit b959394e): /hr counted emp-audit in its
// title, its At a glance block (Contractors 1 was emp-audit, the only
// contractor), its Headcount table and its average tenure. Every count
// /hr states is taken over the same roster headcount() counts, so the
// excluded-role set is decided once.
describe('countedRoster', () => {
  const row = (id: string, role: string | null, status: string | null) => ({ id, role, status });
  const ROLES = [
    { code: 'audit-readonly', metadata: { counts_in_headcount: false } },
    { code: 'brewer', metadata: {} },
  ];

  it('drops the rows whose role Class excludes them, whatever their status', () => {
    const roster = [
      row('emp-audit', 'audit-readonly', 'active'),
      row('emp-001', 'brewer', 'active'),
      row('emp-002', 'brewer', 'on-leave'),
      row('emp-003', null, 'terminated'),
    ];
    expect(countedRoster(roster, ROLES).map((e) => e.id)).toEqual(['emp-001', 'emp-002', 'emp-003']);
  });

  it('is the roster headcount() counts — its active rows are the headcount', () => {
    const roster = [row('emp-audit', 'audit-readonly', 'active'), row('emp-001', 'brewer', 'active')];
    expect(countedRoster(roster, ROLES).filter((e) => e.status === 'active')).toHaveLength(
      headcount(roster, ROLES),
    );
  });
});
