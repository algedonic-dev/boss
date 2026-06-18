// Router resolution snapshot. Pin every canonical path to its
// expected `Route` so a future router edit can't silently let a
// wildcard eclipse a specific case (a wildcard-before-specific
// ordering bug is the recurring failure mode; the `/finance`
// wildcard catches everything tail-shaped as `invoice`).
//
// Add an entry here every time `router.ts` learns a new path.
// Run via `bun test`.

import { beforeAll, describe, expect, test } from 'bun:test';
import { parseRoute } from './router';

// `/jobs` reads `window.location.search` for filter query params.
// Stub a minimal window shape so the test runs in bun's
// non-DOM context.
beforeAll(() => {
  if (typeof (globalThis as { window?: unknown }).window === 'undefined') {
    (globalThis as { window: unknown }).window = {
      location: { search: '' },
    };
  }
});

describe('parseRoute — every specific path matches its specific case', () => {
  // Each entry: [path, expected partial-Route shape].
  // We don't check fields that come from URL-decoding (ids); only
  // that the discriminator `kind` is right + any positional param
  // is captured.
  const cases: Array<[string, Record<string, unknown>]> = [
    // Landing
    ['/', { kind: 'home' }],
    // Browse / domain dashboards
    ['/jobs', { kind: 'jobs' }],
    ['/jobs/abc-123', { kind: 'jobDetail', jobId: 'abc-123' }],
    ['/accounts', { kind: 'accounts' }],
    ['/people', { kind: 'people' }],
    ['/people/emp-aa-004', { kind: 'employee', empId: 'emp-aa-004' }],
    ['/parts', { kind: 'parts' }],
    ['/parts/SKU-1', { kind: 'part', partSku: 'SKU-1' }],
    ['/products', { kind: 'products' }],
    ['/products/FP-IPA-1-2-BBL', { kind: 'product', productSku: 'FP-IPA-1-2-BBL' }],
    // Finance — the regression nest. Specific cases MUST win over
    // the catch-all `/finance/(.+)` → invoice route.
    ['/finance', { kind: 'finance' }],
    ['/finance/new', { kind: 'newInvoice' }],
    ['/finance/journal-entries/new', { kind: 'newJournalEntry' }],
    ['/finance/inv-step-12345678', { kind: 'invoice', invoiceId: 'inv-step-12345678' }],
    // Shipping / shipments / support
    ['/shipping', { kind: 'shipping' }],
    ['/shipments/ship-1', { kind: 'shipmentDetail', shipmentId: 'ship-1' }],
    ['/support', { kind: 'support' }],
    // Calendar / scheduling
    ['/calendar', { kind: 'calendar' }],
    ['/calendar/me', { kind: 'myCalendar' }],
    ['/service/schedule', { kind: 'schedule' }],
    // Exec + IT (post the /cto retirement)
    ['/exec', { kind: 'exec' }],
    ['/it/monitoring', { kind: 'itMonitoring' }],
    ['/it/monitoring/perf', { kind: 'itMonitoringPerf' }],
    ['/it/monitoring/events', { kind: 'itMonitoringEvents' }],
    ['/it/monitoring/atlas', { kind: 'itMonitoringAtlas' }],
    ['/it/kb', { kind: 'itKb' }],
    // Warehouse / catalog / assets
    ['/warehouse', { kind: 'warehouse' }],
    ['/catalog', { kind: 'catalog' }],
    ['/catalog/some-sku', { kind: 'device', sku: 'some-sku' }],
    ['/assets', { kind: 'assets' }],
    ['/assets/asset-1', { kind: 'asset', assetId: 'asset-1' }],
    ['/marketing-assets', { kind: 'marketingAssets' }],
    ['/marketing-assets/mkt-1', { kind: 'marketingAsset', assetId: 'mkt-1' }],
    // Manual + workflows + watchlist + shop
    ['/manual', { kind: 'manual' }],
    ['/workflows', { kind: 'workflows' }],
    ['/manual/intro', { kind: 'manualSection', slug: 'intro' }],
    ['/watchlist', { kind: 'watchlist' }],
    ['/shop', { kind: 'shop' }],
    ['/shop/FP-IPA-1-2-BBL', { kind: 'shopProduct', sku: 'FP-IPA-1-2-BBL' }],
    // PO + purchase orders
    ['/purchase-orders/po-1', { kind: 'po', poId: 'po-1' }],
    ['/vendor-invoices/vi-1', { kind: 'vendorInvoice', vendorInvoiceId: 'vi-1' }],
    // HR + QA + ops
    ['/hr', { kind: 'hr' }],
    ['/qa', { kind: 'qa' }],
    ['/ops', { kind: 'ops' }],
    ['/ops/anything', { kind: 'ops' }],
    // Policy + admin authoring
    ['/policy', { kind: 'policy' }],
  ];

  for (const [path, expected] of cases) {
    test(`${path}`, () => {
      const actual = parseRoute(path);
      for (const [key, value] of Object.entries(expected)) {
        expect((actual as Record<string, unknown>)[key]).toBe(value);
      }
    });
  }
});

describe('parseRoute — wildcard does not shadow specific cases', () => {
  // Pins the canonical fix for the `/finance/(.+)` wildcard
  // precedence bug: specific cases MUST be declared before the
  // wildcard or they resolve as `{ kind: 'invoice', invoiceId: ... }`.
  // Any new `/finance/X` case the SPA introduces should be added to
  // the table above + a regression assertion here.
  test('/finance/new → newInvoice, NOT invoice', () => {
    const r = parseRoute('/finance/new');
    expect(r.kind).toBe('newInvoice');
  });
  test('/finance/journal-entries/new → newJournalEntry, NOT invoice', () => {
    const r = parseRoute('/finance/journal-entries/new');
    expect(r.kind).toBe('newJournalEntry');
  });
});
