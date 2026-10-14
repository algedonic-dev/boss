import { describe, expect, it } from 'bun:test';
import { gateActivity, parseGateRuns } from './crew';

const run = '342b709d-4c41-4e4f-8033-b8dadc1b1fb7';
const record = (id: string, metadata: Record<string, unknown>, status = 'open') => ({ id, status, metadata });
describe('gate activity belongs to the exact dispatched run', () => {
  it('uses native launch and completion timestamps', () => {
    const gates = parseGateRuns({ data: [record('gate-one', { agent_run: run, opened_at: '2026-10-13T20:00:00Z', launched_at: '2026-10-13T20:01:00Z' })] });
    expect(gateActivity(run, gates)).toEqual({ gateId: 'gate-one', label: 'Running', at: '2026-10-13T20:01:00Z' });
    const complete = parseGateRuns({ data: [record('gate-one', { agent_run: run, opened_at: '2026-10-13T20:00:00Z', closed_at: '2026-10-13T20:10:00Z', outcome: 'completed' }, 'closed')] });
    expect(gateActivity(run, complete)?.label).toBe('Passed');
    expect(gateActivity(run, complete)?.at).toBe('2026-10-13T20:10:00Z');
  });
  it('never associates an alien run by branch or title', () => {
    const gates = parseGateRuns({ data: [record('alien', { agent_run: '3673af1d-3ef7-4084-91f5-3ba0823678e3', branch: 'same-branch', opened_at: '2026-10-13T20:00:00Z', outcome: 'completed' }, 'closed')] });
    expect(gateActivity(run, gates)).toBeNull();
  });
  it('shows the latest attempt instead of an earlier pass', () => {
    const gates = parseGateRuns({ data: [
      record('earlier', { agent_run: run, opened_at: '2026-10-13T20:00:00Z', outcome: 'completed' }, 'closed'),
      record('later', { agent_run: run, opened_at: '2026-10-13T21:00:00Z', closed_at: '2026-10-13T21:01:00Z', outcome: 'failed' }, 'closed'),
    ] });
    expect(gateActivity(run, gates)).toEqual({ gateId: 'later', label: 'Failed', at: '2026-10-13T21:01:00Z' });
  });
  it('does not call a pending, lost or undated gate running', () => {
    for (const metadata of [
      { agent_run: run, opened_at: '2026-10-13T20:00:00Z' },
      { agent_run: run, opened_at: '2026-10-13T20:00:00Z', outcome: 'observer-unknown' },
      { agent_run: run, launched_at: 'invalid' },
    ]) expect(gateActivity(run, parseGateRuns({ data: [record('unknown', metadata)] }))?.label).toBe('Unknown');
  });
});
