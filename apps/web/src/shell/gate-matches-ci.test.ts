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

const read = (p: string) => readFileSync(new URL(p, import.meta.url), 'utf8');

/// BOTH workflows, because for a long time this test read only the
/// first one — and the first one does not gate anything.
///
/// `.github` runs on GitHub Actions against the PUBLIC MIRROR, which
/// is pushed by hand with David's sign-off. `.forgejo` runs on the
/// internal forge and is what a train must pass before it merges. The
/// mirror had a `web` job (typecheck, unit, build, mocked smoke) from
/// 2026-08-08; the forge had none until 2026-08-16. So this test
/// asserted a real property of a workflow nobody's branch had to
/// satisfy, and the mocked suite went unrun for long enough that its
/// route-smoke drift test was sitting red against the `/it/*` rename.
const workflows = {
  forge: read('../../../../.forgejo/workflows/ci.yml'),
  mirror: read('../../../../.github/workflows/ci.yml'),
};

const pkg = JSON.parse(read('../../package.json')) as {
  scripts: Record<string, string>;
};

const scriptsIn = (yml: string) =>
  new Set(
    [...yml.matchAll(/run:\s*bun run ([a-z:]+)/g)].map((m) => m[1]!).filter((s) => s !== 'gate'),
  );

/// Every `bun run <script>` invoked anywhere in either workflow, minus
/// `gate` itself (CI runs the steps individually so a failure names the
/// step that broke rather than one opaque red X).
const ciScripts = new Set([...scriptsIn(workflows.forge), ...scriptsIn(workflows.mirror)]);

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

// The union above is the right check for "can this fail after I push",
// but it hides WHICH workflow runs what — and that distinction is the
// whole defect. A script present only on the mirror runs only when
// somebody pushes the mirror by hand.
describe('the forge — not the mirror — is what gates a train', () => {
  const forge = scriptsIn(workflows.forge);

  test('the forge runs the frontend suite', () => {
    // `test:mocked` is the only layer that mounts a page in a browser,
    // so it is the only one that catches a runtime crash: svelte-check
    // passes when the *type* is the thing that is wrong, and the unit
    // suite never mounts a component. Losing it from the forge means
    // losing crash coverage entirely, which is what happened.
    for (const script of ['typecheck', 'test:unit', 'build', 'test:mocked']) {
      expect(
        forge.has(script),
        `.forgejo/workflows/ci.yml does not run \`bun run ${script}\` — ` +
          `the internal forge is what a train must pass, so a frontend check ` +
          `that lives only in .github runs only when the mirror is pushed by hand`,
      ).toBe(true);
    }
  });

  test('the scrape found the forge workflow at all', () => {
    // Guards the test above against passing vacuously if the file moves
    // or the step syntax changes.
    expect(forge.size).toBeGreaterThanOrEqual(4);
  });
});
