import { expect, test } from 'bun:test';

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
  async function run(command: string[]): Promise<{ code: number; output: string }> {
    const process = Bun.spawn(command, {
      cwd: WEB_ROOT, env, stdout: 'pipe', stderr: 'pipe', stdin: 'ignore',
    });
    const [out, err, code] = await Promise.all([
      new Response(process.stdout).text(), new Response(process.stderr).text(), process.exited,
    ]);
    return { code, output: `${out}\n${err}` };
  }
  try {
    const direct = await run(['bun', 'x', 'playwright', 'test', '-c', 'playwright.mocked.config.ts']);
    const safe = await run(['bun', 'tests/run-mocked.ts']);
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

for (const mode of ['local', 'forge-ci'] as const) {
  test(`direct Playwright refuses a foreign server; ${mode} runner tests its own tree`,
    () => assertOwnTree(mode), 300_000);
}
