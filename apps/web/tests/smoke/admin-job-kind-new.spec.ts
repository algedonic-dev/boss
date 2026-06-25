// Admin · New job kind (D6). The New page is now a name-it entry:
// identity + headline fields + "Create & author →", which creates a
// `job-kind-design` Job and opens the authoring workspace (the step
// graph is built there, not here). Live-stack spec — the full
// create → author → publish flow is the Phase-2 gate; this asserts the
// form renders + persists against the real backend.
//
// (The interactive authoring graph/inspector/rail is covered by the
// CI-gated mocked suite under tests/mocked/.)

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Admin new job kind — name-it entry', () => {
  test('identity fields render and persist user input', async ({ page }) => {
    await mountPage(page, '/system/job-kinds/new');

    const slug = page.locator('input.mono').first();
    await slug.fill('smoke-test-kind');
    await expect(slug).toHaveValue('smoke-test-kind');

    // Label — first plain <input> after the slug.
    const label = page.locator('input').nth(1);
    await label.fill('Smoke Test Kind');
    await expect(label).toHaveValue('Smoke Test Kind');

    const desc = page.locator('textarea').first();
    await desc.fill('Smoke test description');
    await expect(desc).toHaveValue('Smoke test description');
  });

  test('subject-kind checkboxes toggle', async ({ page }) => {
    await mountPage(page, '/system/job-kinds/new');
    const first = page.locator('input[type="checkbox"]').first();
    const initial = await first.isChecked();
    await first.click();
    await expect(first).toBeChecked({ checked: !initial });
    await first.click();
    await expect(first).toBeChecked({ checked: initial });
  });

  test('Create & author button is visible + enabled', async ({ page }) => {
    await mountPage(page, '/system/job-kinds/new');
    const create = page.getByRole('button', { name: /create & author/i });
    await expect(create).toBeVisible({ timeout: 10_000 });
    await expect(create).toBeEnabled();
  });
});
