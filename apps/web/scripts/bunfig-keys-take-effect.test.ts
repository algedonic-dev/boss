// EVERY KEY apps/web/bunfig.toml SETS IS ONE BUN APPLIES (backlog ae162cb4).
//
// `[test] timeout = 30000` sat in that file from 2026-09-08 to
// 2026-09-25 and bun 1.3.14 ignored it: the file was read — its preload
// ran — and every unit test still ran at 5 000 ms (backlog 75335234; the
// budget now rides the command line, pinned by
// each-test-file-alone.test.ts). A key that is read and ignored is a
// check that is not running, and a test of the file's TEXT cannot tell
// it apart from one that works. So each key the file sets is read back
// here by its EFFECT, and the file may set no key this list does not
// name — a new key arrives with its own effect test or not at all.
//
// The sweep, 2026-09-27: the web tree's tool config is this bunfig,
// apps/simulator/bunfig.toml (its one key is read back in
// apps/simulator/src/bunfig-keys-take-effect.test.ts) and the two
// Playwright configs (the mocked one read back from inside a running
// test by tests/mocked/the-config-is-what-playwright-applies.mocked.spec.ts;
// the live one's keys are the same kind, read by the same runner, and it
// runs only against an instance). tsconfig.json needs no such test:
// tsc and svelte-check refuse an unknown compiler option by name.
import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { NO_BUNFIG, readDevBundle } from './serve-static-plugins';

const WEB = join(import.meta.dir, '..');

/** Every `[section] key` the bunfig sets, comments skipped. */
function keysSet(toml: string): string[] {
  let section = '';
  return toml.split('\n').flatMap((raw) => {
    const line = raw.replace(/#.*$/, '').trim();
    const head = line.match(/^\[([^\]]+)\]$/);
    if (head) {
      section = head[1]!;
      return [];
    }
    const key = line.match(/^([A-Za-z0-9_.-]+)\s*=/)?.[1];
    return key ? [`[${section}] ${key}`] : [];
  });
}

test('the bunfig sets exactly the keys this file reads back', () => {
  expect(keysSet(readFileSync(join(WEB, 'bunfig.toml'), 'utf8')).sort()).toEqual(
    ['[serve.static] plugins', '[test] preload'].sort(),
  );
});

test('[test] preload: the rune shim ran before this file', () => {
  // This file imports nothing that defines `$state`; only the preload does.
  expect(typeof (globalThis as { $state?: unknown }).$state).toBe('function');
});

test(
  '[serve.static] plugins: the dev-server compiles the root component, and an empty bunfig does not',
  async () => {
    const withBunfig = await readDevBundle(WEB);
    expect(withBunfig.compiledRoot, `with apps/web/bunfig.toml (${withBunfig.answered}):\n${withBunfig.output}`).toBe(true);
    // The control: the same boot with every key unset. If this compiled
    // too, the marker would prove nothing about the key.
    // And it must still SERVE — a control that never booted is not one.
    const without = await readDevBundle(WEB, NO_BUNFIG);
    expect(without.answered, without.output).toBe('/ 200, script 200');
    expect(without.compiledRoot, 'with an empty bunfig').toBe(false);
  },
  30_000,
);
