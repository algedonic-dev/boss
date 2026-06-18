// Shop — public product browser (/shop) + product detail
// (/shop/{sku}). Home page is a hero + product grid; product
// cards link to per-SKU detail. Detail page has Request-quote
// CTA that toggles a quote form (Submit gated on validity,
// Cancel collapses).
//
// Pre-2026-05-04 the home was hero + filter sidebar; the
// sidebar was removed in the brewery rework. The spec here
// matches the current layout.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Shop home (/shop) — controls', () => {
  test('hero + product grid render', async ({ page }) => {
    await mountPage(page, '/shop');
    await expect(page.locator('.shop-hero-title')).toBeVisible({
      timeout: 10_000,
    });
    // The grid container always renders, even before the catalog
    // resolves; cards inside skip-gate below if products empty.
    await expect(page.locator('.shop-grid').first()).toBeVisible();
  });

  test('clicking a product card navigates to /shop/{sku}', async ({ page }) => {
    await mountPage(page, '/shop');
    const card = page.locator('.shop-card').first();
    if ((await card.count()) === 0) {
      test.skip(true, 'no shop products in catalog');
    }
    await Promise.all([
      page.waitForURL(/\/shop\/[^/]+/, { timeout: 10_000 }),
      card.click(),
    ]);
  });
});

test.describe('Shop product detail (/shop/{sku}) — quote form', () => {
  test('quote form opens, Submit gated on email, Cancel closes', async ({
    page,
  }) => {
    // Pick the first product from the home page; skip if empty.
    await mountPage(page, '/shop');
    const card = page.locator('.shop-card').first();
    if ((await card.count()) === 0) {
      test.skip(true, 'no shop products to navigate into');
    }
    await card.click();
    await page.waitForURL(/\/shop\/[^/]+/, { timeout: 10_000 });

    // Click Request a quote → form appears.
    await page.getByRole('button', { name: /request a quote/i }).click();
    const form = page.locator('.shop-quote-form');
    await expect(form).toBeVisible({ timeout: 5_000 });

    const submit = form.getByRole('button', { name: /submit quote request/i });
    await expect(submit).toBeDisabled();

    // Filling name + email enables Submit.
    await form.locator('#sqf-name').fill('Smoke Tester');
    await form.locator('#sqf-email').fill('test@example.com');
    await expect(submit).toBeEnabled({ timeout: 5_000 });

    // Cancel collapses the form.
    await form.getByRole('button', { name: /^cancel$/i }).click();
    await expect(form).toBeHidden({ timeout: 5_000 });
  });
});
