// /it/registry — each kind's packet history (backlogs 112c0535 and
// 5eacf6db, page audit 9da74410).
//
// The catalog drew a kind's active `v{n}` and its in-flight count, and
// nothing else of its packets: 374 of 540 open packets ran a version
// below their kind's active one, and 16 of 64 active kinds had never had
// a packet, and neither was visible here. Both now come from one scoped
// read, GET /api/jobs/kinds (not the public /api/jobs/live, 9274e151).
//
// Pinned here: a kind whose in-flight packets lag names how many and on
// which versions; a kind whose packets are all current says so; a kind
// with history names its packets ever and links its newest terminal; a
// kind the ledger does not name is marked never run; and a failed ledger
// read says so once and marks NO kind never run, because unknown is not
// empty.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { JOBS_KINDS, JOBS_LIVE, installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PAGE = '/it/registry';
const TITLE = /^Workflows$/;
const WORKFLOWS = /\/api\/workflows$/;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const step = (title: string, ready_when: string) => ({
  title, kind: 'task', ready_when, title_template: '', authority_role: null, metadata_defaults: {},
});

const workflow = (kind: string, version: number) => ({
  kind, label: kind, category: 'it', version, status: 'active', description: null,
  subject_kinds: ['custom'],
  steps: [step('start', 'true'), step('finish', 'steps.start.done')],
  metadata_schema: {}, metadata: {}, entitlements: {}, owning_team: 'platform',
  authoring_job_id: null, created_at: '2026-09-01T00:00:00Z',
});

const CATALOG = [workflow('backlog-item', 13), workflow('design-doc', 4), workflow('join-a-node', 1)];

const LEDGER = {
  kinds: [
    {
      kind: 'backlog-item', packets: 1480, open: 392,
      open_by_version: { '11': 6, '12': 327, '13': 59 },
      newest_terminal: { id: 'job-closed-1', title: 'an item', outcome: 'delivered', closed_on: '2026-09-26' },
    },
    { kind: 'design-doc', packets: 23, open: 2, open_by_version: { '4': 2 }, newest_terminal: null },
  ],
};

async function openRegistry(page: Page, ledger: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(WORKFLOWS, (r) => json(r, CATALOG));
  await page.route(JOBS_LIVE, (r) => json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(JOBS_KINDS, ledger);
  await mountPage(page, PAGE, { titleMatch: TITLE });
}

const rowOf = (page: Page, kind: string) =>
  page.locator('.kb-workflow-row').filter({ has: page.locator('.kb-workflow-kind', { hasText: kind }) });

test('each kind says its version lag, its packets ever and its newest terminal', async ({ page }) => {
  await openRegistry(page, (r) => json(r, LEDGER));

  const backlog = rowOf(page, 'backlog-item').locator('.kb-ledger');
  await expect(backlog).toContainText('392 in flight');
  await expect(backlog.locator('.kb-ledger-behind')).toHaveText('333 on older versions');
  await expect(backlog).toContainText('(v11: 6, v12: 327)');
  await expect(backlog).toContainText('1480 packets ever');
  await expect(backlog.getByRole('link', { name: 'Sep 26, 2026' })).toHaveAttribute('href', /\/ux\/jobs\/job-closed-1$/);
  await expect(backlog).toContainText('(delivered)');

  const design = rowOf(page, 'design-doc').locator('.kb-ledger');
  await expect(design).toContainText('2 in flight, all on the active version');
  await expect(design).toContainText('none closed yet');
  await expect(design.locator('.kb-ledger-behind')).toHaveCount(0);

  await expect(rowOf(page, 'join-a-node').locator('.kb-ledger-never')).toHaveText('never run');
  await expect(rowOf(page, 'backlog-item').locator('.kb-ledger-never')).toHaveCount(0);
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
});

test('a refused ledger read says so once and marks no kind never run', async ({ page }) => {
  await openRegistry(page, (r) => json(r, 'reading packets is refused', 403));

  await expect(page.locator(FAILURE_MARKER)).toHaveCount(1);
  await expect(page.locator(FAILURE_MARKER)).toContainText('Packet history unavailable');
  await expect(page.locator(FAILURE_MARKER)).toContainText('HTTP 403');
  // The catalog still renders; the history is unknown, not empty.
  await expect(page.locator('.kb-workflow-row')).toHaveCount(3);
  await expect(page.locator('.kb-ledger-never')).toHaveCount(0);
  await expect(page.locator('.kb-ledger')).toHaveCount(0);
});

test('a dropped ledger connection is a failed read, not an empty ledger', async ({ page }) => {
  await openRegistry(page, (r) => r.abort('connectionreset'));

  await expect(page.locator(FAILURE_MARKER)).toContainText('Packet history unavailable');
  await expect(page.locator('.kb-ledger-never')).toHaveCount(0);
});
