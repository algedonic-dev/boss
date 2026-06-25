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
    // Landing — bare / is the public alias for the UX home; /ux is its
    // canonical perspective root and resolves to the same home view.
    ['/', { kind: 'home' }],
    ['/ux', { kind: 'home' }],
    // Browse / domain dashboards — User Experiences perspective (/ux/*).
    ['/ux/jobs', { kind: 'jobs' }],
    ['/ux/jobs/abc-123', { kind: 'jobDetail', jobId: 'abc-123' }],
    ['/ux/accounts', { kind: 'accounts' }],
    ['/ux/people', { kind: 'people' }],
    ['/ux/people/emp-aa-004', { kind: 'employee', empId: 'emp-aa-004' }],
    ['/ux/parts', { kind: 'parts' }],
    ['/ux/parts/SKU-1', { kind: 'part', partSku: 'SKU-1' }],
    ['/ux/products', { kind: 'products' }],
    ['/ux/products/FP-IPA-1-2-BBL', { kind: 'product', productSku: 'FP-IPA-1-2-BBL' }],
    // Finance — the regression nest. Specific cases MUST win over
    // the catch-all `/finance/(.+)` → invoice route.
    ['/ux/finance', { kind: 'finance' }],
    ['/ux/finance/new', { kind: 'newInvoice' }],
    ['/ux/finance/journal-entries/new', { kind: 'newJournalEntry' }],
    ['/ux/finance/inv-step-12345678', { kind: 'invoice', invoiceId: 'inv-step-12345678' }],
    // Shipping / shipments / support
    ['/ux/shipping', { kind: 'shipping' }],
    ['/ux/shipments/ship-1', { kind: 'shipmentDetail', shipmentId: 'ship-1' }],
    ['/ux/support', { kind: 'support' }],
    // Calendar / scheduling
    ['/ux/calendar', { kind: 'calendar' }],
    ['/ux/calendar/me', { kind: 'myCalendar' }],
    ['/ux/service/schedule', { kind: 'schedule' }],
    // Exec (User Experiences)
    ['/ux/exec', { kind: 'exec' }],
    // System Model perspective — IT surfaces re-rooted under /system/*.
    ['/system/monitoring', { kind: 'systemMonitoring' }],
    ['/system/monitoring/perf', { kind: 'systemMonitoringPerf' }],
    ['/system/monitoring/events', { kind: 'systemMonitoringEvents' }],
    ['/system/monitoring/atlas', { kind: 'systemMonitoringAtlas' }],
    ['/system/kb', { kind: 'systemKb' }],
    ['/system', { kind: 'systemModel' }],
    ['/system/subjects', { kind: 'systemSubjects' }],
    ['/system/design', { kind: 'systemDesign' }],
    ['/system/step-plugins', { kind: 'systemStepPlugins' }],
    ['/system/step-plugins/pour-quality-check', { kind: 'systemStepPluginDetail', pluginSlug: 'pour-quality-check' }],
    ['/system/dispatcher', { kind: 'dispatcherRules' }],
    ['/system/dispatcher/rules', { kind: 'dispatcherRulesList' }],
    ['/system/dispatcher/rules/restock-on-low', { kind: 'dispatcherRuleEdit', ruleName: 'restock-on-low' }],
    // Warehouse / catalog / assets
    ['/ux/warehouse', { kind: 'warehouse' }],
    ['/ux/catalog', { kind: 'catalog' }],
    ['/ux/catalog/some-sku', { kind: 'device', sku: 'some-sku' }],
    ['/ux/assets', { kind: 'assets' }],
    ['/ux/assets/asset-1', { kind: 'asset', assetId: 'asset-1' }],
    ['/ux/marketing-assets', { kind: 'marketingAssets' }],
    ['/ux/marketing-assets/mkt-1', { kind: 'marketingAsset', assetId: 'mkt-1' }],
    // Manual + workflows + watchlist + shop
    ['/ux/manual', { kind: 'manual' }],
    ['/system/workflows', { kind: 'workflows' }],
    ['/ux/manual/intro', { kind: 'manualSection', slug: 'intro' }],
    ['/ux/watchlist', { kind: 'watchlist' }],
    ['/ux/shop', { kind: 'shop' }],
    ['/ux/shop/FP-IPA-1-2-BBL', { kind: 'shopProduct', sku: 'FP-IPA-1-2-BBL' }],
    // PO + purchase orders
    ['/ux/purchase-orders/po-1', { kind: 'po', poId: 'po-1' }],
    ['/ux/vendor-invoices/vi-1', { kind: 'vendorInvoice', vendorInvoiceId: 'vi-1' }],
    // HR + QA + ops
    ['/ux/hr', { kind: 'hr' }],
    ['/ux/qa', { kind: 'qa' }],
    ['/ux/ops', { kind: 'ops' }],
    ['/ux/ops/anything', { kind: 'ops' }],
    // Policy + JobKind authoring (System Model). The job-kinds
    // `/authoring/<jobId>` route is the wildcard-precedence trap: it MUST
    // resolve before the catch-all `/job-kinds/(.+)` detail route.
    ['/system/policy', { kind: 'policy' }],
    ['/system/auth-admin', { kind: 'authAdmin' }],
    ['/system/job-kinds', { kind: 'jobKinds' }],
    ['/system/job-kinds/new', { kind: 'jobKindNew' }],
    ['/system/job-kinds/authoring/job-abc-123', { kind: 'jobKindDesign', jobId: 'job-abc-123' }],
    ['/system/job-kinds/seasonal-release', { kind: 'jobKindDetail', kindSlug: 'seasonal-release' }],
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
  test('/ux/finance/new → newInvoice, NOT invoice', () => {
    const r = parseRoute('/ux/finance/new');
    expect(r.kind).toBe('newInvoice');
  });
  test('/ux/finance/journal-entries/new → newJournalEntry, NOT invoice', () => {
    const r = parseRoute('/ux/finance/journal-entries/new');
    expect(r.kind).toBe('newJournalEntry');
  });
  test('/system/job-kinds/authoring/X → jobKindDesign, NOT jobKindDetail', () => {
    const r = parseRoute('/system/job-kinds/authoring/job-abc-123');
    expect(r.kind).toBe('jobKindDesign');
  });
});
