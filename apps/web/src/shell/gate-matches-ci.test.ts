// `bun run gate` must run exactly what CI runs.
//
// This exists because of a specific, repeated failure: changes passed
// locally and broke in CI, over and over, and the reason was that the
// two command sets barely overlapped.
//
//   CI ran:        typecheck, build, test:mocked
//   I ran locally: typecheck, test:unit
//
// The only shared step was `typecheck`. So `test:mocked` — the
// Playwright suite, the only gate that actually renders a page and
// therefore the only one that catches a runtime crash — never ran
// before a push. And `test:unit` (195 tests, including every equality
// test in this directory) never ran in CI, so it was enforced only by
// whoever remembered to type it.
//
// That is two facts living in two places with nothing keeping them in
// step, which is exactly the shape CLAUDE.md §9a is about. The fix is
// one command, `bun run gate`, plus this test asserting it covers every
// `bun run <script>` the workflow invokes.
//
// If you add a step to the web job in ci.yml, this fails until `gate`
// includes it. That is the point.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

const ci = readFileSync(
  new URL('../../../../.github/workflows/ci.yml', import.meta.url),
  'utf8',
);
const pkg = JSON.parse(
  readFileSync(new URL('../../package.json', import.meta.url), 'utf8'),
) as { scripts: Record<string, string> };

/// Every `bun run <script>` invoked anywhere in the workflow, minus
/// `gate` itself (CI runs the steps individually so a failure names the
/// step that broke rather than one opaque red X).
const ciScripts = new Set(
  [...ci.matchAll(/run:\s*bun run ([a-z:]+)/g)].map((m) => m[1]!).filter((s) => s !== 'gate'),
);

const gate = pkg.scripts['gate'] ?? '';
const gateScripts = new Set([...gate.matchAll(/bun run ([a-z:]+)/g)].map((m) => m[1]!));

describe('the local gate matches CI', () => {
  test('gate runs every script CI runs', () => {
    const missing = [...ciScripts].filter((s) => !gateScripts.has(s)).sort();
    expect(
      missing,
      `CI runs these but \`bun run gate\` does not, so they can only fail after you push: ` +
        `${missing.join(', ')}`,
    ).toEqual([]);
  });

  test('gate runs nothing CI does not', () => {
    // The other direction matters less but still costs trust: a gate
    // that runs extra things is a gate people stop running.
    const extra = [...gateScripts].filter((s) => !ciScripts.has(s)).sort();
    expect(
      extra,
      `\`bun run gate\` runs these but CI does not: ${extra.join(', ')}`,
    ).toEqual([]);
  });

  test('every script named actually exists', () => {
    const unknown = [...gateScripts].filter((s) => !(s in pkg.scripts)).sort();
    expect(unknown, `gate references undefined scripts: ${unknown.join(', ')}`).toEqual([]);
  });

  test('the scrape found something, so a green result means something', () => {
    // Both directions above pass vacuously against two empty sets — a
    // reformatted workflow or a renamed job would silently disable this.
    expect(ciScripts.size).toBeGreaterThanOrEqual(4);
    expect(gateScripts.size).toBeGreaterThanOrEqual(4);
  });
});
