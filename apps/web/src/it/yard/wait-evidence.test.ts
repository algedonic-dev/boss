import { expect, test } from 'bun:test';
import { parseRegions } from './regions';

const wait = {
  packet: 'packet-id', owner: 'agent-reviewer', condition_owner: 'world',
  title: 'Review the change', condition: 'independent review at the recorded head',
  next_check_at: '2026-10-15T14:30:00.123456789Z',
  evidence: { kind: 'review', run: 'run-id', head: 'a'.repeat(40), workflow_version: 7,
    started_at: '2026-10-15T12:30:00Z' },
};

test('the region reader conserves the complete qualified wait', () => {
  const regions = parseRegions({ regions: [{ name: 'garage', state: 'attention', trend: {}, waits: [wait] }] });
  expect(regions.regions[0]).toMatchObject({ waits: [wait] });
});

test('a malformed present wait is refused instead of silently discarded', () => {
  expect(() => parseRegions({ regions: [{ name: 'garage', state: 'attention', trend: {}, waits: [{ ...wait, owner: null }] }] })).toThrow();
});
