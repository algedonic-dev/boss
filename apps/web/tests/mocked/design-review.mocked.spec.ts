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
    path: 'docs/architecture-decisions.md',
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
    status: 'living',
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
  // Empty by default: the common case is a fully-indexed corpus, and
  // it keeps the panel out of the way of the assertions below. The
  // non-empty case has its own test.
  await page.route('**/api/design/rejections', (route) =>
    route.fulfill({ json: [] }),
  );
  await page.route('**/api/jobs?*', (route) =>
    route.fulfill({ json: { jobs: [], total: 0 } }),
  );
});

// The POST contract the page must satisfy: jobs-api deserializes the
// identity-first Subject ({subject_kind, id}) — the retired
// {custom_kind, ref_id} shape 422s with "missing field `id`", which is
// exactly how this page's button silently died in production. The mock
// enforces the shape so the regression fails in CI, not on the box.
async function installJobCreateMock(page: import('@playwright/test').Page) {
  await page.route('**/api/jobs', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const body = route.request().postDataJSON() as {
      kind: string;
      subject?: { subject_kind?: string; id?: string };
    };
    if (!body.subject?.id || !body.subject?.subject_kind) {
      return route.fulfill({
        status: 422,
        body: 'invalid job body: missing field `id`',
      });
    }
    return route.fulfill({
      json: {
        id: 'job-review-1',
        kind: body.kind,
        status: 'open',
        title: 'Review',
        subject: body.subject,
        steps: [],
      },
    });
  });
  await page.route('**/api/jobs/job-review-1', (route) =>
    route.fulfill({
      json: {
        id: 'job-review-1',
        status: 'open',
        steps: [
          { id: 'step-1', kind: 'review-design', status: 'pending', metadata: {} },
        ],
      },
    }),
  );
  await page.route('**/api/jobs/job-review-1/steps/step-1', (route) =>
    route.fulfill({
      json: { id: 'step-1', kind: 'review-design', status: 'pending', metadata: {} },
    }),
  );
}

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

  test('splits docs under discussion from living references', async ({
    page,
  }) => {
    await mountPage(page, '/system/design', { titleMatch: /design review/i });
    // The doc with open questions sits in the reviewing section; the
    // living reference sits below with a reopen affordance — the
    // pre-2026-07-08 page showed both as "in-review".
    const reviewing = page
      .locator('section', { hasText: 'In review & discussion' })
      .first();
    await expect(
      reviewing.locator('tr', { hasText: 'Inventory value conservation' }),
    ).toBeVisible({ timeout: 10_000 });
    const settled = page
      .locator('section', { hasText: 'Living references & settled' })
      .first();
    await expect(
      settled.locator('tr', { hasText: 'correctness protocol' }),
    ).toBeVisible();
    await expect(settled.locator('td', { hasText: 'living' })).toBeVisible();
    await expect(
      settled.getByRole('button', { name: /reopen discussion/i }),
    ).toBeVisible();
  });

  test('docs without an open review offer the review-Job entry point', async ({
    page,
  }) => {
    await mountPage(page, '/system/design', { titleMatch: /design review/i });
    // One doc is under discussion ("Open review Job"), one is a living
    // reference ("Reopen discussion") — every doc gets exactly one
    // affordance, worded for its state.
    await expect(
      page.getByRole('button', { name: /open review job/i }),
    ).toHaveCount(1);
    await expect(
      page.getByRole('button', { name: /reopen discussion/i }),
    ).toHaveCount(1);
  });

  test('Open review Job posts the identity-first subject shape', async ({
    page,
  }) => {
    await installJobCreateMock(page);
    await mountPage(page, '/system/design', { titleMatch: /design review/i });
    const row = page.locator('tr', {
      hasText: 'Inventory value conservation',
    });
    await row.getByRole('button', { name: /open review job/i }).click();
    // A 422 from the shape-enforcing mock surfaces as the page error
    // banner; success re-loads the list. Assert no error rendered.
    await expect(page.locator('.empty', { hasText: /HTTP 422/ })).toHaveCount(
      0,
    );
  });

  test('names docs the reindexer refused, and how long they have been invisible', async ({
    page,
  }) => {
    const sixDaysAgo = new Date(Date.now() - 6 * 86_400_000).toISOString();
    await page.route('**/api/design/rejections', (route) =>
      route.fulfill({
        json: [
          {
            path: 'docs/design/half-written.md',
            reason: 'no title heading',
            first_seen_at: sixDaysAgo,
            last_seen_at: new Date().toISOString(),
          },
        ],
      }),
    );
    await mountPage(page, '/system/design', { titleMatch: /design review/i });
    await expect(
      page.getByRole('heading', { name: /not indexed \(1\)/i }),
    ).toBeVisible();
    const row = page.locator('tr', { hasText: 'docs/design/half-written.md' });
    await expect(row).toContainText('6 days');
    await expect(row).toContainText('no title heading');
  });

  test('an in-review doc links to the full-page step surface, not the job page', async ({
    page,
  }) => {
    // Reading the doc is the entire point of this Job. The job page
    // renders the review plugin in a panel beside the sidebar, the job
    // header and the step list; the step-focus route renders it under
    // the chrome bar with the whole panel to itself. The list must
    // link to the second one.
    await page.route('**/api/jobs?*', (route) =>
      route.fulfill({
        json: {
          data: [
            {
              id: 'job-review-9',
              title: 'Review: Inventory value conservation',
              status: 'open',
              opened_on: '2026-08-01',
              subject: { id: 'docs/architecture-decisions.md' },
              steps: [
                { id: 'step-other', kind: 'sign-off' },
                { id: 'step-rd', kind: 'review-design' },
              ],
            },
          ],
          total: 1,
        },
      }),
    );
    await mountPage(page, '/system/design', { titleMatch: /design review/i });
    const link = page.getByRole('link', { name: /in review/i });
    await expect(link).toHaveAttribute('href', '/jobs/job-review-9/steps/step-rd');
  });

  test('falls back to the job page when the Job has no review step yet', async ({
    page,
  }) => {
    // A Job caught before its steps materialize has no step id. Better
    // the job page than a link to a step id we invented.
    await page.route('**/api/jobs?*', (route) =>
      route.fulfill({
        json: {
          data: [
            {
              id: 'job-review-9',
              title: 'Review: Inventory value conservation',
              status: 'open',
              opened_on: '2026-08-01',
              subject: { id: 'docs/architecture-decisions.md' },
              steps: [],
            },
          ],
          total: 1,
        },
      }),
    );
    await mountPage(page, '/system/design', { titleMatch: /design review/i });
    await expect(page.getByRole('link', { name: /in review/i })).toHaveAttribute(
      'href',
      '/service/job-review-9',
    );
  });

  test('a failing rejections call does not blank the page', async ({
    page,
  }) => {
    // Rejections are supplementary. Throwing on a non-OK response
    // replaced the entire surface with an error banner — the docs
    // list, the counts, every review button — over a panel that
    // renders nothing when empty anyway.
    await page.route('**/api/design/rejections', (route) =>
      route.fulfill({ status: 500, body: 'boom' }),
    );
    await mountPage(page, '/system/design', { titleMatch: /design review/i });
    await expect(
      page.locator('tr', { hasText: 'Inventory value conservation' }),
    ).toHaveCount(1);
    await expect(page.locator('.empty', { hasText: /^Error:/ })).toHaveCount(0);
  });
});
