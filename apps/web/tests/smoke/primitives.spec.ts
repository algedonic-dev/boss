// Phase-1 Playwright smoke — cover the primitive-touching routes.
//
// These are the same shapes as the React suite's tests (jobs list,
// assets list, asset detail, job detail), run against the Svelte
// bundle on port 5174 via the standalone dev server.
//
// The abort criterion for the migration (svelte-migration.md Q7)
// includes "no worse DOM stability than React" — these tests are
// how we verify that claim.

import { test, expect } from '@playwright/test';

test.describe('Phase-1 primitives', () => {
  test('nav shell renders sidebar with My Day + Inbox', async ({ page }) => {
    await page.goto('/');
    // Personal anchors are always present.
    await expect(page.locator('.shell-nav-home')).toBeVisible();
    await expect(page.locator('text=Inbox').first()).toBeVisible();
    // At least one nav group should render for the CEO demo persona.
    await expect(page.locator('.shell-nav-group').first()).toBeVisible();
  });

  test('jobs list loads with rows for open jobs', async ({ page }) => {
    await page.goto('/jobs');
    await expect(page.locator('h1')).toContainText(/jobs/i);
    // Table tbody must have at least one row — the seeded DB has
    // thousands of open sale + refurb jobs for the CEO persona.
    await expect(page.locator('table tbody tr').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('service queue pre-filters to field-service kind', async ({ page }) => {
    await page.goto('/service');
    await expect(page.locator('h1')).toContainText(/service queue/i);
    // Loads without error. With the current replay there are few
    // field-service jobs, so we only assert the table *or* the
    // empty-state renders.
    const panel = page.locator('.list-section');
    await expect(
      panel.locator('table, p.empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('assets list renders kanban + device table', async ({ page }) => {
    await page.goto('/assets');
    await expect(page.locator('h1')).toContainText(/tracked devices/i);
    // Kanban phase columns render.
    await expect(page.locator('.kanban-col').first()).toBeVisible({
      timeout: 10_000,
    });
    // Device table has at least one row.
    await expect(page.locator('table tbody tr').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('asset detail loads for a real serial', async ({ page }) => {
    // Pull one serial off the /assets list, then navigate in.
    await page.goto('/assets');
    const firstSerial = page.locator('table tbody tr').first().locator('a').first();
    await expect(firstSerial).toBeVisible({ timeout: 10_000 });
    const href = await firstSerial.getAttribute('href');
    expect(href).toMatch(/\/assets\//);
    await page.goto(href!);
    await expect(
      page.locator('section').filter({ hasText: /Current state/ }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('job detail loads with step list for a seeded job', async ({ page }) => {
    await page.goto('/jobs');
    const firstRow = page.locator('table tbody tr').first();
    await expect(firstRow).toBeVisible({ timeout: 10_000 });
    const link = firstRow.locator('a').first();
    const href = await link.getAttribute('href');
    expect(href).toMatch(/\/jobs\//);
    await page.goto(href!);
    // Job detail shows subject + steps section.
    await expect(page.locator('section').filter({ hasText: /Subject/ })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator('section').filter({ hasText: /Steps/ })).toBeVisible();
  });
});
