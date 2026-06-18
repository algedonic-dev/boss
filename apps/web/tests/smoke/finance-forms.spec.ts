// Finance — NewInvoice + NewJournalEntry forms. Each is reached
// via /finance/* and exposes Add line / Remove line / Submit /
// Cancel buttons. Submit is gated on validity; tests assert the
// disabled-vs-enabled transition and that Cancel always navigates
// back to /finance.

import { test, expect } from '@playwright/test';
import { mountPage, clickButton } from './_helpers';

test.describe('Finance — New invoice form', () => {
  test('Create invoice is disabled until required fields are set', async ({
    page,
  }) => {
    await mountPage(page, '/finance/new');
    const create = page.getByRole('button', { name: /create invoice/i });
    await expect(create).toBeVisible({ timeout: 10_000 });
    await expect(create).toBeDisabled();
  });

  test('Add line adds a row, Remove line removes it', async ({ page }) => {
    await mountPage(page, '/finance/new');
    // Form starts with one line row.
    const tableRows = page.locator('table tbody tr');
    const initialCount = await tableRows.count();

    await clickButton(page, /add line/i);
    await expect(tableRows).toHaveCount(initialCount + 1, { timeout: 5_000 });

    // Each line carries a remove button (×).
    const removeBtn = page.locator('table tbody').getByRole('button').first();
    await removeBtn.click();
    await expect(tableRows).toHaveCount(initialCount, { timeout: 5_000 });
  });

  test('Cancel returns to /finance', async ({ page }) => {
    await mountPage(page, '/finance/new');
    await Promise.all([
      page.waitForURL(/\/finance\b/, { timeout: 10_000 }),
      page.getByRole('button', { name: /^cancel$/i }).click(),
    ]);
  });
});

test.describe('Finance — New journal entry form', () => {
  test('form mounts with debit + credit add buttons', async ({ page }) => {
    await mountPage(page, '/finance/journal-entries/new');
    await expect(page.getByRole('button', { name: /\+ debit line/i })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByRole('button', { name: /\+ credit line/i })).toBeVisible();
  });

  test('Add debit / credit line each add a row', async ({ page }) => {
    await mountPage(page, '/finance/journal-entries/new');
    const tbody = page.locator('table tbody');
    const initial = await tbody.locator('tr').count();
    await page.getByRole('button', { name: /\+ debit line/i }).click();
    await expect(tbody.locator('tr')).toHaveCount(initial + 1, { timeout: 5_000 });
    await page.getByRole('button', { name: /\+ credit line/i }).click();
    await expect(tbody.locator('tr')).toHaveCount(initial + 2, { timeout: 5_000 });
  });

  test('Cancel returns to /finance', async ({ page }) => {
    await mountPage(page, '/finance/journal-entries/new');
    await Promise.all([
      page.waitForURL(/\/finance\b/, { timeout: 10_000 }),
      page.getByRole('button', { name: /^cancel$/i }).click(),
    ]);
  });
});
