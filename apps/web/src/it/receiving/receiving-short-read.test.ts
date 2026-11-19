import { describe, expect, test } from 'bun:test';
import {
  loadEveryPage,
  loadKind,
  parseJobsPage,
  shortReadSentence,
  shortReads,
  type InboundRow,
} from './receiving';

// THE SHORT-READ NOTICE SAYS WHAT WAS READ (backlog 3e96fe11). Since
// `loadEveryPage` reads every kind to its total, the notice fires only
// when the listing moved under the read and fewer rows were kept than
// the server's total. It used to say "has N packets in the window and
// only 500 were read; the counts below are floors" — false whenever it
// appeared. Decided 2026-09-24: "<rows> of <total> read — the listing
// moved during the read", with that kind's figures.

const job = (id: string): Record<string, unknown> => ({
  id,
  kind: 'backlog-item',
  title: id,
  status: 'open',
  opened_on: '2026-09-24',
  steps: [],
});
const rows = (from: number, n: number): ReadonlyArray<InboundRow> =>
  parseJobsPage({ data: Array.from({ length: n }, (_, i) => job(`row-${from + i}`)), total: n }).rows;

describe('the short-read notice says what was read', () => {
  test('a kind read to its total is not named; a short one carries its own rows and total', () => {
    const got = shortReads(
      ['backlog-item', 'design-doc'],
      [
        { rows: rows(0, 822), total: 823 },
        { rows: rows(0, 12), total: 12 },
      ],
    );
    expect(got).toEqual([{ kind: 'backlog-item', rows: 822, total: 823 }]);
  });

  test('the sentence is the decided one, with that kind’s figures', () => {
    const said = shortReadSentence({ kind: 'backlog-item', rows: 822, total: 823 });
    expect(said).toBe('822 of 823 read — the listing moved during the read');
    expect(said).not.toContain('500');
    expect(said).not.toContain('floor');
  });

  test('a listing that moved during the read is the case it names', async () => {
    const TOTAL = 1201;
    // The table moved under the read: the second page starts one row
    // late, so one row is never seen.
    const load: typeof loadKind = async (_kind, _days, limit, offset = 0) => {
      const start = offset === 0 ? 0 : offset + 1;
      return {
        kind: 'ready',
        data: { rows: rows(start, Math.max(0, Math.min(limit, TOTAL - start))), total: TOTAL },
      };
    };
    const got = await loadEveryPage('backlog-item', 8, 500, load);
    if (got.kind !== 'ready') throw new Error(`the read failed: ${JSON.stringify(got)}`);
    const short = shortReads(['backlog-item'], [got.data]);
    expect(short).toEqual([{ kind: 'backlog-item', rows: got.data.rows.length, total: TOTAL }]);
    expect(short[0]?.rows).toBeLessThan(TOTAL);
  });
});
