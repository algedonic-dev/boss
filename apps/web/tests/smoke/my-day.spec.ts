// Phase-0 Playwright: does the Svelte build render My Day?
//
// This is the same pair of assertions used in the React suite
// (apps/web/tests/smoke/jobs.spec.ts → "My Day"). Running them
// against the Svelte bundle proves the framework swap preserves
// the DOM contract — which is the whole case for why the
// migration doesn't churn the test suite.
//
// Run with:
//   cd apps/web
//   bun run build
//   bun src/dev-server.ts &          # port 5174
//   bunx playwright test --config playwright.config.ts

import { test, expect } from '@playwright/test';

test.describe('My Day (Svelte phase-0)', () => {
  test('loads as the default page with a greeting', async ({ page }) => {
    await page.goto('/');
    await expect(
      page.locator('text=/Good (morning|afternoon|evening)/i'),
    ).toBeVisible();
  });

  test('renders the My Jobs section at /me', async ({ page }) => {
    await page.goto('/me');
    await expect(page.locator('text=My Jobs')).toBeVisible();
  });

  test('At a glance panel shows a job count', async ({ page }) => {
    await page.goto('/me');
    const panel = page.locator('section').filter({ hasText: 'At a glance' });
    await expect(panel).toBeVisible();
    await expect(panel.locator('.me-stat-num').first()).toBeVisible();
  });
});
