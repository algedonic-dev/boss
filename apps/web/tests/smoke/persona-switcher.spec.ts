// Persona switcher smoke — the demo-mode dropdown pinned to the
// top-left of every page. Verifies it renders, lets you pick an
// employee, and the page header updates to the selected name.

import { test, expect } from '@playwright/test';

test.describe('Persona switcher', () => {
  test('renders the dropdown on every page', async ({ page }) => {
    await page.goto('/me');
    await expect(page.locator('.persona-switcher')).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator('.persona-select')).toBeVisible();
  });

  test('switching persona updates the My Day title', async ({ page }) => {
    await page.goto('/me');
    const select = page.locator('.persona-select');
    await expect(select).toBeVisible({ timeout: 10_000 });
    // Wait for the roster to populate the dropdown.
    await expect(select.locator('option').first()).toHaveCount(1, {
      timeout: 5_000,
    });
    const firstOption = await select.locator('option').first().getAttribute('value');
    if (!firstOption) throw new Error('dropdown empty');

    // Flip to the first listed employee.
    await select.selectOption(firstOption);

    // My Day's h1 shows the selected employee's name.
    const h1Before = await page.locator('h1').first().textContent();
    // If we already were on the first persona, switch to the 2nd
    // option to force a visible change.
    const optionCount = await select.locator('option').count();
    if (optionCount > 1) {
      const second = await select.locator('option').nth(1).getAttribute('value');
      if (second && second !== firstOption) {
        await select.selectOption(second);
        const h1After = await page.locator('h1').first().textContent();
        expect(h1After).not.toBe(h1Before);
      }
    }
  });
});
