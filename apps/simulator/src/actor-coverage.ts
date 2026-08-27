// Pure logic behind the Actor-coverage panel: ordering + labeling of
// the telemetry's `actor_coverage` roles. Kept out of the component so
// it's unit-testable (bun test) — the component only renders.

import type { RoleCoverage, RoleStatus } from './types';

/** Display label for a role's coverage status. Operator-held roles are
 *  excluded from sim driving BY DESIGN (the sim never acts as a real
 *  login), so they read as a deliberate exclusion, not a gap. */
export function statusLabel(status: RoleStatus): string {
  return status === 'operator' ? 'operator (not simulated)' : status;
}

const STATUS_RANK: Readonly<Record<RoleStatus, number>> = {
  dormant: 0,
  acting: 1,
  operator: 2,
};

/** Table order: dormant roles first — an under-simulated brewery must be
 *  visible before anything else — then acting roles busiest-first, then
 *  the by-design operator exclusions. Alphabetical within ties. Returns
 *  a new array (no input mutation). */
export function sortRoles(roles: ReadonlyArray<RoleCoverage>): RoleCoverage[] {
  return [...roles].sort(
    (a, b) =>
      STATUS_RANK[a.status] - STATUS_RANK[b.status] ||
      b.completions - a.completions ||
      a.role.localeCompare(b.role),
  );
}

/** The dormant role names, alphabetically — the headline strip. Operator
 *  roles never appear here: not simulated is not the same as not working. */
export function dormantRoles(roles: ReadonlyArray<RoleCoverage>): string[] {
  return roles
    .filter((r) => r.status === 'dormant')
    .map((r) => r.role)
    .sort((a, b) => a.localeCompare(b));
}
