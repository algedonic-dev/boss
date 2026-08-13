import { describe, expect, test } from 'bun:test';
import {
  assembleYard,
  trainStatus,
  ciLamp,
  isSim,
  protocolHue,
  PROTOCOL_PALETTE,
  type JobLite,
} from './yard';

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
  test('packet cards carry protocol, tags, and sim through both queues', () => {
    const y = assembleYard(
      [train({ metadata: { boarded_jobs: ['c2'] }, steps: [s('pr', 'completed')] })],
      ships,
    );
    expect(y.dock[0]?.kind).toBe('ship-a-change');
    expect(y.dock[0]?.sim).toBe(false);
    expect(y.inFlight[0]?.cars[0]?.kind).toBe('ship-a-change');
  });
});

describe('the packet-card grammar', () => {
  test('a simulated packet is named by its data, not a code path', () => {
    const base = { id: 'x', kind: 'ship-a-change', title: 't', status: 'open',
      opened_on: '2026-08-12' } as const;
    // The Job's admission-fixed field is the source of truth …
    expect(isSim({ ...base, simulated: true })).toBe(true);
    expect(isSim({ ...base, simulated: true, tags: [], metadata: {} })).toBe(true);
    // … and the tag / metadata conventions survive as fallback for
    // packets that predate it.
    expect(isSim({ ...base, tags: ['sim'] })).toBe(true);
    expect(isSim({ ...base, tags: ['Simulated'] })).toBe(true);
    expect(isSim({ ...base, metadata: { simulated: true } })).toBe(true);
    expect(isSim({ ...base, simulated: false, tags: ['sim'] })).toBe(true);
    expect(isSim({ ...base, tags: ['fix'], metadata: {} })).toBe(false);
    expect(isSim({ ...base, simulated: false })).toBe(false);
  });
  test('protocol hue is stable, palette-bound, and distinguishes the yard kinds', () => {
    expect(protocolHue('ship-a-change')).toBe(protocolHue('ship-a-change'));
    expect(PROTOCOL_PALETTE).toContain(protocolHue('ship-a-change'));
    expect(PROTOCOL_PALETTE).toContain(protocolHue('some-future-kind'));
    expect(protocolHue('ship-a-change')).not.toBe(protocolHue('pr-train'));
    expect(new Set(PROTOCOL_PALETTE).size).toBe(PROTOCOL_PALETTE.length);
  });
});
