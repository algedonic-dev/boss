// RECEIVING AND MARSHALLING GET INTERIORS (design d2154293, car 4).
//
// Car 3 zoomed the camera into a territory and drew what is MOVING
// inside it — the wagons the floor's Scene stands at that region's
// stations. These two regions hold no rolling stock in transit: they
// hold QUEUES. So their interior is a PLATFORM PER QUEUE — a station
// for marshalling, an input lane for receiving — with the packets standing
// on it as marks, the queue's bound drawn on the track, and the
// packets past that bound flagged. Until this car both said "this
// region's floor is a page of its own", which was an apology printed
// where the region's own activity belongs.
//
// ONE DEFINITION PER NUMBER. Nothing here reads an endpoint and
// nothing here derives a count: the marshalling platforms are built
// from the sidings `marshalling.ts` already joined (depth from
// `/api/stations/load`, rate from `/api/stations/flow`), the receiving
// platforms from the inbound rows `receiving.ts` already parsed and
// classified. The pages that own those reads hand them up, the way
// FloorDeck hands its Scene up for the other six territories — one read
// of a region on the page, never two.
//
// A NUMBER NOBODY COULD TAKE IS UNKNOWN, NEVER NOUGHT. A station whose
// flow the cube is blind to (the dock's Job-metadata clause, a
// per-actor watchlist) carries `rate: null`, and the map draws a `?`
// in its own band — on a queue board a 0 reads as "nothing is being
// worked", which is a different fact and usually a false one. The same
// holds for `standing`: null draws no marks at all, where a real zero
// draws an empty track.
//
// Pure functions, so a platform that lies fails world-interior.test.ts
// before it is a picture. Where the platforms went inside a territory's
// outline (`platformLayout`) retired with the world map (R3 of the
// transit-map retirement, backlog 23a4b4d7); the queue boards draw them.

import {
  AGE_THRESHOLDS,
  ageDays,
  lanesOf,
  standsHere,
  type InboundRow,
} from '../receiving/receiving';
import type { Place } from './regions';
import type { Siding } from '../marshalling/marshalling';

/** Which standing packets are flagged, and from which end of the
 *  queue: the oldest stand at the HEAD (receiving's age bands), the
 *  ones past a WIP bound at the TAIL (marshalling's discipline). */
export type Flag = Readonly<{ from: 'head' | 'tail'; n: number }>;

/** One queue, drawn as a platform with its packets standing on it. */
export type Platform = Readonly<{
  /** WHICH queue — unique among a region's platforms, and what the map
   *  keys it on. Apart from `name` since backlog 846ab934: the shop
   *  floor named its crews by actor and keyed them on that name, and
   *  live every crew is the same actor, so the keyed each threw and
   *  the region never drew. A station and a lane are their own key;
   *  a crew is `session:<id>`, the spelling the server's machines use. */
  key: string;
  /** The queue's own name — a station, or an inbound lane. The
   *  label, which need not be unique. */
  name: string;
  /** Packets standing on it. `null` is a count nobody could take. */
  standing: number | null;
  /** The bound the queue is read against, where it declares one. */
  bound: number | null;
  /** What LEFT in the window. `null` where nobody counted it. */
  rate: number | null;
  flag: Flag;
  /** One short line: the oldest packet's age, what left in the window,
   *  and — where a number is unknown — the clause that blinded it. */
  note: string;
}>;

/** What a zoomed region hands the map. A read that failed is a failure
 *  here too: a region drawn empty would read as a calm one. */
export type Deck =
  | Readonly<{ kind: 'reading' }>
  | Readonly<{ kind: 'unavailable'; why: string }>
  | Readonly<{
      kind: 'ready';
      region: string;
      platforms: ReadonlyArray<Platform>;
    }>;

/** The regions whose interior is platforms rather than wagons in
 *  transit. The other six are region-contents.ts's INTERIOR_REGIONS.
 *  The shop floor joined them on backlog 94c6ffd0 as a platform per
 *  crew. Its actor deck (design 62de32ae, decision 8) went with the
 *  Crew Board (backlog 462fb22a); it stays in this list because its
 *  board mounts under the map the same way the queue boards do. */
export const PLATFORM_REGIONS: ReadonlyArray<string> = ['receiving', 'marshalling', 'shop-floor'];

export function hasPlatforms(region: string): boolean {
  return PLATFORM_REGIONS.includes(region);
}

// ---------------------------------------------------------------------
// What each region's platforms say.
// ---------------------------------------------------------------------

/** Marshalling: a platform per station, in the order `joinSidings`
 *  already put them — the queues under pressure first. */
export function marshallingPlatforms(
  sidings: ReadonlyArray<Siding>,
  windowHours: number,
): ReadonlyArray<Platform> {
  return sidings.map((s): Platform => {
    const oldest = s.oldestAgeDays === null ? null : `oldest ${s.oldestAgeDays} d`;
    const rate = s.flow.kind === 'counted' ? s.flow.served : null;
    const left =
      s.flow.kind === 'counted'
        ? `${s.flow.served} left in ${windowHours}h`
        : `rate not counted — ${s.flow.reason}`;
    return {
      key: s.station,
      name: s.station,
      standing: s.depth,
      bound: null,
      rate,
      flag: { from: 'tail', n: 0 },
      note: [oldest, left].filter((p) => p !== null).join(' · '),
    };
  });
}

/** EACH STATION AT THE PARTITION THE SERVER COUNTED (design 62de32ae,
 *  the rest of decision 5; car E on backlog c3105b2a). A station's
 *  depth is every packet whose step its predicate matches — an
 *  untriaged item's `triage` as much as a triaged one's `build` — and
 *  the marshalling head counts only the packets that are marshalling's.
 *  Drawn at their depths the platforms stood 562 under a head of 236
 *  (live, 2026-09-24). The server's `places` carry the partition per
 *  station, so the platform stands that, and its note keeps the depth
 *  the station holds in all; the flag, a count of packets past the
 *  station's WIP bound, never exceeds what the platform stands. A
 *  platform the server names no place for is left as it was — an older
 *  server, or a station it did not read — and an unknown stays unknown. */
export function withPlaces(
  platforms: ReadonlyArray<Platform>,
  places: ReadonlyArray<Place>,
): ReadonlyArray<Platform> {
  return platforms.map((p): Platform => {
    const place = places.find((x) => x.name === p.key);
    if (place === undefined || p.standing === null || place.count === p.standing) return p;
    const own = `${place.count} of the ${p.standing} here are marshalling’s`;
    return {
      ...p,
      standing: place.count,
      flag: { ...p.flag, n: Math.min(p.flag.n, place.count) },
      note: p.note === '' ? own : `${own} · ${p.note}`,
    };
  });
}

/** Receiving: a platform per input lane the server read ([`lanesOf`]:
 *  every recorded lane the rows carry, and `unclassified` always), the
 *  lanes holding flagged work first, then the deepest. `rows` is every
 *  inbound packet the page read — open ones nothing has taken in stand,
 *  ones that closed inside the window are what left. Until backlog
 *  1eea4554 these were the page's own six channels, and 61 of 64
 *  standing cars stood on one of them. */
export function receivingPlatforms(
  rows: ReadonlyArray<InboundRow>,
  today: string,
  days: ReadonlyArray<string>,
): ReadonlyArray<Platform> {
  const window = new Set(days);
  const platforms = lanesOf(rows).map((lane): Platform => {
    const mine = rows.filter((r) => r.lane === lane);
    // Standing = open AND not yet taken in: a triaged packet waiting on
    // its build is marshalling's (design 62de32ae decision 4).
    const ages = mine
      .filter(standsHere)
      .map((r) => ageDays(r.openedOn, today))
      .sort((a, b) => b - a);
    const stale = ages.filter((d) => d > AGE_THRESHOLDS.stale).length;
    const oldest = ages[0];
    return {
      key: lane,
      name: lane,
      standing: ages.length,
      // A lane has no WIP bound — what it is read against is the
      // age band, which the flag carries.
      bound: null,
      rate: mine.filter((r) => r.closedOn !== null && window.has(r.closedOn)).length,
      flag: { from: 'head', n: stale },
      note:
        oldest === undefined
          ? 'nothing standing'
          : `oldest ${oldest} d${stale > 0 ? ` · ${stale} past the ${AGE_THRESHOLDS.stale}-day band` : ''}`,
    };
  });
  return [...platforms].sort(
    (a, b) => b.flag.n - a.flag.n || (b.standing ?? 0) - (a.standing ?? 0) || a.name.localeCompare(b.name),
  );
}
