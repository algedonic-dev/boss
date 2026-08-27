// The shared write path for platform step surfaces (packet cc9d7fc6).
// The class under test: a non-ok PUT must come back as a visible,
// typed failure — never a silent success — and the failure message
// must carry whatever the server said about why.

import { afterEach, describe, expect, test } from 'bun:test';
import { describeWriteFailure, putStep, writeStep } from './stepWrite';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: (url: string, init?: RequestInit) => Promise<Response>) {
  globalThis.fetch = fn as unknown as typeof fetch;
}

describe('describeWriteFailure', () => {
  test('uses the JSON error field when the server sent one', () => {
    expect(
      describeWriteFailure(400, JSON.stringify({ error: 'scheduled_at is required at done' })),
    ).toBe('HTTP 400 — scheduled_at is required at done');
  });

  test('uses message / detail fields as fallbacks', () => {
    expect(describeWriteFailure(422, JSON.stringify({ message: 'no' }))).toBe('HTTP 422 — no');
    expect(describeWriteFailure(403, JSON.stringify({ detail: 'denied' }))).toBe('HTTP 403 — denied');
  });

  test('names outstanding sign-off roles on the 409 conflict shape', () => {
    const body = JSON.stringify({ missing_or_stale_roles: ['ceo', 'qa-lead'] });
    expect(describeWriteFailure(409, body)).toBe('sign-offs outstanding: ceo, qa-lead');
  });

  test('falls back to plain text, and to the bare status when empty', () => {
    expect(describeWriteFailure(500, 'boom')).toBe('HTTP 500 — boom');
    expect(describeWriteFailure(500, '')).toBe('HTTP 500');
    expect(describeWriteFailure(400, '   ')).toBe('HTTP 400');
  });

  test('a bare JSON string body reads as the message', () => {
    expect(describeWriteFailure(404, JSON.stringify('not found'))).toBe('HTTP 404 — not found');
  });

  test('truncates a long body instead of flooding the surface', () => {
    const msg = describeWriteFailure(500, 'x'.repeat(1000));
    expect(msg.length).toBeLessThanOrEqual(220);
    expect(msg.startsWith('HTTP 500 — ')).toBe(true);
  });
});

describe('writeStep', () => {
  test('an ok response is kind ok and carries the response', async () => {
    stubFetch(async () => new Response('{}', { status: 200 }));
    const res = await writeStep('/api/x', { method: 'PUT' });
    expect(res.kind).toBe('ok');
    if (res.kind === 'ok') expect(res.response.status).toBe(200);
  });

  test('a 400 is kind failed with the server message — never silent', async () => {
    stubFetch(
      async () => new Response(JSON.stringify({ error: 'bad shape' }), { status: 400 }),
    );
    const res = await writeStep('/api/x', { method: 'PUT' });
    expect(res).toEqual({ kind: 'failed', error: 'HTTP 400 — bad shape' });
  });

  test('a thrown fetch (network down) is kind failed, not an exception', async () => {
    stubFetch(async () => {
      throw new TypeError('Failed to fetch');
    });
    const res = await writeStep('/api/x', { method: 'PUT' });
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('Failed to fetch');
  });
});

describe('putStep', () => {
  test('PUTs the JSON body to the step endpoint', async () => {
    let seenUrl = '';
    let seenInit: RequestInit | undefined;
    stubFetch(async (url, init) => {
      seenUrl = url;
      seenInit = init;
      return new Response('{}', { status: 200 });
    });
    const res = await putStep('job-1', 'step-9', { status: 'completed' });
    expect(res.kind).toBe('ok');
    expect(seenUrl).toBe('/api/jobs/job-1/steps/step-9');
    expect(seenInit?.method).toBe('PUT');
    expect(JSON.parse(String(seenInit?.body))).toEqual({ status: 'completed' });
  });
});
