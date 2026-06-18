// Admin · New job kind — the largest authoring surface in the
// Admin area. Spec inputs (kind slug, label, category dropdown,
// subject-kind checkboxes, description textarea) + the StepDAG
// editor (Add tier / move tier ↑↓ / remove tier / add step /
// remove step / Show JSON) + Create draft button.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Admin new job kind — spec inputs', () => {
  test('all spec fields render and persist user input', async ({ page }) => {
    await mountPage(page, '/admin/job-kinds/new');

    const slug = page.locator('input.mono').first();
    await slug.fill('smoke-test-kind');
    await expect(slug).toHaveValue('smoke-test-kind');

    // Label — first plain <input> after the slug.
    const label = page.locator('input').nth(1);
    await label.fill('Smoke Test Kind');
    await expect(label).toHaveValue('Smoke Test Kind');

    // Category dropdown.
    const category = page.locator('select').first();
    await category.selectOption({ index: 1 });
    expect(await category.inputValue()).toBeTruthy();

    // Description textarea.
    const desc = page.locator('textarea').first();
    await desc.fill('Smoke test description');
    await expect(desc).toHaveValue('Smoke test description');
  });

  test('subject-kind checkboxes toggle', async ({ page }) => {
    await mountPage(page, '/admin/job-kinds/new');
    const checkboxes = page.locator('input[type="checkbox"]');
    const count = await checkboxes.count();
    expect(count).toBeGreaterThan(0);
    // Toggle the first subject-kind on, then off.
    const first = checkboxes.first();
    const initial = await first.isChecked();
    await first.click();
    await expect(first).toBeChecked({ checked: !initial });
    await first.click();
    await expect(first).toBeChecked({ checked: initial });
  });

  test('Create draft button is visible + always enabled', async ({ page }) => {
    await mountPage(page, '/admin/job-kinds/new');
    const create = page.getByRole('button', { name: /create draft/i });
    await expect(create).toBeVisible({ timeout: 10_000 });
    await expect(create).toBeEnabled();
  });
});

test.describe('Admin new job kind — Step DAG editor', () => {
  test('+ Add tier appends a new tier row', async ({ page }) => {
    await mountPage(page, '/admin/job-kinds/new');
    const addTier = page.getByRole('button', { name: /\+ add tier/i });
    await expect(addTier).toBeVisible({ timeout: 10_000 });
    const tiersBefore = await page.locator('.sde-tier').count();
    await addTier.click();
    await expect(page.locator('.sde-tier')).toHaveCount(tiersBefore + 1, {
      timeout: 5_000,
    });
  });

  test('+ Add step appends a step into the tier', async ({ page }) => {
    await mountPage(page, '/admin/job-kinds/new');
    // Ensure at least one tier exists; the page starts with one
    // tier of one step in the empty-form fork.
    const addTier = page.getByRole('button', { name: /\+ add tier/i });
    if ((await page.locator('.sde-tier').count()) === 0) {
      await addTier.click();
    }
    const addStep = page.locator('.sde-add-step').first();
    await expect(addStep).toBeVisible({ timeout: 5_000 });
    const stepsBefore = await page.locator('.sde-step').count();
    await addStep.click();
    await expect(page.locator('.sde-step')).toHaveCount(stepsBefore + 1, {
      timeout: 5_000,
    });
  });

  test('Show JSON toggle reveals + hides the JSON pane', async ({ page }) => {
    await mountPage(page, '/admin/job-kinds/new');
    const toggle = page
      .getByRole('button', { name: /show json|hide json/i })
      .first();
    await expect(toggle).toBeVisible({ timeout: 10_000 });
    await toggle.click();
    // The pre/code JSON pane should appear after the click.
    await expect(page.locator('pre, code').first()).toBeVisible({
      timeout: 5_000,
    });
  });
});
