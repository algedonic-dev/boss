// Phase-3 Playwright smokes — Work queues + Admin entries
// (HR, QA, Ops, Sim, IT, Architecture, Operating model, Design,
// Admin: Policy / Job kinds / Step plugins).

import { test, expect } from '@playwright/test';

test.describe('Phase-3 Work + Admin', () => {
  test('HR admin loads with tabs', async ({ page }) => {
    await page.goto('/hr');
    await expect(page.locator('h1')).toContainText(/active employees/i);
    await expect(page.locator('.tabs')).toBeVisible();
  });

  test('QA page loads with tabs', async ({ page }) => {
    await page.goto('/qa');
    await expect(page.locator('h1')).toContainText(/quality assurance/i);
    await expect(page.locator('.tabs')).toBeVisible();
  });

  test('Ops dashboard renders the four panels', async ({ page }) => {
    await page.goto('/ops');
    await expect(page.locator('h1')).toContainText(/cybernetics observability/i);
    // Four panel headers: Stack Health, Dispatch Status, Queue Depths,
    // Message Flow.
    await expect(page.locator('.panel h2')).toHaveCount(4, { timeout: 10_000 });
  });

  test('Sim page loads scenarios or empty state', async ({ page }) => {
    await page.goto('/sim');
    await expect(page.locator('h1')).toBeVisible({ timeout: 10_000 });
    // Either scenarios section or loading state.
    await expect(
      page.locator('section, p.empty').filter({ hasText: /Scenarios|Loading simulator/i }).first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('IT panel renders providers grid', async ({ page }) => {
    await page.goto('/it');
    await expect(page.locator('h1')).toContainText(/external providers/i);
    await expect(page.locator('.provider-card').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('Architecture page renders 4 diagrams', async ({ page }) => {
    await page.goto('/architecture');
    await expect(page.locator('h1')).toContainText(/system architecture/i);
    await expect(page.locator('img')).toHaveCount(4, { timeout: 10_000 });
  });

  test('Operating model renders grid', async ({ page }) => {
    await page.goto('/operating-model');
    await expect(page.locator('h1')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('.sd-grid')).toBeVisible();
  });

  test('Design index loads docs list or error', async ({ page }) => {
    await page.goto('/design');
    // Either the "Decision tracker" h1 + table/empty, or the "Failed to
    // load" p.empty when the upstream isn't reachable in dev.
    await expect(
      page.locator('h1, p.empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('Admin: Policy page loads rule matrix', async ({ page }) => {
    await page.goto('/system/policy');
    await expect(page.locator('h1')).toContainText(/policy rules/i);
    // Either the matrix or an error banner.
    await expect(
      page.locator('table, p.empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('Admin: Job kinds list loads', async ({ page }) => {
    await page.goto('/system/job-kinds');
    await expect(page.locator('h1')).toContainText(/job kinds/i);
    // At least one category section with a table, once seeded.
    await expect(
      page.locator('table tbody tr, p.empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('Admin: Job kind detail loads for a seeded kind', async ({ page }) => {
    await page.goto('/system/job-kinds');
    const firstRow = page.locator('table tbody tr').first();
    await expect(firstRow).toBeVisible({ timeout: 10_000 });
    const link = firstRow.locator('a').first();
    const href = await link.getAttribute('href');
    expect(href).toMatch(/\/admin\/job-kinds\//);
    await page.goto(href!);
    // The detail page renders an h3 "Spec" inside the first Section.
    await expect(page.locator('h3').filter({ hasText: /^Spec$/ })).toBeVisible({
      timeout: 10_000,
    });
  });

  test('Admin: Job kind new page loads the DAG editor', async ({ page }) => {
    await page.goto('/system/job-kinds/new');
    await expect(page.locator('h1')).toContainText(/new job kind/i);
    await expect(page.locator('.sde')).toBeVisible({ timeout: 10_000 });
  });

  test('Admin: Step plugins list loads', async ({ page }) => {
    await page.goto('/system/step-plugins');
    await expect(page.locator('h1')).toContainText(/step ux plugins/i);
    // Either a table row or the "no plugins" empty state.
    await expect(
      page.locator('table tbody tr, p.empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });
});
