// Commerce API fetchers — port of apps/web/src/finance/api.ts.

import type { Invoice } from './types';

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

import { fetchPaged, type Paged } from '../data/paginated';

export async function loadInvoices(): Promise<Paged<Invoice>> {
  return (
    (await fetchPaged<Invoice>(`${API_BASE}/invoices?limit=1000`)) ?? {
      data: [],
      total: 0,
      limit: 0,
      offset: 0,
    }
  );
}

export async function loadCommerceSummary(): Promise<CommerceSummary | null> {
  try {
    const r = await fetch(`${API_BASE}/summary`);
    if (!r.ok) return null;
    return (await r.json()) as CommerceSummary;
  } catch {
    return null;
  }
}

export async function loadApAging(): Promise<ApAging | null> {
  try {
    const r = await fetch('/api/inventory/ap-aging');
    if (!r.ok) return null;
    return (await r.json()) as ApAging;
  } catch {
    return null;
  }
}
