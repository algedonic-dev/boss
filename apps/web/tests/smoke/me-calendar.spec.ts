// My Day + Calendar surfaces — primary landing pages with small
// control surfaces. MePage cards click through to job detail
// (one handler shared by all cards). Launch calendar carries
// 30d/90d/180d window-preset buttons. My Week carries prev/next/
// today week navigation.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('My Day — landing surface', () => {
  test('page mounts with header + topbar', async ({ page }) => {
    await mountPage(page, '/me');
    // Topbar is the AppShell-level container that hosts the
    // PersonaSwitcher. Persona-switcher rendering itself is
    // session-dependent and covered in persona-switcher.spec.ts.
    await expect(page.locator('.shell-topbar')).toBeVisible({
      timeout: 10_000,
    });
  });

  test('clicking a job card navigates to /jobs/{id} when seeded', async ({
    page,
  }) => {
    await mountPage(page, '/me');
    const firstCard = page.locator('.myday-job-card').first();
    if ((await firstCard.count()) === 0) {
      test.skip(true, 'no jobs assigned to default persona in seed');
    }
    await Promise.all([
      page.waitForURL(/\/jobs\/[a-f0-9-]+/, { timeout: 10_000 }),
      firstCard.click(),
    ]);
  });
});

test.describe('Launch calendar (/calendar)', () => {
  test('window-preset buttons toggle active state', async ({ page }) => {
    await mountPage(page, '/calendar', { titleMatch: /launch calendar/i });

    for (const days of ['30', '90', '180']) {
      const btn = page.getByRole('button', {
        name: new RegExp(`next ${days} days`, 'i'),
      });
      await btn.click();
      // Active style is inline: `font-weight: 600` (note the space
      // after the colon when the bundler emits it). Match either
      // form so we don't depend on whitespace stability.
      await expect(btn).toHaveAttribute('style', /font-weight:\s*600/);
    }
  });
});

test.describe('My Week (/calendar/me)', () => {
  test('prev / today / next week controls all render', async ({ page }) => {
    await mountPage(page, '/calendar/me');
    // Page header text is dynamic ("My Week" or "{empId} — week of …");
    // skip titleMatch.
    await expect(page.getByRole('button', { name: /prev week/i })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByRole('button', { name: /this week/i })).toBeVisible();
    await expect(page.getByRole('button', { name: /next week/i })).toBeVisible();
  });

  test('Next week → Prev week → This week round-trip preserves state', async ({
    page,
  }) => {
    await mountPage(page, '/calendar/me');
    const nextBtn = page.getByRole('button', { name: /next week/i });
    const prevBtn = page.getByRole('button', { name: /prev week/i });
    const todayBtn = page.getByRole('button', { name: /this week/i });

    // Capture the title before nav (carries the week anchor).
    const titleBefore = await page.locator('h1').first().textContent();
    await nextBtn.click();
    await expect(page.locator('h1').first()).not.toHaveText(titleBefore!, {
      timeout: 5_000,
    });
    await prevBtn.click();
    await expect(page.locator('h1').first()).toHaveText(titleBefore!, {
      timeout: 5_000,
    });

    // This-week resets explicitly.
    await nextBtn.click();
    await todayBtn.click();
    await expect(page.locator('h1').first()).toHaveText(titleBefore!, {
      timeout: 5_000,
    });
  });
});
