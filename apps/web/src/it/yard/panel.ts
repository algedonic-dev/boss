// THE BOARDS A STATION'S PANEL CARRIES — design e765b3fc, car N3: the
// pages that retired into a station's panel on the Department Map, as a
// table the page draws from.
//
// The panel's reading cells (rate, waiting, stuck, trend, verdict,
// machines, crossings — car N2) retired with the transit map (R3, backlog
// 23a4b4d7): the rates and the trend were a fold over a 24h window, and
// the visuals show real, current activity only (David, 2026-11-08
// 00:50Z). The boards stay: each reads its own current endpoints.
import type { RegionName } from './regions';

/** A board a station's panel carries below its floor — each one a page
 *  of its own until car N3 of design e765b3fc (2026-09-25):
 *  `yard-status` was /it/operate/yard-status, `conductor`
 *  /it/operate/conductor, `feedback` /it/design/feedback and `backlog`
 *  /it/design/backlog. */
export type StationBoard = 'yard-status' | 'conductor' | 'feedback' | 'backlog';

/** WHERE EACH RETIRED PAGE'S CONTENT LIVES NOW — the design's page table
 *  (e765b3fc section 5), as data the page draws from. A train's block,
 *  phase and ETA and the recent trains are the track's detail, and the
 *  conductor is the track's machine; the dock's cars and its boarding
 *  sentence are the dock's; the stranded and held greens are the
 *  garage's (the garage region claims them). A feedback packet is
 *  inbound, so its board is receiving's; a backlog item is inbound until
 *  triaged and then stands at a station, so its board is receiving's AND
 *  marshalling's. The yard status board draws, for each station, only
 *  its own lanes (YardStatusPanel.svelte `part`). */
export const STATION_BOARDS: Readonly<Partial<Record<RegionName, ReadonlyArray<StationBoard>>>> = {
  track: ['yard-status', 'conductor'],
  dock: ['yard-status'],
  garage: ['yard-status'],
  receiving: ['feedback', 'backlog'],
  marshalling: ['backlog'],
};

/** The boards `station`'s panel carries, in the order it draws them —
 *  none for a station the table does not name, or a name that is not a
 *  station at all. */
export function boardsOf(station: string): ReadonlyArray<StationBoard> {
  return (STATION_BOARDS as Readonly<Record<string, ReadonlyArray<StationBoard> | undefined>>)[station] ?? [];
}
