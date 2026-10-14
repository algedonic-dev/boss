import { afterEach, expect, test } from 'bun:test';
import { loadCommerceSummary } from './api';

const originalFetch = globalThis.fetch;
function answering(response: () => Promise<Response>): typeof fetch {
  return Object.assign(response, { preconnect: originalFetch.preconnect });
}
afterEach(() => { globalThis.fetch = originalFetch; });

test('summary retains the failed read and its named HTTP reason', async () => {
  globalThis.fetch = answering(() => Promise.resolve(new Response('commerce down', { status: 503 })));
  expect(await loadCommerceSummary()).toEqual({
    kind: 'failed', error: '/api/commerce/summary: HTTP 503',
  });
});

test('summary retains a malformed successful response as a named failure', async () => {
  globalThis.fetch = answering(() => Promise.resolve(Response.json([])));
  const result = await loadCommerceSummary();
  expect(result).toMatchObject({ kind: 'failed' });
  expect(result && 'error' in result ? result.error : '').toContain('/api/commerce/summary');
});

test('summary names a thrown network failure without losing its cause', async () => {
  globalThis.fetch = answering(() => Promise.reject(new Error('network control')));
  expect(await loadCommerceSummary()).toEqual({
    kind: 'failed', error: '/api/commerce/summary: network control',
  });
});

test('a successful empty summary remains a ready answer', async () => {
  const summary = {
    revenue_ttm: [], total_revenue_ttm_cents: 0, total_cogs_ttm_cents: 0,
    total_gross_margin_ttm_cents: 0, ar_aging: [], total_outstanding_cents: 0,
    total_invoice_count: 0, revenue_by_month: [], currency: 'USD',
  };
  globalThis.fetch = answering(() => Promise.resolve(Response.json(summary)));
  expect(await loadCommerceSummary()).toEqual({ kind: 'ready', data: summary });
});
