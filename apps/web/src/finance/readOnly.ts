// Whether the finance page offers its write buttons (backlog 432f0eb4).
//
// financeReadOnly is not a policy answer; it is the read-only floor the
// rest of the app gates on — the session the gateway classified
// read-only (what WriteGate reads), or a signed-in
// row whose role is on the floor, READ_ONLY_GUEST_ROLES, held equal to
// `boss_core::roles::READ_ONLY_FLOOR_ROLES` by a pin. It replaced a
// compare against the role string 'auditor', which no one carries.

import { READ_ONLY_GUEST_ROLES, type SessionEnvelope } from '@boss/web-kit/session/classify';

export function financeReadOnly(env: Pick<SessionEnvelope, 'value' | 'readonly'>): boolean {
  if (env.readonly) return true;
  return env.value.kind === 'ready' && READ_ONLY_GUEST_ROLES.includes(env.value.user.role);
}

/// Whether the page offers one write: the floor above lets it AND policy
/// answered Allow for the session user on that write's own (action,
/// resource) — `permission(...)` from @boss/web-kit/session/permission.
/// The floor alone let a tenant grant it does not know show buttons that
/// 403, and hid them from a writer it did not know (backlog 9dad102c).
/// `allowed` is false while the check is in flight and when it failed,
/// so the button is hidden until an Allow arrives; the server's 403 stays
/// the authority on every write either way.
export function financeOffersWrite(
  env: Pick<SessionEnvelope, 'value' | 'readonly'>,
  allowed: boolean,
): boolean {
  return allowed && !financeReadOnly(env);
}
