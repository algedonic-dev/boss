import { expect, test } from 'bun:test';
import { parseRegions } from './regions';
import { parseBorders } from './borders';
import { sectionCells, stationCells } from './panel';

const wait = {
  packet: 'packet-id', owner: 'agent-reviewer', condition_owner: 'world',
  title: 'Review the change', condition: 'independent review at the recorded head',
  next_check_at: '2026-10-15T14:30:00.123456789Z',
  evidence: { kind: 'review', run: 'run-id', head: 'a'.repeat(40), workflow_version: 7,
    started_at: '2026-10-15T12:30:00Z' },
};

test('region and border readers conserve the same complete qualified wait', () => {
  const regions = parseRegions({ regions: [{ name: 'garage', state: 'attention', trend: {}, waits: [wait] }] });
  expect(regions.regions[0]).toMatchObject({ waits: [wait] });
  const borders = parseBorders({ borders: [{ from: 'dock', to: 'track', state: 'attention', rate: {}, machine: {},
    holds: [{ what: 'change', why: 'held for review', wait }] }] });
  expect(borders.borders[0]?.holds[0]).toEqual({ what: 'change', why: 'held for review', wait });
});

test('a malformed present wait is refused instead of silently discarded', () => {
  expect(() => parseRegions({ regions: [{ name: 'garage', state: 'attention', trend: {}, waits: [{ ...wait, owner: null }] }] })).toThrow();
  expect(() => parseBorders({ borders: [{ from: 'dock', to: 'track', state: 'attention', rate: {}, machine: {},
    holds: [{ what: 'change', why: 'held', wait: { ...wait, evidence: null } }] }] })).toThrow();
});

test('station readings show the wait owner and check beside the server verdict', () => {
  for (const state of ['attention', 'troubled']) {
    const region = parseRegions({ regions: [{ name: 'garage', state, trend: {}, waits: [wait] }] }).regions[0];
    if (!region) throw new Error('fixture region missing');
    const cells = stationCells(region, null, '2026-10-15T13:00:00Z');
    const waiting = cells.find(cell => cell.field === 'waiting')?.lines.join('\n') ?? '';
    expect(waiting).toContain(wait.owner);
    expect(waiting).toContain(wait.condition);
    expect(waiting).toContain(wait.next_check_at);
    expect(cells.find(cell => cell.field === 'verdict')?.lines[0]).toContain(state);
  }
});

test('crossing readings show the same wait evidence instead of only hold prose', () => {
  const border = parseBorders({ borders: [{ from: 'dock', to: 'track', state: 'attention', rate: {}, machine: {},
    holds: [{ what: 'change', why: 'held for review', wait }] }] }).borders[0] ?? null;
  const waiting = sectionCells(border, '2026-10-15T13:00:00Z').find(cell => cell.field === 'waiting')?.lines.join('\n') ?? '';
  expect(waiting).toContain('held for review');
  expect(waiting).toContain(wait.owner);
  expect(waiting).toContain(wait.next_check_at);
});
