import { describe, expect, test } from 'bun:test';
import { failedRead, loadingRead, okRead } from '../data/readState';
import {
  assetsHeader, listLine, matchesQuery, overflowHint, phaseLabel, phaseTiles, summaryOf,
} from './view';
import type { Asset, AssetsSummary } from './types';

// The /ux/assets audit (page-audit 015c3935), gaps 3-8: what the page is
// allowed to say, given which of its reads answered and in what shape.

const SUMMARY: AssetsSummary = {
  phase_counts: [
    { phase: 'registered', count: 1 }, { phase: 'received', count: 700 },
    { phase: 'installed', count: 496 }, { phase: 'decommissioned', count: 0 },
  ],
  total_systems: 1_197, in_field_count: 1_197, open_tickets_total: 3, warranty_expiring_30d: 1,
};

const asset = (over: Partial<Asset>): Asset => ({
  asset_id: 'A-100', sku: 'SKU-FERM-01', phase: 'installed', holder_kind: null, holder_id: null,
  warranty_through: null, open_ticket_count: 0, first_seen: '2026-01-02T00:00:00Z',
  last_event_at: '2026-09-20T10:00:00Z', oem_serial: null, ...over,
});

describe('summaryOf — gap 6 (e1cb1ef3): the summary is checked, never cast', () => {
  test('a well-formed summary is data', () => {
    expect(summaryOf(SUMMARY)).toEqual(SUMMARY);
  });

  test('a 200 of the wrong shape is refused, not painted as zeros', () => {
    expect(summaryOf([])).toBeNull();
    expect(summaryOf({})).toBeNull();
    expect(summaryOf({ ...SUMMARY, phase_counts: undefined })).toBeNull();
    expect(summaryOf({ ...SUMMARY, total_systems: '5' })).toBeNull();
    expect(summaryOf(null)).toBeNull();
  });
});

describe('phaseTiles — gaps 3 and 4 (53fecfc9, 3da7e008): the tiles are the phases the server counted', () => {
  const LABELS = new Map([['registered', 'Registered'], ['received', 'Received'], ['installed', 'Installed']]);

  test('one tile per counted phase, in the server order, labelled from the Class rows', () => {
    expect(phaseTiles(SUMMARY.phase_counts, LABELS)).toEqual([
      { phase: 'registered', label: 'Registered', count: 1 },
      { phase: 'received', label: 'Received', count: 700 },
      { phase: 'installed', label: 'Installed', count: 496 },
      // No Class row for it here: the code itself, never a typed guess.
      { phase: 'decommissioned', label: 'decommissioned', count: 0 },
    ]);
  });

  test('the tiles sum to the header total — registered is counted in a tile, not only in the total', () => {
    const sum = phaseTiles(SUMMARY.phase_counts, LABELS).reduce((n, t) => n + t.count, 0);
    expect(sum).toBe(SUMMARY.total_systems);
  });

  test('a phase no Class row names is labelled by its code', () => {
    expect(phaseLabel(new Map(), 'out-for-service')).toBe('out-for-service');
    expect(phaseLabel(new Map([['out-for-service', 'Out for Service']]), 'out-for-service')).toBe('Out for Service');
  });
});

describe('matchesQuery — gap 8 (8429a34a): search reads every identifier the row carries', () => {
  test('BOSS ID, SKU and the OEM serial, case-insensitively', () => {
    const a = asset({ asset_id: 'A-100', sku: 'SKU-FERM-01', oem_serial: 'OEM-9001' });
    expect(matchesQuery(a, 'a-100')).toBe(true);
    expect(matchesQuery(a, 'sku-ferm')).toBe(true);
    expect(matchesQuery(a, 'oem-9001')).toBe(true);
    expect(matchesQuery(a, 'nothing')).toBe(false);
  });

  test('an empty query matches, and null identifiers are not the word "null"', () => {
    expect(matchesQuery(asset({ sku: null, oem_serial: null }), '')).toBe(true);
    expect(matchesQuery(asset({ sku: null, oem_serial: null }), 'null')).toBe(false);
  });
});

describe('assetsHeader — gap 5 (93c1e5f1): no count the summary did not give', () => {
  test('an answered summary gives the counts', () => {
    expect(assetsHeader({ read: okRead, body: SUMMARY }, 'tracked assets')).toEqual({
      title: '1,197 tracked assets',
      subtitle: '496 installed · 3 open tickets · 1 warranties expiring (30d)',
    });
  });

  test('while the summary is in flight the header counts nothing', () => {
    expect(assetsHeader({ read: loadingRead, body: null }, 'tracked assets')).toEqual({
      title: 'Counting tracked assets…', subtitle: '',
    });
  });

  test('a failed or malformed summary says the counts are unavailable', () => {
    expect(assetsHeader({ read: failedRead('summary HTTP 500'), body: null }, 'tracked vessels')).toEqual({
      title: 'Tracked vessels unavailable', subtitle: "Couldn't load the asset summary",
    });
  });
});

describe('listLine — gap 8 (8429a34a): an empty registry and an excluding filter are two lines', () => {
  test('the read outranks the rows', () => {
    expect(listLine(loadingRead, 0, 0)).toEqual({ kind: 'loading' });
    expect(listLine(failedRead('HTTP 503'), 0, 0)).toEqual({ kind: 'failed', error: 'HTTP 503' });
  });

  test('no rows loaded is an empty registry; rows loaded and none visible is the filter', () => {
    expect(listLine(okRead, 0, 0)).toEqual({ kind: 'none-tracked' });
    expect(listLine(okRead, 5, 0)).toEqual({ kind: 'none-match' });
    expect(listLine(okRead, 5, 2)).toEqual({ kind: 'rows' });
  });
});

describe('overflowHint — gap 7 (1382989d): the capped list says what reaches past it', () => {
  test('with the summary answered, the counts cover every asset and the filters only the loaded rows', () => {
    expect(overflowHint(okRead)).toBe(
      'The counts above cover every asset; search and the phase buttons look only at the rows loaded here.',
    );
  });

  test('without it there are no counts to speak of', () => {
    expect(overflowHint(failedRead('x'))).toBe('Search looks only at the rows loaded here.');
    expect(overflowHint(loadingRead)).toBe('Search looks only at the rows loaded here.');
  });
});
