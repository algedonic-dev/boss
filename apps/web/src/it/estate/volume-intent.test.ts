import { expect, test } from 'bun:test';
import { capacityIntentLine, parseCapacityIntent, parseVolumeReadings } from './estate';

test('the estate retains the comparator capacity intent and its assignment, separate from filesystem figures', () => {
  const assignment = { packet: '32e7cf87-7d8e-4764-94a3-3b0cfa8548c4', step: 'a40668dd-27d0-467b-822c-10f5d28b657a', question: 'Q1', decided_by: 'emp-david', decided_at: '2026-10-02T23:53:23.553088Z' };
  const verdict = { verdict: 'match', desired_bytes: 32212254720, requested_bytes: 32212254720, assignment, reason: null };
  const readings = parseVolumeReadings([{ payload: { scope: 'instance-volumes', observed_at: '2026-10-03T01:00:00Z', observer: 'fixture', volumes: [{ id: 'boss/pgdata-postgres-0', requested: '30Gi', capacity_bytes: 31526436864, capacity_intent: verdict }] } }]);
  expect(readings[0]?.volumes[0]).toMatchObject({ requested: '30Gi', capacity_bytes: 31526436864, capacity_intent: verdict });
});

const assignment = { packet: '32e7cf87-7d8e-4764-94a3-3b0cfa8548c4', step: 'a40668dd-27d0-467b-822c-10f5d28b657a', question: 'Q1', decided_by: 'emp-david', decided_at: '2026-10-02T23:53:23.553088Z' };
const matching = { verdict: 'match', desired_bytes: 32212254720, requested_bytes: 32212254720, assignment, reason: null };

test('recorded drift and exact assignment survive, while hollow or contradictory success stays unknown', () => {
  const drift = { ...matching, verdict: 'drift' as const, requested_bytes: 21474836480 };
  expect(parseCapacityIntent(drift)).toEqual(drift);
  for (const raw of [undefined, null, [], { ...matching, assignment: null }, { ...matching, requested_bytes: null }, { ...matching, requested_bytes: 21474836480 }, { ...drift, requested_bytes: 32212254720 }, { ...matching, desired_bytes: Number.NaN }, { ...matching, requested_bytes: 9007199254740992 }, { ...matching, assignment: { ...assignment, decided_by: '' } }]) {
    expect(parseCapacityIntent(raw).verdict).toBe('unknown');
  }
});

test('legacy readings and named missing intent stay unknown alongside independent filesystem data', () => {
  const [reading] = parseVolumeReadings([{ payload: { scope: 'instance-volumes', observed_at: '2026-10-03T01:00:00Z', volumes: [{ id: 'control/data', requested: '20Gi', tight: false, free_bytes: 100 }] } }]);
  const row = reading!.volumes[0]!;
  expect(row.tight).toBe(false);
  expect(capacityIntentLine(row)).toContain('Capacity intent unknown');
  expect(capacityIntentLine(row)).toContain('requested 20Gi');
  const unknown = { ...matching, verdict: 'unknown' as const, requested_bytes: null, reason: 'declared claim is not observed' };
  expect(parseCapacityIntent(unknown)).toEqual(unknown);
  expect(parseCapacityIntent({ ...unknown, assignment: null }).desired_bytes).toBeNull();
});
