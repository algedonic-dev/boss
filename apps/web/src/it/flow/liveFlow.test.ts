import { describe, it, expect } from 'bun:test';

import { isMachineActor } from './liveFlow';

describe('isMachineActor', () => {
  it('counts named automations as the machine', () => {
    expect(isMachineActor('automation:dispatcher')).toBe(true);
    expect(isMachineActor('automation:rule:bill-approve')).toBe(true);
    expect(isMachineActor('rule:bill-approve')).toBe(true);
  });

  it('counts agent sessions as the machine, not as staff', () => {
    expect(isMachineActor('claude:opus-5')).toBe(true);
    expect(isMachineActor('claude:fable')).toBe(true);
  });

  it('leaves employees human', () => {
    expect(isMachineActor('emp-032')).toBe(false);
    expect(isMachineActor('emp-bootstrap-admin')).toBe(false);
    expect(isMachineActor('')).toBe(false);
  });
});
