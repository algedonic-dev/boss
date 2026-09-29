// What /it/registry/policy is allowed to draw, as pure functions of the
// rules the read carried — page audit b2af346a, 2026-09-27.
//
// WHY THE AXES ARE DERIVED (backlog 720d6345). The matrix used to walk a
// hardcoded list of 13 resources and 8 actions. The live table carried
// 22 resources, so 43 of 186 rules were never drawn — among them
// break-glass's sign-off on step-signoff:platform-admin, the one grant a
// privilege page most needs to show. Resource is a free string in Rust
// and cannot be pinned by enumeration, and the 8 actions were the Rust
// Action enum living a second time with no pin (CLAUDE.md 9a). So both
// axes are the rules' own: rows are every resource ANY role's rules
// carry and columns every action — the union across roles, so two roles
// compared side by side are drawn on the same grid.

import type { ReadState } from '../data/readState';
import type { PolicyRule, Scope } from './policyTypes';

const sortedUnique = (xs: ReadonlyArray<string>): string[] => Array.from(new Set(xs)).sort();

export function matrixAxes(
  rules: ReadonlyArray<PolicyRule>,
): { resources: string[]; actions: string[] } {
  return {
    resources: sortedUnique(rules.map((r) => r.resource)),
    actions: sortedUnique(rules.map((r) => r.action)),
  };
}

/// The roles the Role select offers: every active role Class AND every
/// role a rule carries (backlog 47b7f120). Four of the seven live role
/// Classes held no rules, engineering-agent among them, and could not be
/// picked — so "what may this actor do" had no readable answer when the
/// honest answer is "nothing here".
export function roleList(
  rules: ReadonlyArray<PolicyRule>,
  roleClassCodes: ReadonlyArray<string>,
): string[] {
  return sortedUnique([...roleClassCodes, ...rules.map((r) => r.role)]);
}

export function rulesOf(rules: ReadonlyArray<PolicyRule>, role: string): PolicyRule[] {
  return rules.filter((r) => r.role === role);
}

/// The role the page opens on (backlog 656dc0de): the signed-in session's
/// own role when it holds rules here, else the first role listed. It was
/// the literal 'ceo', a brewery-demo role with no rules on this instance,
/// so every open painted a false-empty matrix for a role that is not here.
export function defaultRole(
  roles: ReadonlyArray<string>,
  rules: ReadonlyArray<PolicyRule>,
  sessionRole: string | null,
): string | null {
  if (sessionRole !== null && rules.some((r) => r.role === sessionRole)) return sessionRole;
  return roles[0] ?? null;
}

/// The header's one line. A pending read is not a zero (backlog 5a7ab4b1)
/// and a failed one claims no count (sweep c3e4edcc).
export function policySubtitle(
  read: ReadState,
  rules: ReadonlyArray<PolicyRule>,
  roles: ReadonlyArray<string>,
): string {
  if (read.kind === 'loading') return 'Loading rules…';
  if (read.kind === 'failed') return 'Rule count unknown — the policy read failed';
  const holding = new Set(rules.map((r) => r.role)).size;
  return `${rules.length} active rules · ${holding} of ${roles.length} roles hold rules`;
}

export function scopeForDisplay(s: Scope): string {
  if (typeof s === 'string') return s;
  return `department:${s.department}`;
}

export type ScopeOption = Readonly<{ value: string; label: string }>;

// Scope shapes that are not a department are core policy primitives, part
// of the policy model rather than tenant data, so they stay listed here.
const CORE_SCOPES: ReadonlyArray<ScopeOption> = [
  { value: 'none', label: 'None — denied' },
  { value: 'self', label: 'Self — own rows' },
  { value: 'team', label: 'Team — self + direct reports' },
  { value: 'territory', label: 'Territory — account team' },
  { value: 'all', label: 'All — org-wide' },
];

/// The flyout's Scope options: the core primitives, one per department in
/// the departments registry (GET /api/departments — the `(employee,
/// department)` Classes it once read are retired, backlog c87e3d6d), and
/// ALWAYS the rule's current scope (backlog ba8411da) — a
/// department the registry no longer lists, or one it could not be read
/// for, used to open the select with nothing selected, which a Save then
/// silently rewrote.
export function scopeOptions(
  departmentCodes: ReadonlyArray<string>,
  current: string,
): ScopeOption[] {
  const listed: ScopeOption[] = [
    ...CORE_SCOPES,
    ...departmentCodes.map((code) => ({ value: `department:${code}`, label: `Department: ${code}` })),
  ];
  if (listed.some((o) => o.value === current)) return listed;
  const code = current.startsWith('department:') ? current.slice('department:'.length) : current;
  return [...listed, { value: current, label: `Department: ${code} (current; not in the departments registry)` }];
}
