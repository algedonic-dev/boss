import { afterEach, describe, it, expect } from 'bun:test';
import {
  ENTRIES_PER_ACCOUNT_CAP,
  headlineText,
  loadEntriesForAccount,
  loadHeadline,
  loadPeriods,
} from './ledger';

// Backlog 1a2b67c9 (page audit 3f964c57, /ux/finance, 2026-09-23):
// loadPeriods and loadEntriesForAccount folded a failed read into [],
// so a ledger outage painted "No periods yet." and "No entries for this
// account." — the page's normal empty states. Each now hands back the
// read's outcome beside its rows, and the reason with it.

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: (url: string) => Promise<Response>) {
  globalThis.fetch = ((input: RequestInfo | URL) => fn(String(input))) as unknown as typeof fetch;
}

const PERIOD = {
  id: 'per-2026-09', kind: 'month', starts_on: '2026-09-01', ends_on: '2026-09-30',
  status: 'open', locked_at: null, locked_by: null, locked_rule_version: null,
  locked_checksum: null, entry_count: 3, total_debits: 100, total_credits: 100,
};

const ENTRY = {
  id: 'ent-1', fact_id: 'fact-1', posted_on: '2026-09-02', memo: null, rule_version: 1,
  fact_kind: 'manual', fact_source_table: null, fact_source_id: null,
};

describe('loadPeriods', () => {
  it('keeps a refusal as a failed read, with the status and the server text', async () => {
    stubFetch(async () => new Response('ledger down', { status: 500 }));
    const res = await loadPeriods();
    expect(res.read).toEqual({ kind: 'failed', error: 'HTTP 500: ledger down' });
    expect(res.periods).toEqual([]);
  });

  it('keeps a network error as a failed read', async () => {
    stubFetch(async () => {
      throw new Error('connection refused');
    });
    const res = await loadPeriods();
    expect(res.read).toEqual({ kind: 'failed', error: 'connection refused' });
  });

  it('an empty ledger is an ok read with no rows — the only true "No periods yet."', async () => {
    stubFetch(async () => new Response('[]', { status: 200 }));
    const res = await loadPeriods();
    expect(res.read).toEqual({ kind: 'ok' });
    expect(res.periods).toEqual([]);
  });

  it('hands the rows back on a healthy read', async () => {
    stubFetch(async () => new Response(JSON.stringify([PERIOD]), { status: 200 }));
    const res = await loadPeriods();
    expect(res.read).toEqual({ kind: 'ok' });
    expect(res.periods.map((p) => p.id)).toEqual(['per-2026-09']);
  });
});

// Backlog 801b6a7a (page audit 3f964c57 gap 3; design c2820008 D1): the
// /finance headline read boss-commerce's trailing revenue, which no
// Algedonic protocol writes, so it could not agree with the ledger's own
// income statement on the same page. It now reads that statement over
// the trailing year, and a failed or malformed read is named, never a
// number.
describe('loadHeadline', () => {
  const WINDOW = { from: '2025-09-23', to: '2026-09-23' };
  const STATEMENT = {
    from: WINDOW.from, to: WINDOW.to, revenue: [], total_revenue_cents: 4_500_000,
    cogs: [], total_cogs_cents: 1_500_000, gross_profit_cents: 3_000_000,
    operating_expenses: [], total_operating_expenses_cents: 2_200_000,
    net_income_cents: 800_000, currency: 'USD',
  };

  it('reads the ledger income statement over the window, never commerce', async () => {
    const seen: string[] = [];
    stubFetch(async (url) => {
      seen.push(url);
      return new Response(JSON.stringify(STATEMENT), { status: 200 });
    });
    const res = await loadHeadline(WINDOW);
    expect(seen).toEqual(['/api/ledger/income-statement?from=2025-09-23&to=2026-09-23']);
    expect(res).toEqual({
      kind: 'ready',
      data: { revenue: 4_500_000, grossProfit: 3_000_000, netIncome: 800_000, currency: 'USD' },
    });
  });

  it('keeps a refusal as a failed read', async () => {
    stubFetch(async () => new Response('ledger down', { status: 503 }));
    const res = await loadHeadline(WINDOW);
    expect(res.kind).toBe('failed');
  });

  it('a statement missing its totals is a failed read, never a NaN headline', async () => {
    stubFetch(async () => new Response(JSON.stringify({ ...STATEMENT, gross_profit_cents: null }), { status: 200 }));
    const res = await loadHeadline(WINDOW);
    expect(res.kind).toBe('failed');
  });
});

describe('headlineText', () => {
  it('names the ledger figures', () => {
    expect(headlineText({
      kind: 'ready',
      data: { revenue: 4_500_000, grossProfit: 3_000_000, netIncome: 800_000, currency: 'USD' },
    })).toEqual({ title: '$45K trailing revenue', subtitle: '$30K gross profit (66.7%) · $8K net income' });
  });

  it('a year with no revenue is a zero margin, not a division by zero', () => {
    expect(headlineText({
      kind: 'ready', data: { revenue: 0, grossProfit: 0, netIncome: -4_000, currency: 'USD' },
    })).toEqual({ title: '$0 trailing revenue', subtitle: '$0 gross profit (0.0%) · -$40 net income' });
  });

  it('says it is loading, and names the read that failed', () => {
    expect(headlineText({ kind: 'loading' })).toEqual({ title: 'Finance', subtitle: 'Loading…' });
    expect(headlineText({ kind: 'failed', error: 'HTTP 503' }))
      .toEqual({ title: 'Finance', subtitle: 'Income statement unavailable' });
  });
});

describe('loadEntriesForAccount', () => {
  it('keeps a refusal as a failed read, never an uncapped empty page', async () => {
    stubFetch(async () => new Response('ledger down', { status: 503 }));
    const res = await loadEntriesForAccount('1000');
    expect(res.read).toEqual({ kind: 'failed', error: 'HTTP 503: ledger down' });
    expect(res.data).toEqual([]);
    expect(res.capped).toBe(false);
  });

  it('an account with no entries is an ok read', async () => {
    stubFetch(async () => new Response('[]', { status: 200 }));
    const res = await loadEntriesForAccount('1000');
    expect(res.read).toEqual({ kind: 'ok' });
    expect(res.data).toEqual([]);
  });

  it('no account selected reads nothing and is not a failure', async () => {
    const seen: string[] = [];
    stubFetch(async (url) => {
      seen.push(url);
      return new Response('[]', { status: 200 });
    });
    const res = await loadEntriesForAccount(null);
    expect(seen).toEqual([]);
    expect(res.read).toEqual({ kind: 'ok' });
  });

  it('still trims an over-cap page and marks it capped', async () => {
    const rows = Array.from({ length: ENTRIES_PER_ACCOUNT_CAP + 1 }, (_, i) => ({ ...ENTRY, id: `ent-${i}` }));
    stubFetch(async () => new Response(JSON.stringify(rows), { status: 200 }));
    const res = await loadEntriesForAccount('1000');
    expect(res.read).toEqual({ kind: 'ok' });
    expect(res.data.length).toBe(ENTRIES_PER_ACCOUNT_CAP);
    expect(res.capped).toBe(true);
  });
});
