import { describe, it, expect } from 'bun:test';

import { formatActor } from './actor';

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

  it('reads a legacy null/empty actor as the platform automation', () => {
    expect(formatActor(null)).toBe('Platform');
    expect(formatActor(undefined)).toBe('Platform');
    expect(formatActor('')).toBe('Platform');
  });
});
