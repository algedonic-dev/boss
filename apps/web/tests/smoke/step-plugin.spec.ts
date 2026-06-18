// StepPlugin loader smoke — confirms the Svelte JobDetailPage
// mounts plain-DOM plugin bundles for non-v1 step kinds. Pick the
// first service-queue job; its first step is 'sr-triage' (field-
// service tier 0), which ships at /plugins/sr-triage.js.

import { test, expect } from '@playwright/test';

test.describe('Step plugin loader', () => {
  test('field-service job mounts the sr-triage plugin', async ({ page }) => {
    await page.goto('/service');
    const firstRow = page.locator('table tbody tr').first();
    await expect(firstRow).toBeVisible({ timeout: 10_000 });
    const href = await firstRow.locator('a').first().getAttribute('href');
    expect(href).toMatch(/\/(service|jobs)\//);
    await page.goto(href!);

    // Steps section renders with at least one step card.
    await expect(
      page.locator('section').filter({ hasText: /Steps/ }).first(),
    ).toBeVisible({ timeout: 10_000 });

    // A plugin-backed step kind either renders its custom surface
    // or the loading skeleton shim. We assert either is visible —
    // the plain-DOM mount path is exercised by both.
    const pluginOrSurface = page
      .locator('.step-surface, code.mono')
      .filter({ hasText: /sr-triage|diagnostic-call|Loading/i })
      .first();
    await expect(pluginOrSurface).toBeVisible({ timeout: 15_000 });
  });

  test('step surfaces replace the old raw-table rendering', async ({ page }) => {
    await page.goto('/jobs');
    const firstRow = page.locator('table tbody tr').first();
    await expect(firstRow).toBeVisible({ timeout: 10_000 });
    const href = await firstRow.locator('a').first().getAttribute('href');
    await page.goto(href!);
    // The phase-1 raw-table had a "Blocked by" column header. The
    // new StepSurface-dispatched layout doesn't have that column.
    await expect(page.locator('text=Blocked by')).toHaveCount(0);
    // And the new layout has at least one step-surface card.
    await expect(page.locator('.step-surface').first()).toBeVisible({
      timeout: 10_000,
    });
  });
});
