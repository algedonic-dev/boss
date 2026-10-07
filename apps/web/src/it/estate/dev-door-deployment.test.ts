import { afterEach, expect, test } from 'bun:test';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { parseDevDoor } from './estate';

const root = fileURLToPath(new URL('../../../../../', import.meta.url));
const scratch: string[] = [];
afterEach(() => { for (const path of scratch.splice(0)) rmSync(path, { recursive: true, force: true }); });
const dir = (): string => { const path = mkdtempSync(join(tmpdir(), 'dev-door-declaration-')); scratch.push(path); return path; };
const declaration = readFileSync(join(root, 'infra/estate/dev-door.json'), 'utf8');
const malformedHostDeclarations = [
  'workspace.example.test\n', 'workspace.example.test\r\n', '\nworkspace.example.test',
  'workspace\n.example.test', 'workspace.example.test\t', 'workspace.example.test ',
  'workspace.example.test\0', 'workspace.example.tést',
].map((host) => JSON.stringify({ ...(JSON.parse(declaration) as Record<string, unknown>), host }));

test('actual deployment staging preserves bytes for this and a second deployment and an explicit none', () => {
  for (const source of [declaration, declaration.replaceAll('dev.algedonic.dev', 'workspace.example.test'), '{}\n']) {
    const path = dir();
    writeFileSync(join(path, 'input.json'), source);
    const out = join(path, 'output');
    const run = Bun.spawnSync(['bash', join(root, 'infra/estate/stage-dev-door.sh'), join(path, 'input.json'), out]);
    expect(run.exitCode).toBe(0);
    expect(readFileSync(join(out, 'dev-door.json'), 'utf8')).toBe(source);
    expect(() => parseDevDoor(JSON.parse(source) as unknown)).not.toThrow();
  }
});

test('staging refuses missing, multi-document and malformed declarations before emitting a replacement', () => {
  for (const source of [null, '', ' \n\t\n', '{}\n{}', '[]', '{bad', '{"host":7}', '{"host":""}', '{"host":"x;exit"}', '{"host":"x.test"}', '{"host":null,"steps":"unknown"}', '{"steps":[null]}', ...malformedHostDeclarations]) {
    const path = dir();
    if (source !== null) writeFileSync(join(path, 'input.json'), source);
    const out = join(path, 'output');
    const run = Bun.spawnSync(['bash', join(root, 'infra/estate/stage-dev-door.sh'), join(path, 'input.json'), out]);
    expect(run.exitCode).toBe(2);
    expect(existsSync(join(out, 'dev-door.json'))).toBe(false);
  }
});

test('unsupported hostname bytes are refused before rendering a replacement ConfigMap', () => {
  for (const source of malformedHostDeclarations) {
    const path = dir();
    writeFileSync(join(path, 'input.json'), source);
    const out = join(path, 'output');
    const run = Bun.spawnSync(['bash', join(root, 'infra/estate/render-dev-door-config.sh'), join(path, 'input.json'), out, 'test-install']);
    expect(run.exitCode).toBe(2);
    expect(existsSync(join(out, 'dev-door.json'))).toBe(false);
    expect(existsSync(join(out, 'configmap.json'))).toBe(false);
    expect(() => parseDevDoor(JSON.parse(source) as unknown)).toThrow();
  }
});

test('the real converge function applies only a complete validated declaration through its existing write port', () => {
  const runner = readFileSync(join(root, 'infra/forge/cluster-deploy-runner.sh'), 'utf8');
  const actual = runner.match(/converge_dev_door\(\) \{[\s\S]*?\n\}/)?.[0];
  expect(actual).toBeDefined();
  for (const source of [declaration, declaration.replaceAll('dev.algedonic.dev', 'workspace.example.test'), '{}\n', '', ' \n\t\n', '{bad', '{}\n{}', null, ...malformedHostDeclarations]) {
    const path = dir();
    const repo = join(path, 'repo');
    Bun.spawnSync(['mkdir', '-p', join(repo, 'infra/estate')]);
    Bun.spawnSync(['mkdir', '-p', join(repo, 'infra/lib')]);
    writeFileSync(join(repo, 'infra/lib/jq.sh'), readFileSync(join(root, 'infra/lib/jq.sh')));
    for (const name of ['stage-dev-door.sh', 'render-dev-door-config.sh']) {
      writeFileSync(join(repo, 'infra/estate', name), readFileSync(join(root, 'infra/estate', name)));
    }
    if (source !== null) writeFileSync(join(repo, 'infra/estate/dev-door.json'), source);
    const capture = join(path, 'applied.json');
    const script = `set -euo pipefail\nREPO="$1"\nAPPLY_DIR="$2"\nCAPTURE="$3"\nKAPPLY=apply_port\napply_port() { test "$*" = 'apply -f -'; cat > "$CAPTURE"; }\n${actual}\nconverge_dev_door test-install\n`;
    const fixture = join(path, 'run.sh');
    writeFileSync(fixture, script);
    const run = Bun.spawnSync(['bash', fixture, repo, path, capture]);
    const valid = source !== null && source !== '' && source !== ' \n\t\n' && source !== '{bad' && source !== '{}\n{}' && !malformedHostDeclarations.includes(source);
    if (valid) {
      expect(run.exitCode).toBe(0);
      const applied = JSON.parse(readFileSync(capture, 'utf8')) as { metadata: { name: string; namespace: string }; data: Record<string, string> };
      expect(applied.metadata).toEqual({ name: 'boss-instance-config', namespace: 'test-install' });
      expect(applied.data['dev-door.json']).toBe(source);
    } else {
      expect(run.exitCode).not.toBe(0);
      expect(existsSync(capture)).toBe(false);
    }
  }
});
