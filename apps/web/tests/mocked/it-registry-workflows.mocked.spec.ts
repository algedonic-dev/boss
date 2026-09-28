// /it/registry — the Workflow catalog, pinned (page audit 9da74410).
//
// Nothing rendered this page before this spec (c19efb57): only the
// static enamel-token scan named its file. So both of its reads could
// fail any way at all and no check would say so — and one did, in
// silence: a failed GET /api/jobs/live dropped every "N in flight" chip
// and said nothing, which reads as zero packets in flight on every kind
// (398913af; 540 over 14 kinds the day it was measured). The failed
// catalog read also printed a search miss under its own alert, and an
// empty registry read as a search for nothing (384160e5).
//
// Pinned here: each read's failure in the shape it fails (a refused
// status, and for the in-flight read a dropped connection), the empty
// registry apart from a failed one, the search, every kind link landing
// on the kind's detail page, the eight Registry tabs, the New workflow
// link, and the active version's provenance (6147420a) — its publish
// date and, where one exists, the packet that authored it.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { JOBS_LIVE, WORKFLOW_DETAIL, installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PAGE = '/it/registry';
const TITLE = /^Workflows$/;
const WORKFLOWS = /\/api\/workflows$/;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const step = (title: string, ready_when: string) => ({
  title, kind: 'task', ready_when, title_template: '', authority_role: null, metadata_defaults: {},
});

const workflow = (
  kind: string,
  label: string,
  category: string,
  extra: Readonly<{ version?: number; created_at?: string; authoring_job_id?: string | null; description?: string | null }> = {},
) => ({
  kind, label, category,
  version: extra.version ?? 1,
  status: 'active',
  description: extra.description ?? null,
  subject_kinds: ['custom'],
  steps: [step('start', 'true'), step('finish', 'steps.start.done')],
  metadata_schema: {}, metadata: {}, entitlements: {},
  owning_team: 'platform',
  authoring_job_id: extra.authoring_job_id ?? null,
  created_at: extra.created_at ?? '2026-01-01T00:00:00Z',
});

/// Four kinds over three categories. One carries its authoring packet;
/// one has a description the search reaches.
const CATALOG = [
  workflow('backlog-item', 'Backlog item', 'it', {
    version: 13, created_at: '2026-09-20T17:45:02Z', authoring_job_id: 'job-authored-1',
  }),
  workflow('ship-a-change', 'Ship a change', 'it', { version: 22, created_at: '2026-09-18T09:00:00Z' }),
  workflow('brew-batch', 'Brew a batch', 'production', { description: 'Mash, boil, ferment.' }),
  workflow('hire', 'Hire someone', 'people'),
] as const;

const REGISTRY_TABS = ['Workflows', 'Dispatcher', 'Rules', 'Step plugins', 'Policy', 'Subjects', 'Drift', 'Agents'];

async function openRegistry(
  page: Page,
  opts: Readonly<{ workflows?: (r: Route) => Promise<void>; live?: (r: Route) => Promise<void> }> = {},
): Promise<void> {
  await installSmokeMocks(page);
  await page.route(WORKFLOWS, opts.workflows ?? ((r) => json(r, CATALOG)));
  await page.route(
    JOBS_LIVE,
    opts.live ?? ((r) => json(r, { counts: { 'backlog-item': 7 }, open_total: 7, recent: [], sim_clock: {} })),
  );
  await mountPage(page, PAGE, { titleMatch: TITLE });
}

const subtitle = (page: Page) => page.locator('.catalog header.exec-header p');
const kindLinks = (page: Page) => page.locator('.kb-workflow-header a');
const row = (page: Page, kind: string) => page.locator('.kb-workflow-row').filter({ hasText: kind });
const noMatch = (page: Page) => page.getByText(/^No Workflows match/);
const emptyRegistry = (page: Page) => page.getByText('The registry holds 0 active workflows.');

test.describe('/it/registry — the catalog as it reads', () => {
  test('every kind links to its detail page, with the header, tabs and New workflow around it', async ({ page }) => {
    await openRegistry(page);

    await expect(page.locator('.catalog .exec-eyebrow')).toHaveText('IT · Registry');
    await expect(subtitle(page)).toHaveText(
      '4 active Workflows across 3 categories — every kind of work this instance runs',
    );
    await expect(page.locator('nav[aria-label="IT registry"] a')).toHaveText(REGISTRY_TABS);
    await expect(page.getByRole('link', { name: '+ New workflow' })).toHaveAttribute('href', '/it/registry/new');

    await expect(kindLinks(page)).toHaveCount(CATALOG.length);
    for (const k of CATALOG) {
      await expect(page.locator('.kb-workflow-header').filter({ hasText: k.kind }).locator('a')).toHaveAttribute(
        'href',
        `/it/registry/${k.kind}`,
      );
    }
    await expect(row(page, 'backlog-item').locator('.kb-workflow-live')).toHaveText('7 in flight');
    await expect(page.locator('.kb-workflow-live')).toHaveCount(1);
    await expect(page.locator('.catalog [role=alert]')).toHaveCount(0);
    await expect(noMatch(page)).toHaveCount(0);
  });

  test('a kind link lands on that kind’s detail page', async ({ page }) => {
    await openRegistry(page);
    const hire = CATALOG[3];
    await page.route(WORKFLOW_DETAIL, (r) => json(r, hire));

    await page.locator('.kb-workflow-header a', { hasText: hire.kind }).click();
    await expect(page).toHaveURL(new RegExp(`/it/registry/${hire.kind}$`));
    await expect(page.locator('h1').first()).toHaveText(hire.label);
  });

  test('the active version names its publish date, and its authoring packet where one exists', async ({ page }) => {
    await openRegistry(page);

    const authored = row(page, 'backlog-item').locator('.kb-workflow-meta');
    await expect(authored).toContainText('v13 · published Sep 20, 2026');
    await expect(authored.getByRole('link', { name: 'authoring packet' })).toHaveAttribute(
      'href',
      '/ux/jobs/job-authored-1',
    );
    await expect(row(page, 'ship-a-change').locator('.kb-workflow-meta')).toContainText('v22 · published Sep 18, 2026');
    // Three of the four carry no authoring packet, and say nothing of one.
    await expect(page.getByRole('link', { name: 'authoring packet' })).toHaveCount(1);
  });

  test('the search filters the rows, and a query nothing matches says so', async ({ page }) => {
    await openRegistry(page);
    const search = page.getByPlaceholder('Search workflows by kind, label, category…');

    await search.fill('ferment');
    await expect(kindLinks(page)).toHaveText(['brew-batch']);

    // A category is searched too: `people` holds one kind.
    await search.fill('people');
    await expect(kindLinks(page)).toHaveText(['hire']);

    await search.fill('zzz-nothing');
    await expect(kindLinks(page)).toHaveCount(0);
    await expect(noMatch(page)).toHaveText('No Workflows match "zzz-nothing".');
    await expect(emptyRegistry(page)).toHaveCount(0);
  });
});

test.describe('/it/registry — a failed in-flight read is said, and the catalog stays', () => {
  test('a refused GET /api/jobs/live names the read and its status', async ({ page }) => {
    await openRegistry(page, {
      live: (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'jobs down' }),
    });

    const failure = page.locator(`.catalog ${FAILURE_MARKER}[role=alert]`);
    await expect(failure).toHaveCount(1);
    await expect(failure).toHaveText('In-flight counts unavailable — GET /api/jobs/live answered 503.');
    // The catalog is its own read, and it answered.
    await expect(kindLinks(page)).toHaveCount(CATALOG.length);
    await expect(subtitle(page)).toHaveText(
      '4 active Workflows across 3 categories — every kind of work this instance runs',
    );
    await expect(page.locator('.kb-workflow-live')).toHaveCount(0);
  });

  test('a dropped connection is the same marked line', async ({ page }) => {
    await openRegistry(page, { live: (r) => r.abort('connectionrefused') });

    const failure = page.locator(`.catalog ${FAILURE_MARKER}[role=alert]`);
    await expect(failure).toHaveCount(1);
    await expect(failure).toContainText('In-flight counts unavailable — GET /api/jobs/live failed');
    await expect(kindLinks(page)).toHaveCount(CATALOG.length);
  });
});

test.describe('/it/registry — a failed catalog read is not an empty one, and neither is a search miss', () => {
  test('a refused registry read says the count is unknown and claims no search miss', async ({ page }) => {
    await openRegistry(page, {
      workflows: (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'registry down' }),
    });

    await expect(subtitle(page)).toHaveText('Workflow count unknown — the registry read failed');
    const failure = page.locator(`.catalog ${FAILURE_MARKER}[role=alert]`);
    await expect(failure).toHaveCount(1);
    await expect(failure).toHaveText('Failed to load: HTTP 503: registry down');
    await expect(noMatch(page)).toHaveCount(0);
    await expect(emptyRegistry(page)).toHaveCount(0);
    await expect(kindLinks(page)).toHaveCount(0);
  });

  test('an empty registry says it holds none, unmarked, and is not a search miss', async ({ page }) => {
    await openRegistry(page, { workflows: (r) => json(r, []) });

    await expect(emptyRegistry(page)).toHaveCount(1);
    await expect(noMatch(page)).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.locator('.catalog [role=alert]')).toHaveCount(0);
  });
});
