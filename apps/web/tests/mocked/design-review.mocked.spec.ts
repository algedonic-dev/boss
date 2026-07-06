// Design review list (/system/design) — content-level guard for the
// docs-review surface: the table must show each indexed doc with its
// LIVE open-question count (a doc with 3 unresolved `### Qn:` anchors
// must not read "0" — the pre-2026-07-06 page showed pending_count,
// i.e. unflushed decisions, under an "Open Qs" header) and offer the
// review-Job entry point. Route-smoke only asserts the page mounts.

import { test, expect } from '@playwright/test';
import { mountPage } from '../smoke/_helpers';

const DOCS = [
  {
    path: 'docs/design/inventory-value-conservation.md',
    title: 'Inventory value conservation (costing PR 6)',
    status: 'in-review',
    open_questions: 3,
    pending_count: 0,
    word_count: 941,
    last_modified: new Date().toISOString(),
    last_author: 'david',
    last_indexed_at: new Date().toISOString(),
    last_commit_sha: 'abc1234',
    content_html: '<h1>Inventory value conservation</h1>',
  },
  {
    path: 'docs/design/correctness-protocol.md',
    title: 'The BOSS correctness protocol',
    status: 'in-review',
    open_questions: 0,
    pending_count: 0,
    word_count: 1398,
    last_modified: new Date().toISOString(),
    last_author: 'david',
    last_indexed_at: new Date().toISOString(),
    last_commit_sha: 'def5678',
    content_html: '<h1>The BOSS correctness protocol</h1>',
  },
];

test.beforeEach(async ({ page }) => {
  await page.route('**/api/design/docs', (route) =>
    route.fulfill({ json: DOCS }),
  );
  await page.route('**/api/jobs?*', (route) =>
    route.fulfill({ json: { jobs: [], total: 0 } }),
  );
});

test.describe('Design review list', () => {
  test('shows live open-question counts, not pending decisions', async ({
    page,
  }) => {
    await mountPage(page, '/system/design', { titleMatch: /design review/i });

    const row = page.locator('tr', {
      hasText: 'Inventory value conservation',
    });
    await expect(row).toBeVisible({ timeout: 10_000 });
    // Column order: doc, status, open Qs, pending decisions, …
    await expect(row.locator('td').nth(2)).toHaveText('3');
    await expect(row.locator('td').nth(3)).toHaveText('0');

    const settled = page.locator('tr', { hasText: 'correctness protocol' });
    await expect(settled.locator('td').nth(2)).toHaveText('0');
  });

  test('docs without an open review offer the review-Job entry point', async ({
    page,
  }) => {
    await mountPage(page, '/system/design', { titleMatch: /design review/i });
    const buttons = page.getByRole('button', { name: /open review job/i });
    await expect(buttons.first()).toBeVisible({ timeout: 10_000 });
    await expect(buttons).toHaveCount(DOCS.length);
  });
});
