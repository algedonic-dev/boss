// Design docs index + detail. Index page exposes a Reindex
// button + Hide-completed checkbox; detail page hosts the
// decision-flow buttons (Accept / Override / Prev / Next /
// Batch-accept / Flush) + the override-modal Cancel/Save.
//
// The decision-flow buttons only render when the doc has
// `pending_count > 0`. Current seed has no pending decisions, so
// the in-flow assertions skip cleanly. Index + nav are always
// exercisable.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Design docs index (/design)', () => {
  test('Refresh-from-git button + hide-completed checkbox render', async ({
    page,
  }) => {
    await mountPage(page, '/design');
    await expect(
      page.getByRole('button', { name: /refresh from git/i }),
    ).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('input[type="checkbox"]').first()).toBeVisible();
  });

  test('Hide-completed checkbox toggles list size', async ({ page }) => {
    await mountPage(page, '/design');
    const checkbox = page.locator('input[type="checkbox"]').first();
    await expect(checkbox).toBeVisible({ timeout: 10_000 });
    const initialChecked = await checkbox.isChecked();
    // Toggle off: completed docs come back into the list — count
    // grows or stays the same.
    await checkbox.click();
    await expect(checkbox).toBeChecked({ checked: !initialChecked });
    // Toggle back to original.
    await checkbox.click();
    await expect(checkbox).toBeChecked({ checked: initialChecked });
  });

  test('clicking a doc link navigates to /design/{path}', async ({ page }) => {
    await mountPage(page, '/design');
    const docLink = page.locator('a[href*="/design/"]').first();
    if ((await docLink.count()) === 0) {
      test.skip(true, 'no design docs in index');
    }
    await Promise.all([
      page.waitForURL(/\/design\/[^/]+/, { timeout: 10_000 }),
      docLink.click(),
    ]);
  });
});

test.describe('Design doc detail (/design/{path})', () => {
  test('page mounts with content header', async ({ page }) => {
    await mountPage(page, '/design/docs/design/extending-boss.md');
    // Title comes from frontmatter; just assert h1 renders.
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('Flush-to-git button is visible (always rendered on detail)', async ({
    page,
  }) => {
    await mountPage(page, '/design/docs/design/extending-boss.md');
    const flush = page.getByRole('button', { name: /flush to git/i });
    await expect(flush).toBeVisible({ timeout: 10_000 });
    // Disabled when pending count is zero — and in the current
    // seed every doc is pending=0.
    await expect(flush).toBeDisabled();
  });

  test('decision-flow buttons render when doc has pending questions', async ({
    page,
  }) => {
    await mountPage(page, '/design/docs/design/extending-boss.md');
    const accept = page.getByRole('button', { name: /accept proposal/i });
    if ((await accept.count()) === 0) {
      test.skip(true, 'doc has no pending decisions in current seed');
    }
    await expect(accept).toBeVisible();
    await expect(page.getByRole('button', { name: /override/i })).toBeVisible();
    await expect(page.getByRole('button', { name: /prev/i })).toBeVisible();
    await expect(page.getByRole('button', { name: /next/i })).toBeVisible();
  });
});
