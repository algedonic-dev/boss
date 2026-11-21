// /finance draws a commerce or inventory section only for a tenant that
// runs that module (backlog 801b6a7a, page audit 3f964c57 gap 3;
// f6d54f7b; design c2820008 D1).
//
// AR aging, gross margin by product line, the Invoices tab and New
// invoice read boss-commerce; AP aging and PO Approvals read
// boss-inventory. The live Algedonic manifest lists no `commerce` and
// `warehouse = false`, and no Algedonic protocol writes either source,
// so the page showed another tenant's model as zeros beside a ledger
// income statement that said otherwise. D1: a section whose module the
// tenant does not run is ABSENT — not drawn, not read — and the
// headline reads the ledger for everyone (car 80145201). The brewery
// lists both, and every section comes back.
//
// The section rules themselves are finance/sections.test.ts; this file
// pins the page that draws them, and that it makes no read of a module
// the tenant does not run.

import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import {
  installSmokeMocks,
  installTenantManifest,
  LIVE_MANIFEST_RECORDED_AT,
  MODULES_LIVE,
  signInWithPolicy,
} from './_smokeMocks';
import { pageAt } from '../fixtures/station-tree';

const PATH = pageAt('finance');

const LEDGER_TABS = ['Income statement', 'Balance sheet', 'Cash flow', 'Trial Balance', 'Tax liability'];

async function shape(page: Page, modules: Readonly<Record<string, boolean>>): Promise<string[]> {
  await installSmokeMocks(page);
  await signInWithPolicy(page);
  await installTenantManifest(page, modules, { inline: true });
  const reads: string[] = [];
  page.on('request', (request) => {
    if (/\/api\/(commerce|inventory)\//.test(request.url())) reads.push(new URL(request.url()).pathname);
  });
  return reads;
}

const tabs = (page: Page) => page.getByRole('tab');
const tab = (page: Page, name: string) => page.getByRole('tab', { name, exact: true });
const panel = (page: Page) => page.locator('.tab-panel');

test(`as live (recorded ${LIVE_MANIFEST_RECORDED_AT}): no commerce or inventory section is drawn or read`, async ({ page }) => {
  // The recording is the premise: if the live tenant ever lists either
  // module, this leg is about a different tenant and must say so.
  expect(MODULES_LIVE.commerce === true).toBe(false);
  expect(MODULES_LIVE.warehouse === true).toBe(false);
  const reads = await shape(page, MODULES_LIVE);
  await mountPage(page, PATH);

  // The default Overview has no section left, so the page opens on the
  // first tab the tenant runs, and the URL says which.
  await expect(tab(page, 'Income statement')).toHaveAttribute('aria-selected', 'true');
  await expect(tabs(page)).toHaveText(LEDGER_TABS);
  await expect(page).toHaveURL(/[?&]tab=income-statement/);
  await expect(page.getByRole('link', { name: '+ New journal entry' })).toBeVisible();
  await expect(page.getByRole('link', { name: '+ New invoice' })).toHaveCount(0);
  await expect(page.locator('.exec-header h1')).toHaveText(/trailing revenue|Finance/);

  // A link into a tab the tenant does not run opens one it does.
  await page.goto(`${PATH}?tab=invoices`);
  await expect(tab(page, 'Income statement')).toHaveAttribute('aria-selected', 'true');
  await expect(page).toHaveURL(/[?&]tab=income-statement/);
  await page.goto(`${PATH}?tab=approvals`);
  await expect(tab(page, 'Income statement')).toHaveAttribute('aria-selected', 'true');

  expect(reads).toEqual([]);
});

test('a tenant running commerce and warehouse (the brewery) keeps every section', async ({ page }) => {
  const reads = await shape(page, { ...MODULES_LIVE, commerce: true, warehouse: true });
  await mountPage(page, PATH);
  await expect(tabs(page)).toHaveText(['Overview', 'Invoices', 'PO Approvals', ...LEDGER_TABS]);
  await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
  await expect(panel(page).locator('h3')).toHaveText([
    'Accounts receivable aging',
    'Accounts payable aging',
    'Gross margin by product line (trailing 12 months)',
  ]);
  await expect(page.getByRole('link', { name: '+ New invoice' })).toBeVisible();
  await expect.poll(() => reads).toEqual(expect.arrayContaining([
    '/api/commerce/summary',
    '/api/commerce/invoices',
    '/api/inventory/ap-aging',
  ]));
});

test('commerce without warehouse keeps receivables and margin, and drops payables and PO Approvals unread', async ({ page }) => {
  const reads = await shape(page, { ...MODULES_LIVE, commerce: true, warehouse: false });
  await mountPage(page, PATH);
  await expect(tabs(page)).toHaveText(['Overview', 'Invoices', ...LEDGER_TABS]);
  await expect(panel(page).locator('h3')).toHaveText([
    'Accounts receivable aging',
    'Gross margin by product line (trailing 12 months)',
  ]);
  await tab(page, 'Invoices').click();
  await expect(panel(page).getByPlaceholder('Invoice, account…')).toBeVisible();
  expect(reads.filter((p) => p.startsWith('/api/inventory/'))).toEqual([]);
});

test('warehouse without commerce keeps payables and PO Approvals, and drops receivables, margin and Invoices unread', async ({ page }) => {
  const reads = await shape(page, { ...MODULES_LIVE, commerce: false, warehouse: true });
  await mountPage(page, PATH);
  await expect(tabs(page)).toHaveText(['Overview', 'PO Approvals', ...LEDGER_TABS]);
  await expect(panel(page).locator('h3')).toHaveText(['Accounts payable aging']);
  await expect(page.getByRole('link', { name: '+ New invoice' })).toHaveCount(0);
  expect(reads.filter((p) => p.startsWith('/api/commerce/'))).toEqual([]);
});
