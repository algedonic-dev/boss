import { expect, test } from 'bun:test';
import { liveSteps, openAge } from './lifecycle';
import type { ListedStep } from './types';

const step = (id: string, status: ListedStep['status'], order: number): ListedStep => ({
  id, job_id: 'packet', kind: 'task', title: id, status, sort_order: order,
  assignee_id: null, blocked_by: [], completed_on: null, slim: true,
});

test('all live parallel steps are ordered without changing the listed input', () => {
  const steps = [step('later', 'ready', 3), step('done', 'completed', 0), step('active', 'active', 1), step('pending', 'pending', 2)];
  expect(liveSteps({ steps }).map(s => s.id)).toEqual(['active', 'later']);
  expect(steps.map(s => s.id)).toEqual(['later', 'done', 'active', 'pending']);
  expect(liveSteps({})).toEqual([]);
});

test('calendar age names legacy date precision and refuses invalid or future records', () => {
  expect(openAge({ status: 'open', opened_at: '2026-09-01T23:59:00-04:00', opened_on: '2026-09-01' }, '2026-10-03')).toBe('31 days');
  expect(openAge({ status: 'open', opened_on: '2026-09-01' }, '2026-10-03')).toBe('32 days (date only)');
  expect(openAge({ status: 'open', opened_on: '2026-10-02' }, '2026-10-03')).toBe('1 day (date only)');
  expect(openAge({ status: 'open', opened_on: '2026-02-30' }, '2026-10-03')).toBe('Unknown');
  expect(openAge({ status: 'open', opened_at: 'broken', opened_on: '2026-09-01' }, '2026-10-03')).toBe('Unknown');
  expect(openAge({ status: 'open', opened_on: '2026-10-04' }, '2026-10-03')).toBe('Opened in the future');
  expect(openAge({ status: 'closed', opened_on: '2026-09-01' }, '2026-10-03')).toBe('—');
});
