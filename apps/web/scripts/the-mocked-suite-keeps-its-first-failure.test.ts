// A forge retry can turn a failed first attempt into a green suite.
// Read the real configuration in separate processes: importing it once
// would cache the CI value and leave the other execution lanes untested.
import { expect, test } from 'bun:test';

import { scaled } from '../src/dev-load';

const configUrl = new URL('../playwright.mocked.config.ts', import.meta.url).href;
const readRetries = `import config from ${JSON.stringify(configUrl)};
console.log(JSON.stringify({ retries: config.retries }));`;

/// How long one child may take to import the config and print it
/// (backlog a653fe65, 2026-10-09). It was a flat 10 000 ms, the one
/// budget in this file that the gate's load did not scale, and train
/// gate 09d635a3 killed all three children at it (10 802 / 10 012 /
/// 10 011 ms, exit null) — a child that takes ~330 ms on a quiet dev pod,
/// importing @playwright/test from a node_modules the loaded gate reads
/// hundreds of times slower. Stated for a quiet run and scaled like
/// every other web budget (src/dev-load.ts): 25 s quiet, 100 s in the
/// gate — each just under the runner's per-test budget (30 s, 120 s),
/// so a child that hangs is reported as the child's kill, below, and
/// not as a test timeout. spawnSync blocks the test's thread, so this
/// is the only clock that can stop a hung child at all.
const CHILD_BUDGET_MS = scaled(25_000);

for (const ci of [undefined, 'true', 'false'] as const) {
  test(`the actual mocked configuration keeps first failures with CI=${ci ?? 'unset'}`, () => {
    const { CI: _inheritedCi, ...environment } = process.env;
    const result = Bun.spawnSync([process.execPath, '-e', readRetries], {
      env: ci === undefined ? environment : { ...environment, CI: ci },
      stdout: 'pipe',
      stderr: 'pipe',
      timeout: CHILD_BUDGET_MS,
    });

    // One expectation, so a killed child says it was killed, and at
    // which budget, instead of "Expected: 0, Received: null".
    expect({
      exitCode: result.exitCode,
      killedAtBudget: result.exitedDueToTimeout === true ? CHILD_BUDGET_MS : null,
      stderr: result.stderr.toString(),
    }).toEqual({ exitCode: 0, killedAtBudget: null, stderr: '' });
    const configuration: unknown = JSON.parse(result.stdout.toString());
    expect(configuration).toEqual({ retries: 0 });
  });
}
