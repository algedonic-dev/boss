// Commerce API fetchers — port of apps/web/src/finance/api.ts.

import type { Invoice } from './types';
import { fetchRemote, type Remote } from '../data/remote';

const API_BASE = '/api/commerce';

export type ArAgingBucket = {
  label: string;
  count: number;
  total_cents: number;
};

export type ApAgingBucket = {
  label: string;
  count: number;
  total_cents: number;
};

export type ApAging = {
  buckets: ReadonlyArray<ApAgingBucket>;
  total_outstanding_cents: number;
  total_invoice_count: number;
  currency: string;
};

export type CategoryMargin = {
  category: string;
  revenue_cents: number;
  cogs_cents: number;
  gross_margin_cents: number;
  margin_pct: number;
};

export type MonthlyRevenue = {
  month: string;
  revenue_cents: number;
  invoice_count: number;
};

export type CommerceSummary = {
  revenue_ttm: ReadonlyArray<CategoryMargin>;
  total_revenue_ttm_cents: number;
  total_cogs_ttm_cents: number;
  total_gross_margin_ttm_cents: number;
  ar_aging: ReadonlyArray<ArAgingBucket>;
  total_outstanding_cents: number;
  total_invoice_count: number;
  revenue_by_month: ReadonlyArray<MonthlyRevenue>;
  currency: string;
};

import { fetchPaged, type PagedResult } from '../data/paginated';
import { ApAgingSchema, CommerceSummarySchema } from './schemas';

/// The one invoice list read, named so the empty state can say which
/// read failed (backlog 0ef5e008).
export const INVOICES_LIST_URL = `${API_BASE}/invoices?limit=1000`;

/// The invoice list, failure included. The old signature swallowed a
/// failed fetch into an empty page, so an outage rendered the filters
/// line with every count at 0 (packet 3fba9c35, the false-empty sweep).
export function loadInvoices(): Promise<PagedResult<Invoice>> {
  return fetchPaged<Invoice>(INVOICES_LIST_URL);
}

/// Keep the failed read and its cause through the overview (98ef10a6).
/// A malformed 200 is a failed answer too, never a partial summary.
export function loadCommerceSummary(): Promise<Exclude<Remote<CommerceSummary>, { kind: 'loading' }>> {
  return fetchRemote(`${API_BASE}/summary`, (raw) => {
    const parsed = CommerceSummarySchema.safeParse(raw);
    if (!parsed.success) throw new Error(`${API_BASE}/summary: invalid response — ${parsed.error.message}`);
    return parsed.data;
  }).then((result) => result.kind === 'failed' && !result.error.includes(`${API_BASE}/summary`)
    ? { ...result, error: `${API_BASE}/summary: ${result.error}` }
    : result);
}

/// The AP consumer renders null as its unavailable alert (9045a570).
export async function loadApAging(): Promise<ApAging | null> {
  try {
    const r = await fetch('/api/inventory/ap-aging');
    if (!r.ok) return null;
    const parsed = ApAgingSchema.safeParse(await r.json());
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}
