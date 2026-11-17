// The "File an ops request" form's module (backlog 7aef2405): the verb
// list parsed and never judged, the form's fields mapped onto positional
// args, and the call POSTed as {host, verb, args} with the server's
// refusal of a bad param shown verbatim.
import { afterEach, describe, expect, test } from 'bun:test';

import { argsFor, FILE_PATH, fileAnOpsRequest, paramHint, parseOpsVerbs, type OpsVerb } from './fileAnOpsRequest';

const REBUILD = {
  name: 'rebuild-a-failed-replica',
  about: 'MUTATING — delete one failed Longhorn replica so the volume rebuilds it',
  hosts: ['forge'],
  params: [
    { name: 'pvc', pattern: '^pvc-[0-9a-f-]{36}$', one_of: null, default: null, optional: false, max: null },
    { name: 'replica', pattern: '^pvc-[0-9a-f-]{36}-r-[0-9a-f]{8}$', one_of: null, default: null, optional: false, max: null },
  ],
  mutating: true,
  requires_approval: true,
  requires_review: false,
  approvers: ['emp-david'],
  plan_verb: 'plan-a-failed-replica-rebuild',
  plan_for: null,
} as const;

const TAIL: OpsVerb = {
  name: 'journal-tail',
  about: 'last N journal lines',
  hosts: ['boss-gcp', 'forge'],
  params: [
    { name: 'unit', pattern: '^[a-z.@-]+$', one_of: null, default: null, optional: false, max: null },
    { name: 'lines', pattern: '^[0-9]{1,3}$', one_of: null, default: '100', optional: false, max: 200 },
    { name: 'mode', pattern: null, one_of: ['--dry-run'], default: null, optional: true, max: null },
  ],
  mutating: false,
  requires_approval: false,
  requires_review: false,
  approvers: [],
  plan_verb: null,
  plan_for: null,
};

describe('parseOpsVerbs', () => {
  test('the server list parses to the verbs it names', () => {
    expect(parseOpsVerbs({ verbs: [REBUILD] })).toEqual([REBUILD]);
  });

  test('a list in no shape this page knows THROWS, so the read fails loudly instead of offering no verbs', () => {
    for (const raw of [null, [], { verbs: 'all' }, { verbs: [{ ...REBUILD, params: 'none' }] }, { verbs: [{ ...REBUILD, hosts: [1] }] }]) {
      expect(() => parseOpsVerbs(raw)).toThrow('cannot read');
    }
  });
});

describe('argsFor', () => {
  test('fields become positional args, trailing blanks left to the verb file', () => {
    expect(argsFor(TAIL, { unit: 'boss-jobs' })).toEqual({ kind: 'args', args: ['boss-jobs'] });
    expect(argsFor(TAIL, { unit: ' boss-jobs ', lines: '50' })).toEqual({ kind: 'args', args: ['boss-jobs', '50'] });
  });

  test('a blank before a filled field takes its default, or names the field to fill', () => {
    expect(argsFor(TAIL, { unit: 'boss-jobs', mode: '--dry-run' })).toEqual({
      kind: 'args',
      args: ['boss-jobs', '100', '--dry-run'],
    });
    const gap = argsFor(TAIL, { lines: '50' });
    expect(gap.kind).toBe('refused');
    if (gap.kind === 'refused') expect(gap.why).toContain('fill unit');
  });

  test('no field is invented: a required field left blank is the server’s to refuse', () => {
    expect(argsFor(REBUILD, {})).toEqual({ kind: 'args', args: [] });
  });
});

describe('paramHint', () => {
  test('says what a param admits', () => {
    expect(paramHint(TAIL.params[1]!)).toBe('matches ^[0-9]{1,3}$ · at most 200 · default 100');
    expect(paramHint(TAIL.params[2]!)).toBe('one of --dry-run · optional');
    expect(paramHint(REBUILD.params[0])).toBe('matches ^pvc-[0-9a-f-]{36}$ · required');
  });
});

describe('fileAnOpsRequest', () => {
  const real = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = real;
  });

  test('POSTs the call as {host, verb, args} and nothing else, and returns the filed id', async () => {
    const sent: Array<{ url: string; init: RequestInit | undefined }> = [];
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      sent.push({ url, init });
      return new Response(JSON.stringify({ id: 'req-1' }), { status: 201 });
    }) as typeof fetch;
    const call = { host: 'forge', verb: REBUILD.name, args: ['pvc-a', 'pvc-a-r-1'] };
    expect(await fileAnOpsRequest(call)).toEqual({ kind: 'filed', id: 'req-1' });
    expect(sent.map((s) => s.url)).toEqual([FILE_PATH]);
    expect(sent[0]?.init?.method).toBe('POST');
    expect(JSON.parse(String(sent[0]?.init?.body))).toEqual(call);
  });

  test('the server’s refusal of a bad param is returned verbatim with its status', async () => {
    const why = 'REFUSED — arg `pvc` value `../../etc` does not match ^pvc-[0-9a-f-]{36}$';
    globalThis.fetch = (async () =>
      new Response(JSON.stringify({ error: why }), { status: 422 })) as unknown as typeof fetch;
    expect(await fileAnOpsRequest({ host: 'forge', verb: REBUILD.name, args: ['../../etc', 'x'] })).toEqual({
      kind: 'refused',
      why: `file the request: HTTP 422: ${why}`,
    });
  });

  test('a created answer with no id is not called filed', async () => {
    globalThis.fetch = (async () => new Response('{}', { status: 201 })) as unknown as typeof fetch;
    const r = await fileAnOpsRequest({ host: 'forge', verb: 'uptime', args: [] });
    expect(r.kind).toBe('refused');
  });
});
