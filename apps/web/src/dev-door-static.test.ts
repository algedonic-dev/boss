import { expect, test } from 'bun:test';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

test('the actual packaged and dev-server default is explicit none without an instance hostname', async () => {
  const path = join(import.meta.dir, 'dev-door-static.ts');
  expect(await Bun.file(path).exists()).toBe(true);
  if (!await Bun.file(path).exists()) return;
  const port = await import('./dev-door-static');
  const dir = await mkdtemp(join(tmpdir(), 'dev-door-default-'));
  try {
    await port.stageInstanceConfig(dir);
    const bytes = await readFile(join(dir, 'instance-config/dev-door.json'), 'utf8');
    expect(JSON.parse(bytes) as unknown).toEqual({});
    const response = await port.serveDevDoorDefault();
    expect(response.status).toBe(200);
    expect(await response.text()).toBe(bytes);
    expect(bytes).not.toContain('algedonic');
  } finally { await rm(dir, { recursive: true, force: true }); }
});
