import { describe, expect, test } from 'bun:test';

import { scaled } from '../src/dev-load';
import { runWithin } from './run-within';

// Backlog 3f421243: train gate c773d182 killed the direct-Playwright pin
// at its 300 s test timeout with nothing to read — both of its children
// were awaited to the end of their output, so bun's timeout discarded
// everything they had printed. A leg now owns a budget below the test's
// and, when it runs out, fails with the leg's name and its whole output,
// whose last line is the stage it was waiting on.
describe('runWithin — a child that outlives its budget says where it was (3f421243)', () => {
  test('a child that exits in time returns its code and both streams', async () => {
    const result = await runWithin('quick leg', ['/bin/sh', '-c', 'echo to-out; echo to-err >&2; exit 3'], {
      cwd: import.meta.dir,
      env: process.env,
      budgetMs: scaled(20_000),
    });
    expect(result.code).toBe(3);
    expect(result.output).toContain('to-out');
    expect(result.output).toContain('to-err');
  }, scaled(25_000));

  test('a child still running at its budget is killed and named, with everything it printed', async () => {
    const budgetMs = scaled(1_000);
    const started = Date.now();
    let refusal = '';
    try {
      await runWithin('stuck leg', ['/bin/sh', '-c', 'echo starting the bundle; exec sleep 600'], {
        cwd: import.meta.dir,
        env: process.env,
        budgetMs,
      });
    } catch (err) {
      refusal = err instanceof Error ? err.message : String(err);
    }
    expect(refusal).toContain(`stuck leg: /bin/sh -c echo starting the bundle; exec sleep 600 had not exited after ${budgetMs} ms`);
    expect(refusal).toContain('starting the bundle');
    // Killed at the budget, not left to the test's own timeout.
    expect(Date.now() - started).toBeLessThan(budgetMs + scaled(5_000));
  }, scaled(10_000));
});
