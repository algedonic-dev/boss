import { describe, expect, it } from 'bun:test';
import {
  PLATFORM_REGIONS,
  hasPlatforms,
  marshallingPlatforms,
  receivingPlatforms,
  withPlaces,
  type Platform,
} from './world-interior';
import type { Siding } from '../marshalling/marshalling';
import type { InboundRow } from '../receiving/receiving';

// THE TWO TERRITORIES WHOSE INTERIOR IS PLATFORMS (design d2154293,
// car 4). Receiving and marshalling hold queues, not rolling stock in
// transit, so their interior is a platform per queue with the packets
// standing on it — and every number on a platform is one the region's
// own read model already derived. These pin the arithmetic: which
// platforms a region has and what each one says. Where the marks went
// inside a territory's outline retired with the world map (R3). Nothing here touches a DOM, so a platform that
// lies is a failing unit test before it is a picture.

const siding = (over: Partial<Siding> & Pick<Siding, 'station' | 'depth'>): Siding => ({
  kind: 'equipment',
  workflowKind: null,
  oldestAgeDays: null,
  flow: { kind: 'counted', arrived: 0, served: 0, net: 0 },
  ...over,
});

const inbound = (over: Partial<InboundRow> & Pick<InboundRow, 'id' | 'openedOn'>): InboundRow => ({
  kind: 'backlog-item',
  title: `packet ${over.id}`,
  status: 'open',
  closedOn: null,
  priority: 'standard',
  lane: 'user-feedback',
  laneBasis: 'recorded',
  source: { basis: 'missing', kind: null, key: null, id: null, branch: null },
  area: null,
  active: [],
  ready: [],
  takenIn: false,
  ...over,
});

const byName = (platforms: ReadonlyArray<Platform>, name: string): Platform =>
  platforms.find((p) => p.name === name)!;

describe('which regions have platforms', () => {
  it('is receiving, marshalling and the shop floor, and nothing else', () => {
    expect([...PLATFORM_REGIONS]).toEqual(['receiving', 'marshalling', 'shop-floor']);
    expect(hasPlatforms('receiving')).toBe(true);
    expect(hasPlatforms('marshalling')).toBe(true);
    expect(hasPlatforms('shop-floor')).toBe(true);
    expect(hasPlatforms('dock')).toBe(false);
    expect(hasPlatforms('nowhere')).toBe(false);
  });
});

describe('the marshalling interior — one platform per station', () => {
  it('carries the depth and what left in the window', () => {
    const p = marshallingPlatforms(
      [
        siding({
          station: 'loading-dock',
          depth: 4,
          oldestAgeDays: 1,
          flow: { kind: 'counted', arrived: 9, served: 7, net: 2 },
        }),
      ],
      24,
    );
    expect(p).toHaveLength(1);
    expect(p[0]!.name).toBe('loading-dock');
    expect(p[0]!.standing).toBe(4);
    expect(p[0]!.bound).toBeNull();
    expect(p[0]!.rate).toBe(7);
    expect(p[0]!.flag.n).toBe(0);
    expect(p[0]!.note).toContain('oldest 1 d');
    expect(p[0]!.note).toContain('7 left in 24h');
  });

  it('a rate the log could not count is unknown, never nought', () => {
    const p = marshallingPlatforms(
      [
        siding({
          station: 'my-watchlist',
          depth: 3,
          flow: { kind: 'unavailable', reason: 'the dock is a Job-metadata clause' },
        }),
      ],
      24,
    );
    // The DEPTH was read, so it stands; the RATE was not, so it is
    // null — the platform renders a `?` there rather than a 0, which
    // on a queue board reads as "nothing is being worked".
    expect(p[0]!.standing).toBe(3);
    expect(p[0]!.rate).toBeNull();
    expect(p[0]!.note).toContain('the dock is a Job-metadata clause');
  });

  it('keeps the read model order — the queues under pressure first', () => {
    const p = marshallingPlatforms(
      [siding({ station: 'first', depth: 9 }), siding({ station: 'second', depth: 0 })],
      24,
    );
    expect(p.map((x) => x.name)).toEqual(['first', 'second']);
  });
});

describe('the receiving interior — one platform per lane the server read', () => {
  const today = '2026-09-20';
  const days = ['2026-09-19', '2026-09-20'];

  it('stands the open packets of each lane and counts what left in the window', () => {
    const p = receivingPlatforms(
      [
        inbound({ id: 'a', openedOn: '2026-09-19', lane: 'user-feedback' }),
        inbound({ id: 'b', openedOn: '2026-09-20', lane: 'user-feedback' }),
        inbound({ id: 'c', openedOn: '2026-09-01', lane: 'user-feedback', status: 'closed', closedOn: '2026-09-20' }),
      ],
      today,
      days,
    );
    const feedback = byName(p, 'user-feedback');
    expect(feedback.standing).toBe(2);
    expect(feedback.rate).toBe(1);
    expect(feedback.bound).toBeNull();
    expect(feedback.note).toContain('oldest 1 d');
  });

  // THE PARTITION (design 62de32ae decision 4): a packet an actor has
  // taken in is marshalling's, however long it stays open — so the
  // interior stands what the region's header counts.
  it('stands nothing an actor has taken in, open or not', () => {
    const p = receivingPlatforms(
      [
        inbound({ id: 'untriaged', openedOn: '2026-09-19', lane: 'discovery-while-working' }),
        inbound({ id: 'triaged', openedOn: '2026-08-01', lane: 'discovery-while-working', takenIn: true }),
      ],
      today,
      days,
    );
    const session = byName(p, 'discovery-while-working');
    expect(session.standing).toBe(1);
    expect(session.flag).toEqual({ from: 'head', n: 0 });
  });

  it('flags the packets past the stale band, from the head — the oldest stand at the front', () => {
    const p = receivingPlatforms(
      [
        inbound({ id: 'old', openedOn: '2026-08-01', lane: 'design-resolution' }),
        inbound({ id: 'new', openedOn: '2026-09-20', lane: 'design-resolution' }),
      ],
      today,
      days,
    );
    const design = byName(p, 'design-resolution');
    expect(design.standing).toBe(2);
    expect(design.flag).toEqual({ from: 'head', n: 1 });
  });

  it('always names the unclassified lane, even empty — a track nothing stands on is an answer', () => {
    const p = receivingPlatforms([], today, days);
    expect(p.map((x) => x.name)).toEqual(['unclassified']);
    expect(p.every((x) => x.standing === 0)).toBe(true);
  });

  // Backlog 1eea4554: the page's own six channels put 61 of 64 standing
  // cars on one platform. A lane is the server's reading, in its spelling.
  it('draws a platform per lane the rows carry, and nothing the rows do not', () => {
    const p = receivingPlatforms(
      [
        inbound({ id: 'a', openedOn: '2026-09-20', lane: 'review-finding' }),
        inbound({ id: 'b', openedOn: '2026-09-20', lane: 'unclassified', laneBasis: 'unclassified' }),
      ],
      today,
      days,
    );
    expect(p.map((x) => x.name).sort()).toEqual(['review-finding', 'unclassified']);
  });

  it('puts the lanes holding flagged work first, then the deepest', () => {
    const p = receivingPlatforms(
      [
        inbound({ id: 'a', openedOn: '2026-09-20', lane: 'discovery-while-working' }),
        inbound({ id: 'b', openedOn: '2026-09-20', lane: 'discovery-while-working' }),
        inbound({ id: 'c', openedOn: '2026-08-01', lane: 'telemetry/monitoring' }),
      ],
      today,
      days,
    );
    expect(p[0]!.name).toBe('telemetry/monitoring');
    expect(p[1]!.name).toBe('discovery-while-working');
  });
});

it('keys a queue platform by the queue it is, so the region map keys never collide', () => {
  const marshalled = marshallingPlatforms([siding({ station: 'q.a', depth: 1 }), siding({ station: 'q.b', depth: 2 })], 24);
  expect(marshalled.map((p) => p.key).sort()).toEqual(['q.a', 'q.b']);
  const received = receivingPlatforms([], '2026-09-24', []);
  expect(received.map((p) => p.key)).toEqual(received.map((p) => p.name));
});

// ---------------------------------------------------------------------
// EACH STATION AT MARSHALLING'S OWN COUNT (design 62de32ae, the rest of
// decision 5; car E on backlog c3105b2a). Live on 2026-09-24 the
// platforms drew each station's full depth — 562 standings under a head
// of 236 — because the station reads carry no partition. The server's
// `places` carry it, and the platform stands what they say.
// ---------------------------------------------------------------------

describe('withPlaces — the platforms at the partition the server counted', () => {
  const deep = marshallingPlatforms(
    [
      siding({ station: 'q.platform-admin.task', depth: 321,  }),
      siding({ station: 'a.platform-admin.opus', depth: 303 }),
      siding({ station: 'sign-off', depth: 7 }),
    ],
    24,
  );
  const places = [
    { name: 'q.platform-admin.task', count: 88 },
    { name: 'a.platform-admin.opus', count: 80 },
    { name: 'sign-off', count: 7 },
  ];

  it('stands each station at the members the partition left there, and says its full depth', () => {
    const p = withPlaces(deep, places);
    expect(p.map((x) => x.standing)).toEqual([88, 80, 7]);
    expect(byName(p, 'q.platform-admin.task').note).toStartWith('88 of the 321 here are marshalling’s');
    // A station whose depth IS its partition says nothing extra.
    expect(byName(p, 'sign-off').note).toBe(byName(deep, 'sign-off').note);
  });

  it('leaves a platform the server has no place for as it was — an older server, or a station it did not read', () => {
    expect(withPlaces(deep, [])).toEqual(deep);
    expect(byName(withPlaces(deep, places.slice(0, 1)), 'a.platform-admin.opus').standing).toBe(303);
  });

  it('keeps a count nobody could take unknown', () => {
    const unknown: Platform = { ...deep[0]!, standing: null };
    expect(withPlaces([unknown], places)[0]!.standing).toBeNull();
  });
});
