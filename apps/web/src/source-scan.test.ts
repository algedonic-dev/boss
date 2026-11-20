import { describe, expect, test } from 'bun:test';
import { join } from 'node:path';

import { SCAN_WIDTH, readSources } from './source-scan';

// Backlog 3f421243: train gate c773d182 ran regions.test.ts's scan of
// src/ for 183 s against a 120 s budget, one read after another while
// the gate drive stalled, and the red said only "timed out". These pin
// the two halves of the repair with an injected reader, so neither
// depends on a drive or a wall clock: the reads go out side by side, and
// a read that never returns is NAMED when the scan's own budget ends.
describe('readSources — a source scan reads side by side and names what it waited on (3f421243)', () => {
  test('keeps SCAN_WIDTH reads in flight at once, and returns every text in path order', async () => {
    const paths = Array.from({ length: SCAN_WIDTH * 3 + 5 }, (_, i) => `dir/f${i}.ts`);
    let inFlight = 0;
    let most = 0;
    const read = async (path: string): Promise<string> => {
      inFlight += 1;
      most = Math.max(most, inFlight);
      await Bun.sleep(1);
      inFlight -= 1;
      return `text of ${path}`;
    };
    const sources = await readSources('src', paths, 60_000, read);
    expect(most).toBe(SCAN_WIDTH);
    expect(sources).toEqual(paths.map((path) => ({ path, text: `text of ${join('src', path)}` })));
  });

  test('a read that never returns is named when the scan runs out of budget — never a bare test timeout', async () => {
    const paths = ['a.ts', 'it/yard/stuck.svelte', 'c.ts'];
    const read = (path: string): Promise<string> =>
      path.endsWith('stuck.svelte') ? new Promise<string>(() => {}) : Promise.resolve('');
    let refusal = '';
    try {
      await readSources('src', paths, 50, read);
    } catch (err) {
      refusal = err instanceof Error ? err.message : String(err);
    }
    expect(refusal).toContain('read 2 of 3 files under src in 50 ms');
    expect(refusal).toContain('still waiting on: it/yard/stuck.svelte');
  }, 5_000);
});
