// The /it/kb diagrams have ONE definition: the Mermaid source under
// docs/architecture/. Backlog 4718d918 (decided on page audit 8cd38edd,
// gap 1, 2026-09-25): the page used to show SVG renders committed in
// two places (docs/architecture/*.svg + *.png, and a copy under
// it/kb-assets/), and nothing compared a render with its source — so
// when car B of 467175e7 retired boss-observability from the .mmd, all
// four renders went on drawing it. The page now renders the source in
// the browser, and these two tests hold that shape: every source is
// drawn from the file itself, and no render is committed to drift.

import { expect, test } from 'bun:test';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { KB_DIAGRAMS } from './kbDiagrams';

const ARCHITECTURE = join(import.meta.dir, '..', '..', '..', '..', 'docs', 'architecture');

test('the page draws every Mermaid source in docs/architecture, read from the file itself', () => {
  const sources = readdirSync(ARCHITECTURE).filter((f) => f.endsWith('.mmd')).sort();
  expect(sources.length).toBeGreaterThan(0);
  expect(KB_DIAGRAMS.map((d) => d.file)).toEqual(sources);
  for (const d of KB_DIAGRAMS) {
    expect(d.source).toBe(readFileSync(join(ARCHITECTURE, d.file), 'utf8'));
  }
});

test('no rendered copy of a diagram is committed, beside its source or beside the page', () => {
  // A render is a second copy of the source that nothing regenerates;
  // re-adding one re-opens 4718d918. Name what was found, so the
  // failure says which file to delete.
  const renders = readdirSync(ARCHITECTURE).filter((f) => !f.endsWith('.mmd'));
  expect(renders).toEqual([]);
  expect(existsSync(join(import.meta.dir, 'kb-assets'))).toBe(false);
});
