// WHICH REGION A FLOOR STATION STANDS IN — the one table the floor deck
// and its scoping read (region-page.ts), and which regions' panels open
// on the deck (`hasInterior`). It derives no count and invents no state.
//
// The room inside a territory's rect (`contentsBox`, `overflowNoteAt`)
// retired with the world map and the region canvases it measured for
// (R3 of the transit-map retirement, backlog 23a4b4d7).

import { REGION_NAMES, type RegionName } from './regions';
import type { Station } from './yard-floor';

// ---------------------------------------------------------------------
// Which territory a wagon is standing in.
// ---------------------------------------------------------------------

/** Every station of the floor, mapped to the territory that covers it.
 *  A total record, not a lookup with a fallback: a new Station is a
 *  type error here, which is how a wagon can never quietly vanish from
 *  the world (it would otherwise stand in no region and be drawn
 *  nowhere, the false-empty class). The groupings are the ones
 *  regions.ts already made when it named each region's floor panel:
 *  the gates region IS the approach — publishing, queued, in a bay, in
 *  limbo — and the cancelled siding is drawn in the arrivals yard.
 *
 *  `as const` keeps each value's literal, so the record also DEFINES
 *  the floor regions (`FloorRegion`) and which stations each one holds
 *  (`StationOf`). */
const STATION_REGION = {
  approach: 'gates',
  'gate-queue': 'gates',
  gate: 'gates',
  limbo: 'gates',
  dock: 'dock',
  garage: 'garage',
  train: 'track',
  arrivals: 'arrivals',
  cancelled: 'arrivals',
  'inspection-shed': 'shed',
  'siding-event': 'shed',
  'siding-no-probe': 'shed',
} as const satisfies Readonly<Record<Station, RegionName>>;

/** The six regions the yard's floor stands wagons in — read off
 *  STATION_REGION's values, so a region no station maps to is not one. */
export type FloorRegion = (typeof STATION_REGION)[Station];

/** The stations a floor region holds: `StationOf<'gates'>` is
 *  approach | gate-queue | gate | limbo. A layout keyed by it must name
 *  every one of them and no other region's. */
export type StationOf<R extends FloorRegion> = {
  [S in Station]: (typeof STATION_REGION)[S] extends R ? S : never;
}[Station];

export function regionOfStation(station: Station): FloorRegion {
  return STATION_REGION[station];
}

/** The regions whose interior is WAGONS IN TRANSIT — the six the
 *  yard's floor stands rolling stock in. Receiving and marshalling
 *  hold queues instead, and their interior is a platform deck
 *  (world-interior.ts, car 4), so they are absent here by design. */
export const INTERIOR_REGIONS: ReadonlyArray<RegionName> = REGION_NAMES.filter((name) =>
  (Object.values(STATION_REGION) as ReadonlyArray<RegionName>).includes(name),
);

export function hasInterior(region: string): boolean {
  return (INTERIOR_REGIONS as ReadonlyArray<string>).includes(region);
}
