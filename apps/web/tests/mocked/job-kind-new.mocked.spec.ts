// Admin · New job kind (D6 flow). The New page is now a name-it entry:
// it collects identity + headline fields and, on submit, creates a
// `job-kind-design` Job and hands off to the authoring workspace. This
// guards that wiring (fields persist; submit routes to /authoring/:id).

import { test, expect } from '@playwright/test';
import { mountPage } from '../smoke/_helpers';
import { installAuthoringMocks, JOB_ID, KIND_SLUG } from './_mockApi';

test.beforeEach(async ({ page }) => {
  await installAuthoringMocks(page);
});

test.describe('Admin new job kind — name-it entry', () => {
  test('identity fields render and persist input', async ({ page }) => {
    await mountPage(page, '/admin/job-kinds/new');

    const slug = page.locator('input.mono').first();
    await slug.fill(KIND_SLUG);
    await expect(slug).toHaveValue(KIND_SLUG);

    const label = page.locator('input').nth(1);
    await label.fill('Seasonal Release');
    await expect(label).toHaveValue('Seasonal Release');

    const desc = page.locator('textarea').first();
    await desc.fill('A seasonal beer release workflow');
    await expect(desc).toHaveValue('A seasonal beer release workflow');
  });

  test('subject-kind checkboxes toggle', async ({ page }) => {
    await mountPage(page, '/admin/job-kinds/new');
    const first = page.locator('input[type="checkbox"]').first();
    const initial = await first.isChecked();
    await first.click();
    await expect(first).toBeChecked({ checked: !initial });
  });

  test('Create & author → creates the design Job and opens the workspace', async ({ page }) => {
    await mountPage(page, '/admin/job-kinds/new');

    await page.locator('input.mono').first().fill(KIND_SLUG);
    await page.locator('input').nth(1).fill('Seasonal Release');

    const create = page.getByRole('button', { name: /create & author/i });
    await expect(create).toBeEnabled();

    await Promise.all([
      page.waitForURL(new RegExp(`/admin/job-kinds/authoring/${JOB_ID}`), { timeout: 15_000 }),
      create.click(),
    ]);
    // Landed on the workspace for the new design Job.
    await expect(page.locator('h1').first()).toContainText(/Authoring/i, { timeout: 10_000 });
  });
});
