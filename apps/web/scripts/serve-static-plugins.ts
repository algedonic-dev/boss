// DOES BUN APPLY `[serve.static] plugins`? — read by its effect
// (backlog ae162cb4).
//
// apps/web/bunfig.toml and apps/simulator/bunfig.toml each register
// bun-plugin-svelte for the dev-server's HTML bundle. Nothing checked
// that bun APPLIES the key, and an ignored key is silent here too:
// measured 2026-09-27 on bun 1.3.14, a dev-server booted with an empty
// bunfig (`bun --config=<empty> src/dev-server.ts`) still answers `/`
// 200 and serves its script 200 — the script just carries no compiled
// component, so the page is blank and no log says why. With the plugin
// applied the bundle carries Svelte's dev-mode compile of the root
// component, `App[$.FILENAME] = "src/App.svelte"`; without it that line
// is absent.
//
// So the check boots the app's own dev-server the way `bun run dev`
// does (from the app directory, so bun finds its bunfig), fetches the
// page and the script it names, and says whether the root component was
// compiled. Each caller also runs it once under an empty bunfig as the
// control, so the marker is proven to tell the two apart on the bun the
// gate has, not assumed to.

import { MOCKED_FLAG } from '../src/dev-mocked';

/** Svelte's dev-mode stamp on the compiled root component. */
export const COMPILED_ROOT = /\[\$\.FILENAME\] = "src\/App\.svelte"/;

/** An empty bunfig for the control leg: every key unset. */
export const NO_BUNFIG = '/dev/null';

export type BundleRead = Readonly<{
  /** What the page and its script answered, e.g. "/ 200, script 200". */
  answered: string;
  compiledRoot: boolean;
  /** The dev-server's own output, for a failure message. */
  output: string;
}>;

function freePort(): number {
  const probe = Bun.serve({ port: 0, fetch: () => new Response(null) });
  const port = probe.port ?? 0;
  void probe.stop(true);
  return port;
}

async function readOnce(origin: string): Promise<Omit<BundleRead, 'output'>> {
  const deadline = Date.now() + 25_000;
  let page: Response | null = null;
  while (page === null && Date.now() < deadline) {
    page = await fetch(`${origin}/`).catch(() => null);
    if (page === null) await Bun.sleep(100);
  }
  if (page === null) return { answered: '/ never answered', compiledRoot: false };
  const script = (await page.text()).match(/<script[^>]+src="([^"]+)"/)?.[1];
  if (script === undefined) return { answered: `/ ${page.status}, no script`, compiledRoot: false };
  const js = await fetch(new URL(script, origin));
  const body = await js.text();
  return { answered: `/ ${page.status}, script ${js.status}`, compiledRoot: COMPILED_ROOT.test(body) };
}

/**
 * Boot `bun src/dev-server.ts` in `appDir` — under `--config=<configPath>`
 * in place of the app's bunfig when one is given — read `/` and the
 * script it names, then stop the server. The environment is the mocked
 * runner's: no scratch proxy table, and mocked mode, so web's server
 * answers an unrouted /api/** locally.
 */
export async function readDevBundle(appDir: string, configPath?: string): Promise<BundleRead> {
  const port = freePort();
  const proc = Bun.spawn(
    [process.execPath, ...(configPath ? [`--config=${configPath}`] : []), 'src/dev-server.ts'],
    {
      cwd: appDir,
      env: { ...process.env, PORT: String(port), BOSS_SCRATCH: '0', [MOCKED_FLAG]: '1' },
      stdout: 'pipe',
      stderr: 'pipe',
    },
  );
  let read: Omit<BundleRead, 'output'>;
  try {
    read = await readOnce(`http://127.0.0.1:${port}`);
  } finally {
    proc.kill();
  }
  const [out, err] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()]);
  await proc.exited;
  return { ...read, output: out + err };
}
