// Runtime schemas for the finance overview's two reads (backlog 9045a570).
//
// loadCommerceSummary and loadApAging cast their bodies, so a 200 that
// was not the shape rendered `undefined.toLocaleString()` and the tab
// died mid-render — seen on every mocked run of 2026-09-25 while the
// mocked floor answered ap-aging with `[]`. Parsed here, a wrong shape
// is the "unavailable" line the page already paints for a failed read.
//
// Each schema is typed against the api.ts type it validates, so the two
// cannot drift apart: a field added to the type and not here fails to
// compile (CLAUDE.md §9a).

import { z } from '../data/parseResponse';
import type { ApAging, CommerceSummary } from './api';

const AgingBucketSchema = z.object({
  label: z.string(),
  count: z.number(),
  total_cents: z.number(),
});

export const ApAgingSchema: z.ZodType<ApAging> = z.object({
  buckets: z.array(AgingBucketSchema),
  total_outstanding_cents: z.number(),
  total_invoice_count: z.number(),
  currency: z.string(),
});

export const CommerceSummarySchema: z.ZodType<CommerceSummary> = z.object({
  revenue_ttm: z.array(
    z.object({
      category: z.string(),
      revenue_cents: z.number(),
      cogs_cents: z.number(),
      gross_margin_cents: z.number(),
      margin_pct: z.number(),
    }),
  ),
  total_revenue_ttm_cents: z.number(),
  total_cogs_ttm_cents: z.number(),
  total_gross_margin_ttm_cents: z.number(),
  ar_aging: z.array(AgingBucketSchema),
  total_outstanding_cents: z.number(),
  total_invoice_count: z.number(),
  revenue_by_month: z.array(
    z.object({
      month: z.string(),
      revenue_cents: z.number(),
      invoice_count: z.number(),
    }),
  ),
  currency: z.string(),
});
