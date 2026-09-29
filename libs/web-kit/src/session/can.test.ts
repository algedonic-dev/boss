// can(action, resource) — the web asking policy about ITSELF (backlog
// 9dad102c). Each test hands the helper an in-memory fetch, so what is
// pinned is the request it sends and what it makes of each answer:
// true only on an Allow, false on a Deny, a refusal, a malformed body or
// a network error — fail closed, the server's 403 staying the authority.

import { describe, expect, test } from 'bun:test';
import { makeCan, POLICY_CHECK, type CanFetch } from './can';
import { probePolicyUser, type PolicyUser } from './classify';

const DAVID: PolicyUser = { id: 'emp-001', role: 'platform-admin' };

type Sent = { url: string; init: RequestInit };

function answering(respond: () => Promise<{ ok: boolean; json: () => Promise<unknown> }>): {
  fetchFn: CanFetch;
  sent: Sent[];
} {
  const sent: Sent[] = [];
  const fetchFn: CanFetch = (url, init) => {
    sent.push({ url, init });
    return respond();
  };
  return { fetchFn, sent };
}

const body = (ok: boolean, value: unknown) => () =>
  Promise.resolve({ ok, json: () => Promise.resolve(value) });

describe('can', () => {
  test('an Allow is true, and the request names the session user, the action and the resource', async () => {
    const { fetchFn, sent } = answering(body(true, { decision: 'allow', scope: 'all' }));
    expect(await makeCan(fetchFn)(DAVID, 'create', 'ledger')).toBe(true);
    expect(sent).toHaveLength(1);
    expect(sent[0]!.url).toBe(POLICY_CHECK);
    expect(sent[0]!.init.method).toBe('POST');
    expect(JSON.parse(String(sent[0]!.init.body))).toEqual({
      user: { id: 'emp-001', role: 'platform-admin' },
      action: 'create',
      resource: 'ledger',
    });
  });

  test('a Deny is false', async () => {
    const { fetchFn } = answering(body(true, { decision: 'deny', reason: 'no rule' }));
    expect(await makeCan(fetchFn)(DAVID, 'close', 'ledger-period')).toBe(false);
  });

  test('a refusal, a body that is not a decision, and a network error are each false', async () => {
    const refused = answering(body(false, 'forbidden'));
    expect(await makeCan(refused.fetchFn)(DAVID, 'create', 'ledger')).toBe(false);
    const malformed = answering(body(true, []));
    expect(await makeCan(malformed.fetchFn)(DAVID, 'create', 'ledger')).toBe(false);
    const down = answering(() => Promise.reject(new Error('connection refused')));
    expect(await makeCan(down.fetchFn)(DAVID, 'create', 'ledger')).toBe(false);
  });

  test('no session user asks nothing and is false', async () => {
    const { fetchFn, sent } = answering(body(true, { decision: 'allow', scope: 'all' }));
    expect(await makeCan(fetchFn)(null, 'create', 'ledger')).toBe(false);
    expect(sent).toHaveLength(0);
  });

  test('one check per (action, resource) per page load, for the same user', async () => {
    const { fetchFn, sent } = answering(body(true, { decision: 'allow', scope: 'all' }));
    const can = makeCan(fetchFn);
    await Promise.all([can(DAVID, 'create', 'ledger'), can(DAVID, 'create', 'ledger')]);
    await can(DAVID, 'create', 'ledger');
    await can(DAVID, 'create', 'invoice');
    await can({ id: 'emp-002', role: 'controller' }, 'create', 'ledger');
    expect(sent).toHaveLength(3);
  });
});

// The identity the check is asked AS. It is the gateway's own
// `policy_user` off /api/session — the id and role it signs into
// x-boss-user on the very request — and never the people row, because
// policy's self-arm admits only that pair (the gateway pins the two
// equal: the_session_answer_names_the_identity_every_request_carries).
describe('probePolicyUser', () => {
  test('reads the gateway policy_user', () => {
    expect(probePolicyUser({
      username: 'david@algedonic.dev', employee_id: 'emp-001', role: 'platform-admin',
      policy_user: { id: 'emp-001', role: 'platform-admin' },
    })).toEqual(DAVID);
  });

  test('a probe without one, or with a blank field, names no one', () => {
    expect(probePolicyUser({ username: 'david@algedonic.dev', employee_id: 'emp-001', role: 'cto' })).toBeNull();
    expect(probePolicyUser({ policy_user: { id: '', role: 'cto' } })).toBeNull();
    expect(probePolicyUser({ policy_user: { id: 'emp-001', role: '' } })).toBeNull();
  });
});
