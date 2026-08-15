import { describe, it, expect } from 'bun:test';

import { isMachineActor, sharedExecutors } from './liveFlow';

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

describe('sharedExecutors', () => {
  const hit = (actor: string, jobKind: string) =>
    ({
      actor,
      jobKind,
      jobId: 'j',
      jobTitle: 't',
      slug: 's',
      phase: 'done',
      stepKind: 'task',
      machine: false,
      at: 0,
    }) as const;

  it('surfaces only executors working in more than one workflow', () => {
    // The point of the view: the dispatcher spans two routes, so it is
    // a shared station; the sim actor is busy but confined to one.
    const out = sharedExecutors([
      hit('automation:dispatcher', 'pr-train'),
      hit('automation:dispatcher', 'user-feedback'),
      hit('emp-sim', 'brewery-batch'),
      hit('emp-sim', 'brewery-batch'),
      hit('emp-sim', 'brewery-batch'),
    ]);
    expect(out.map((e) => e.actor)).toEqual(['automation:dispatcher']);
    expect(out[0]?.kinds).toEqual(['pr-train', 'user-feedback']);
  });

  it('orders by span before volume', () => {
    // A three-route executor outranks a louder two-route one: the page
    // answers "where does work move", and span is that answer.
    const out = sharedExecutors([
      hit('claude:opus-5', 'a'),
      hit('claude:opus-5', 'b'),
      hit('claude:opus-5', 'c'),
      ...Array.from({ length: 20 }, () => hit('emp-1', 'a')),
      ...Array.from({ length: 20 }, () => hit('emp-1', 'b')),
    ]);
    expect(out.map((e) => e.actor)).toEqual(['claude:opus-5', 'emp-1']);
    expect(out[0]?.machine).toBe(true);
    expect(out[1]?.machine).toBe(false);
  });

  it('ignores unattributed frames rather than inventing a station', () => {
    expect(sharedExecutors([hit('', 'a'), hit('', 'b')])).toEqual([]);
  });
});
