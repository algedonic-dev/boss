// Mermaid, served as its own prebuilt ESM files beside the SPA bundle
// (backlog 4718d918). Server/build side only — the browser needs just
// the URL, it/mermaidUrl.ts.
//
// WHY NOT `import('mermaid')`. /it/kb draws the architecture diagrams
// from their Mermaid source in the browser, so no committed render can
// drift from it — and the renderer was meant to load on that page only.
// `Bun.build` does split a dynamic import into its own chunks, but the
// dev-server's HTML bundle does not: it inlined Mermaid into the one
// script every page loads, 7.3 MB -> 16.8 MB, and a page that never
// draws a diagram (/it/estate, measured 2026-09-28, 8 loads each) took
// ~1.0-1.5 s to mount instead of ~0.5 s. The mocked suite loads that
// script for every test it runs. Loading Mermaid by URL keeps it out of
// both bundles: the build copies its ESM entry and the chunks that
// entry imports into dist/vendor/mermaid/ (the gateway serves dist as
// is), and the dev-server answers the same paths from node_modules.

import { cpSync, existsSync, mkdirSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { MERMAID_ENTRY, MERMAID_ROUTE } from './it/mermaidUrl';

/** The chunk directory the ESM entry's relative imports reach. */
const CHUNKS = 'chunks/mermaid.esm.min';

/** The installed mermaid package's dist/ directory. */
function mermaidDist(): string {
  return dirname(Bun.resolveSync('mermaid', import.meta.dir));
}

/** The one shape of name served: the entry, or a chunk module. No
 *  separators beyond the chunk directory, so nothing can climb out. */
function servedName(rel: string): boolean {
  return rel === MERMAID_ENTRY || new RegExp(`^${CHUNKS}/[A-Za-z0-9_-]+\\.mjs$`).test(rel);
}

/** The file on disk a dev-server request for `pathname` names, or null
 *  when the path is not one Mermaid's ESM build serves. */
export function vendoredMermaidFile(pathname: string): string | null {
  if (!pathname.startsWith(MERMAID_ROUTE)) return null;
  const rel = pathname.slice(MERMAID_ROUTE.length);
  if (!servedName(rel)) return null;
  const file = join(mermaidDist(), rel);
  return existsSync(file) ? file : null;
}

/** Copy the entry and its chunk modules (no source maps) into
 *  `<outDir>/vendor/mermaid/`; returns how many files it copied. Throws
 *  when the entry is missing, so a build never ships a page that cannot
 *  draw. */
export function copyMermaidInto(outDir: string): number {
  const dist = mermaidDist();
  const target = join(outDir, MERMAID_ROUTE);
  if (!existsSync(join(dist, MERMAID_ENTRY))) {
    throw new Error(`mermaid-vendor: ${join(dist, MERMAID_ENTRY)} is missing — is mermaid installed?`);
  }
  mkdirSync(join(target, CHUNKS), { recursive: true });
  const names = [
    MERMAID_ENTRY,
    ...readdirSync(join(dist, CHUNKS)).map((f) => `${CHUNKS}/${f}`),
  ].filter(servedName);
  names.forEach((rel) => cpSync(join(dist, rel), join(target, rel)));
  return names.length;
}
