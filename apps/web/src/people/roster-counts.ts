// What /ux/people may count, by whether its roster was read (backlog
// 47eadca3; page audit 0c0265a3 GAP 3, 2026-09-23). The roster starts
// as `[]`, and the header and every filter button derived their numbers
// from it regardless, so while the read was in flight — and above the
// "Couldn't load the roster" line when it failed — the page stated
// "0 active employees" and Active (0): an unread roster counted as an
// empty one. A count is printed only for a roster that was read. The
// read state and the filter-button label are data/readState.ts's
// (`readStateOfLoad`, `countLabel`) — this page and /ux/parts had each
// built their own copies of both (backlog a97d4cf2).
//
// /hr is the other People page with a roster header, and until page
// audit b959394e it built its own and printed "0 active employees" over
// a roster it had not read (backlog d8d48a49). It now takes the same
// header and the same counted roster from here, so the two pages cannot
// say "unknown" — or count emp-audit — differently.

import type { ReadState } from '../data/readState';

/// The headcount the header states: active rows, less those whose
/// role's Class says `counts_in_headcount: false` (backlog 6a123f1f;
/// page audit 0c0265a3 GAP 12, 2026-09-23). The live page read "2
/// active employees" for one person and emp-audit, the System Audit
/// Account the operator baseline ships in every deployment. The rule is
/// the registry's, not an id list: `audit-readonly` carries the key
/// (migration 20260926223752), and a tenant can mark any role worn by
/// accounts rather than people the same way. `is_system_role` is NOT
/// the rule — `platform-admin` carries it too, and the founder wears it.
/// A role the registry does not know (or has not loaded) is counted:
/// only a Class that says so excludes anyone.
export function headcount(
  roster: ReadonlyArray<Readonly<{ role: string | null; status: string | null }>>,
  roleClasses: ReadonlyArray<RoleClass>,
): number {
  return countedRoster(roster, roleClasses).filter((e) => e.status === 'active').length;
}

type RoleClass = Readonly<{ code: string; metadata: Readonly<Record<string, unknown>> }>;

/// The roster a count is taken over: every row, whatever its status,
/// less those whose role's Class says `counts_in_headcount: false`.
/// headcount() is its active rows. /hr states more counts than the one
/// header — At a glance, the Headcount table, average tenure — and
/// counted emp-audit in all of them (backlog ad2da739, page audit
/// b959394e), so the excluded-role set is decided here, once, for both
/// People pages.
export function countedRoster<T extends Readonly<{ role: string | null }>>(
  roster: ReadonlyArray<T>,
  roleClasses: ReadonlyArray<RoleClass>,
): ReadonlyArray<T> {
  const excluded = new Set(
    roleClasses.filter((c) => c.metadata.counts_in_headcount === false).map((c) => c.code),
  );
  return roster.filter((e) => !(e.role !== null && excluded.has(e.role)));
}

const plural = (n: number, one: string, many: string): string => `${n} ${n === 1 ? one : many}`;

/// The page header. A read roster is counted — zero included, because
/// a roster that was read and is empty IS empty; an unread one says
/// why it has no count instead of printing one.
export function rosterHeader(
  read: ReadState,
  activeCount: number,
  expiringCount: number,
): Readonly<{ title: string; subtitle: string }> {
  if (read.kind === 'ok') {
    return {
      title: plural(activeCount, 'active employee', 'active employees'),
      subtitle: `${plural(expiringCount, 'certification', 'certifications')} expiring in 90 days`,
    };
  }
  return {
    title: 'Active employees',
    subtitle:
      read.kind === 'loading' ? 'Loading the roster…' : 'Counts unknown: the roster did not load',
  };
}
