import { describe, expect, test } from 'bun:test';
import { join } from 'node:path';
import { scaled } from '../dev-load';
import { readSources } from '../source-scan';

// NO SURFACE LINKS UNDER /it (backlog 92bfcb04, car 3 of design df0ecd8b:
// "IT retires as a department: its work now lives in Engineering,
// Operations and the company"). Every page IT hosted has moved to the
// station the design names, and each move deleted its /it route with no
// alias (David, 2026-09-25: no shims before 1.0.0) — the family pages
// with backlog 0bc2eb68, and the station tree floor, a station's page and
// the alarms board's selections with this car. A link still spelled
// under /it now lands on a not-found page, so no source file may build
// one: a link is the page's catalog address, derived from its host.
//
// What it reads: every quoted string in a non-test .ts or .svelte file
// under src/ that IS /it or starts with /it/, /it? or /it#. Comment lines
// are prose, and say where a page used to be.

/** The lines of `text` that spell a link under /it, comments skipped. */
function linksUnderIt(text: string): ReadonlyArray<string> {
  return text
    .split('\n')
    .filter((line) => {
      const t = line.trimStart();
      return !(t.startsWith('//') || t.startsWith('*') || t.startsWith('/*') || t.startsWith('<!--'));
    })
    .filter((line) => /['"`]\/it(?:['"`/?#])/.test(line))
    .map((line) => line.trim());
}

describe('no surface links under /it', () => {
  test('the reader catches the spellings that shipped, and passes prose (control)', () => {
    expect(linksUnderIt("export const HELD_CARS_HREF = '/it?at=dock#held-cars';")).toHaveLength(1);
    expect(linksUnderIt("  return s === '' ? '/it/tree' : `/it/tree?${s}`;")).toHaveLength(1);
    expect(linksUnderIt('<a href="/it" onclick={(e) => go(e, \'/it\')}>close</a>')).toHaveLength(1);
    expect(linksUnderIt("  // the IT tab links to '/it', IT's landing")).toEqual([]);
    expect(linksUnderIt(" *  `/it?at=gates` was the board's selection")).toEqual([]);
    expect(linksUnderIt("const kb = '/items';")).toEqual([]);
  });

  test('no non-test source file under src/ spells one', async () => {
    const root = join(import.meta.dir, '..');
    const paths = [...new Bun.Glob('**/*.{ts,svelte}').scanSync(root)].filter((f) => !f.endsWith('.test.ts'));
    expect(paths.length).toBeGreaterThan(100);
    const spelled = (await readSources(root, paths, scaled(25_000))).flatMap(({ path, text }) =>
      linksUnderIt(text).map((line) => `${path}: ${line}`),
    );
    expect(spelled).toEqual([]);
  }, scaled(30_000));
});
