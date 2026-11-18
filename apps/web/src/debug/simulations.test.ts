// The debug gear's custom-subject flows send the Subject shape the jobs
// API reads (backlog a8844a57). boss_core::job::Subject is
// {subject_kind, id} for every kind, and create_job answers 422 on a
// body that lacks `id` — which is what {custom_kind, ref_id} lacked, so
// "Trigger anomaly" and "Run a hire" could open no Job at all.
import { afterEach, describe, expect, test } from 'bun:test';

import { runHire, triggerAnomaly } from './simulations';

const realFetch = globalThis.fetch;

afterEach(() => {
  globalThis.fetch = realFetch;
});

/// Answer every request 201 `{}` and keep each POST /api/jobs body.
function recordJobCreates(): Array<Record<string, unknown>> {
  const creates: Array<Record<string, unknown>> = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    if (url === '/api/jobs' && init?.method === 'POST') {
      creates.push(JSON.parse(String(init.body)) as Record<string, unknown>);
    }
    return new Response('{}', { status: 201, headers: { 'content-type': 'application/json' } });
  }) as typeof fetch;
  return creates;
}

describe('a custom-subject flow sends {subject_kind, id}', () => {
  test('Trigger anomaly opens its incident on a custom subject with an id', async () => {
    const creates = recordJobCreates();
    await triggerAnomaly(() => {});
    expect(creates).toHaveLength(1);
    const subject = creates[0]!['subject'] as Record<string, unknown>;
    expect(Object.keys(subject).sort()).toEqual(['id', 'subject_kind']);
    expect(subject['subject_kind']).toBe('custom');
    expect(subject['id']).toMatch(/^anomaly-/);
  });

  test('Run a hire opens its hiring Job on a custom subject with an id', async () => {
    const creates = recordJobCreates();
    await runHire(() => {});
    expect(creates).toHaveLength(1);
    const subject = creates[0]!['subject'] as Record<string, unknown>;
    expect(Object.keys(subject).sort()).toEqual(['id', 'subject_kind']);
    expect(subject['subject_kind']).toBe('custom');
    expect(subject['id']).toMatch(/^sim-/);
  });
});
