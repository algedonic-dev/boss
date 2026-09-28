// The /ux/exec page's reads, the line each read shows, and each card's
// link — as data, so ExecPage.test.ts can pin all three without a
// Svelte pass (page audit a1d62870, gap 10: backlog 1495cf68).
//
// EVERY READ IS A Remote (data/remote.ts): loading, ready or failed,
// and the failed arm carries the HTTP status or the error text. Until
// 2026-09-27 the Active jobs card set its summary only when `r.ok` and
// swallowed the rest (`// ignore`), so a role the jobs API refuses saw
// "Jobs data unavailable" with no status, and after the scoping car
// 19f08bd6 a self-scoped executive saw only their own counts, with
// nothing saying so (backlog 1445813b). A card renders `noteOf` its
// read — one line for loading, one for failed (on the shared
// `load-failed` marker), one for a true empty — or its data; there is
// no fourth branch in which a card can say nothing.
//
// WHAT RETIRED WITH THIS FILE. The Finished-goods card read
// /api/products for the brewery's "what do we have to sell?" and linked
// to a page gated on a module our instance does not run (6775546c).
// The Revenue-mix card read /api/commerce/summary, invoices no protocol
// of ours produces, beside a ledger that holds our revenue (d93cfb99):
// it now reads the ledger's income statement, the book every tenant's
// money protocols post to, so one money source cannot disagree with
// itself.

import { fetchRemote, type Remote } from '../data/remote';
import { departmentJobsPath } from '../shell/nav-catalog';

/** A read that has answered, one way or the other. */
export type Loaded<T> = Exclude<Remote<T>, { kind: 'loading' }>;

/** The assignments API's own ceiling (boss-jobs http MAX_LIMIT). A page
 *  that fills it is a floor, and says so (a-limit-is-not-a-filter). */
export const WAITING_LIMIT = 1000;

/** The trailing-twelve-month window, as the income statement takes it. */
export type Ttm = Readonly<{ from: string; to: string }>;

/** Every read the page makes. */
export const EXEC_READS = {
  jobs: '/api/jobs/summary?status=open',
  workflows: '/api/workflows',
  balance: '/api/ledger/balance-sheet',
  waiting: `/api/jobs/assignments?for=me&limit=${WAITING_LIMIT}`,
  revenue: (w: Ttm): string => `/api/ledger/income-statement?from=${w.from}&to=${w.to}`,
} as const;

/** The year ending `today` (YYYY-MM-DD): the same date a year earlier
 *  through today, the window the audit measured the ledger over. */
export function trailingYear(today: string): Ttm {
  const d = new Date(`${today}T00:00:00Z`);
  if (Number.isNaN(d.getTime())) return { from: today, to: today };
  d.setUTCFullYear(d.getUTCFullYear() - 1);
  return { from: d.toISOString().slice(0, 10), to: today };
}

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

// --- Active jobs -----------------------------------------------------------

export type KindCount = Readonly<{ kind: string; count: number }>;
export type JobsSummary = Readonly<{ total: number; byKind: ReadonlyArray<KindCount> }>;

/** The counts the summary answers, largest kind first. An answer with
 *  no `counts` object is a malformed read, never "no open jobs". */
export function parseJobsSummary(raw: unknown): JobsSummary {
  if (!isObject(raw) || !isObject(raw.counts)) {
    throw new Error('the jobs summary answered no counts');
  }
  const byKind = Object.entries(raw.counts)
    .filter((e): e is [string, number] => typeof e[1] === 'number')
    .map(([kind, count]) => ({ kind, count }))
    .sort((a, b) => b.count - a.count);
  const total =
    typeof raw.total === 'number' ? raw.total : byKind.reduce((s, r) => s + r.count, 0);
  return { total, byKind };
}

export function loadJobsSummary(): Promise<Loaded<JobsSummary>> {
  return fetchRemote(EXEC_READS.jobs, parseJobsSummary);
}

/** The summary counts only the packets the caller may read (19f08bd6):
 *  a self-scoped role sees its own. Shown beside every count. */
export const JOBS_SCOPE_NOTE = 'Counted over the packets you can read.';

/** Kind → label from the workflow registry. A failed read answers no
 *  labels and every kind shows as its code — honest, and pinned so it
 *  stays that way rather than hiding the kinds (a1d62870 gap 10). */
export async function loadKindLabels(): Promise<ReadonlyMap<string, string>> {
  const r = await fetchRemote(EXEC_READS.workflows, (raw) => {
    const rows = Array.isArray(raw) ? raw : [];
    return new Map(
      rows
        .filter(isObject)
        .filter((k) => typeof k.kind === 'string' && typeof k.label === 'string')
        .map((k) => [k.kind as string, k.label as string] as const),
    );
  });
  return r.kind === 'ready' ? r.data : new Map();
}

export function kindLabelOf(labels: ReadonlyMap<string, string>, kind: string): string {
  return labels.get(kind) ?? kind;
}

// --- Revenue mix -----------------------------------------------------------

export type RevenueRow = Readonly<{ code: string; name: string; amount: number; share: number }>;
export type RevenueMix = Readonly<{ window: Ttm; total: number; rows: ReadonlyArray<RevenueRow> }>;

/** The income statement's revenue lines by account, largest first,
 *  each with its share of total revenue. A zero line is no revenue. */
export function parseRevenueMix(raw: unknown, window: Ttm): RevenueMix {
  if (!isObject(raw) || !Array.isArray(raw.revenue)) {
    throw new Error('the income statement answered no revenue lines');
  }
  const lines = raw.revenue
    .filter(isObject)
    .map((l) => ({
      code: String(l.account_code ?? ''),
      name: String(l.account_name ?? l.account_code ?? ''),
      amount: typeof l.amount_cents === 'number' ? l.amount_cents : 0,
    }))
    .filter((l) => l.amount !== 0);
  const total =
    typeof raw.total_revenue_cents === 'number'
      ? raw.total_revenue_cents
      : lines.reduce((s, l) => s + l.amount, 0);
  const rows = lines
    .map((l) => ({ ...l, share: total > 0 ? l.amount / total : 0 }))
    .sort((a, b) => b.amount - a.amount);
  return { window, total, rows };
}

export function loadRevenueMix(window: Ttm): Promise<Loaded<RevenueMix>> {
  return fetchRemote(EXEC_READS.revenue(window), (raw) => parseRevenueMix(raw, window));
}

// --- Cash & receivables ----------------------------------------------------

export type BalanceStat = Readonly<{ label: string; cents: number }>;
export type Balances = Readonly<{ asOf: string; stats: ReadonlyArray<BalanceStat> }>;

/** The four working-capital lines the card shows, from the GL
 *  projection: cash (1000), cash in transit (1010, only when not
 *  zero), receivables (1100) and payables (2100). */
export function parseBalances(raw: unknown): Balances {
  if (!isObject(raw) || !Array.isArray(raw.assets) || typeof raw.as_of !== 'string') {
    throw new Error('the balance sheet answered no assets');
  }
  const assets = raw.assets.filter(isObject);
  const liabilities = Array.isArray(raw.liabilities) ? raw.liabilities.filter(isObject) : [];
  const find = (lines: ReadonlyArray<Record<string, unknown>>, code: string) =>
    lines.find((l) => l.account_code === code && typeof l.amount_cents === 'number');
  const cash = find(assets, '1000');
  const transit = find(assets, '1010');
  const ar = find(assets, '1100');
  const ap = find(liabilities, '2100');
  const stats: BalanceStat[] = [
    ...(cash ? [{ label: 'Cash', cents: cash.amount_cents as number }] : []),
    ...(transit && transit.amount_cents !== 0
      ? [{ label: 'In transit', cents: transit.amount_cents as number }]
      : []),
    ...(ar ? [{ label: 'Accounts receivable', cents: ar.amount_cents as number }] : []),
    ...(ap ? [{ label: 'Accounts payable', cents: ap.amount_cents as number }] : []),
  ];
  return { asOf: raw.as_of, stats };
}

export function loadBalances(): Promise<Loaded<Balances>> {
  return fetchRemote(EXEC_READS.balance, parseBalances);
}

// --- Waiting on you --------------------------------------------------------

export type Waiting = Readonly<{ count: number; capped: boolean }>;

/** How many workable steps the viewer's own queue holds — the count
 *  My Day lists (f0f04375: one fact, rendered once as a list there and
 *  once as a number here, never as a second list). */
export function parseWaiting(raw: unknown): Waiting {
  if (!isObject(raw) || !Array.isArray(raw.data)) {
    throw new Error('the assignments read answered no rows');
  }
  const count = typeof raw.total === 'number' ? raw.total : raw.data.length;
  return { count, capped: count >= WAITING_LIMIT };
}

export function loadWaiting(): Promise<Loaded<Waiting>> {
  return fetchRemote(EXEC_READS.waiting, parseWaiting);
}

export function waitingText(w: Waiting): string {
  const n = `${w.count}${w.capped ? '+' : ''}`;
  return w.count === 1 && !w.capped
    ? `${n} step is waiting on you.`
    : `${n} steps are waiting on you.`;
}

// --- Lines -----------------------------------------------------------------

/** The line a card shows in place of its data. */
export type Note = Readonly<{ text: string; failed: boolean }>;

export type Lines<T> = Readonly<{
  loading: string;
  failed: (error: string) => string;
  /** The true-empty sentence, or null when the data has something. */
  empty: (data: T) => string | null;
}>;

/** The line for a read's state, or null when the card renders data. */
export function noteOf<T>(r: Remote<T>, lines: Lines<T>): Note | null {
  if (r.kind === 'loading') return { text: lines.loading, failed: false };
  if (r.kind === 'failed') return { text: lines.failed(r.error), failed: true };
  const empty = lines.empty(r.data);
  return empty === null ? null : { text: empty, failed: false };
}

export const EXEC_LINES: Readonly<{
  jobs: Lines<JobsSummary>;
  revenue: Lines<RevenueMix>;
  balance: Lines<Balances>;
  waiting: Lines<Waiting>;
}> = {
  jobs: {
    loading: 'Loading open jobs…',
    failed: (e) => `Couldn't load open jobs — ${e}`,
    empty: (s) => (s.total === 0 ? 'No open jobs among the packets you can read.' : null),
  },
  revenue: {
    loading: 'Loading revenue mix…',
    failed: (e) => `Couldn't load the revenue mix — ${e}`,
    empty: (m) =>
      m.rows.length === 0
        ? `No revenue posted to the ledger between ${m.window.from} and ${m.window.to}.`
        : null,
  },
  balance: {
    loading: 'Loading balance-sheet snapshot…',
    failed: (e) => `Couldn't load the balance sheet — ${e}`,
    // 6d074e62: an "As of" line over an empty stat row read as a broken
    // render.
    empty: (b) =>
      b.stats.length === 0
        ? `No cash, receivable or payable balances on the ledger as of ${b.asOf}.`
        : null,
  },
  waiting: {
    loading: 'Loading what is waiting on you…',
    failed: (e) => `Couldn't load what is waiting on you — ${e}`,
    empty: (w) => (w.count === 0 ? 'Nothing is waiting on you.' : null),
  },
};

// --- Links -----------------------------------------------------------------

export type CardLink = Readonly<{ to: string; label: string }>;

/** Each card's deeper view — the route that shows the card's own
 *  number, so the footer's "each card links to the deeper view" is
 *  true of every card (c270e944). */
export const EXEC_LINKS: Readonly<{
  revenue: CardLink;
  balance: CardLink;
  jobs: CardLink;
  waiting: CardLink;
}> = {
  revenue: { to: '/ux/finance?tab=income-statement', label: 'Open income statement →' },
  balance: { to: '/ux/finance?tab=balance-sheet', label: 'Open balance sheet →' },
  jobs: { to: '/ux/jobs', label: 'View all jobs →' },
  waiting: { to: '/ux/me', label: 'Open My Day →' },
};

/** The Executive work card's link: the department's own jobs view. */
export function executiveWorkLink(code: string): CardLink {
  return { to: departmentJobsPath(code), label: 'Open the Executive department →' };
}
