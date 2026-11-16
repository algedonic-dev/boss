import { describe, expect, it } from 'bun:test';
import { STATION_BOARDS, boardsOf, type StationBoard } from './panel';

// THE BOARDS THE RETIRED PAGES BECAME (design e765b3fc, car N3). Four
// pages held content no station drew — yard status, the conductor's
// feed, the feedback board and the backlog board — and each was MOVED
// into the panel of the station it describes before its page retired,
// so nothing a reader used vanished. This is the one table that says
// where each went; the page draws exactly what it lists.
describe('boardsOf — the retired pages each station panel carries', () => {
  it('names, per station, the boards the design moved there', () => {
    expect(boardsOf('track')).toEqual(['yard-status', 'conductor']);
    expect(boardsOf('dock')).toEqual(['yard-status']);
    expect(boardsOf('garage')).toEqual(['yard-status']);
    expect(boardsOf('receiving')).toEqual(['feedback', 'backlog']);
    expect(boardsOf('marshalling')).toEqual(['backlog']);
  });

  it('carries every retired board somewhere — no content was lost with its page', () => {
    const ALL: ReadonlyArray<StationBoard> = ['yard-status', 'conductor', 'feedback', 'backlog'];
    const carried = new Set(Object.values(STATION_BOARDS).flat());
    expect(ALL.filter((b) => !carried.has(b))).toEqual([]);
  });

  it('a station the table does not name carries none, and a name that is no station carries none', () => {
    expect(boardsOf('gates')).toEqual([]);
    expect(boardsOf('plant')).toEqual([]);
    expect(boardsOf('')).toEqual([]);
  });
});
