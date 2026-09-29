// /it/codebase — the measurement's age, a failed newest run, and the two
// reads' unknown envelopes, rendered (page audit f82b05a9).
//
// Until this spec the route was reached only by the crawls, which see
// the `[]` catch-all and the outage and nothing else. The audit's
// decisions, each pinned here as the page draws it:
//   a91a39a4 — the 00 heading names the measurement's age against the
//              reader's clock and marks it warn past 26 h (one daily
//              cadence plus slack); a newest packet that is not the
//              measured one is named with its outcome, its run's
//              `result`, and its link. The heading said "THE CODEBASE
//              NOW" over a row eleven hours old;
//   b64b3c04 — a 200 whose envelope neither read knows is a failed read,
//              never "the 05:10 measurement has not filed" or "no
//              surface open is recorded";
//   13ded76c — (part a) the deletion-candidate list says its roster is
//              the nav catalog, with the count, and the opened routes
//              the catalog does not hold are listed on their own.
//
// The measured packet is the live f955d61b as filed 2026-09-27 05:13Z,
// cut to the fields the page reads.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PAGE = '/it/codebase';
const TITLE = /Codebase/;
const MEASURED = 'f955d61b-1dc5-4200-818d-a9b9bc561e59';
const FAILED = 'a1b2c3d4-0000-4000-8000-000000000928';
const MEASURED_AT = '2026-09-27T05:13:03Z';
const HOURS = 3_600_000;
const at = (hoursAfter: number): Date => new Date(Date.parse(MEASURED_AT) + hoursAfter * HOURS);

const run = (result: string) => [
  { spec_slug: 'scheduled', title: 'Timer fired', metadata: {} },
  { spec_slug: 'run', title: 'Measure the tree and file the row', metadata: { result } },
];

const measuredPacket = {
  id: MEASURED,
  kind: 'maintenance-codebase-metrics',
  status: 'closed',
  title: 'Codebase metrics — 2026-09-27',
  opened_at: '2026-09-27T05:12:51Z',
  steps: run('ok'),
  metadata: {
    outcome: 'completed',
    measured: {
      at: MEASURED_AT,
      ref: 'main',
      head: '950779ff0000000000000000000000000000abcd',
      head_at: '2026-09-27T04:58:00Z',
      backfill: false,
      since: '4c250f91',
      counts: { crates: 54, rust_files: 900, web_files: 430, lints: 93, migrations: 140 },
      window: { adds: 120, dels: 80, net: 40, landings: 2, delete_add_pct: 67, by_bucket: {} },
      totals: null,
      registry: null,
    },
    landings: [
      { sha: '950779ff', at: '2026-09-27T04:58:00Z', subject: 'train #762', add: 100, del: 30, test_add: 0, test_del: 0 },
      { sha: 'db8ba241', at: '2026-09-26T21:01:00Z', subject: 'train #759', add: 20, del: 50, test_add: 0, test_del: 0 },
    ],
  },
};

/// The next day's run, closed failed before it measured anything —
/// boss-step.sh records the unit's result on the run step.
const failedPacket = {
  id: FAILED,
  kind: 'maintenance-codebase-metrics',
  status: 'closed',
  title: 'Codebase metrics — 2026-09-28',
  opened_at: '2026-09-28T05:11:40Z',
  steps: run('exit-code'),
  metadata: { outcome: 'failed', closed_at: '2026-09-28T05:11:52Z' },
};

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

type Install = Readonly<{ metrics?: unknown; rollup?: unknown }>;

async function install(page: Page, opts: Install = {}): Promise<void> {
  await installSmokeMocks(page);
  await page.route(
    (u) => u.pathname === '/api/jobs' && u.searchParams.get('kind') === 'maintenance-codebase-metrics',
    (r) => json(r, opts.metrics ?? { data: [measuredPacket], total: 1 }),
  );
  await page.route(
    (u) => u.pathname === '/api/surface-opens/rollup',
    (r) => json(r, opts.rollup ?? { since: '2026-09-20T12:00:00Z', until: '2026-09-27T12:00:00Z', rows: [] }),
  );
}

const heading = (page: Page) => page.locator('.ct-section').first();

test.describe('/it/codebase — the measurement is dated against now (a91a39a4)', () => {
  test('inside the daily period the heading names the age, unmarked, and never says NOW', async ({ page }) => {
    await page.clock.setFixedTime(at(11));
    await install(page);
    await mountPage(page, PAGE, { titleMatch: TITLE });

    await expect(heading(page)).toContainText('at head 950779ff');
    await expect(heading(page)).not.toContainText('NOW');
    const age = heading(page).locator('.ct-age');
    await expect(age).toHaveText('11h ago');
    await expect(age).not.toHaveClass(/\bwarn\b/);
    await expect(page.locator('.ct-newer')).toHaveCount(0);
  });

  test('past 26 h the age is marked warn and says a run was missed', async ({ page }) => {
    await page.clock.setFixedTime(at(27));
    await install(page);
    await mountPage(page, PAGE, { titleMatch: TITLE });

    const age = heading(page).locator('.ct-age');
    await expect(age).toHaveClass(/\bwarn\b/);
    await expect(age).toHaveText('27h ago — past the daily 26h, so a 05:10Z run was missed');
  });

  test('a newer packet that failed is named with its outcome, its run result, and its link', async ({ page }) => {
    await page.clock.setFixedTime(at(27));
    await install(page, { metrics: { data: [failedPacket, measuredPacket], total: 2 } });
    await mountPage(page, PAGE, { titleMatch: TITLE });

    const note = page.locator('.ct-newer');
    await expect(note).toHaveCount(1);
    await expect(note).toContainText('closed failed');
    await expect(note).toContainText('run result exit-code');
    await expect(note.getByRole('link', { name: 'Codebase metrics — 2026-09-28' })).toHaveAttribute('href', `/jobs/${FAILED}`);
    // The read answered: a failed run is not a failed read.
    await expect(page.locator(`.ct-newer${FAILURE_MARKER}`)).toHaveCount(0);
    // And the last measured row is still drawn, dated.
    await expect(heading(page)).toContainText('at head 950779ff');
  });
});

test.describe('/it/codebase — an envelope neither read knows is a failed read (b64b3c04)', () => {
  test('the metrics read', async ({ page }) => {
    await install(page, { metrics: { items: [] } });
    await mountPage(page, PAGE, { titleMatch: TITLE });

    const fail = page.locator(FAILURE_MARKER).filter({ hasText: 'The metrics packets did not answer' });
    await expect(fail).toHaveCount(1);
    await expect(fail).toContainText('unrecognised');
    await expect(page.locator('.ct-notice')).toHaveCount(0);
  });

  test('the surface roll-up read', async ({ page }) => {
    await install(page, { rollup: { data: [] } });
    await mountPage(page, PAGE, { titleMatch: TITLE });

    const fail = page.locator(FAILURE_MARKER).filter({ hasText: 'The surface roll-up did not answer' });
    await expect(fail).toHaveCount(1);
    await expect(fail).toContainText('unrecognised');
    await expect(page.getByText('No surface open is recorded')).toHaveCount(0);
  });
});

test.describe('/it/codebase — the deletion candidates state their roster (13ded76c part a)', () => {
  test('names the nav catalog as the roster and lists the opened routes outside it', async ({ page }) => {
    await install(page, {
      rollup: {
        since: '2026-09-20T12:00:00Z',
        until: '2026-09-27T12:00:00Z',
        rows: [
          { actor_id: 'emp-david', route: '/it/codebase', opens: 3, last_at: '2026-09-27T11:00:00Z' },
          { actor_id: 'emp-david', route: '/ux/jobs/:jobId', opens: 2, last_at: '2026-09-27T10:00:00Z' },
        ],
      },
    });
    await mountPage(page, PAGE, { titleMatch: TITLE });

    await expect(page.locator('.su-scope')).toContainText('The roster is the nav catalog');
    const outside = page.locator('.su-uncatalogued');
    await expect(outside).toContainText('Opened but not in the catalog · 1');
    await expect(outside.locator('li')).toHaveText(['/ux/jobs/:jobId']);
  });
});
