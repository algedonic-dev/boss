import { afterEach, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import * as estate from './estate';
const { devDoorSteps } = estate;
const declaration = JSON.parse(readFileSync(new URL('../../../../../infra/estate/dev-door.json', import.meta.url), 'utf8')) as unknown;
const originalFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = originalFetch; });

// D4 of approved c6f08b60: absence must never become a command with
// an empty or invented destination. The existing default declaration
// is separately retained by the real reader/packaging controls.
test('an explicitly undeclared dev door renders no setup commands', () => {
  expect(devDoorSteps(null)).toEqual([]);
});

test('a blank dev door cannot produce a working-looking terminal block', () => {
  expect(() => devDoorSteps('')).toThrow();
});

test('the actual deployment reader distinguishes declared, none and unavailable input', async () => {
  // The optional port check gives a behavioral RED rather than a TS
  // missing-export error before the new deployment reader exists.
  const reader = (estate as unknown as { fetchDevDoor?: () => Promise<unknown> }).fetchDevDoor;
  expect(typeof reader).toBe('function');
  if (!reader) return;
  for (const [body, kind] of [[declaration, 'ready'], [{}, 'ready'], [{ host: 7 }, 'failed']] as const) {
    globalThis.fetch = Object.assign(async () => Response.json(body), { preconnect: originalFetch.preconnect });
    expect(await reader()).toMatchObject({ kind });
  }
  globalThis.fetch = Object.assign(async () => new Response('refused', { status: 503 }), { preconnect: originalFetch.preconnect });
  expect(await reader()).toMatchObject({ kind: 'failed' });
});

test('absence is an explicit valid envelope, never malformed or incomplete evidence', () => {
  expect(estate.parseDevDoor({})).toEqual({ host: null, steps: [] });
  expect(estate.parseDevDoor({ host: null })).toEqual({ host: null, steps: [] });
  for (const raw of [null, [], 'missing', { host: '' }, { host: 1 }, { host: 'x;exit' }, { host: 'x.test' }, { host: null, steps: 'unread' }, { steps: [null] }]) {
    expect(() => estate.parseDevDoor(raw)).toThrow();
  }
});

test('shell syntax and incomplete steps never reach a rendered command', () => {
  const current = estate.parseDevDoor(declaration);
  for (const host of ['x;exit', '$(id)', '`id`', 'x\nexit', 'x.test && id', '-x.test', 'x..test', 'x/test', 'x.test\"', "x.test'", 'workspace.example.test\n', 'workspace.example.test\r\n', '\nworkspace.example.test', 'workspace\n.example.test', 'workspace.example.test\t', 'workspace.example.test ', 'workspace.example.test\0', 'workspace.example.tést']) {
    expect(() => estate.parseDevDoor({ ...current, host })).toThrow();
    expect(() => devDoorSteps(host, current.steps)).toThrow();
  }
  for (const steps of [null, {}, [], [null], [{ what: 'setup', command: ' ', why: 'reason' }], [{ what: 'setup', command: 'safe' }]]) {
    expect(() => estate.parseDevDoor({ ...current, steps })).toThrow();
  }
});

test('current and second declarations render their own host with all reviewed steps', () => {
  const current = estate.parseDevDoor(declaration);
  expect(current.host).not.toBeNull();
  const steps = devDoorSteps(current.host, current.steps);
  expect(steps.length).toBe(3);
  expect(steps[2]?.command).toBe(`ssh root@${current.host}`);
  const second = estate.parseDevDoor({ ...current, host: 'workspace.example.test' });
  const rendered = devDoorSteps(second.host, second.steps);
  expect(rendered[2]?.command).toBe('ssh root@workspace.example.test');
  expect(JSON.stringify(rendered)).not.toContain(current.host!);
});

test('the shipped default is the exact input the reader accepts as none', () => {
  const raw = JSON.parse(readFileSync(new URL('../../../instance-config/dev-door.json', import.meta.url), 'utf8')) as unknown;
  expect(estate.parseDevDoor(raw)).toEqual({ host: null, steps: [] });
});
