import { describe, expect, it } from 'bun:test';
import type { Alert } from './yard-alerts';
import type { BoardRow, Station } from './yard-floor';
import { alertsFor, boardFor, quietText } from './region-page';

// A STATION'S PANEL IS SCOPED TO ITS REGION (design 62de32ae, decision
// 7). The review of 2026-09-24 (finding 8) measured every floor region
// mounting the WHOLE yard's deck: the garage's departure board listed
// dock cars and trains at CI, and "No alerts — every machine is working
// or idle by design" stood directly under a TROUBLED shed. These pin the
// selections and the words a station's deck is scoped by. The region
// page's rails and interior notes retired with the transit map (R3,
// backlog 23a4b4d7).

// The board and the strip are SCOPED, and what they leave out is
// COUNTED — a region's panel that quietly drops the rest of the floor
// would read as a floor with nothing else on it.
const wagon = (id: string, station: Station) => ({ id, station });
const row = (id: string, landed = false): BoardRow => ({ id, where: id, landed });
const floor = {
  wagons: [
    wagon('parked', 'dock'),
    wagon('aboard', 'train'),
    wagon('in-bay', 'gate'),
    wagon('landed', 'arrivals'),
    wagon('red', 'garage'),
  ],
  boardRows: [row('in-bay'), row('parked'), row('aboard'), row('red'), row('landed', true)],
};

describe("boardFor — the departure board filtered to the region's stations", () => {
  it('keeps the rows standing in the region, in the board order, and counts the rest', () => {
    const dock = boardFor(floor, 'dock');
    expect(dock.rows.map((r) => r.id)).toEqual(['parked']);
    expect(dock.elsewhere).toBe(4);
  });

  it('keeps the landed rows on the arrivals board, and a gate bay on the gates board', () => {
    expect(boardFor(floor, 'arrivals').rows.map((r) => r.id)).toEqual(['landed']);
    expect(boardFor(floor, 'gates').rows.map((r) => r.id)).toEqual(['in-bay']);
  });

  it('keeps a row whose wagon it cannot place, rather than dropping it', () => {
    const orphan = { ...floor, boardRows: [...floor.boardRows, row('ghost')] };
    expect(boardFor(orphan, 'dock').rows.map((r) => r.id)).toEqual(['parked', 'ghost']);
  });
});

const alert = (subject: string, sev: 'err' | 'warn' = 'warn'): Alert => ({
  id: subject,
  subject,
  sev,
  text: subject,
  since: null,
});

describe("alertsFor — the alerts strip scoped to the region's floor", () => {
  const strip = [
    alert('train:t1', 'err'),
    alert('conductor', 'err'),
    alert('cluster', 'err'),
    alert('runner'),
    alert('bay:0'),
    alert('approach'),
    alert('garage'),
    alert('car:parked'),
  ];

  it('places each alert in the region its subject stands in', () => {
    const at = (region: string) => alertsFor(strip, region, floor).here.map((a) => a.subject);
    expect(at('track')).toEqual(['train:t1', 'conductor']);
    expect(at('arrivals')).toEqual(['cluster', 'runner']);
    expect(at('gates')).toEqual(['bay:0', 'approach']);
    expect(at('garage')).toEqual(['garage']);
    expect(at('dock')).toEqual(['car:parked']);
    expect(at('shed')).toEqual([]);
  });

  it('counts what the strip leaves to the other regions', () => {
    expect(alertsFor(strip, 'garage', floor).elsewhere).toBe(7);
  });

  it('keeps on every strip an alert whose subject it cannot place', () => {
    const odd = [alert('something-new')];
    expect(alertsFor(odd, 'dock', floor).here).toHaveLength(1);
    expect(alertsFor(odd, 'shed', floor).here).toHaveLength(1);
  });
});

describe('quietText — a quiet strip cannot contradict the region above it', () => {
  it('a clear region: nothing here is failing', () => {
    expect(quietText('dock', 'clear', 0)).toBe('No alerts in the dock — every machine here is working or idle by design.');
  });

  it('a troubled region: no machine alert, and the state above is the region reading, not a machine', () => {
    const t = quietText('shed', 'troubled', 0);
    expect(t).not.toContain('every machine here is working');
    expect(t).toBe(
      "No machine alerts in the shed — its troubled state above is the region's own reading, not a machine's.",
    );
  });

  it('counts the alerts elsewhere on the floor', () => {
    expect(quietText('dock', 'clear', 2)).toContain('2 alerts elsewhere on the floor.');
    expect(quietText('dock', 'clear', 1)).toContain('1 alert elsewhere on the floor.');
  });

  it('claims nothing about a region whose state could not be read, or is still being read', () => {
    expect(quietText('dock', 'unread', 0)).toBe("No alerts in the dock — the region's own state could not be read.");
    expect(quietText('dock', 'reading', 0)).toBe('No alerts in the dock.');
  });
});
