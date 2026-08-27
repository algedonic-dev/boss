import { describe, it, expect } from 'bun:test';

import { formatActor, isHumanActor } from './actor';

describe('isHumanActor', () => {
  it('is true only for an employee id', () => {
    expect(isHumanActor('emp-032')).toBe(true);
    expect(isHumanActor('emp-bootstrap-admin')).toBe(true);
    expect(isHumanActor('emp-aa-001')).toBe(true);
  });

  it('is false for every machine CPU — automation and agent alike', () => {
    expect(isHumanActor('automation:dispatcher')).toBe(false);
    expect(isHumanActor('automation:rule:bill-approve')).toBe(false);
    expect(isHumanActor('rule:bill-approve')).toBe(false);
    // The whole point: an agent is a CPU, never staff.
    expect(isHumanActor('claude:opus-5')).toBe(false);
    expect(isHumanActor('claude:fable')).toBe(false);
  });

  it('is false for an absent actor — Platform is not a person', () => {
    expect(isHumanActor(null)).toBe(false);
    expect(isHumanActor(undefined)).toBe(false);
    expect(isHumanActor('')).toBe(false);
  });
});

describe('formatActor', () => {
  it('renders a dispatch rule readably', () => {
    expect(formatActor('automation:rule:bill-approve')).toBe('Rule · bill-approve');
  });

  it('maps known automations to friendly names', () => {
    expect(formatActor('automation:dispatcher')).toBe('Dispatcher');
    expect(formatActor('automation:sim')).toBe('Simulator');
    expect(formatActor('automation:platform')).toBe('Platform');
  });

  it('title-cases a service automation slug', () => {
    expect(formatActor('automation:account-provisioning')).toBe('Account Provisioning');
  });

  it('resolves a human via empNames, else shows the bare id', () => {
    expect(formatActor('emp-032', new Map([['emp-032', 'Dana Ng']]))).toBe('Dana Ng');
    expect(formatActor('emp-099')).toBe('emp-099');
  });

  it('renders an agent as its mode and model, never as a missing employee', () => {
    expect(formatActor('claude:opus-5')).toBe('Claude · opus-5');
    expect(formatActor('claude:fable')).toBe('Claude · fable');
    // The model half stays whole — dots and further colons are the
    // vendor's model string, not structure we get to reinterpret.
    expect(formatActor('claude:claude-opus-5.1')).toBe('Claude · claude-opus-5.1');
    expect(formatActor('claude:opus-5:1m')).toBe('Claude · opus-5:1m');
  });

  it('never mistakes an agent id for an employee lookup', () => {
    // An empNames map that happens to be present must not turn an
    // agent into "unknown employee claude:fable".
    const names = new Map([['emp-032', 'Dana Ng']]);
    expect(formatActor('claude:fable', names)).toBe('Claude · fable');
  });

  it('keeps `automation:` winning over the agent split', () => {
    // Same branch order as ActorId::from_str: a dispatch rule is not
    // an `automation`-mode agent.
    expect(formatActor('automation:rule:bill-approve')).toBe('Rule · bill-approve');
  });

  it('reads a legacy null/empty actor as the platform automation', () => {
    expect(formatActor(null)).toBe('Platform');
    expect(formatActor(undefined)).toBe('Platform');
    expect(formatActor('')).toBe('Platform');
  });
});
