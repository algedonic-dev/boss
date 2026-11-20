// THE BOARDS A STATION'S PANEL CARRIES on the alarms board at /it — the
// one table the page draws from.
//
// Since step 1 of design 01fa5bdf (David, 2026-11-17: "IT disappears,
// the old map is pulled out") the panel carries only the boards that
// hold a WRITE no other page offers yet: the track's deck, where a
// troubled train is cancelled (FloorDeck.svelte, backlog 7a24caf3), and
// the yard status board, where a held car is released (YardStatusPanel,
// backlog ed8e5c85). The station page takes both in that design's step
// 4; this table, and the page, go then. Every other board went with the
// map: the gates', arrivals' and shed's decks, the receiving and
// marshalling queue boards, the sensors panel, the conductor's feed and
// the feedback and backlog boards.
import type { RegionName } from './regions';

/** A board a station's panel carries: `deck` is the station's floor
 *  deck, `yard-status` the yard status board's lanes for that station
 *  (YardStatusPanel.svelte `part`). */
export type StationBoard = 'deck' | 'yard-status';

/** Where the release and cancel controls live until the station page
 *  takes them: cancel on the track's deck, release on the dock's and
 *  the garage's lanes of the yard status board (the track's lanes carry
 *  the train's block and the recent trains beside its deck). */
export const STATION_BOARDS: Readonly<Partial<Record<RegionName, ReadonlyArray<StationBoard>>>> = {
  track: ['deck', 'yard-status'],
  dock: ['yard-status'],
  garage: ['yard-status'],
};

/** The boards `station`'s panel carries, in the order it draws them —
 *  none for a station the table does not name, or a name that is not a
 *  station at all. */
export function boardsOf(station: string): ReadonlyArray<StationBoard> {
  return (STATION_BOARDS as Readonly<Record<string, ReadonlyArray<StationBoard> | undefined>>)[station] ?? [];
}
