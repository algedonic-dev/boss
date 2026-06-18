// Landing page (`/`) and AppShell — the surfaces shared across
// every authed route. Landing is the unauth playground entry
// point with a JobKind picker; AppShell wraps every page with
// the sidebar nav.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Landing (/) — JobKind picker', () => {
  test('hero + jobs-in-flight stat render', async ({ page }) => {
    await mountPage(page, '/');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('.live-stat-num')).toBeVisible();
  });

  test('clicking a kind picker button flips its active state', async ({
    page,
  }) => {
    await mountPage(page, '/');
    const kindBtns = page.locator('button.live-count');
    if ((await kindBtns.count()) === 0) {
      test.skip(true, 'no JobKinds in registry yet');
    }
    const second = kindBtns.nth(1);
    if ((await second.count()) === 0) {
      test.skip(true, 'fewer than 2 JobKinds registered');
    }
    await second.click();
    await expect(second).toHaveClass(/\bactive\b/);
  });

  test('clicking a recent-job row picks its kind', async ({ page }) => {
    await mountPage(page, '/');
    const jobBtns = page.locator('button.live-job');
    if ((await jobBtns.count()) === 0) {
      test.skip(true, 'no in-flight jobs');
    }
    await jobBtns.first().click();
    // The clicked button gets the active class via class:active.
    await expect(jobBtns.first()).toHaveClass(/\bactive\b/);
  });
});

test.describe('AppShell — sidebar nav', () => {
  test('My Day + Inbox links are always rendered', async ({ page }) => {
    await mountPage(page, '/me');
    await expect(page.locator('a.shell-nav-item:has-text("My Day")')).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator('a.shell-nav-item:has-text("Inbox")')).toBeVisible();
  });

  test('clicking a sidebar group header link navigates', async ({ page }) => {
    await mountPage(page, '/me');
    // The "All jobs" sidebar link is in the Work group; click it
    // and assert URL changes to /jobs.
    const jobsLink = page.locator('a[href="/jobs"]').first();
    if ((await jobsLink.count()) === 0) {
      test.skip(true, 'sidebar role-gated, current persona has no work group');
    }
    await Promise.all([
      page.waitForURL(/\/jobs/, { timeout: 10_000 }),
      jobsLink.click(),
    ]);
  });
});
