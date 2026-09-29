// Mermaid is served as its own prebuilt ESM files, beside the SPA
// bundle rather than inside it (backlog 4718d918). See mermaid-vendor.ts
// for why: imported through the bundler, the dev-server inlined it into
// EVERY page's script (7.3 MB -> 16.8 MB) and a page that never draws a
// diagram took twice as long to mount. These pin the two halves that
// serve it: the build's copy, and the dev-server's lookup.

import { afterAll, expect, test } from 'bun:test';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { copyMermaidInto, vendoredMermaidFile } from './mermaid-vendor';
import { MERMAID_ENTRY, MERMAID_ROUTE } from './it/mermaidUrl';

const out = mkdtempSync(join(tmpdir(), 'mermaid-vendor-'));
afterAll(() => rmSync(out, { recursive: true, force: true }));

/** Every relative module a vendored file imports, statically or not. */
function importsOf(file: string): ReadonlyArray<string> {
  const text = readFileSync(file, 'utf8');
  const found = [...text.matchAll(/(?:from|import)\s*\(?\s*"(\.{1,2}\/[^"]+\.mjs)"/g)];
  return found.map((m) => join(dirname(file), m[1] ?? ''));
}

test('the build copies the entry and every module it can reach, and nothing it cannot', () => {
  const copied = copyMermaidInto(out);
  const root = join(out, MERMAID_ROUTE);
  const entry = join(root, MERMAID_ENTRY);
  expect(existsSync(entry)).toBe(true);
  // Walk the import graph from the entry: a module the browser will ask
  // for and the copy left behind is a diagram that never draws.
  const seen = new Set<string>();
  const queue = [entry];
  while (queue.length > 0) {
    const next = queue.pop() ?? '';
    if (seen.has(next)) continue;
    seen.add(next);
    expect(existsSync(next)).toBe(true);
    queue.push(...importsOf(next));
  }
  expect(seen.size).toBeGreaterThan(10);
  expect(copied).toBeGreaterThanOrEqual(seen.size);
});

test('the dev-server serves the entry and its chunks, and refuses anything else', () => {
  const entry = vendoredMermaidFile(`${MERMAID_ROUTE}${MERMAID_ENTRY}`);
  expect(entry !== null && existsSync(entry)).toBe(true);
  const chunk = importsOf(entry ?? '')[0] ?? '';
  const chunkPath = `${MERMAID_ROUTE}${chunk.slice(dirname(entry ?? '').length + 1)}`;
  expect(vendoredMermaidFile(chunkPath)).not.toBeNull();
  for (const refused of [
    `${MERMAID_ROUTE}../package.json`,
    `${MERMAID_ROUTE}chunks/mermaid.esm.min/../../mermaid.core.mjs`,
    `${MERMAID_ROUTE}${MERMAID_ENTRY}.map`,
    `${MERMAID_ROUTE}mermaid.core.mjs`,
    `${MERMAID_ROUTE}chunks/mermaid.core/x.mjs`,
    '/vendor/other/x.mjs',
    // Another release's path: the version is in the route because the
    // gateway caches every asset as immutable (mermaidUrl.ts).
    `/vendor/mermaid-0.0.0/${MERMAID_ENTRY}`,
  ]) {
    expect(vendoredMermaidFile(refused)).toBeNull();
  }
});
