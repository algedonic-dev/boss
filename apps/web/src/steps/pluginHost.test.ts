// The one-fetch downgrade (packet cc9d7fc6): a single failed
// /api/jobs/step-plugins fetch used to be cached as "no plugins" for
// the whole session, permanently downgrading every plugin-backed step
// to the generic surface. Failures must be reported as failures and
// must NOT be cached — the next probe retries.

import { afterEach, describe, expect, test } from 'bun:test';
import { _resetPluginRegistryForTests, hasActivePluginFor, probeActivePlugin } from './pluginHost';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
  _resetPluginRegistryForTests();
});

function stubFetch(fn: () => Promise<Response>) {
  globalThis.fetch = fn as unknown as typeof fetch;
}

const SPEC = {
  kind: 'review-design',
  frontend_url: 'review-design.js',
};

describe('probeActivePlugin', () => {
  test('reports failure distinctly from "no plugin registered"', async () => {
    stubFetch(async () => new Response('nope', { status: 500 }));
    _resetPluginRegistryForTests();
    const probe = await probeActivePlugin('review-design');
    expect(probe.kind).toBe('failed');
  });

  test('a failed registry fetch is not cached — the next probe retries', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response('down', { status: 503 });
      return new Response(JSON.stringify([SPEC]), { status: 200 });
    });
    _resetPluginRegistryForTests();

    const first = await probeActivePlugin('review-design');
    expect(first.kind).toBe('failed');

    const second = await probeActivePlugin('review-design');
    expect(second).toEqual({ kind: 'ok', active: true });
    expect(calls).toBe(2);
  });

  test('a successful load IS cached — later probes cost no fetch', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      return new Response(JSON.stringify([SPEC]), { status: 200 });
    });
    _resetPluginRegistryForTests();

    expect(await probeActivePlugin('review-design')).toEqual({ kind: 'ok', active: true });
    expect(await probeActivePlugin('other-kind')).toEqual({ kind: 'ok', active: false });
    expect(calls).toBe(1);
  });
});

describe('hasActivePluginFor', () => {
  test('still answers a plain boolean for callers that only branch', async () => {
    stubFetch(async () => new Response(JSON.stringify([SPEC]), { status: 200 }));
    _resetPluginRegistryForTests();
    expect(await hasActivePluginFor('review-design')).toBe(true);
    expect(await hasActivePluginFor('unknown')).toBe(false);
  });
});
