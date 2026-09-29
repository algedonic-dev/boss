import { afterEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  BROKER_ROOT_SECRET,
  declareBody,
  declareIdProblem,
  fetchBrokerRules,
  fetchCredentials,
  fileJob,
  gapsOf,
  lastRotationText,
  nextDueText,
  parseBrokerRules,
  parseCredentials,
  provenanceOf,
  REQUESTED_FROM,
  retireBody,
  rotateBody,
  ROTATE_KIND,
  scopesText,
  type BrokerRule,
  type Credential,
} from './credentials';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

const REPO = join(import.meta.dir, '../../../../..');

// Three rows as the system of record served them on 2026-09-27
// (`boss-api GET /api/credentials`, 9 rows) — one of each provenance —
// trimmed of their notes.
const LIVE = [
  {
    id: 'boss-credential-broker-root',
    kind: 'forgejo-access-token',
    issuer: 'forgejo (the forge host)',
    principal: 'user david (admin)',
    scopes: [],
    storage_location: 'k8s Secret boss/boss-credential-broker-root key forgejo-token',
    consumers: [{ kind: 'env', location: 'dispatcher env BOSS_BROKER_FORGEJO_TOKEN in the boss pod (boss.yaml)' }],
    rotation_policy: 'on-demand',
    rotated_at: null,
    notes: 'scope unverified — audit fills this.',
  },
  {
    id: 'forge-host-checkout-token',
    kind: 'forgejo-access-token',
    issuer: 'forgejo (the forge host)',
    principal: 'user david',
    scopes: ['write:repository'],
    storage_location: 'k8s Secret boss/forge-host-checkout-token key token',
    consumers: [{ kind: 'file', location: 'the forge host checkout /home/david/boss' }],
    rotation_policy: 'on-demand',
    rotated_at: '2026-09-26T23:05:58.924649Z',
    notes: '',
  },
  {
    id: 'dauld-github-token',
    kind: 'github-personal-access-token',
    issuer: 'github.com (minted by David in the GitHub UI, as dauld)',
    principal: 'user dauld',
    scopes: [],
    storage_location: '/etc/boss-publish/github.token on the forge host',
    consumers: [],
    rotation_policy: 'on-demand',
    rotated_at: null,
    notes: '',
  },
];

// The broker's rules as `/api/dispatcher/rules` serves them: the rotate
// rule names its credential in `when`; the daily revoke carries no
// `when` and names it in an arg; an unrelated rule names nothing.
const RULES = {
  rules: [
    {
      name: 'broker-rotates-the-forge-host-checkout-token',
      on_event: 'step.done.credential-rotation',
      when: 'subject_id = "forge-host-checkout-token"',
      do: [{ handler: 'credential.rotate.forgejo', args: { secret_name: '"forge-host-checkout-token"' } }],
      version: 1,
      status: 'active',
    },
    {
      name: 'broker-revokes-the-cloudflare-tunnel-daily',
      schedule: { cadence: 'daily', anchor_date: '2026-09-16' },
      when: null,
      do: [{ handler: 'credential.rotate.cloudflare-tunnel', args: { credential_id: '"cloudflare-tunnel-credentials"' } }],
      version: 1,
      status: 'active',
    },
    {
      name: 'backlog-item-closes-on-land',
      on_event: 'step.done.land',
      when: 'subject_id = "dauld-github-token"',
      do: [{ handler: 'jobs.close', args: {} }],
      version: 1,
      status: 'active',
    },
  ],
  handler_emits: {},
  system_edges: [],
};

const creds = (): ReadonlyArray<Credential> => parseCredentials(LIVE);
const byId = (id: string): Credential => {
  const c = creds().find((x) => x.id === id);
  if (!c) throw new Error(`fixture has no ${id}`);
  return c;
};
const rules = (): ReadonlyArray<BrokerRule> => parseBrokerRules(RULES);

describe('parseCredentials — the registry row, as the page draws it', () => {
  test('reads every field the registry serves', () => {
    const c = byId('forge-host-checkout-token');
    expect(c).toEqual({
      id: 'forge-host-checkout-token',
      kind: 'forgejo-access-token',
      issuer: 'forgejo (the forge host)',
      principal: 'user david',
      scopes: ['write:repository'],
      storageLocation: 'k8s Secret boss/forge-host-checkout-token key token',
      consumers: [{ kind: 'file', location: 'the forge host checkout /home/david/boss' }],
      rotationPolicy: 'on-demand',
      rotatedAt: '2026-09-26T23:05:58.924649Z',
      notes: '',
    });
  });

  test('a body that is not the list is a failed read, never an empty registry', () => {
    expect(() => parseCredentials({ error: 'nope' })).toThrow(/not a list/);
    expect(() => parseCredentials(null)).toThrow(/not a list/);
    expect(parseCredentials([])).toEqual([]);
  });

  test('the row shape has no field a value could ride in', () => {
    const planted = parseCredentials([{ ...LIVE[0], value: 'hunter2', token: 'hunter2' }]);
    expect(JSON.stringify(planted)).not.toContain('hunter2');
  });
});

describe('provenanceOf — root placed by David, minted by the broker, or placed by hand', () => {
  test('a row stored in the broker root Secret is root material', () => {
    expect(provenanceOf(byId('boss-credential-broker-root'), { kind: 'ready', data: rules() })).toEqual({ kind: 'root' });
    // A root needs no rule read to be known as one.
    expect(provenanceOf(byId('boss-credential-broker-root'), { kind: 'failed', error: 'down' })).toEqual({ kind: 'root' });
  });

  test('a row a credential.rotate rule names is minted by the broker, with the handler and rule named', () => {
    expect(provenanceOf(byId('forge-host-checkout-token'), { kind: 'ready', data: rules() })).toEqual({
      kind: 'broker',
      handlers: ['credential.rotate.forgejo'],
      rules: ['broker-rotates-the-forge-host-checkout-token'],
    });
  });

  test('a rule naming the credential only in an arg still counts; one with another handler does not', () => {
    const tunnel = { ...byId('dauld-github-token'), id: 'cloudflare-tunnel-credentials' };
    expect(provenanceOf(tunnel, { kind: 'ready', data: rules() })).toMatchObject({
      kind: 'broker',
      handlers: ['credential.rotate.cloudflare-tunnel'],
    });
    // `backlog-item-closes-on-land` names dauld-github-token but is not a
    // credential.rotate handler: no broker rotates it.
    expect(provenanceOf(byId('dauld-github-token'), { kind: 'ready', data: rules() })).toEqual({ kind: 'hand' });
  });

  test('an id is matched whole, as the quoted literal a rule spells', () => {
    const shorter = { ...byId('dauld-github-token'), id: 'host-checkout-token' };
    expect(provenanceOf(shorter, { kind: 'ready', data: rules() })).toEqual({ kind: 'hand' });
  });

  test('an unread rule set is said, not guessed as hand-placed', () => {
    expect(provenanceOf(byId('dauld-github-token'), { kind: 'failed', error: '/api/dispatcher/rules: HTTP 403' })).toEqual({
      kind: 'unknown',
      why: '/api/dispatcher/rules: HTTP 403',
    });
  });

  test('parseBrokerRules keeps only ACTIVE credential.rotate rules, and refuses a body with no rule list', () => {
    expect(rules().map((r) => r.name)).toEqual([
      'broker-rotates-the-forge-host-checkout-token',
      'broker-revokes-the-cloudflare-tunnel-daily',
    ]);
    // A retired (or unstated) rule rotates nothing: its credential is not the broker's.
    const retired = { ...RULES.rules[0], name: 'retired-rotator', when: 'subject_id = "dauld-github-token"', status: 'retired' };
    const unstated = { ...RULES.rules[0], name: 'unstated-rotator', when: 'subject_id = "dauld-github-token"', status: undefined };
    const withStale = parseBrokerRules({ ...RULES, rules: [...RULES.rules, retired, unstated] });
    expect(withStale.map((r) => r.name)).not.toContain('retired-rotator');
    expect(withStale.map((r) => r.name)).not.toContain('unstated-rotator');
    expect(provenanceOf(byId('dauld-github-token'), { kind: 'ready', data: withStale })).toEqual({ kind: 'hand' });
    expect(() => parseBrokerRules([])).toThrow(/no rule list/);
    expect(() => parseBrokerRules({ rules: [], error: 'db down' })).toThrow(/db down/);
  });
});

describe('gaps — what the registry does not say is drawn as missing', () => {
  test('no consumer, no recorded rotation and unverified scopes are gaps', () => {
    expect(gapsOf(byId('dauld-github-token'))).toEqual(['no consumer declared', 'no rotation recorded', 'scopes unverified']);
    expect(gapsOf(byId('forge-host-checkout-token'))).toEqual([]);
  });

  test('the text each gap cell carries', () => {
    expect(scopesText(byId('dauld-github-token'))).toEqual({ text: 'unverified — the audit fills this', gap: true });
    expect(scopesText(byId('forge-host-checkout-token'))).toEqual({ text: 'write:repository', gap: false });
    expect(lastRotationText(byId('dauld-github-token'))).toEqual({ text: 'never recorded', gap: true });
    expect(lastRotationText(byId('forge-host-checkout-token'))).toEqual({ text: '2026-09-26 23:05Z', gap: false });
  });

  test('next rotation: on-demand has none by policy; scheduled with no interval is a gap', () => {
    expect(nextDueText(byId('forge-host-checkout-token'))).toEqual({
      text: 'on demand — no date is due until a rotation is filed',
      gap: false,
    });
    const scheduled = { ...byId('forge-host-checkout-token'), rotationPolicy: 'scheduled' };
    expect(nextDueText(scheduled)).toEqual({ text: 'scheduled, but the registry declares no interval', gap: true });
    expect(gapsOf(scheduled)).toContain('no interval for a scheduled rotation');
    const odd = { ...scheduled, rotationPolicy: 'weekly' };
    expect(nextDueText(odd)).toEqual({ text: 'policy "weekly" is not one the registry knows', gap: true });
  });
});

describe('the lifecycle as packets', () => {
  test('rotate files rotate-a-credential on the credential id', () => {
    expect(rotateBody('forge-host-checkout-token', 'emp-david')).toEqual({
      kind: ROTATE_KIND,
      title: 'Rotate forge-host-checkout-token',
      tags: [],
      subject: { id: 'forge-host-checkout-token', subject_kind: 'custom' },
      owner_id: 'emp-david',
      status: 'open',
      priority: 'standard',
      metadata: { credential: 'forge-host-checkout-token', requested_from: REQUESTED_FROM },
    });
  });

  test('retire and declare file backlog items on the credential id, naming the work and carrying no value', () => {
    const retire = retireBody(byId('forge-host-checkout-token'), 'emp-david');
    expect(retire.kind).toBe('backlog-item');
    expect(retire.subject).toEqual({ id: 'forge-host-checkout-token', subject_kind: 'custom' });
    expect(retire.title).toBe('Retire the credential forge-host-checkout-token');
    expect(retire.metadata.area).toBe('credentials');
    expect(retire.metadata.input_channel).toBe('roadmap');
    expect(String(retire.metadata.description)).toContain('revoke it at its issuer (forgejo (the forge host))');
    expect(String(retire.metadata.description)).toContain('the forge host checkout /home/david/boss');

    const declare = declareBody('github-app-installation', 'emp-david');
    expect(declare.kind).toBe('backlog-item');
    expect(declare.subject).toEqual({ id: 'github-app-installation', subject_kind: 'custom' });
    expect(declare.title).toBe('Declare the credential github-app-installation in the registry');
    expect(String(declare.metadata.description)).toContain('seeds/credentials.toml');
    expect(declare.metadata.requested_from).toBe(REQUESTED_FROM);
  });

  test('the declared id is refused when it is empty, not kebab-case, already a row, or shaped like a value', () => {
    const have = ['boss-dev-forge-token'];
    expect(declareIdProblem('', have)).toMatch(/Name the credential/);
    expect(declareIdProblem('Boss_Token', have)).toMatch(/kebab-case/);
    expect(declareIdProblem('boss-dev-forge-token', have)).toMatch(/already declared/);
    // A forgejo token is 40 lowercase hex characters — kebab-valid, and
    // exactly what must never be typed here.
    expect(declareIdProblem('3f9a0c1e5b7d2a4c6e8f0a1b3c5d7e9f1a2b3c4d', have)).toMatch(/looks like a value/);
    expect(declareIdProblem('github-app-installation', have)).toBeNull();
  });

  // Scope review of 39747321: the kebab refusal echoed the typed input,
  // so a pasted token that is not kebab-case — mixed case, `_`, `+`, `/`
  // — was drawn in the DOM. The value-shape check runs first, over any
  // charset, and no refusal carries what was typed.
  test('a token-shaped input in any charset is refused as a value, and no refusal repeats the input', () => {
    const have = ['boss-dev-forge-token'];
    const pasted = ['ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8', 'sk_live+AbCdEf/0123456789ABCDEFxyz='];
    for (const token of pasted) {
      expect(declareIdProblem(token, have)).toMatch(/looks like a value/);
    }
    for (const typed of [...pasted, 'Boss_Token', 'boss-dev-forge-token', '3f9a0c1e5b7d2a4c6e8f0a1b3c5d7e9f1a2b3c4d']) {
      expect(declareIdProblem(typed, have) ?? '').not.toContain(typed);
    }
  });

  test('fileJob POSTs the body and answers the new id; a refusal or a missing id throws', async () => {
    const seen: Array<{ url: string; method: string; body: string }> = [];
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      seen.push({ url, method: init?.method ?? 'GET', body: String(init?.body) });
      return new Response(JSON.stringify({ id: 'job-1' }), { status: 201 });
    }) as typeof fetch;
    const body = rotateBody('forge-host-checkout-token', 'emp-david');
    expect(await fileJob(body)).toBe('job-1');
    expect(seen).toEqual([{ url: '/api/jobs', method: 'POST', body: JSON.stringify(body) }]);

    globalThis.fetch = (async () => new Response('no workflow rotate-a-credential', { status: 422 })) as unknown as typeof fetch;
    await expect(fileJob(body)).rejects.toThrow(/HTTP 422: no workflow rotate-a-credential/);

    globalThis.fetch = (async () => new Response('{}', { status: 201 })) as unknown as typeof fetch;
    await expect(fileJob(body)).rejects.toThrow(/no id/);
  });
});

describe('the reads', () => {
  test('a failed registry read is failed, and names the status', async () => {
    globalThis.fetch = (async () => new Response('', { status: 403 })) as unknown as typeof fetch;
    expect(await fetchCredentials()).toEqual({ kind: 'failed', error: '/api/credentials: HTTP 403' });
    expect(await fetchBrokerRules()).toEqual({ kind: 'failed', error: '/api/dispatcher/rules: HTTP 403' });
  });

  test('a registry answering a non-list is failed, not empty', async () => {
    globalThis.fetch = (async () => new Response('{"data":[]}', { status: 200 })) as unknown as typeof fetch;
    const r = await fetchCredentials();
    expect(r.kind).toBe('failed');
  });
});

// The facts this page reads out of other files, each held to its source
// (CLAUDE.md §9a) so a rename there reds here instead of silently
// reclassifying every row.
describe('pins', () => {
  test('the broker root Secret is the one boss-credential-broker.yaml names', () => {
    const manifest = readFileSync(join(REPO, 'infra/cluster/manifests/boss-credential-broker.yaml'), 'utf8');
    expect(manifest).toContain(`THE ROOT CREDENTIAL is Secret boss/${BROKER_ROOT_SECRET}`);
  });

  test('rotate-a-credential is a workflow on a custom Subject, as the rotate packet files it', () => {
    const toml = readFileSync(join(REPO, 'infra/platform/workflows/rotate-a-credential.toml'), 'utf8');
    expect(toml).toContain(`kind = "${ROTATE_KIND}"`);
    expect(toml).toContain('subject_kinds = ["custom"]');
  });
});
