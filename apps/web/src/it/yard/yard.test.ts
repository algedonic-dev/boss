import { describe, expect, test } from 'bun:test';
import { assembleYard, trainStatus, ciLamp, type JobLite } from './yard';

function train(over: Partial<JobLite>): JobLite {
  return {
    id: 't1', kind: 'pr-train', title: 'PR train', status: 'open',
    opened_on: '2026-08-12', metadata: {}, steps: [], ...over,
  };
}

const s = (slug: string, status: string, metadata: Record<string, unknown> = {}) =>
  ({ spec_slug: slug, title: slug, status, metadata });

describe('trainStatus', () => {
  test('walks BOARDING → BOARDED → DEPARTED → ARRIVED', () => {
    expect(trainStatus(train({ steps: [s('pr', 'ready')] }))).toBe('BOARDING');
    expect(trainStatus(train({ steps: [s('pr', 'completed')] }))).toBe('BOARDED');
    expect(
      trainStatus(train({ steps: [s('pr', 'completed'), s('merged', 'completed')] })),
    ).toBe('DEPARTED');
    expect(
      trainStatus(
        train({ steps: [s('merged', 'completed'), s('deployed', 'completed')] }),
      ),
    ).toBe('ARRIVED');
    expect(trainStatus(train({ status: 'closed' }))).toBe('ARRIVED');
  });
});

describe('ciLamp', () => {
  test('reads the ci step result; pending until a verdict exists', () => {
    expect(ciLamp(train({ steps: [s('ci', 'ready')] }))).toBe('pending');
    expect(ciLamp(train({ steps: [s('ci', 'completed', { result: 'green' })] }))).toBe('green');
    expect(ciLamp(train({ steps: [s('ci', 'completed', { result: 'failing' })] }))).toBe('failing');
  });
});

describe('assembleYard', () => {
  const ships: JobLite[] = [
    { id: 'c1', kind: 'ship-a-change', title: 'A car', status: 'open',
      opened_on: '2026-08-12', metadata: { branch: 'feat/a' },
      steps: [s('review', 'ready')] },
    { id: 'c2', kind: 'ship-a-change', title: 'Boarded car', status: 'open',
      opened_on: '2026-08-12', metadata: { branch: 'feat/b', train: 't1' },
      steps: [s('review', 'completed')] },
  ];
  test('dock holds only parked, unboarded cars; consists join by id', () => {
    const y = assembleYard(
      [train({ metadata: { boarded_jobs: ['c2'] }, steps: [s('pr', 'completed')] })],
      ships,
    );
    expect(y.dock.map(c => c.id)).toEqual(['c1']);
    expect(y.inFlight[0]?.cars[0]?.branch).toBe('feat/b');
    expect(y.inFlight[0]?.live).toBe(true);
  });
  test('closed trains are arrivals, never live', () => {
    const y = assembleYard([train({ id: 't9', status: 'closed' })], []);
    expect(y.arrivals.length).toBe(1);
    expect(y.arrivals[0]?.live).toBe(false);
  });
});
