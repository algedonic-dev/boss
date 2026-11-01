// A MOCKED RUN NEVER BORROWS A SERVER ANOTHER RUN WILL STOP (backlog
// 14ab6f08).
//
// tests/run-mocked.ts reused any dev-server answering TREE_PATH with
// its own tree, and stopped the server it had started the moment its own
// Playwright exited. Two runs in one worktree therefore shared a server
// whose lifetime was only the first run's: measured 2026-10-11 by
// agent-run b9e9795a, the owner's 8 s spec went green, it stopped its
// server, and the borrower's 12 s follow-up navigation to TREE_PATH died
// ERR_CONNECTION_REFUSED.
//
// src/dev-tree.test.ts pins chooseTarget against hand-built answers.
// This file pins the joint those cannot see: what the REAL dev-server
// says about itself, started the way each of its starters starts it.
// Mocked mode is how every run starts one (tests/run-mocked.ts,
// playwright.mocked.config.ts, scripts/serve-static-plugins.ts), and
// each stops it when the run ends, so that server must not be borrowed;
// a standing `bun run dev` (not mocked) is the warm server reuse exists
// for, and must still be. /__boss_tree answers before the SPA bundle, so
// neither leg pays for a bundle.
import { afterEach, expect, test } from 'bun:test';

import { MOCKED_FLAG } from '../src/dev-mocked';
import { waitForReadyLine } from '../src/dev-ready';
import { chooseTarget, freePort } from '../src/dev-tree';

// apps/web — this file is in scripts/.
const WEB_ROOT = new URL('..', import.meta.url).pathname;

// A server that binds in milliseconds on an idle pod; the bound is for
// a starved gate host, where even the import graph crawls.
const LISTENING_TIMEOUT_MS = 60_000;

const started: ReturnType<typeof Bun.spawn>[] = [];

afterEach(async () => {
  for (const server of started.splice(0)) {
    server.kill('SIGTERM');
    await server.exited;
  }
});

async function startDevServer(mocked: boolean): Promise<number> {
  const port = await freePort();
  const server = Bun.spawn(['bun', 'src/dev-server.ts'], {
    cwd: WEB_ROOT,
    env: { ...process.env, PORT: String(port), BOSS_SCRATCH: '0', [MOCKED_FLAG]: mocked ? '1' : '' },
    stdout: 'pipe',
    stderr: 'inherit',
    stdin: 'ignore',
  });
  started.push(server);
  await waitForReadyLine(server.stdout, port, LISTENING_TIMEOUT_MS, () => undefined);
  return port;
}

// The server is up and answers, so no probe deadline a starved pod
// could reach decides the verdict (backlog 75335234, as in
// dev-tree.test.ts); a real hang is the per-test budget's to report.
const ANSWERED = { probeTimeoutMs: 10 * 60_000 } as const;

test(
  'a second run in the same tree starts its own server rather than borrow a run-owned one',
  async () => {
    const port = await startDevServer(true);

    const target = await chooseTarget(port, {}, ANSWERED);

    expect(target.reuse).toBe(false);
    expect(target.port).not.toBe(port);
    expect(target.reason).toContain('another run');
  },
  120_000,
);

test(
  'a standing same-tree dev-server is still reused',
  async () => {
    const port = await startDevServer(false);

    const target = await chooseTarget(port, {}, ANSWERED);

    expect(target.reuse).toBe(true);
    expect(target.port).toBe(port);
  },
  120_000,
);
