import { copyFile, mkdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';

// One generic default for both bundling and the dev server. Deployment
// configuration is delivered separately; it is never baked into JS.
const defaultFile = new URL('../instance-config/dev-door.json', import.meta.url);

export async function stageInstanceConfig(out: string): Promise<void> {
  const dir = join(out, 'instance-config');
  await mkdir(dir, { recursive: true });
  await copyFile(defaultFile, join(dir, 'dev-door.json'));
}

export async function serveDevDoorDefault(): Promise<Response> {
  try {
    return new Response(await readFile(defaultFile, 'utf8'), {
      headers: { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' },
    });
  } catch {
    return new Response('dev door declaration unavailable', { status: 503 });
  }
}
