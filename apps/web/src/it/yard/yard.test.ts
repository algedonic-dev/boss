import { afterEach, describe, expect, test } from 'bun:test';
import {
  assembleYard,
  disciplineLabel,
  fetchYard,
  trainStatus,
  ciLamp,
  isSim,
  protocolHue,
  wipAdvisory,
  PROTOCOL_PALETTE,
  type JobLite,
  type StationQueueEnvelope,
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

// ---------------------------------------------------------------------------
// The dock as a registry-backed lens (stations.md): when the station
// endpoint serves, the envelope is authoritative — membership AND
// order come from the server, and the header shows the station's own
// facts (discipline, advisory WIP verdict).
// ---------------------------------------------------------------------------

function envelope(over: Partial<StationQueueEnvelope> = {}): StationQueueEnvelope {
  return {
    station: 'loading-dock',
    kind: 'batch',
    discipline: ['priority', 'age'],
    wip_limit: null,
    over_limit: false,
    total: 0,
    data: [],
    ...over,
  };
}

const dockJob = (id: string, over: Partial<JobLite> = {}): JobLite => ({
  id, kind: 'ship-a-change', title: `car ${id}`, status: 'open',
  opened_on: '2026-08-10', metadata: { branch: `feat/${id}` }, ...over,
});

describe('the dock from the station envelope', () => {
  test('envelope rows map to the same packet-card grammar as dockRows', () => {
    const env = envelope({
      total: 2,
      data: [
        dockJob('s1', {
          tags: ['hotfix'],
          metadata: { branch: 'feat/s1', skip_reason: 'CI red' },
          simulated: true,
        }),
        dockJob('s2'),
      ],
    });
    const y = assembleYard([], [], env);
    expect(y.dock.map(c => c.id)).toEqual(['s1', 's2']);
    expect(y.dock[0]).toEqual({
      id: 's1', kind: 'ship-a-change', branch: 'feat/s1', title: 'car s1',
      tags: ['hotfix'], sim: true, skipReason: 'CI red',
    });
    expect(y.dock[1]?.sim).toBe(false);
    expect(y.dock[1]?.skipReason).toBeNull();
  });

  test('the envelope is authoritative: membership does not re-derive from ships', () => {
    // A ship that dockRows would park, but the station did not serve.
    const parked: JobLite = {
      id: 'c1', kind: 'ship-a-change', title: 'A car', status: 'open',
      opened_on: '2026-08-12', metadata: { branch: 'feat/a' },
      steps: [s('review', 'ready')],
    };
    const y = assembleYard([], [parked], envelope({ total: 1, data: [dockJob('s9')] }));
    expect(y.dock.map(c => c.id)).toEqual(['s9']);
  });

  test('server order is preserved — no client re-sort by age or anything else', () => {
    // Deliberately NOT in age order: any client-side re-sort would flip it.
    const env = envelope({
      total: 3,
      data: [
        dockJob('newer', { opened_on: '2026-08-12' }),
        dockJob('oldest', { opened_on: '2026-08-01' }),
        dockJob('middle', { opened_on: '2026-08-07' }),
      ],
    });
    const y = assembleYard([], [], env);
    expect(y.dock.map(c => c.id)).toEqual(['newer', 'oldest', 'middle']);
  });

  test('the header facts come off the envelope', () => {
    const y = assembleYard([], [], envelope({ wip_limit: 5, over_limit: true, total: 7 }));
    expect(y.dockStation).toEqual({
      source: 'station',
      discipline: ['priority', 'age'],
      wipLimit: 5,
      overLimit: true,
      total: 7,
    });
  });

  test('without an envelope the dock falls back to the derived rows', () => {
    const parked: JobLite = {
      id: 'c1', kind: 'ship-a-change', title: 'A car', status: 'open',
      opened_on: '2026-08-12', metadata: { branch: 'feat/a' },
      steps: [s('review', 'ready')],
    };
    const y = assembleYard([], [parked], null);
    expect(y.dock.map(c => c.id)).toEqual(['c1']);
    expect(y.dockStation).toEqual({ source: 'derived' });
    // The 2-arg call sites mean the same thing.
    expect(assembleYard([], [parked]).dockStation).toEqual({ source: 'derived' });
  });
});

describe('the station header idiom', () => {
  test('discipline renders in the mono-caps idiom', () => {
    expect(disciplineLabel(['priority', 'age'])).toBe('PRIORITY → AGE');
    expect(disciplineLabel(['due'])).toBe('DUE');
    // A key published tomorrow renders with zero code change.
    expect(disciplineLabel(['shortest-job-first'])).toBe('SHORTEST-JOB-FIRST');
  });

  test('the WIP chip appears only on an over-limit station', () => {
    const over = assembleYard([], [], envelope({ wip_limit: 5, over_limit: true, total: 7 }));
    expect(wipAdvisory(over.dockStation)).toBe('WIP 7/5');
    const under = assembleYard([], [], envelope({ wip_limit: 5, over_limit: false, total: 3 }));
    expect(wipAdvisory(under.dockStation)).toBeNull();
    // No declared limit -> never a chip, whatever the flag says.
    const limitless = assembleYard([], [], envelope({ over_limit: true, total: 9 }));
    expect(wipAdvisory(limitless.dockStation)).toBeNull();
    // The derived dock has no station facts to advertise.
    expect(wipAdvisory({ source: 'derived' })).toBeNull();
  });
});

describe('fetchYard against the station endpoint', () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  const json = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), { status });

  function stub(station: () => Response | Promise<Response>) {
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/api/stations/loading-dock/queue')) return station();
      if (url.includes('kind=pr-train')) return json({ data: [] });
      if (url.includes('kind=ship-a-change'))
        return json({
          data: [
            {
              id: 'c1', kind: 'ship-a-change', title: 'A car', status: 'open',
              opened_on: '2026-08-12', metadata: { branch: 'feat/a' },
              steps: [s('review', 'ready')],
            },
          ],
        });
      throw new Error(`unexpected fetch: ${url}`);
    }) as typeof fetch;
  }

  test('when the endpoint serves, the dock reads its own station row', async () => {
    stub(() => json(envelope({ total: 1, data: [dockJob('s1')] })));
    const y = await fetchYard();
    expect(y?.dock.map(c => c.id)).toEqual(['s1']);
    expect(y?.dockStation.source).toBe('station');
  });

  test('a cluster that predates the registry still renders the yard whole', async () => {
    // 404 (no station row), 503 (registry not configured), and a
    // thrown network error all mean the same thing: derive locally.
    for (const station of [
      () => json('no such station', 404),
      () => json('station registry not configured', 503),
      () => Promise.reject(new Error('connection refused')),
    ]) {
      stub(station as () => Response | Promise<Response>);
      const y = await fetchYard();
      expect(y?.dock.map(c => c.id)).toEqual(['c1']);
      expect(y?.dockStation).toEqual({ source: 'derived' });
    }
  });

  test('a 200 that is not a queue envelope falls back too', async () => {
    stub(() => json({ hello: 'not an envelope' }));
    const y = await fetchYard();
    expect(y?.dock.map(c => c.id)).toEqual(['c1']);
    expect(y?.dockStation).toEqual({ source: 'derived' });
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
