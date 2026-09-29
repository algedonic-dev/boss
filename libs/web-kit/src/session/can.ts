// can(action, resource) — may this session, as itself, do `action` on
// `resource`? (backlog 9dad102c, 2026-09-28)
//
// The finance page hid its write buttons by the read-only role list,
// which is not a policy answer: a tenant grant or deny override the list
// does not know showed buttons that 403, or hid ones that would work.
// This asks policy instead, through the gateway, with the session: POST
// /api/policy/check {user, action, resource}, where `user` is the
// gateway's own `policy_user` (./classify probePolicyUser) — the only
// pair boss-policy's self-arm answers for a signed caller.
//
// True ONLY on an Allow. A Deny, a refusal, a body that is not a
// decision, a network error, or no session user are all false, and so is
// the wait: a write hidden while loading and on error, never offered on
// a guess. The server's 403 stays the authority either way.
//
// Pure and rune-free so bun can test it; ./permission.svelte.ts is the
// reactive wrapper a component uses.

import type { PolicyUser } from './classify';

export const POLICY_CHECK = '/api/policy/check';

export type CanFetch = (
  url: string,
  init: RequestInit,
) => Promise<Pick<Response, 'ok' | 'json'>>;

export type Can = (user: PolicyUser | null, action: string, resource: string) => Promise<boolean>;

async function ask(fetchFn: CanFetch, user: PolicyUser, action: string, resource: string): Promise<boolean> {
  try {
    const resp = await fetchFn(POLICY_CHECK, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ user: { id: user.id, role: user.role }, action, resource }),
    });
    if (!resp.ok) return false;
    const body = (await resp.json()) as unknown;
    return typeof body === 'object' && body !== null && (body as { decision?: unknown }).decision === 'allow';
  } catch {
    return false;
  }
}

/// A `can` with its own answers: ONE check per (user, action, resource)
/// for as long as it lives — the page load, for the module's `can`. An
/// error is kept too; a reload asks again.
export function makeCan(fetchFn: CanFetch = (url, init) => fetch(url, init)): Can {
  const asked = new Map<string, Promise<boolean>>();
  return (user, action, resource) => {
    if (!user) return Promise.resolve(false);
    const key = JSON.stringify([user.id, user.role, action, resource]);
    const known = asked.get(key);
    if (known) return known;
    const answer = ask(fetchFn, user, action, resource);
    asked.set(key, answer);
    return answer;
  };
}

export const can: Can = makeCan();
