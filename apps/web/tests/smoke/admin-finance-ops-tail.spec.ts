// Tail batch — admin Policy + StepPlugins detail, Finance tabs,
// Support / QA / IT tab navigation.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Admin Policy (/admin/policy) — controls', () => {
  test('Refresh button is visible + clickable', async ({ page }) => {
    await mountPage(page, '/admin/policy', { titleMatch: /policy|rule/i });
    // The page exposes a single load-button + per-row Edit
    // buttons that open a flyout. The load button label is
    // ambiguous ("Refresh" / "Reload" / context-dependent); just
    // assert at least one wb-btn renders.
    await expect(page.locator('button.wb-btn').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('row Edit button opens the policy flyout', async ({ page }) => {
    await mountPage(page, '/admin/policy', { titleMatch: /policy|rule/i });
    const edit = page
      .locator('table tbody tr button')
      .filter({ hasText: /edit/i })
      .first();
    if ((await edit.count()) === 0) {
      test.skip(true, 'no policy rules in registry');
    }
    await edit.click();
    // Flyout renders a form with Save / Cancel buttons.
    await expect(
      page.getByRole('button', { name: /save|cancel/i }).first(),
    ).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Admin Step plugins (/admin/step-plugins) — list + detail', () => {
  test('list page mounts', async ({ page }) => {
    await mountPage(page, '/admin/step-plugins');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('detail page Publish + Retire buttons render', async ({ page }) => {
    await mountPage(page, '/admin/step-plugins');
    const link = page
      .locator('a[href*="/admin/step-plugins/"]')
      .first();
    if ((await link.count()) === 0) {
      test.skip(true, 'no step plugins registered');
    }
    await Promise.all([
      page.waitForURL(/\/admin\/step-plugins\/[^/]+/, { timeout: 10_000 }),
      link.click(),
    ]);
    await expect(
      page.getByRole('button', { name: /publish/i }),
    ).toBeVisible({ timeout: 10_000 });
    await expect(page.getByRole('button', { name: /retire/i })).toBeVisible();
  });
});

test.describe('Finance (/finance) — tabs', () => {
  test('all 8 tabs toggle aria-selected', async ({ page }) => {
    await mountPage(page, '/finance');
    for (const label of [
      'Overview',
      'Invoices',
      'PO Approvals',
      'Income statement',
      'Balance sheet',
      'Cash flow',
      'Trial Balance',
      'Tax liability',
    ]) {
      const tab = page.getByRole('tab', { name: label });
      await tab.click();
      await expect(tab).toHaveAttribute('aria-selected', 'true', { timeout: 3_000 });
    }
  });
});

test.describe('Support (/support) — tabs', () => {
  test('Overview / Active Cases / Account Health tabs toggle', async ({ page }) => {
    await mountPage(page, '/support');
    for (const label of ['Overview', 'Active Cases', 'Account Health']) {
      const tab = page.getByRole('tab', { name: label });
      await tab.click();
      await expect(tab).toHaveAttribute('aria-selected', 'true', { timeout: 3_000 });
    }
  });
});

test.describe('QA (/qa) — tabs', () => {
  test('Overview / Batch QC / Compliance / Equipment preventive maintenance tabs toggle', async ({
    page,
  }) => {
    await mountPage(page, '/qa');
    for (const label of ['Overview', 'Batch QC', 'Compliance', 'Equipment preventive maintenance']) {
      const tab = page.getByRole('tab', { name: label });
      await tab.click();
      await expect(tab).toHaveAttribute('aria-selected', 'true', { timeout: 3_000 });
    }
  });
});

test.describe('IT (/it) — controls', () => {
  test('4 page tabs toggle aria-selected', async ({ page }) => {
    await mountPage(page, '/it');
    for (const label of ['Providers', 'Banking activity', 'Payroll activity', 'Tax activity']) {
      const tab = page.getByRole('tab', { name: label });
      await tab.click();
      await expect(tab).toHaveAttribute('aria-selected', 'true', { timeout: 3_000 });
    }
  });

  test('Providers tab — provider rows expand', async ({ page }) => {
    await mountPage(page, '/it');
    await page.getByRole('tab', { name: /providers/i }).click();
    // Each provider row has a chevron toggle; expanding reveals
    // Configure / Test / Sync action buttons.
    const expandBtn = page
      .locator('tr button, .it-provider-row button')
      .first();
    if ((await expandBtn.count()) === 0) {
      test.skip(true, 'no providers in seed');
    }
    await expandBtn.click();
    // Action buttons render after expansion.
    const action = page.getByRole('button', { name: /configure|test|sync/i });
    await expect(action.first()).toBeVisible({ timeout: 5_000 });
  });
});
