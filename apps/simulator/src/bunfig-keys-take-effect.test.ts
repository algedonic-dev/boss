// EVERY KEY apps/simulator/bunfig.toml SETS IS ONE BUN APPLIES
// (backlog ae162cb4) — the simulator's half of
// apps/web/scripts/bunfig-keys-take-effect.test.ts, which says why.
//
// Its one key registers bun-plugin-svelte for `bun run dev`, and nothing
// in the gate boots this dev-server, so an ignored key here would be
// silent twice over: the server still answers 200 with a blank page.
// The per-test budget is stated on the test because this package runs
// `bun test src/` bare, at bun's 5 000 ms default.
import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { scaled } from '../../web/src/dev-load';
import { NO_BUNFIG, readDevBundle } from '../../web/scripts/serve-static-plugins';

const SIMULATOR = join(import.meta.dir, '..');

test('the bunfig sets only [serve.static] plugins, the key read back below', () => {
  const keys = readFileSync(join(SIMULATOR, 'bunfig.toml'), 'utf8')
    .split('\n')
    .map((l) => l.replace(/#.*$/, '').trim())
    .filter((l) => /^[A-Za-z0-9_.-]+\s*=/.test(l))
    .map((l) => l.split('=')[0]!.trim());
  expect(keys).toEqual(['plugins']);
});

test(
  '[serve.static] plugins: the dev-server compiles the root component, and an empty bunfig does not',
  async () => {
    const withBunfig = await readDevBundle(SIMULATOR);
    expect(withBunfig.compiledRoot, `with apps/simulator/bunfig.toml (${withBunfig.answered}):\n${withBunfig.output}`).toBe(true);
    const without = await readDevBundle(SIMULATOR, NO_BUNFIG);
    expect(without.answered, without.output).toBe('/ 200, script 200');
    expect(without.compiledRoot, 'with an empty bunfig').toBe(false);
  },
  // Two dev-server boots, each bundling the SPA: 5.0 s alone on the dev
  // pod, 30 155 ms and red in a loaded train gate (backlog ebb750cd), so
  // the budget scales with the load the gate-runner declares.
  scaled(30_000),
);
