import { describe, expect, test } from 'bun:test';

import {
  authorship,
  emittedKinds,
  nextDue,
  parseDispatcherSchedule,
  ruleWhy,
  type ScheduleRead,
} from './ruleSummary';

// The editor's read-only summary (backlog 0034d5ef, page-audit bd5018e1
// gap 3): the page where Retire is decided showed none of a rule's
// provenance or activity. Why/source and last-fired/failing reuse the
// list's own helpers (ruleProvenance, ruleActivity); what is new is the
// next due of a schedule rule (GET /api/dispatcher/schedule, design
// ea906603) and the kinds a rule's handlers emit.

const NOW = '2026-09-27T12:00:00Z';

const schedule = (over: Partial<ScheduleRead> = {}): ScheduleRead => ({
  now: NOW,
  schedule: [
    { name: 'cadence-silence-sweep-daily', next_due: '2026-09-27T15:00:00Z', next_due_why: null },
    { name: 'monthly-close', next_due: null, next_due_why: 'business calendar `us-banking` could not be read (x)' },
  ],
  schedule_error: null,
  withheld: false,
  ...over,
});

describe('parseDispatcherSchedule', () => {
  test('the served rows, each with its next due and the reason when there is none', () => {
    const read = parseDispatcherSchedule({
      now: NOW,
      simulated: false,
      retention_days: 30,
      schedule: [
        {
          name: 'cadence-silence-sweep-daily',
          version: 5,
          cadence: 'daily',
          anchor_date: '2026-09-01',
          business_calendar: null,
          when: null,
          last_fired: null,
          last_fired_why: 'no firing recorded',
          next_due: '2026-09-27T15:00:00Z',
          next_due_why: null,
        },
      ],
      schedule_error: null,
    });
    expect(read).toEqual({
      now: NOW,
      schedule: [{ name: 'cadence-silence-sweep-daily', next_due: '2026-09-27T15:00:00Z', next_due_why: null }],
      schedule_error: null,
      withheld: false,
    });
  });

  test('a withheld schedule is null beside its reason, never an empty list', () => {
    const read = parseDispatcherSchedule({ now: NOW, schedule: null, schedule_error: 'withheld: scope' });
    expect(read.schedule).toBeNull();
    expect(read.schedule_error).toBe('withheld: scope');
    // The flag is read off the wire, never off the reason (1805bac0).
    expect(read.withheld).toBe(false);
    expect(parseDispatcherSchedule({ now: NOW, schedule: null, schedule_error: 'x', withheld: true }).withheld).toBe(true);
  });

  test('a body without the schedule key is a wrong server, not an empty schedule', () => {
    expect(() => parseDispatcherSchedule([])).toThrow('dispatcher schedule');
    expect(() => parseDispatcherSchedule({ now: NOW })).toThrow('dispatcher schedule');
  });
});

describe('nextDue', () => {
  test('a due firing reads as its distance and names its instant', () => {
    expect(nextDue('cadence-silence-sweep-daily', schedule())).toEqual({
      text: 'in 3h',
      why: 'next due 2026-09-27T15:00:00Z',
    });
  });

  test('a firing the runner cannot place is unknown, with the runner’s reason', () => {
    expect(nextDue('monthly-close', schedule())).toEqual({
      text: 'unknown',
      why: 'business calendar `us-banking` could not be read (x)',
    });
  });

  test('a withheld schedule is unknown with its reason, never "not scheduled"', () => {
    expect(nextDue('cadence-silence-sweep-daily', schedule({ schedule: null, schedule_error: 'withheld' }))).toEqual({
      text: 'unknown',
      why: 'withheld',
    });
  });

  // Backlog bd506215: the server's refusal by scope (d0058c92) is said
  // as the scope; a policy service that could not answer stays unknown.
  // Since 1805bac0 the dispatcher's `withheld` flag says which.
  test('a schedule withheld by policy scope says so, never unknown', () => {
    const why = "this caller's policy scope does not read every packet, and the schedule is not scoped by packet, so it is withheld from it";
    expect(nextDue('cadence-silence-sweep-daily', schedule({ schedule: null, schedule_error: why, withheld: true }))).toEqual({
      text: 'not in your policy scope',
      why,
    });
    const failed = 'policy check failed, so the schedule is withheld until policy can answer';
    expect(nextDue('cadence-silence-sweep-daily', schedule({ schedule: null, schedule_error: failed })).text).toBe('unknown');
    // The refusal's words without the flag decide nothing (CLAUDE.md 9a).
    expect(nextDue('cadence-silence-sweep-daily', schedule({ schedule: null, schedule_error: why })).text).toBe('unknown');
  });

  test('a rule the schedule does not list says so', () => {
    expect(nextDue('not-there', schedule()).text).toBe('not on the dispatcher’s schedule');
  });
});

describe('ruleWhy', () => {
  const READ = { dir: '/opt/boss/infra/dispatcher/rules', rules: 3, error: null };
  const enforced = { why: null, source: 'product', authored: true };

  test('the file’s why, trimmed', () => {
    expect(ruleWhy({ ...enforced, why: '  A TIMER.\n' }, READ)).toBe('A TIMER.');
  });

  test('a rule with no active version is not enforced, so the registry says nothing of it', () => {
    expect(ruleWhy(null, READ)).toBe('not enforced — no version of this rule is active');
  });

  test('a tenant rule’s why lives in its seeds, which the dispatcher does not read', () => {
    expect(ruleWhy({ why: null, source: 'tenant:algedonic', authored: false }, READ)).toBe(
      'kept in the tenant’s seeds/rules.toml, which this dispatcher does not read',
    );
  });

  test('an unread authored registry is unknown, never "no why"', () => {
    expect(ruleWhy(enforced, { ...READ, error: 'BOSS_DISPATCHER_RULES is not set' })).toBe(
      'unknown — the authored registry could not be read',
    );
    expect(ruleWhy(enforced, null)).toBe('unknown — the authored registry could not be read');
  });

  test('a read registry with no why for the rule says so', () => {
    expect(ruleWhy({ ...enforced, authored: false }, READ)).toBe('no file records a why for this rule');
  });
});

describe('authorship', () => {
  const v = { status: 'active' as const, created_by: 'agent-claude', published_by: null, retired_by: null };

  test('each act the row’s status implies, null as unrecorded', () => {
    expect(authorship(v)).toBe('drafted by agent-claude · published by (unrecorded)');
    expect(authorship({ ...v, status: 'retired', retired_by: 'emp-david' })).toBe(
      'drafted by agent-claude · published by (unrecorded) · retired by emp-david',
    );
  });

  test('a draft was never published, so it claims no publisher, recorded or not', () => {
    expect(authorship({ ...v, status: 'draft', created_by: null })).toBe('drafted by (unrecorded)');
  });
});

describe('emittedKinds', () => {
  const emits = { 'jobs.auto-park': ['jobs.car.filed', 'jobs.car.parked'], 'sensor.poll': [], 'jobs.spawn': ['jobs.job.created'] };

  test('the kinds its handlers emit, each once, in do order', () => {
    const rule = { do: [{ handler: 'jobs.auto-park', args: {} }, { handler: 'jobs.spawn', args: {} }, { handler: 'jobs.auto-park', args: {} }] };
    expect(emittedKinds(rule, emits)).toBe('jobs.car.filed, jobs.car.parked, jobs.job.created');
  });

  test('a rule whose handlers are all sinks emits nothing, and says so', () => {
    expect(emittedKinds({ do: [{ handler: 'sensor.poll', args: {} }] }, emits)).toBe('nothing — its handler is a sink');
  });

  test('a handler this dispatcher does not carry is named, not dropped', () => {
    expect(emittedKinds({ do: [{ handler: 'jobs.spwan', args: {} }] }, emits)).toBe(
      'unknown — jobs.spwan is not in this dispatcher’s handler roster',
    );
  });
});
