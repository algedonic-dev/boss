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

test('standalone manifest drift is visible separately from assigned intent and an unmounted filesystem', () => {
  const recorded = {
    verdict: 'drift', namespace: 'boss', claim: 'boss-files',
    file: 'infra/cluster/manifests/boss.yaml', line: 245,
    declared_bytes: 21474836480, live_bytes: 42949672960,
  };
  const read = (comparison: unknown) => parseVolumeReadings([{ payload: {
    scope: 'instance-volumes', observed_at: '', volumes: [{
      id: 'boss/boss-files', requested: '40Gi', capacity_bytes: null, free_bytes: null,
      capacity_intent: matching, standalone_declaration_comparison: comparison,
    }],
  } }])[0]!.volumes[0]!;
  const row = read(recorded);
  expect(capacityIntentLine(row)).toContain('infra/cluster/manifests/boss.yaml:245');
  expect(capacityIntentLine(row)).toContain('tree write-back owed');
  expect(capacityIntentLine(row)).toContain('42949672960');
  expect(row.capacity_bytes).toBeNull();
  const covered = read({ ...recorded, verdict: 'covered', declared_bytes: 64424509440 });
  expect(covered.standalone_declaration?.verdict).toBe('covered');
  expect(capacityIntentLine(covered)).not.toContain('tree write-back owed');
  for (const malformed of [{ ...recorded, namespace: 'other' }, { ...recorded, file: '../secret' }, { ...recorded, live_bytes: null }]) {
    expect(capacityIntentLine(read(malformed))).toContain('Standalone manifest comparison unknown');
  }
});

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
