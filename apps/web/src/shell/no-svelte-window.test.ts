// `<svelte:window>` and friends are banned in this app.
//
// The bun+svelte bundler crashes on the svelte:window event lookup —
// `$.window` resolves undefined — and the failure is not local to the
// component that used it. The whole app goes down: `.app-shell` never
// mounts, and every route that renders the offending component dies
// with "Cannot read properties of undefined (reading
// 'addEventListener')".
//
// DebugGear.svelte hit this first and left a comment explaining the
// workaround. A comment in one file is not a mechanism: TriageBoard
// later reached for the obvious construct to wire Escape-to-close, and
// took down eleven mocked specs and the whole route-smoke crawl. The
// unit suite stayed green throughout, because a bundler crash is only
// visible once something renders the page.
//
// The replacement is a plain `$effect` with addEventListener and a
// teardown — same behaviour, no bundler-internal dependency:
//
//   $effect(() => {
//     function onKeyDown(e: KeyboardEvent) { ... }
//     window.addEventListener('keydown', onKeyDown);
//     return () => window.removeEventListener('keydown', onKeyDown);
//   });
//
// This is CLAUDE.md §9a applied to a construct rather than a value: the
// knowledge lived in one file's comment, drifted the moment someone
// edited a different file, and now has a test that names the offender.

import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

/// Every `.svelte` file under a root, recursively.
function svelteFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    if (entry === 'node_modules' || entry === '.svelte-kit') continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) out.push(...svelteFiles(full));
    else if (entry.endsWith('.svelte')) out.push(full);
  }
  return out;
}

const ROOTS = [
  new URL('../', import.meta.url).pathname, // apps/web/src
  new URL('../../../../libs/web-kit/src/', import.meta.url).pathname,
];

/// The special elements that go through the same broken lookup.
const BANNED = ['<svelte:window', '<svelte:document', '<svelte:body'];

describe('no svelte:window', () => {
  test('no component uses a special element the bundler cannot resolve', () => {
    const offenders: string[] = [];
    for (const root of ROOTS) {
      for (const file of svelteFiles(root)) {
        const src = readFileSync(file, 'utf8');
        // Strip comments first — this file's own guidance, and the
        // explanatory comments in TriageBoard/DebugGear, name the
        // construct precisely so that they can explain it.
        const code = src.replace(/<!--[\s\S]*?-->/g, '').replace(/\/\/.*$/gm, '');
        for (const banned of BANNED) {
          if (code.includes(banned)) {
            offenders.push(`${file.replace(/.*\/(apps|libs)\//, '$1/')} uses ${banned}`);
          }
        }
      }
    }
    expect(
      offenders.sort(),
      'these crash the bun+svelte bundler at runtime and take the whole app down — ' +
        'use a $effect with addEventListener + a teardown instead (see TriageBoard.svelte)',
    ).toEqual([]);
  });

  test('the scan actually reads files, so a green result means something', () => {
    // Both assertions above pass vacuously against an empty file list —
    // a moved directory or a changed extension would silently disable
    // the lint.
    const all = ROOTS.flatMap((r) => svelteFiles(r));
    expect(all.length).toBeGreaterThan(50);
  });
});
