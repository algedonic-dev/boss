// /ux/exec — every read's loading, failure and empty line, and every
// card's link target, pinned (page audit a1d62870, gap 10: backlog
// 1495cf68 — "0 test files name ExecPage").
//
// The unit runner has no Svelte pass (tests/rune-preload.ts), so the
// page's reads, lines and links live in ./exec.ts as data and the page
// renders exactly what that module answers: a read is a Remote, the
// line a Remote shows is `noteOf`, and a card's link is EXEC_LINKS.
// This file drives each read through a stubbed fetch and pins the line
// it becomes; the source pins at the bottom hold the page to rendering
// those lines and links rather than its own. The rendered page is
// pinned by tests/mocked/exec-page.mocked.spec.ts.
//
// The class this guards is the false empty: until this car the Active
// jobs card set its summary only when `r.ok`, so a refused read (403
// for a denied role, backlog 1445813b) painted "Jobs data unavailable"
// with no status — or, after the scoping car 19f08bd6, a self-scoped
// executive saw their own counts with nothing saying so.

import { afterEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { parseRoute } from '../router';
import {
  EXEC_LINES,
  EXEC_LINKS,
  EXEC_READS,
  JOBS_SCOPE_NOTE,
  WAITING_LIMIT,
  executiveWorkLink,
  kindLabelOf,
  loadBalances,
  loadJobsSummary,
  loadKindLabels,
  loadRevenueMix,
  loadWaiting,
  noteOf,
  trailingYear,
  waitingText,
} from './exec';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

/** Every GET answered from `routes` by exact URL; anything else is a
 *  test bug and fails loudly. `asked` records the URLs, in order. */
function stub(routes: Record<string, () => Response | Promise<Response>>): string[] {
  const asked: string[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    asked.push(url);
    const answer = routes[url];
    if (!answer) throw new Error(`unstubbed read: ${url}`);
    return answer();
  }) as typeof fetch;
  return asked;
}
const ok = (body: unknown): Response =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });
const status = (n: number): Response => new Response('refused', { status: n });
const down = (): never => {
  throw new TypeError('Failed to fetch');
};

describe('every read shows a loading line before it answers', () => {
  test('each card has its own loading line, never a failure', () => {
    for (const lines of Object.values(EXEC_LINES)) {
      expect(noteOf({ kind: 'loading' }, lines as never)).toEqual({
        text: lines.loading,
        failed: false,
      });
    }
    expect(EXEC_LINES.jobs.loading).toBe('Loading open jobs…');
    expect(EXEC_LINES.revenue.loading).toBe('Loading revenue mix…');
    expect(EXEC_LINES.balance.loading).toBe('Loading balance-sheet snapshot…');
    expect(EXEC_LINES.waiting.loading).toBe('Loading what is waiting on you…');
  });
});

describe('Active jobs — /api/jobs/summary?status=open', () => {
  test('a refusal renders its HTTP status on the failure marker (1445813b)', async () => {
    stub({ [EXEC_READS.jobs]: () => status(403) });
    const r = await loadJobsSummary();
    expect(noteOf(r, EXEC_LINES.jobs)).toEqual({
      text: "Couldn't load open jobs — /api/jobs/summary?status=open: HTTP 403",
      failed: true,
    });
  });

  test('a network failure renders its error text', async () => {
    stub({ [EXEC_READS.jobs]: down });
    const r = await loadJobsSummary();
    expect(noteOf(r, EXEC_LINES.jobs)).toEqual({
      text: "Couldn't load open jobs — Failed to fetch",
      failed: true,
    });
  });

  test('an answer with no counts is a failure, not zero open jobs', async () => {
    stub({ [EXEC_READS.jobs]: () => ok([]) });
    const r = await loadJobsSummary();
    expect(r.kind).toBe('failed');
    expect(noteOf(r, EXEC_LINES.jobs)?.failed).toBe(true);
  });

  test('no open jobs is its own line, scoped to the viewer', async () => {
    stub({ [EXEC_READS.jobs]: () => ok({ counts: {}, total: 0, status: 'open' }) });
    const r = await loadJobsSummary();
    expect(noteOf(r, EXEC_LINES.jobs)).toEqual({
      text: 'No open jobs among the packets you can read.',
      failed: false,
    });
  });

  test('counts answer by kind, largest first, and say whose they are', async () => {
    stub({ [EXEC_READS.jobs]: () => ok({ counts: { a: 1, b: 5, c: 2 }, total: 8 }) });
    const r = await loadJobsSummary();
    expect(noteOf(r, EXEC_LINES.jobs)).toBeNull();
    expect(r).toEqual({
      kind: 'ready',
      data: {
        total: 8,
        byKind: [
          { kind: 'b', count: 5 },
          { kind: 'c', count: 2 },
          { kind: 'a', count: 1 },
        ],
      },
    });
    // The summary counts only what the caller may read (19f08bd6); the
    // card says so every time it shows a number.
    expect(JOBS_SCOPE_NOTE).toBe('Counted over the packets you can read.');
  });
});

describe('the workflow labels degrade to kind codes', () => {
  test('a read that works names each kind by its label', async () => {
    stub({ [EXEC_READS.workflows]: () => ok([{ kind: 'sale', label: 'Sale' }]) });
    const labels = await loadKindLabels();
    expect(kindLabelOf(labels, 'sale')).toBe('Sale');
    expect(kindLabelOf(labels, 'other')).toBe('other');
  });

  test('a failed read shows the codes themselves — honest, and it stays that way', async () => {
    stub({ [EXEC_READS.workflows]: () => status(500) });
    const labels = await loadKindLabels();
    expect(labels.size).toBe(0);
    expect(kindLabelOf(labels, 'sale')).toBe('sale');
  });
});

describe('Revenue mix — the ledger income statement, trailing 12 months (d93cfb99)', () => {
  const W = trailingYear('2026-09-27');

  test('the window is the trailing year ending today', () => {
    expect(W).toEqual({ from: '2025-09-27', to: '2026-09-27' });
    expect(EXEC_READS.revenue(W)).toBe(
      '/api/ledger/income-statement?from=2025-09-27&to=2026-09-27',
    );
  });

  test('revenue lines by account, largest first, with their share of the total', async () => {
    stub({
      [EXEC_READS.revenue(W)]: () =>
        ok({
          from: W.from,
          to: W.to,
          revenue: [
            { account_code: '4200', account_name: 'Sponsorship revenue', amount_cents: 100 },
            { account_code: '4000', account_name: 'Sales', amount_cents: 300 },
            { account_code: '4100', account_name: 'Idle', amount_cents: 0 },
          ],
          total_revenue_cents: 400,
        }),
    });
    const r = await loadRevenueMix(W);
    expect(noteOf(r, EXEC_LINES.revenue)).toBeNull();
    expect(r).toEqual({
      kind: 'ready',
      data: {
        window: W,
        total: 400,
        rows: [
          { code: '4000', name: 'Sales', amount: 300, share: 0.75 },
          { code: '4200', name: 'Sponsorship revenue', amount: 100, share: 0.25 },
        ],
      },
    });
  });

  test('a ledger with no revenue in the window says so, with the window', async () => {
    stub({
      [EXEC_READS.revenue(W)]: () => ok({ revenue: [], total_revenue_cents: 0 }),
    });
    const r = await loadRevenueMix(W);
    expect(noteOf(r, EXEC_LINES.revenue)).toEqual({
      text: 'No revenue posted to the ledger between 2025-09-27 and 2026-09-27.',
      failed: false,
    });
  });

  test('a failed read says the read failed, not that there was no revenue', async () => {
    stub({ [EXEC_READS.revenue(W)]: () => status(503) });
    const r = await loadRevenueMix(W);
    expect(noteOf(r, EXEC_LINES.revenue)).toEqual({
      text: `Couldn't load the revenue mix — ${EXEC_READS.revenue(W)}: HTTP 503`,
      failed: true,
    });
  });
});

describe('Cash & receivables — /api/ledger/balance-sheet', () => {
  const sheet = (assets: unknown[], liabilities: unknown[] = []) => ({
    as_of: '2026-09-27',
    assets,
    liabilities,
    total_assets_cents: 0,
  });
  const line = (account_code: string, amount_cents: number) => ({
    account_code,
    account_name: account_code,
    amount_cents,
  });

  test('cash, in-transit (when not zero), receivables and payables', async () => {
    stub({
      [EXEC_READS.balance]: () =>
        ok(sheet([line('1000', 100), line('1010', 0), line('1100', 250)], [line('2100', 40)])),
    });
    const r = await loadBalances();
    expect(noteOf(r, EXEC_LINES.balance)).toBeNull();
    expect(r).toEqual({
      kind: 'ready',
      data: {
        asOf: '2026-09-27',
        stats: [
          { label: 'Cash', cents: 100 },
          { label: 'Accounts receivable', cents: 250 },
          { label: 'Accounts payable', cents: 40 },
        ],
      },
    });
  });

  test('none of the four lines renders a sentence, not an empty row (6d074e62)', async () => {
    stub({ [EXEC_READS.balance]: () => ok(sheet([line('1500', 9)])) });
    const r = await loadBalances();
    expect(noteOf(r, EXEC_LINES.balance)).toEqual({
      text: 'No cash, receivable or payable balances on the ledger as of 2026-09-27.',
      failed: false,
    });
  });

  test('a failed read carries its status', async () => {
    stub({ [EXEC_READS.balance]: () => status(502) });
    const r = await loadBalances();
    expect(noteOf(r, EXEC_LINES.balance)).toEqual({
      text: "Couldn't load the balance sheet — /api/ledger/balance-sheet: HTTP 502",
      failed: true,
    });
  });
});

describe('Waiting on you — /api/jobs/assignments?for=me (f0f04375)', () => {
  test('the read is the viewer’s own queue, one page at the API ceiling', () => {
    expect(EXEC_READS.waiting).toBe(`/api/jobs/assignments?for=me&limit=${WAITING_LIMIT}`);
    expect(WAITING_LIMIT).toBe(1000);
  });

  test('a count of the steps waiting', async () => {
    stub({ [EXEC_READS.waiting]: () => ok({ data: [{}, {}, {}], total: 3 }) });
    const r = await loadWaiting();
    expect(noteOf(r, EXEC_LINES.waiting)).toBeNull();
    expect(r.kind === 'ready' && waitingText(r.data)).toBe('3 steps are waiting on you.');
  });

  test('one step reads in the singular', async () => {
    stub({ [EXEC_READS.waiting]: () => ok({ data: [{}], total: 1 }) });
    const r = await loadWaiting();
    expect(r.kind === 'ready' && waitingText(r.data)).toBe('1 step is waiting on you.');
  });

  test('a page at the ceiling is a floor, never the whole count', async () => {
    stub({ [EXEC_READS.waiting]: () => ok({ data: [], total: WAITING_LIMIT }) });
    const r = await loadWaiting();
    expect(r.kind === 'ready' && waitingText(r.data)).toBe('1000+ steps are waiting on you.');
  });

  test('nothing waiting is its own line', async () => {
    stub({ [EXEC_READS.waiting]: () => ok({ data: [], total: 0 }) });
    const r = await loadWaiting();
    expect(noteOf(r, EXEC_LINES.waiting)).toEqual({
      text: 'Nothing is waiting on you.',
      failed: false,
    });
  });

  test('a refusal is a failure, never "nothing is waiting"', async () => {
    stub({ [EXEC_READS.waiting]: () => status(401) });
    const r = await loadWaiting();
    expect(noteOf(r, EXEC_LINES.waiting)).toEqual({
      text: `Couldn't load what is waiting on you — ${EXEC_READS.waiting}: HTTP 401`,
      failed: true,
    });
  });
});

describe('every card links to the deeper view (c270e944)', () => {
  // A link is a path plus a query, and the router reads them apart.
  const routeOf = (to: string) => {
    const u = new URL(to, 'http://exec.test');
    return parseRoute(u.pathname, u.search);
  };

  test('each link lands on the route that shows the card’s own number', () => {
    expect(routeOf(EXEC_LINKS.revenue.to)).toEqual({
      kind: 'finance',
      view: { tab: 'income-statement', entry: '', fact: '' },
    });
    expect(routeOf(EXEC_LINKS.balance.to)).toEqual({
      kind: 'finance',
      view: { tab: 'balance-sheet', entry: '', fact: '' },
    });
    expect(routeOf(EXEC_LINKS.jobs.to).kind).toBe('jobs');
    expect(routeOf(EXEC_LINKS.waiting.to)).toEqual({ kind: 'me' });
    expect(routeOf(executiveWorkLink('executive').to)).toEqual({
      kind: 'department',
      code: 'executive',
    });
  });

  test('the link texts', () => {
    expect(EXEC_LINKS.revenue.label).toBe('Open income statement →');
    expect(EXEC_LINKS.balance.label).toBe('Open balance sheet →');
    expect(EXEC_LINKS.jobs.label).toBe('View all jobs →');
    expect(EXEC_LINKS.waiting.label).toBe('Open My Day →');
    expect(executiveWorkLink('executive').label).toBe('Open the Executive department →');
  });
});

// ---------------------------------------------------------------------------
// The page renders what the module answers — source pins, because the
// unit runner cannot mount a component. Comments are stripped first:
// the story of a retired read may be told in prose, never run.

const source = readFileSync(new URL('./ExecPage.svelte', import.meta.url), 'utf8');
const code = source
  .replace(/<!--[\s\S]*?-->/g, '')
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/(^|[^:])\/\/.*$/gm, '$1');
const markup = code.slice(code.indexOf('</script>'));

describe('ExecPage renders the module’s lines and links', () => {
  test('every card carries a link, outside its read’s branches', () => {
    const cards = markup.split('<section class="exec-card').slice(1);
    expect(cards.length).toBe(5);
    for (const card of cards) {
      const body = card.slice(0, card.indexOf('</section>'));
      expect(body).toMatch(/<Link to=\{href\((EXEC_LINKS\.\w+|workLink)\.to\)\}>/);
      // The link is the card's last element, after every {/if}: a card
      // whose read failed still leads to the view that may answer.
      expect(body.lastIndexOf('<Link')).toBeGreaterThan(body.lastIndexOf('{/if}'));
    }
  });

  test('every read’s line comes from noteOf over its own lines', () => {
    for (const key of ['jobs', 'revenue', 'balance', 'waiting']) {
      expect(code).toContain(`noteOf(${key}, EXEC_LINES.${key})`);
    }
    // One place paints a failed read, on the shared marker.
    expect(markup).toContain('<p class="empty load-failed" role="alert">{n.text}</p>');
  });

  test('the Executive work card is the department’s own in / working / out (1f33e125)', () => {
    expect(markup).toContain('<DepartmentThirds code={department} />');
  });

  test('the retired reads are gone (6775546c, d93cfb99)', () => {
    expect(code).not.toContain('/api/products');
    expect(code).not.toContain('/api/commerce/summary');
    expect(code).not.toContain('loadCommerceSummary');
    expect(code).not.toContain('Finished goods');
    expect(code).not.toContain("import Section from");
  });

  test('the header promises only what a card shows (faf3f635)', () => {
    expect(markup).toContain('Money, open work, and what is waiting on you');
    expect(markup).not.toContain('What changed');
  });
});
