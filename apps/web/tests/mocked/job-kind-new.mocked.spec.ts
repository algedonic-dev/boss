// Admin · New job kind (D6 flow). The New page is now a name-it entry:
// it collects identity + headline fields and, on submit, creates a
// `workflow-design` Job and hands off to the authoring workspace. This
// guards that wiring (fields persist; submit routes to /authoring/:id).

import { test, expect, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installAuthoringMocks, JOB_ID, KIND_SLUG } from './_mockApi';
import { FAILURE_MARKER } from './_routes';

test.beforeEach(async ({ page }) => {
  await installAuthoringMocks(page);
});

test.describe('Admin new job kind — name-it entry', () => {
  test('identity fields render and persist input', async ({ page }) => {
    await mountPage(page, '/it/registry/new');

    const slug = page.getByPlaceholder('seasonal-release', { exact: true });
    await slug.fill(KIND_SLUG);
    await expect(slug).toHaveValue(KIND_SLUG);

    // By placeholder, not index. `locator('input').nth(1)` counted
    // every input on the page, chrome included, so adding the global
    // search box to the chrome bar shifted this onto the slug field.
    const label = page.getByPlaceholder('Seasonal Release', { exact: true });
    await label.fill('Seasonal Release');
    await expect(label).toHaveValue('Seasonal Release');

    const desc = page.getByPlaceholder('What this kind of Job accomplishes');
    await desc.fill('A seasonal beer release workflow');
    await expect(desc).toHaveValue('A seasonal beer release workflow');
  });

  test('subject-kind checkboxes toggle', async ({ page }) => {
    await mountPage(page, '/it/registry/new');
    // Named subject kind rather than `.first()`: the checkboxes are
    // already wrapped in real <label> elements, so the accessible name
    // is a stable handle, and the assertion now says which box it
    // toggled. `asset` comes from the mocked /api/subject-kinds.
    const assetBox = page.getByLabel('asset', { exact: true });
    const initial = await assetBox.isChecked();
    await assetBox.click();
    await expect(assetBox).toBeChecked({ checked: !initial });
  });

  test('Create & author → creates the design Job and opens the workspace', async ({ page }) => {
    await mountPage(page, '/it/registry/new');

    await page.getByPlaceholder('seasonal-release', { exact: true }).fill(KIND_SLUG);
    await page
      .getByPlaceholder('Seasonal Release', { exact: true })
      .fill('Seasonal Release');

    const create = page.getByRole('button', { name: /create & author/i });
    await expect(create).toBeEnabled();

    await Promise.all([
      page.waitForURL(new RegExp(`/it/registry/authoring/${JOB_ID}`)),
      create.click(),
    ]);
    // Landed on the workspace for the new design Job. Its h1 paints only
    // once the design Job's read answers, so it waits under the suite's
    // stated budget, not a tighter cap of its own (backlog e614c5de).
    await expect(page.locator('h1').first()).toContainText(/Authoring/i);
  });
});

// Backlog aaeb02d6. The page's two suggestion reads — /api/workflows for
// the Category list, /api/subject-kinds for the checkboxes — each ended
// in `if (!r.ok) return` and a bare catch, so a failure kept the defaults
// and the page looked exactly like a registry with no categories and no
// kinds beyond the built-in seven. It sat on the outage crawl's SILENT
// map for that. Each read now paints its own line, on the shared marker,
// naming the read and what it leaves missing; the form stays usable.
test.describe('Admin new job kind — a failed suggestion read says so', () => {
  const refused = (r: Route) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify('down') });

  test('a refused /api/workflows names itself and the Category list it empties', async ({ page }) => {
    await page.route('**/api/workflows', refused);
    await mountPage(page, '/it/registry/new');

    await expect(page.locator(FAILURE_MARKER)).toHaveText([
      "Couldn't load the existing job kinds — /api/workflows: HTTP 500. Category has no suggestions; type one.",
    ]);
    await expect(page.locator(FAILURE_MARKER)).toHaveAttribute('role', 'alert');
  });

  test('a refused /api/subject-kinds names itself and says only the built-in kinds are offered', async ({ page }) => {
    await page.route('**/api/subject-kinds', refused);
    await mountPage(page, '/it/registry/new');

    await expect(page.locator(FAILURE_MARKER)).toHaveText([
      "Couldn't load subject kinds — /api/subject-kinds: HTTP 500. Only the built-in subject kinds are listed below.",
    ]);
    await expect(page.getByLabel('asset', { exact: true })).toBeVisible();
  });

  test('an unreachable /api/workflows is named too, not swallowed', async ({ page }) => {
    await page.route('**/api/workflows', (r) => r.abort('connectionrefused'));
    await mountPage(page, '/it/registry/new');

    await expect(page.locator(FAILURE_MARKER)).toHaveText([
      /Couldn't load the existing job kinds — \/api\/workflows: .+\. Category has no suggestions; type one\./,
    ]);
  });

  test('both reads answering paint no failure line', async ({ page }) => {
    await mountPage(page, '/it/registry/new');
    await expect(page.getByLabel('account', { exact: true })).toBeVisible();
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });
});
