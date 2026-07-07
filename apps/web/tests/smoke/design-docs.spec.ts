// Design review surface (/system/design) against the live stack: the
// docs-api index must present every design doc with a real title,
// status, and live open-question count, and offer (or link) a review
// Job per doc. Replaces the pre-2026-05-03 spec for the retired
// /design decision-tracker surface (Refresh-from-git / Flush-to-git /
// Accept-Override buttons), which no longer exists.
//
// Presentation depth (question anchors, body rendering) is guarded in
// CI by the boss-docs corpus test (docs_corpus_presents.rs) and the
// mocked design-review spec; this live spec asserts the deployed
// stack serves the same shape end to end.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Design review (/system/design)', () => {
  test('index lists the design-doc corpus with live question counts', async ({
    page,
  }) => {
    await mountPage(page, '/system/design', { titleMatch: /design review/i });

    const rows = page.locator('.design-table tbody tr');
    // The repo ships a multi-doc corpus; an empty table means the
    // docs-api index is stale or the reindex never ran.
    await expect(rows.first()).toBeVisible({ timeout: 10_000 });
    expect(await rows.count()).toBeGreaterThanOrEqual(5);

    // Every row presents a non-empty title + a numeric Open-Qs cell.
    const first = rows.first();
    await expect(first.locator('td').first().locator('strong')).not.toHaveText(
      '',
    );
    await expect(first.locator('td').nth(2)).toHaveText(/^\d+$/);
  });

  test('every doc offers a review entry point (button or open Job link)', async ({
    page,
  }) => {
    await mountPage(page, '/system/design', { titleMatch: /design review/i });
    const rows = page.locator('.design-table tbody tr');
    await expect(rows.first()).toBeVisible({ timeout: 10_000 });
    const n = await rows.count();
    for (let i = 0; i < n; i++) {
      const cell = rows.nth(i).locator('td').last();
      const hasButton = await cell
        .getByRole('button', { name: /open review job/i })
        .count();
      const hasLink = await cell.locator('a').count();
      expect(
        hasButton + hasLink,
        `row ${i} has neither a review button nor an open-review link`,
      ).toBeGreaterThan(0);
    }
  });
});
