// Phase-2 Finance Playwright smokes — cover the 8-tab Finance page
// plus the invoice detail, new-invoice form, and new-journal-entry
// form. Each tab fetches a different ledger endpoint; if any of them
// break the page's shape silently, these catch it.

import { test, expect } from '@playwright/test';

test.describe('Phase-2 Finance', () => {
  test('finance page renders overview + tabs', async ({ page }) => {
    await page.goto('/finance');
    await expect(page.locator('h1')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('.tabs')).toBeVisible();
    // Overview tab lands first — AR aging table renders.
    await expect(
      page.locator('section').filter({ hasText: /Accounts receivable aging/i }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('invoices tab renders filters + list', async ({ page }) => {
    await page.goto('/finance');
    await page.locator('.tab').filter({ hasText: 'Invoices' }).click();
    // Either a table or an empty state.
    const panel = page.locator('.list-section');
    await expect(panel.locator('table, p.empty').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('trial balance tab renders the table', async ({ page }) => {
    await page.goto('/finance');
    await page.locator('.tab').filter({ hasText: 'Trial Balance' }).click();
    await expect(
      page.locator('section').filter({ hasText: /Trial balance/i }).first(),
    ).toBeVisible({ timeout: 10_000 });
    // Either the full table or an "unavailable" state.
    await expect(
      page.locator('.tb-table, p.empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('income statement tab renders date pickers', async ({ page }) => {
    await page.goto('/finance');
    await page.locator('.tab').filter({ hasText: 'Income statement' }).click();
    await expect(page.locator('input[type="date"]').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('balance sheet tab renders date picker', async ({ page }) => {
    await page.goto('/finance');
    await page.locator('.tab').filter({ hasText: 'Balance sheet' }).click();
    await expect(page.locator('input[type="date"]').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('cash flow tab renders', async ({ page }) => {
    await page.goto('/finance');
    await page.locator('.tab').filter({ hasText: 'Cash flow' }).click();
    await expect(
      page.locator('section').filter({ hasText: /Cash flow statement/i }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('tax liability tab renders', async ({ page }) => {
    await page.goto('/finance');
    await page.locator('.tab').filter({ hasText: 'Tax liability' }).click();
    // Either liabilities section or "unavailable" message.
    await expect(
      page
        .locator('section, p.empty')
        .filter({ hasText: /Outstanding tax liability|Tax liability unavailable/i })
        .first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('PO approvals tab renders', async ({ page }) => {
    await page.goto('/finance');
    await page.locator('.tab').filter({ hasText: 'PO Approvals' }).click();
    await expect(
      page
        .locator('section, p.empty')
        .filter({ hasText: /Pending approval|Loading purchase orders/i })
        .first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('new invoice form renders', async ({ page }) => {
    await page.goto('/finance/new');
    await expect(page.locator('h1')).toContainText(/new invoice/i);
    await expect(page.locator('input#ni-account-search')).toBeVisible();
    await expect(page.locator('input#ni-issued')).toBeVisible();
  });

  test('new journal entry form renders', async ({ page }) => {
    await page.goto('/finance/journal-entries/new');
    await expect(page.locator('h1')).toContainText(/new journal entry/i);
    await expect(page.locator('input#mje-posted')).toBeVisible();
    // Two debit+credit rows default out of the box.
    await expect(page.locator('table.ni-lines-table tbody tr')).toHaveCount(2, {
      timeout: 10_000,
    });
  });

  test('invoice detail loads for a known id', async ({ page }) => {
    // Pull an invoice id off the invoices tab.
    await page.goto('/finance');
    await page.locator('.tab').filter({ hasText: 'Invoices' }).click();
    const firstRow = page.locator('.list-section table tbody tr').first();
    await expect(firstRow).toBeVisible({ timeout: 10_000 });
    const invoiceLink = firstRow.locator('a').first();
    const href = await invoiceLink.getAttribute('href');
    if (href) {
      await page.goto(href);
      // Invoice detail shows a Line items section header.
      await expect(
        page.locator('h3').filter({ hasText: /^Line items/ }),
      ).toBeVisible({ timeout: 10_000 });
    }
  });
});
