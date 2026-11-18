// A STATION'S PANEL, SCOPED TO ITS REGION (design 62de32ae, decision 7;
// approved by David 2026-09-24, folded into fe77a1d2's car 3).
//
// The review of 2026-09-24 (finding 8) measured the half-swap: every
// floor region's departure board listed every car in every place, so the
// garage's board showed dock cars and trains at CI; and "No alerts —
// every machine is working or idle by design" stood directly under a
// TROUBLED shed. This module is the selections and the words a station's
// floor deck is scoped by. It derives no count and no state, and what a
// region's panel leaves to the other regions is COUNTED, never dropped.
//
// The region page's map, its rails and its interior-against-head notes
// retired with the transit map (R3, backlog 23a4b4d7).

import { regionOfStation, type FloorRegion } from './region-contents';
import type { RegionState } from './regions';
import type { Alert } from './yard-alerts';
import type { BoardRow, Station } from './yard-floor';

// ---------------------------------------------------------------------
// The departure board and the alerts strip, scoped to the region.
// ---------------------------------------------------------------------

/** What the region's panels need of the floor: where each wagon stands,
 *  and the board's rows in the board's order. `Scene` satisfies it. */
export type FloorView = Readonly<{
  wagons: ReadonlyArray<Readonly<{ id: string; station: Station }>>;
  boardRows: ReadonlyArray<BoardRow>;
}>;

/** A region's share of the floor, and how much stands elsewhere. */
export type Scoped<T> = Readonly<{ here: ReadonlyArray<T>; elsewhere: number }>;

const regionOfWagon = (floor: FloorView, id: string): FloorRegion | null => {
  const w = floor.wagons.find((x) => x.id === id);
  return w === undefined ? null : regionOfStation(w.station);
};

/** The departure board filtered to the region's stations, in the
 *  board's own order. A row whose wagon cannot be placed is KEPT: it is
 *  the floor's to explain, and a row dropped for want of a place is a
 *  car drawn nowhere. */
export function boardFor(
  floor: FloorView,
  region: string,
): Readonly<{ rows: ReadonlyArray<BoardRow>; elsewhere: number }> {
  const rows = floor.boardRows.filter((r) => {
    const at = regionOfWagon(floor, r.id);
    return at === null || at === region;
  });
  return { rows, elsewhere: floor.boardRows.length - rows.length };
}

/** The areas an alert may name as its subject, each in the region that
 *  draws it — a station's read off region-contents.ts, the three
 *  machines' named here since the floor canvas that drew them
 *  (floor-slices.ts) retired with the transit map (R3, backlog
 *  23a4b4d7). */
const AREA_REGION: Readonly<Record<string, FloorRegion>> = {
  track: regionOfStation('train'),
  dock: regionOfStation('dock'),
  garage: regionOfStation('garage'),
  approach: regionOfStation('approach'),
  'gate-queue': regionOfStation('gate-queue'),
  arrivals: regionOfStation('arrivals'),
  cancelled: regionOfStation('cancelled'),
  'inspection-shed': regionOfStation('inspection-shed'),
  // The floor's machines, each in the region that houses it.
  runner: 'arrivals',
  cluster: 'arrivals',
  conductor: 'track',
};

/** The region an alert's subject stands in; null when the subject is
 *  not one this page can place. */
function regionOfAlert(a: Alert, floor: FloorView): FloorRegion | null {
  if (a.subject.startsWith('car:')) return regionOfWagon(floor, a.subject.slice(4));
  if (a.subject.startsWith('train:')) return regionOfStation('train');
  if (a.subject.startsWith('bay:')) return regionOfStation('gate');
  return AREA_REGION[a.subject] ?? null;
}

/** The alerts strip scoped to the region. An alert this page cannot
 *  place is shown on EVERY region's strip: an alarm that stands in no
 *  region would be read nowhere. */
export function alertsFor(alerts: ReadonlyArray<Alert>, region: string, floor: FloorView): Scoped<Alert> {
  const here = alerts.filter((a) => {
    const at = regionOfAlert(a, floor);
    return at === null || at === region;
  });
  return { here, elsewhere: alerts.length - here.length };
}

/** The strip's line when nothing here alerts. It must not contradict
 *  the region's head above it: under a troubled shed "every machine is
 *  working or idle by design" read as a clear shed (review finding 8).
 *  An alert is a MACHINE's or a packet's trouble; the region's state is
 *  the regions read's own judgement, and the line says which is which.
 *  `reading` is the read not yet landed, which claims nothing either
 *  way; `unread` is the read that failed. */
export function quietText(region: string, state: RegionState | 'reading' | 'unread', elsewhere: number): string {
  const said =
    state === 'reading'
      ? `No alerts in the ${region}.`
      : state === 'unread'
      ? `No alerts in the ${region} — the region's own state could not be read.`
      : state === 'clear'
        ? `No alerts in the ${region} — every machine here is working or idle by design.`
        : `No machine alerts in the ${region} — its ${state} state above is the region's own reading, not a machine's.`;
  if (elsewhere === 0) return said;
  return `${said} ${elsewhere} ${elsewhere === 1 ? 'alert' : 'alerts'} elsewhere on the floor.`;
}
