// Sim runner (/sim) — Reset scratch DB / scenario picker /
// Steppable checkbox / Run / Cancel / Resume / Pause / Step
// buttons. The Resume/Pause/Step trio only render while a run
// is active; tested defensively.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Sim (/sim) — controls', () => {
  test('Reset scratch DB button is visible', async ({ page }) => {
    await mountPage(page, '/sim', { titleMatch: /scratch-stack/i });
    await expect(
      page.getByRole('button', { name: /reset scratch db/i }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('Steppable checkbox + Run button render', async ({ page }) => {
    await mountPage(page, '/sim', { titleMatch: /scratch-stack/i });
    // Run button is rendered once scenarios load (the page
    // auto-selects the first one).
    const run = page.getByRole('button', { name: /run\b/i });
    await expect(run).toBeVisible({ timeout: 10_000 });
    // Steppable checkbox renders.
    await expect(
      page.locator('input[type="checkbox"]').first(),
    ).toBeVisible();
  });

  test('clicking a scenario card flips the active style', async ({ page }) => {
    await mountPage(page, '/sim', { titleMatch: /scratch-stack/i });
    const scenarios = page.locator('li[role="button"]');
    const count = await scenarios.count();
    if (count < 2) {
      test.skip(true, 'need ≥2 scenarios to verify the picker');
    }

    // Click the second scenario; assert its inline border-color
    // flips to the active blue (#3b82f6 ↔ rgb(59, 130, 246) after
    // the browser canonicalizes it).
    const second = scenarios.nth(1);
    await second.click();
    await expect(second).toHaveAttribute(
      'style',
      /border:\s*2px solid (?:#3b82f6|rgb\(59,\s*130,\s*246\))/,
    );
  });

  test('Steppable checkbox toggles', async ({ page }) => {
    await mountPage(page, '/sim', { titleMatch: /scratch-stack/i });
    const cb = page.locator('input[type="checkbox"]').first();
    const initial = await cb.isChecked();
    await cb.click();
    await expect(cb).toBeChecked({ checked: !initial });
    await cb.click();
    await expect(cb).toBeChecked({ checked: initial });
  });
});
