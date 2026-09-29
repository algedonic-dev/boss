import { afterEach, describe, expect, test } from 'bun:test';
import { historyOf, loadKindLedger, parseKindLedger } from './kindLedger';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

// GET /api/jobs/kinds as the jobs API answers it: one row per kind with
// a packet the caller may read, open packets keyed by the version each
// is pinned to (a JSON object, so the version is a string key).
const BODY = {
  kinds: [
    {
      kind: 'backlog-item',
      packets: 1480,
      open: 392,
      open_by_version: { '11': 6, '12': 327, '13': 59 },
      newest_terminal: { id: 'job-1', title: 'closed item', outcome: 'delivered', closed_on: '2026-09-27' },
    },
    {
      kind: 'page-audit',
      packets: 18,
      open: 18,
      open_by_version: { '2': 18 },
      newest_terminal: null,
    },
  ],
};

describe('parseKindLedger', () => {
  test('reads each row by kind', () => {
    const ledger = parseKindLedger(BODY);
    expect(ledger.get('backlog-item')?.packets).toBe(1480);
    expect(ledger.get('page-audit')?.newest_terminal).toBeNull();
    expect(ledger.size).toBe(2);
  });

  test('refuses a body that is not the ledger, rather than reading it as no packets', () => {
    // The mock floor's `[]` and a wrong endpoint's object both lack
    // `kinds`; read as an empty ledger they would draw every kind as
    // never run, which is the claim this read exists to make honestly.
    expect(() => parseKindLedger([])).toThrow();
    expect(() => parseKindLedger({ counts: {} })).toThrow();
    expect(() => parseKindLedger({ kinds: [{ kind: 'x' }] })).toThrow();
  });
});

describe('historyOf', () => {
  const ledger = parseKindLedger(BODY);

  test('counts the in-flight packets on a version below the active one', () => {
    const h = historyOf(ledger, 'backlog-item', 13);
    expect(h).toEqual({
      kind: 'ran',
      packets: 1480,
      inFlight: 392,
      older: 333,
      olderVersions: [
        [11, 6],
        [12, 327],
      ],
      newest: BODY.kinds[0]!.newest_terminal,
    });
  });

  test('a packet on a version above the active one is not behind it', () => {
    // An experiment admits packets to its draft candidate (ce8b7d66);
    // those run AHEAD of the active row, never behind it.
    const h = historyOf(ledger, 'backlog-item', 11);
    expect(h.kind === 'ran' && h.older).toBe(0);
  });

  test('every open packet of a kind whose active version moved on is behind', () => {
    const h = historyOf(ledger, 'page-audit', 3);
    expect(h.kind === 'ran' && [h.older, h.inFlight]).toEqual([18, 18]);
    expect(h.kind === 'ran' && h.newest).toBeNull();
  });

  test('a kind the ledger does not name has never run', () => {
    expect(historyOf(ledger, 'join-a-node', 1)).toEqual({ kind: 'never-run' });
  });
});

describe('loadKindLedger', () => {
  test('a refused read is failed, naming the status — not an empty ledger', async () => {
    globalThis.fetch = (async () => new Response('reading packets is refused', { status: 403 })) as unknown as typeof fetch;
    const r = await loadKindLedger();
    expect(r.kind).toBe('failed');
    expect(r.kind === 'failed' && r.error).toContain('HTTP 403');
  });

  test('a wrong-shaped body is failed', async () => {
    globalThis.fetch = (async () => Response.json([])) as unknown as typeof fetch;
    expect((await loadKindLedger()).kind).toBe('failed');
  });

  test('reads /api/jobs/kinds', async () => {
    let asked = '';
    globalThis.fetch = (async (url: string) => {
      asked = url;
      return Response.json(BODY);
    }) as unknown as typeof fetch;
    const r = await loadKindLedger();
    expect(asked).toBe('/api/jobs/kinds');
    expect(r.kind === 'ready' && r.data.size).toBe(2);
  });
});
