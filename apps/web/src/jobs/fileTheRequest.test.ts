// The file-the-request reading, parsed and never judged (backlog
// 7aef2405). Whether a packet is a plan, whether it rendered clean and
// what it would file is the jobs API's reading; this module draws it, and
// a reading it cannot parse is said as a refusal rather than dropped as
// "not a plan".
import { afterEach, describe, expect, test } from 'bun:test';

import {
  carriesAnOpsRequest,
  fileTheRequest,
  fileTheRequestPath,
  parsePlanPreview,
  waitsOn,
} from './fileTheRequest';

const REQUEST = {
  host: 'forge',
  verb: 'rebuild-a-failed-replica',
  args: ['pvc-a', 'pvc-a-r-1'],
  title: 'rebuild-a-failed-replica pvc-a pvc-a-r-1 on forge',
  mutating: true,
  requires_approval: true,
  requires_review: false,
  approvers: ['emp-david'],
};

describe('parsePlanPreview', () => {
  test('a fileable plan carries the write the server read off it', () => {
    expect(parsePlanPreview({ plan: true, fileable: true, request: REQUEST, open_request: null })).toEqual({
      kind: 'fileable',
      request: REQUEST,
    });
  });

  test('a packet that is no plan draws nothing', () => {
    expect(parsePlanPreview({ plan: false, why: 'uptime is no write’s plan' })).toEqual({ kind: 'not-a-plan' });
  });

  test('a refusal keeps its reason and the open request it names', () => {
    expect(
      parsePlanPreview({ plan: true, fileable: false, write: 'reap-terminated-pods', why: 'still open', open_request: 'r-1' }),
    ).toEqual({ kind: 'refused', write: 'reap-terminated-pods', why: 'still open', open_request: 'r-1' });
  });

  test('a reading in no shape this page knows is a refusal that says so', () => {
    for (const raw of [null, [], { plan: true, fileable: true, request: { ...REQUEST, args: [1] } }, { plan: 'yes' }]) {
      const p = parsePlanPreview(raw);
      expect(p.kind).toBe('refused');
      if (p.kind === 'refused') expect(p.why).toContain('cannot read');
    }
  });
});

describe('carriesAnOpsRequest', () => {
  test('asks only of a packet carrying a verb and a host', () => {
    expect(carriesAnOpsRequest({ verb: 'plan-a-pod-reap', host: 'forge' })).toBe(true);
    expect(carriesAnOpsRequest({ verb: 'plan-a-pod-reap' })).toBe(false);
    expect(carriesAnOpsRequest(undefined)).toBe(false);
  });
});

describe('waitsOn', () => {
  test('names the approvers, and the review first when the verb asks for one', () => {
    expect(waitsOn(REQUEST)).toBe('It waits on a passkey approval by emp-david.');
    expect(waitsOn({ ...REQUEST, requires_review: true })).toBe(
      'It waits on an independent review of the plan, then a passkey approval by emp-david.',
    );
  });
});

describe('fileTheRequest', () => {
  const real = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = real;
  });

  test('POSTs NO body — the request is read off the plan, and no filer supplies plan_sha256', async () => {
    const sent: Array<{ url: string; init: RequestInit | undefined }> = [];
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      sent.push({ url, init });
      return new Response(JSON.stringify({ id: 'req-1' }), { status: 201 });
    }) as typeof fetch;
    expect(await fileTheRequest('plan-1')).toEqual({ kind: 'filed', id: 'req-1' });
    expect(sent).toEqual([{ url: fileTheRequestPath('plan-1'), init: { method: 'POST' } }]);
  });

  test('a refusal is returned verbatim with its status', async () => {
    globalThis.fetch = (async () => new Response('opening a ops-request packet needs Create', { status: 403 })) as unknown as typeof fetch;
    expect(await fileTheRequest('plan-1')).toEqual({
      kind: 'refused',
      why: 'file the request: HTTP 403: opening a ops-request packet needs Create',
    });
  });
});
