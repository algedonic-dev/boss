// All Jobs view — entry-points to create a new Job.
//
// Covers the two buttons + inline form shipped in 23294d7:
//   - "Start a new Job" opens the form with kind blank.
//   - "Create Ad Hoc Job" preselects ad-hoc.
// Submitting the form POSTs /api/jobs and navigates to /jobs/{id}.

import { test, expect } from '@playwright/test';

test.describe('Jobs list — create-job entry points', () => {
  test('Start a new Job opens the form blank', async ({ page }) => {
    await page.goto('/jobs');
    const form = page.locator('.new-job-form');
    await expect(form).toBeHidden();

    await page.getByRole('button', { name: /start a new job/i }).click();
    await expect(form).toBeVisible({ timeout: 5_000 });

    // Kind select is the first <select> in the form, defaulted blank.
    const kindSelect = form.locator('select').first();
    await expect(kindSelect).toHaveValue('');
    // Cancel collapses the form.
    await form.getByRole('button', { name: /cancel/i }).click();
    await expect(form).toBeHidden();
  });

  test('Create Ad Hoc Job preselects ad-hoc + creates a Job', async ({ page }) => {
    await page.goto('/jobs');

    await page.getByRole('button', { name: /create ad hoc job/i }).click();
    const form = page.locator('.new-job-form');
    await expect(form).toBeVisible({ timeout: 5_000 });

    const kindSelect = form.locator('select').first();
    // Wait for /api/jobs/kinds to populate the dropdown options.
    await expect
      .poll(async () => kindSelect.locator('option[value="ad-hoc"]').count(), {
        timeout: 5_000,
      })
      .toBeGreaterThan(0);
    await expect(kindSelect).toHaveValue('ad-hoc');

    // Subject id is the only text input on the first form-row group;
    // ad-hoc allows account/system/employee. Use a real seeded
    // account id — the boss-jobs-api Phase-2 existence checker
    // rejects ghost ids with 400, so a synthetic id would 400 here
    // even though earlier in the suite this case "passed" pre-check.
    const subjectId = form.locator('input[type="text"]').first();
    await subjectId.fill('acc-bigseed-0001');

    // Submit and follow the navigation to /jobs/{id}.
    await Promise.all([
      page.waitForURL(/\/jobs\/[0-9a-f-]{36}$/, { timeout: 10_000 }),
      form.getByRole('button', { name: /create job/i }).click(),
    ]);
  });
});
