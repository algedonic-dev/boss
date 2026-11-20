import { describe, expect, it } from 'bun:test';
import { STATION_BOARDS, boardsOf, type StationBoard } from './panel';

// THE BOARDS THAT STAY on the alarms board at /it (design 01fa5bdf step
// 1): only the two that carry a write no other page offers — cancel on
// the track's deck, release on the yard status board's dock and garage
// lanes. The station page takes both in that design's step 4.
describe('boardsOf — the boards each station panel still carries', () => {
  it('names, per station, the boards that hold the release and cancel controls', () => {
    expect(boardsOf('track')).toEqual(['deck', 'yard-status']);
    expect(boardsOf('dock')).toEqual(['yard-status']);
    expect(boardsOf('garage')).toEqual(['yard-status']);
  });

  it('carries every board somewhere — a board no station draws is unreachable', () => {
    const ALL: ReadonlyArray<StationBoard> = ['deck', 'yard-status'];
    const carried = new Set(Object.values(STATION_BOARDS).flat());
    expect(ALL.filter((b) => !carried.has(b))).toEqual([]);
  });

  it('the stations whose boards went with the map carry none, nor does a name that is no station', () => {
    for (const gone of ['gates', 'arrivals', 'shed', 'receiving', 'marshalling', 'sensors', 'plant', '']) {
      expect({ gone, boards: boardsOf(gone) }).toEqual({ gone, boards: [] });
    }
  });
});
