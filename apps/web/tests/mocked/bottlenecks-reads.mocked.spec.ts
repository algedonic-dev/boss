// THE BOTTLENECKS PAGE'S READS SAY WHEN THEY FAIL, AND WHICH KIND THEY
// ARE — page audit ac519138 (2026-09-27), five of its gaps pinned here:
//
//   ffc3444f  a failed stage-durations read painted Done (7d) as 0 on
//             every row — "nothing drains", the bottleneck signal
//             inverted. Now the three flow columns read "unread" and
//             one failure line names the read's HTTP status and the
//             server's own sentence.
//   0fb86007  the 10 s poll swallowed every failure, so the numbers
//             aged with no mark. Now a failed poll keeps the last good
//             values under a stale line naming the read and the age of
//             its last good answer, cleared by the next good read.
//   44a8b680  a poll for kind A that answered after a switch to kind B
//             painted A's depth on B's map. Now an answer for a kind no
//             longer selected is dropped, and a tick is skipped while
//             the last one is still out.
//   f949d19a  the page opened on the alphabetically first kind
//             (agent-run, 10 of 457 in-flight steps) and its picker
//             carried no count. Now it opens on the deepest kind, and
//             the picker is sorted by in-flight steps, each with its
//             count, idle kinds last but listed.
//   c7c5c1de  a chosen kind had no address. Now every choice in the
//             picker writes ?kind= (replaceState), and a reload opens
//             on it; the kind the page opens on is not written. The link
//             INTO this page from the Department Map's sidings is
//             pinned in it-marshalling-controls.mocked.spec.ts.
//
// The views aggregates (the fleet and the stage durations) are not
// readable through the pod door (gap 8, FOR DAVID), so these mocked
// specs are the proof.

import { expect, test, type Page, type Route } from './_test';
import { mountPage, openedRequests, recordPageRequests } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';

const PATH = '/it/operate/bottlenecks';
const TITLE = { titleMatch: /Bottlenecks/ };

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const KINDS = ['agent-run', 'backlog-item', 'zeta-idle'] as const;
type Kind = (typeof KINDS)[number];

/// Every kind shares the step slug `build`, so a depth drawn on the
/// wrong kind's map lands on a real node rather than off it.
const workflow = (kind: string) => ({
  kind, version: 1, status: 'active', label: kind, description: null, category: 'platform',
  subject_kinds: ['custom'], metadata_schema: {}, metadata: {}, entitlements: {},
  owning_team: 'platform', authoring_job_id: null, created_at: '2026-01-01T00:00:00.000Z',
  steps: [
    { title: 'build', kind: 'task', ready_when: 'true', title_template: '', sign_offs_required: [], authority_role: null, metadata_defaults: {} },
    { title: 'done', kind: 'generic', ready_when: 'steps.build.done', terminal: { outcome: 'completed' }, title_template: '', sign_offs_required: [], authority_role: null, metadata_defaults: {} },
  ],
});

const fleet = (kind: string, ready: number) => ({
  workflow_kind: kind,
  open_jobs: ready,
  as_of: '2026-09-27T12:00:00Z',
  nodes: [{ slug: 'build', ready, active: 0, unassigned: 0, by_role: {}, oldest_ready_wall: null }],
});

const STAGES = { stages: [{ slug: 'build', completed: 14, p50_seconds: 600, p90_seconds: 900, max_seconds: 1200 }] };

/// The queue-age lens: backlog-item holds three in-flight steps,
/// agent-run one, zeta-idle none.
const QUEUE_AGE = {
  now: '2026-09-27T12:00:00Z',
  total: 4,
  data: [
    ...['b1', 'b2', 'b3'].map((id) => ({ job_id: id, job_kind: 'backlog-item', job_title: id, step_title: 'build', status: 'ready', waiting_days: 1 })),
    { job_id: 'a1', job_kind: 'agent-run', job_title: 'a1', step_title: 'build', status: 'ready', waiting_days: 1 },
  ],
};

const kindOf = (url: string, re: RegExp): Kind => decodeURIComponent(re.exec(url)?.[1] ?? '') as Kind;

const FLEET = /\/api\/views\/fleet\/([^/?]+)$/;
const STAGE_DURATIONS = /\/api\/views\/stage-durations\/([^/?]+)\?days=7$/;

/// The page's reads, well formed: depth 1 for agent-run, 3 for
/// backlog-item, none for zeta-idle.
async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/workflows$/, (r) => json(r, KINDS.map(workflow)));
  await page.route(/\/api\/workflows\/[^/]+$/, (r) => json(r, workflow(kindOf(r.request().url(), /\/api\/workflows\/([^/]+)$/))));
  await page.route(/\/api\/jobs\/queue-age$/, (r) => json(r, QUEUE_AGE));
  await page.route(/\/api\/jobs\?kind=[^&]+&status=open/, (r) => json(r, { data: [], total: 0, limit: 1000, offset: 0 }));
  await page.route(FLEET, (r) => {
    const k = kindOf(r.request().url(), FLEET);
    return json(r, fleet(k, k === 'backlog-item' ? 3 : k === 'agent-run' ? 1 : 0));
  });
  await page.route(STAGE_DURATIONS, (r) => json(r, STAGES));
}

const buildRow = (page: Page) => page.locator('table.fleet-table tbody tr', { hasText: 'build' });
/// Step, Ready, Active, Unclaimed, Role lenses, Oldest wait, Done (7d), p50 / max, Expected wait.
const cell = (page: Page, i: number) => buildRow(page).locator('td').nth(i);
const READY = 1;
const DONE_7D = 6;
const P50 = 7;
const EXPECTED_WAIT = 8;

test.describe('a failed flow read is unread, not zero (ffc3444f)', () => {
  test('a 503 from stage-durations marks the flow columns unread and names the status and the server’s words', async ({ page }) => {
    await mocks(page);
    await page.route(STAGE_DURATIONS, (r) =>
      r.fulfill({ status: 503, contentType: 'text/plain', body: 'stage durations need a postgres-backed views service' }),
    );
    await mountPage(page, `${PATH}?kind=backlog-item`, TITLE);

    // The live depth stays: the fleet read answered.
    await expect(cell(page, READY)).toHaveText('3');
    const line = page.locator(`${FAILURE_MARKER}`, { hasText: 'stage-durations' });
    await expect(line).toContainText('HTTP 503');
    await expect(line).toContainText('stage durations need a postgres-backed views service');
    // Done (7d) is not a count it never read.
    await expect(cell(page, DONE_7D)).toHaveText('unread');
    await expect(cell(page, P50)).toHaveText('unread');
    await expect(cell(page, EXPECTED_WAIT)).toHaveText('unread');
  });

  test('a flow read that answers paints its numbers and no failure line', async ({ page }) => {
    await mocks(page);
    await mountPage(page, `${PATH}?kind=backlog-item`, TITLE);
    await expect(cell(page, DONE_7D)).toHaveText('14');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });
});

test.describe('a failed poll looks stale (0fb86007)', () => {
  test('keeps the last good depth under a line naming the read and its age, and clears on the next good read', async ({ page }) => {
    await page.clock.install({ time: new Date('2026-09-27T12:00:00Z') });
    await mocks(page);
    let fleetDown = false;
    await page.route(FLEET, (r) =>
      fleetDown ? json(r, { error: 'backend down' }, 502) : json(r, fleet('backlog-item', 3)),
    );
    await mountPage(page, `${PATH}?kind=backlog-item`, TITLE);
    await expect(cell(page, READY)).toHaveText('3');
    const stale = page.locator('.fleet-stale');
    await expect(stale).toHaveCount(0);

    fleetDown = true;
    await page.clock.runFor(10_000);
    await expect(stale).toContainText('the fleet read failed');
    await expect(stale).toContainText('HTTP 502 — backend down');
    await expect(stale).toContainText(/its last good answer, \d+ s old/);
    // The last good answer is still drawn.
    await expect(cell(page, READY)).toHaveText('3');

    fleetDown = false;
    await page.clock.runFor(10_000);
    await expect(stale).toHaveCount(0);
  });
});

test.describe('a poll across a kind switch (44a8b680)', () => {
  test('an answer for a kind no longer selected is dropped, and no tick overlaps one still out', async ({ page }) => {
    await page.clock.install({ time: new Date('2026-09-27T12:00:00Z') });
    await recordPageRequests(page);
    await mocks(page);
    // agent-run's fleet: the mount's read answers at once; the poll's is
    // held, and when released it carries a depth no kind here has.
    let agentReads = 0;
    let backlogReads = 0;
    let release: () => void = () => {};
    const released = new Promise<void>((r) => {
      release = r;
    });
    await page.route(FLEET, async (r) => {
      const k = kindOf(r.request().url(), FLEET);
      // backlog-item: the switch's read says 3, every poll after it 4 —
      // so a later tick of its own cannot pass for the late answer
      // having been dropped.
      if (k !== 'agent-run') return json(r, fleet(k, (backlogReads += 1) === 1 ? 3 : 4));
      agentReads += 1;
      if (agentReads === 1) return json(r, fleet(k, 1));
      await released;
      return json(r, fleet(k, 99));
    });
    await mountPage(page, `${PATH}?kind=agent-run`, TITLE);
    await expect(cell(page, READY)).toHaveText('1');

    await page.clock.runFor(10_000); // the poll goes out, and is held
    await expect.poll(() => agentReads).toBe(2);
    await page.clock.runFor(10_000); // a tick while it is out: skipped
    await page.locator('.fleet-pick select').selectOption('backlog-item');
    await expect(cell(page, READY)).toHaveText('3');

    release();
    // The page has read the late answer and done what it does with it
    // (its body read, and a task run since).
    await expect
      .poll(async () => (await openedRequests(page)).filter((e) => /\/api\/views\/fleet\/agent-run$/.test(e.path) && e.read).length)
      .toBe(2);
    // Read ONCE, now: a retrying assertion would wait for backlog-item's
    // own next tick to repaint over a late answer that had landed.
    expect(await cell(page, READY).textContent(), 'agent-run’s late depth drawn on backlog-item’s map').toMatch(/^[34]$/);
    expect(await page.locator('table.fleet-table').textContent()).not.toContain('99');
    expect(agentReads, 'the second tick went out while the first was still held').toBe(2);
  });
});

test.describe('the first view and the picker (f949d19a)', () => {
  test('opens on the kind holding the most in-flight steps, the picker sorted by that count with each count shown', async ({ page }) => {
    await mocks(page);
    await mountPage(page, PATH, TITLE);
    const select = page.locator('.fleet-pick select');
    await expect(select).toHaveValue('backlog-item');
    await expect(select.locator('option')).toHaveText([
      'backlog-item — 3 in flight',
      'agent-run — 1 in flight',
      'zeta-idle — 0 in flight',
    ]);
    await expect(cell(page, READY)).toHaveText('3');
  });

  test('a lens that does not answer leaves the picker alphabetical, and says so', async ({ page }) => {
    await mocks(page);
    await page.route(/\/api\/jobs\/queue-age$/, (r) => json(r, 'down', 503));
    await mountPage(page, PATH, TITLE);
    const select = page.locator('.fleet-pick select');
    await expect(select).toHaveValue('agent-run');
    await expect(select.locator('option')).toHaveText([...KINDS]);
    await expect(page.locator(FAILURE_MARKER, { hasText: 'queue-age' })).toContainText('HTTP 503');
  });
});

test.describe('a chosen kind has an address (c7c5c1de)', () => {
  test('a switch writes ?kind= without a history entry, and a reload opens on it', async ({ page }) => {
    await mocks(page);
    await mountPage(page, PATH, TITLE);
    // The kind the page opens on is not written: a link lands where it
    // pointed (the tab strip's own pin, audit-log-page).
    await expect(page.locator('.fleet-pick select')).toHaveValue('backlog-item');
    await expect(page).toHaveURL(/\/it\/operate\/bottlenecks$/);
    const before = await page.evaluate(() => history.length);
    await page.locator('.fleet-pick select').selectOption('agent-run');
    await expect(page).toHaveURL(/\/it\/operate\/bottlenecks\?kind=agent-run$/);
    expect(await page.evaluate(() => history.length)).toBe(before);
    await expect(cell(page, READY)).toHaveText('1');

    await page.reload();
    await expect(page.locator('.fleet-pick select')).toHaveValue('agent-run');
    await expect(cell(page, READY)).toHaveText('1');
  });
});
