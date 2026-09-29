import { describe, expect, test } from 'bun:test';
import {
  ageText,
  failureText,
  fmtDur,
  inFlightByKind,
  openingKind,
  optionLabel,
  pickerOrder,
  searchWithKind,
  staleLine,
} from './fleet';

const wait = (job_id: string, job_kind: string) => ({
  job_id,
  job_kind,
  job_title: 't',
  step_title: 's',
  status: 'ready',
  waiting_days: 1,
});

describe('the picker (f949d19a)', () => {
  test('counts in-flight steps per kind off the queue-age lens, one per row', () => {
    const counts = inFlightByKind({
      data: [wait('j1', 'backlog-item'), wait('j1', 'backlog-item'), wait('j2', 'backlog-item'), wait('j3', 'agent-run')],
      total: 4,
    });
    expect([...counts.entries()]).toEqual([
      ['backlog-item', 3],
      ['agent-run', 1],
    ]);
  });

  test('a body that is not the envelope is refused, not counted as nothing', () => {
    expect(() => inFlightByKind([])).toThrow('/api/jobs/queue-age');
  });

  test('orders by in-flight count, ties and idle kinds by name, idle last but listed', () => {
    const counts = new Map([
      ['backlog-item', 363],
      ['agent-run', 10],
      ['design-doc', 10],
    ]);
    expect(pickerOrder(['agent-run', 'zeta', 'backlog-item', 'alpha', 'design-doc'], counts)).toEqual([
      'backlog-item',
      'agent-run',
      'design-doc',
      'alpha',
      'zeta',
    ]);
  });

  test('without counts the order is alphabetical and the labels carry no number', () => {
    expect(pickerOrder(['b', 'a'], null)).toEqual(['a', 'b']);
    expect(optionLabel('a', null)).toBe('a');
  });

  test('each label carries its count, an idle kind its zero', () => {
    const counts = new Map([['backlog-item', 363]]);
    expect(optionLabel('backlog-item', counts)).toBe('backlog-item — 363 in flight');
    expect(optionLabel('zeta', counts)).toBe('zeta — 0 in flight');
  });

  test('opens on the asked kind when the registry has it, else the head of the picker', () => {
    expect(openingKind(['backlog-item', 'agent-run'], 'agent-run')).toBe('agent-run');
    expect(openingKind(['backlog-item', 'agent-run'], 'wholesale-keg-order')).toBe('backlog-item');
    expect(openingKind(['backlog-item'], null)).toBe('backlog-item');
    expect(openingKind([], null)).toBeNull();
  });
});

describe('the address (c7c5c1de)', () => {
  test('sets ?kind= and keeps every other parameter', () => {
    expect(searchWithKind('', 'backlog-item')).toBe('?kind=backlog-item');
    expect(searchWithKind('?kind=agent-run&x=1', 'design-doc')).toBe('?kind=design-doc&x=1');
  });
});

describe('a failed read says what failed (ffc3444f)', () => {
  test('names the read, the status, and the server’s own sentence', () => {
    expect(failureText('stage-durations backlog-item', 503, 'stage durations need a postgres-backed views service')).toBe(
      'stage-durations backlog-item: HTTP 503 — stage durations need a postgres-backed views service',
    );
  });

  test('takes a JSON body’s error string, and says only the status for an empty body', () => {
    expect(failureText('fleet x', 502, '{"error":"backend down"}')).toBe('fleet x: HTTP 502 — backend down');
    expect(failureText('fleet x', 500, '  ')).toBe('fleet x: HTTP 500');
  });
});

describe('a failed poll looks stale (0fb86007)', () => {
  test('names each failed read and the age of its last good answer', () => {
    const line = staleLine({ fleet: 'fleet x: HTTP 502' }, { fleet: 1_000, items: 1_000, flow: 1_000 }, 21_000);
    expect(line).toBe(
      'Not refreshed — the fleet read failed (fleet x: HTTP 502), so its numbers are its last good answer, 20 s old. This clears on the next good read.',
    );
  });

  test('is absent when every read answered', () => {
    expect(staleLine({}, { fleet: 1, items: 1, flow: 1 }, 5)).toBeNull();
  });

  test('stage durations read in seconds, then minutes, then hours (08650f01 kept it)', () => {
    expect(fmtDur(45)).toBe('45s');
    expect(fmtDur(600)).toBe('10m');
    expect(fmtDur(3 * 3600)).toBe('3.0h');
  });

  test('ages read in seconds, then minutes, then hours', () => {
    expect(ageText(10_000)).toBe('10 s');
    expect(ageText(5 * 60_000)).toBe('5 min');
    expect(ageText(3 * 3_600_000)).toBe('3.0 h');
  });
});
