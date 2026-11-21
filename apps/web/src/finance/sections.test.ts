import { describe, expect, test } from 'bun:test';
import { FINANCE_TABS } from './financeQuery';
import { financeModules, financeTabs, overviewShown, tabShown } from './sections';
import type { ManifestState } from '@boss/web-kit/session/manifest-inline';

// /finance's AR aging, gross margin and Invoices read boss-commerce, and
// its AP aging and PO Approvals read boss-inventory; Algedonic runs
// neither, so the page answered from sources no protocol of the tenant
// writes (backlog 801b6a7a, page audit 3f964c57 gap 3; f6d54f7b). Design
// c2820008 D1: a section whose module the tenant does not run is ABSENT,
// not zero. These are the page's sections as a function of the manifest.

const ready = (modules: Record<string, boolean>): ManifestState => ({
  kind: 'ready',
  modules,
  labels: {},
  displayName: 'T',
});

const ids = (m: ReturnType<typeof financeModules>): string[] => financeTabs(m).map((t) => t.id);

describe('financeModules', () => {
  test('a module is on only when the manifest lists it true', () => {
    expect(financeModules(ready({ commerce: true, warehouse: false }))).toEqual({ commerce: true, warehouse: false });
    expect(financeModules(ready({}))).toEqual({ commerce: false, warehouse: false });
  });

  test('the storefront is not the invoice book: shop does not switch commerce on', () => {
    expect(financeModules(ready({ shop: true })).commerce).toBe(false);
  });
});

describe('financeTabs', () => {
  test('a tenant running neither keeps only the ledger statements', () => {
    expect(ids({ commerce: false, warehouse: false })).toEqual([
      'income-statement',
      'balance-sheet',
      'cash-flow',
      'trial-balance',
      'tax-liability',
    ]);
  });

  test('a tenant running both keeps every tab, in the page order', () => {
    expect(ids({ commerce: true, warehouse: true })).toEqual(FINANCE_TABS.map((t) => t.id));
  });

  test('commerce alone brings the Overview and Invoices back, not PO Approvals', () => {
    const got = ids({ commerce: true, warehouse: false });
    expect(got).toContain('overview');
    expect(got).toContain('invoices');
    expect(got).not.toContain('approvals');
  });

  test('warehouse alone brings the Overview (AP aging) and PO Approvals, not Invoices', () => {
    const got = ids({ commerce: false, warehouse: true });
    expect(got).toContain('overview');
    expect(got).toContain('approvals');
    expect(got).not.toContain('invoices');
  });
});

describe('overviewShown', () => {
  test('the Overview has a section only when the tenant runs one of its modules', () => {
    expect(overviewShown({ commerce: false, warehouse: false })).toBe(false);
    expect(overviewShown({ commerce: true, warehouse: false })).toBe(true);
    expect(overviewShown({ commerce: false, warehouse: true })).toBe(true);
  });
});

describe('tabShown', () => {
  const none = { commerce: false, warehouse: false };

  test('a tab the tenant runs is shown as asked', () => {
    expect(tabShown('trial-balance', none)).toBe('trial-balance');
    expect(tabShown('invoices', { commerce: true, warehouse: false })).toBe('invoices');
  });

  test('an absent tab — a deep link or the default Overview — opens the first tab the tenant runs', () => {
    expect(tabShown('overview', none)).toBe('income-statement');
    expect(tabShown('invoices', none)).toBe('income-statement');
    expect(tabShown('approvals', { commerce: true, warehouse: false })).toBe('overview');
  });
});
