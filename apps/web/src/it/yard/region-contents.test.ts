import { describe, expect, it } from 'bun:test';
import { REGION_NAMES } from './regions';
import { INTERIOR_REGIONS, hasInterior, regionOfStation } from './region-contents';
import type { Station } from './yard-floor';

// WHICH REGION A FLOOR STATION STANDS IN (backlog ca37478f): the part of
// this module that outlived the world map (R3, backlog 23a4b4d7).

describe('what is moving inside a territory', () => {
  it('sends every station a wagon can stand at to exactly one territory', () => {
    const stations: readonly Station[] = [
      'approach', 'gate-queue', 'gate', 'limbo', 'dock', 'garage', 'train',
      'arrivals', 'cancelled', 'inspection-shed', 'siding-event', 'siding-no-probe',
    ];
    for (const s of stations) {
      const region = regionOfStation(s);
      expect(REGION_NAMES, s).toContain(region);
    }
    // The gates territory is the whole approach — publishing, queued and
    // in a bay — as regions.ts already says the gates floor is.
    expect(regionOfStation('gate-queue')).toBe('gates');
    expect(regionOfStation('limbo')).toBe('gates');
    // A withdrawn car stands on the cancelled siding, which is drawn in
    // the arrivals yard: the terminal tracks are one territory.
    expect(regionOfStation('cancelled')).toBe('arrivals');
    expect(regionOfStation('siding-no-probe')).toBe('shed');
    expect(regionOfStation('train')).toBe('track');
  });

  it('the regions with an interior are the yard regions, not the two with pages of their own', () => {
    expect([...INTERIOR_REGIONS].sort()).toEqual(['arrivals', 'dock', 'garage', 'gates', 'shed', 'track']);
    expect(hasInterior('dock')).toBe(true);
    expect(hasInterior('receiving')).toBe(false);
    expect(hasInterior('marshalling')).toBe(false);
    expect(hasInterior('nowhere')).toBe(false);
  });
});

