// Who the finance page hides its write buttons from (backlog 432f0eb4).
//
// It hid them for `role === 'auditor'`, a role no seed, fixture or core
// role carries — the platform's auditor is `audit-readonly` — so it hid
// them from nobody, the same phantom the ledger's server-side
// `reject_if_auditor` refused. The answer is now the read-only floor:
// the session the gateway classified read-only, or a signed-in row whose
// role is on the floor (READ_ONLY_GUEST_ROLES, held equal to the
// server's by a pin).

import { describe, expect, test } from 'bun:test';
import { guestEmployee, type Employee } from '@boss/web-kit/session/classify';
import { financeOffersWrite, financeReadOnly } from './readOnly';

const controller: Employee = guestEmployee('emp-ctl', 'controller');

describe('financeReadOnly', () => {
  test('the read-only guest session hides the writes', () => {
    expect(
      financeReadOnly({ value: { kind: 'ready', user: guestEmployee('guest') }, readonly: true }),
    ).toBe(true);
  });

  test('a signed-in row on the read-only floor hides the writes', () => {
    for (const role of ['audit-readonly', 'visitor']) {
      const user = { ...controller, role };
      expect(financeReadOnly({ value: { kind: 'ready', user }, readonly: false })).toBe(true);
    }
  });

  test('a finance lead sees the writes', () => {
    expect(
      financeReadOnly({ value: { kind: 'ready', user: controller }, readonly: false }),
    ).toBe(false);
  });

  test('the phantom role string decides nothing', () => {
    // No one carries it; it is not on the floor, so it is judged like
    // any other role — by the server's policy answer, not by this page.
    const user = { ...controller, role: 'auditor' };
    expect(financeReadOnly({ value: { kind: 'ready', user }, readonly: false })).toBe(false);
  });

  test('a session still loading hides nothing it has not judged', () => {
    expect(financeReadOnly({ value: { kind: 'loading' }, readonly: false })).toBe(false);
  });
});

// The buttons ask policy too (backlog 9dad102c): a write is offered only
// when the floor above lets it AND policy answered Allow for the session
// user (@boss/web-kit/session/can). Still loading, a Deny and a failed
// check all arrive here as false.
describe('financeOffersWrite', () => {
  const lead = { value: { kind: 'ready' as const, user: controller }, readonly: false };

  test('a finance lead policy allows is offered the write', () => {
    expect(financeOffersWrite(lead, true)).toBe(true);
  });

  test('a finance lead policy has not allowed — denied, unanswered or failed — is not', () => {
    expect(financeOffersWrite(lead, false)).toBe(false);
  });

  test('the read-only floor withholds the write whatever policy said', () => {
    const guest = { value: { kind: 'ready' as const, user: guestEmployee('guest') }, readonly: true };
    expect(financeOffersWrite(guest, true)).toBe(false);
    const auditor = { value: { kind: 'ready' as const, user: { ...controller, role: 'audit-readonly' } }, readonly: false };
    expect(financeOffersWrite(auditor, true)).toBe(false);
  });
});
