import { expect, test } from 'bun:test';

import { scaled } from '../src/dev-load';
import { runWithin } from './run-within';

const WEB_ROOT = new URL('..', import.meta.url).pathname.replace(/\/$/, '');

// The foreign server answers a shell and names its tree. A responsive
// server is not evidence that it serves this run's files (e89446d8).
// CI is a different supported selection path, not a foreign-tree probe.
// Clearing CI alone leaves Forgejo's inherited Actions marker active.
async function assertOwnTree(mode: 'local' | 'forge-ci'): Promise<void> {
  const foreign = Bun.serve({
    hostname: '127.0.0.1',
    port: 0,
    fetch(request) {
      return new URL(request.url).pathname === '/__boss_tree'
        ? Response.json({ tree: '/foreign/ownership-pin' })
        : new Response('<html><body>foreign tree</body></html>', {
            headers: { 'content-type': 'text/html' },
          });
    },
  });
  const env = {
    ...process.env,
    CI: mode === 'forge-ci' ? 'true' : '',
    GITHUB_ACTIONS: mode === 'forge-ci' ? 'true' : '',
    FORGEJO_ACTIONS: mode === 'forge-ci' ? 'true' : '',
    PWTEST_SKIP_DEVSERVER: '',
    PORT: String(foreign.port),
    BOSS_MOCKED_TEST_DIR: './tests/ownership-pin',
    BOSS_EXPECTED_TREE: WEB_ROOT,
  };
  const run = (leg: string, command: string[]) =>
    runWithin(`${mode} ${leg}`, command, { cwd: WEB_ROOT, env, budgetMs: LEG_BUDGET_MS });
  try {
    const direct = await run('direct Playwright', ['bun', 'x', 'playwright', 'test', '-c', 'playwright.mocked.config.ts']);
    const safe = await run('mocked runner', ['bun', 'tests/run-mocked.ts']);
    expect(safe.code).toBe(0);
    expect(safe.output).toContain(mode === 'forge-ci'
      ? `CI: :${foreign.port} is occupied, starting our own on a free port`
      : `:${foreign.port} is serving a DIFFERENT tree (/foreign/ownership-pin) — not reusing it`);
    expect(safe.output).not.toContain('[mocked-runner] reusing the server');
    expect(safe.output).toContain(`serving tree: ${WEB_ROOT}`);
    expect(safe.output).toContain('1 passed');
    // A failed identity assertion would also exit nonzero. The direct
    // path must refuse BEFORE it runs a browser against foreign files.
    expect(direct.code).not.toBe(0);
    expect(direct.output).toContain('already used');
    expect(direct.output).not.toContain('Running 1 test');
  } finally {
    foreign.stop(true);
  }
}

// Each leg's budget, and the test's: two legs and a margin, so a leg that
// runs out fails as that leg, with its output, and never as the test
// (backlog 3f421243). Train gate c773d182 killed the forge-ci case at a
// flat 300 s with nothing to read, while the local case took 29.8 s; a
// leg takes about 3 s on a quiet worker, and the whole file 12-50 s in
// the gate (web_timings of the 59 gate-runs before it). Stated quiet and
// scaled like every web budget (src/dev-load.ts): 35 s a leg here,
// 140 s in the gate, where the test keeps the 300 s it had.
const LEG_BUDGET_MS = scaled(35_000);
const TEST_BUDGET_MS = 2 * LEG_BUDGET_MS + scaled(5_000);

for (const mode of ['local', 'forge-ci'] as const) {
  test(`direct Playwright refuses a foreign server; ${mode} runner tests its own tree`,
    () => assertOwnTree(mode), TEST_BUDGET_MS);
}
