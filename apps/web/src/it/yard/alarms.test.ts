import { describe, expect, it } from 'bun:test';
import { REGION_NAMES, type Region, type Regions } from './regions';
import { alarmsOf } from './alarms';

// The alarms board's rule, moved out of transit.ts when the transit map
// retired (R3, backlog 23a4b4d7). The order is the declared regions'
// (REGION_NAMES), never a hand-placed station list.

const region = (name: string, over: Partial<Region> = {}): Region => ({
  name,
  count: 2,
  bound: null,
  bound_kind: null,
  unit: 'cars parked',
  state: 'clear',
  why: 'fine',
  band: null,
  trend: { metric: 'm', unit: 'u', current: null, previous: null, samples: 0, previous_samples: 0 },
  kpi: [],
  machines: [],
  places: [],
  ...over,
});

const read = (regions: ReadonlyArray<Region>): Regions => ({
  window_hours: 24,
  regions,
  now: '2026-09-24T22:15:00Z',
  thirds: [],
  machines: null,
  plant: [],
  outranks: { kind: 'rows', rows: [] },
  next_up: { kind: 'rows', rows: [] },
});

const OVER: Readonly<Record<string, Partial<Region>>> = {
  shed: { state: 'troubled', why: '9 of 10 open past 24h' },
  receiving: { state: 'attention', why: '80 packets standing' },
  publish: { state: 'troubled', why: 'a pull request open 26 hours' },
  'shop-floor': { state: 'troubled', why: '1 run finished and not reported' },
};

describe('the alarms board', () => {
  it("lists every non-clear station with the server's why — troubled first, then attention, in declared order", () => {
    const alarms = alarmsOf(read(REGION_NAMES.map((n) => region(n, OVER[n]))));
    expect(alarms.map((a) => [a.name, a.state])).toEqual([
      ['shed', 'troubled'],
      ['shop-floor', 'troubled'],
      ['publish', 'troubled'],
      ['receiving', 'attention'],
    ]);
    expect(alarms[0]!.why).toBe('9 of 10 open past 24h');
    expect(alarms[1]!.label).toBe('shop floor');
  });

  it('a declared station the read did not carry is an alarm, never a quiet station', () => {
    const alarms = alarmsOf(read(REGION_NAMES.filter((n) => n !== 'dock').map((n) => region(n))));
    expect(alarms.map((a) => [a.name, a.state])).toEqual([['dock', 'troubled']]);
    expect(alarms[0]!.why).toContain('no reading');
  });

  it('a region the read carries beyond the declared ones still raises its alarm', () => {
    const alarms = alarmsOf(read([...REGION_NAMES.map((n) => region(n)), region('new-room', { state: 'attention', why: 'new' })]));
    expect(alarms.map((a) => a.name)).toEqual(['new-room']);
  });

  it('a full station is no alarm — at capacity and moving is the good state (design e765b3fc §4a)', () => {
    const alarms = alarmsOf(read(REGION_NAMES.map((n) => (n === 'gates' ? region('gates', { state: 'full', why: 'at its capacity, and moving' }) : region(n)))));
    expect(alarms).toEqual([]);
  });

  it('is empty when every station is clear', () => {
    expect(alarmsOf(read(REGION_NAMES.map((n) => region(n))))).toEqual([]);
  });
});
