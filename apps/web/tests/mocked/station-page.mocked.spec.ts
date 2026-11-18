// THE STATION PAGE (design c4854fd4 E2, decided by David 2026-11-07
// 23:38Z; backlog 72d58e00; re-cut by design 08d1530e car A, backlog
// 950ad972), against MOCKED live reads: the tree, the load, the open
// claims and the human-required view. The pure rules are pinned in
// src/it/tree/station-page.test.ts and station-answer.test.ts; this pins
// the page — the answer band (Working · Stuck · Needs you · binding), the
// right rail (waits ladder, pools as strips with the caveat said once,
// Stuck, Needs you, moves), no In and no Out and no queue read, the
// breadcrumb as the parent chain, a part opened as a view swap, and a
// failed claims read drawing the red rail and no in-use figure.

import { expect, test, type Page, type Route } from './_test';
import { claimsRead, heldClaim, stationList, stationLoad, treeRead } from '../fixtures/station-tree';
import { STATIONS_LOAD, installSmokeMocks } from './_smokeMocks';

// The viewer's zone, pinned so the local half of every time reads the
// same on every machine (user-feedback 0b1fd59f, design b03ef2d4).
test.use({ timezoneId: 'America/Los_Angeles' });

const STATIONS = /\/api\/stations(\?|$)/;
const QUEUE = /\/api\/stations\/[^/]+\/queue$/;
const CLAIMS = /\/api\/claims(\?|$)/;
const NEEDS = /\/api\/jobs\?.*human_required_view=true/;

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const HELD = [
  heldClaim('j-gate-1', 'gates', { holder: 'automation:gate-runner', executor_kind: 'automaton' }),
  heldClaim('j-build-1', 'engineering'),
  heldClaim('j-ci-1', 'forge-ci', { holder: 'automation:forge-actions-runner', executor_kind: 'automaton' }),
  heldClaim('j-old', null, { holder: 'automation:train-conductor', executor_kind: 'automaton' }),
];

/** The canonical human-required view, as jobs.rs serves it: one job with
 *  a ready sign-off a person must do. */
const needsRead = (url: string) => {
  const offset = Number(new URL(url).searchParams.get('offset') ?? '0');
  const data = offset > 0 ? [] : [{
    id: 'j-sign', kind: 'ship-a-change', title: 'Sign off the dock change', status: 'open', priority: 'standard', opened_on: '2026-11-08',
    steps: [{ id: 'st-sign', kind: 'sign-off', title: 'Sign it off', status: 'ready', human_required: true }],
  }];
  return { human_required_view: 1, data, total: 1, limit: 500, offset };
};

type Opts = Readonly<{ claims?: 'ok' | 'down'; tree?: 'ok' | 'down'; held?: ReadonlyArray<Readonly<Record<string, unknown>>>; binding?: string | null }>;

async function mocks(page: Page, o: Opts = {}): Promise<Readonly<{ queues: string[]; needs: string[] }>> {
  const queues: string[] = [];
  const needs: string[] = [];
  await installSmokeMocks(page);
  await page.route(STATIONS, (r) => {
    const under = new URL(r.request().url()).searchParams.get('under');
    if (under === null) return json(r, stationList());
    if (o.tree === 'down') return r.fulfill({ status: 503, body: 'the station registry is not configured' });
    return json(r, treeRead(under.startsWith('id-') ? under.slice(3) : under));
  });
  await page.route(STATIONS_LOAD, (r) => json(r, stationLoad()));
  await page.route(QUEUE, (r) => {
    queues.push(r.request().url());
    return json(r, { data: [], total: 0 });
  });
  await page.route(NEEDS, (r) => {
    needs.push(r.request().url());
    return json(r, needsRead(r.request().url()));
  });
  await page.route(CLAIMS, (r) => (o.claims === 'down'
    ? r.fulfill({ status: 503, body: 'claims history unavailable: read deadline exhausted' })
    : json(r, {
      ...claimsRead(o.held ?? HELD),
      window_hours: 24,
      binding: o.binding === undefined ? 'host:w-1' : o.binding,
      pools: [{ pool_key: 'host:w-1', pool: { name: 'host:w-1', unit: 'vCPU', capacity: { state: 'configured', amount: '32' } }, reserved: '7.5', failed_reservations: 1, releases: 3, state: 'attention', band: null }],
    })));
  return { queues, needs };
}

test('opening a station: the answer band, then the parts and Working, with waits and pools on the right rail', async ({ page }, testInfo) => {
  const { queues, needs } = await mocks(page);
  await page.goto('/it/tree/engineering');
  const root = page.locator('[data-station-page="engineering"]');
  await expect(root.locator('h1')).toHaveText('engineering');
  // The title said once: "engineering — a station" under engineering.
  await expect(root.locator('[data-subtitle]')).toHaveText('a station');

  // The breadcrumb is the parent chain.
  await expect(page.locator('[data-crumb]')).toHaveText(['company', 'engineering']);

  // THE ANSWER BAND: linked counts and the binding, and no CLEAR plate.
  const working = page.locator('[data-answer="working"]');
  await expect(working.locator('[data-answer-count]')).toHaveText('3');
  await expect(working).toContainText('working: 1 at engineering itself, 1 at forge, 1 at gates');
  await expect(working).toHaveAttribute('href', '#working');
  const stuck = page.locator('[data-answer="stuck"]');
  await expect(stuck.locator('[data-answer-count]')).toHaveText('1');
  await expect(stuck).toContainText('1 claim at no station');
  await expect(page.locator('[data-answer="needs you"] [data-answer-count]')).toHaveText('1');
  await expect(page.locator('[data-binding="pool"] [data-binding-pool]')).toHaveText('host:w-1 · 7.5 of 32 vCPU reserved');
  await expect(page.locator('[data-binding="pool"]')).toContainText('1 reservation refused in 24 h');
  // The always-CLEAR plate went from the header; the part cards keep theirs.
  await expect(page.locator('header.head [data-plate]')).toHaveCount(0);
  await expect(page.locator('header.head')).not.toContainText('standing');

  // The human-required view is asked over the protocols engineering's stations queue.
  await expect.poll(() => needs.length).toBeGreaterThan(0);
  expect(decodeURIComponent(needs[0] ?? '')).toContain('kinds=["gate-run","ship-a-change"]');

  // THE RAIL: the waits ladder worst first; a claim at no station means no seat is judged idle.
  const rail = page.locator('[data-station-rail="engineering"]');
  await expect(rail.locator('[data-rung]')).toHaveCount(2);
  await expect(rail.locator('[data-rung]').first()).toHaveAttribute('data-rung', 'loading-dock');
  await expect(rail.locator('[data-rung="gates"]')).toContainText('Seats busy not recorded: 1 claim stands at no station.');
  await expect(rail.locator('[data-waits-caveat]')).toContainText('budget');

  // The pools, one strip each, with the caveat said ONCE and no per-pool note.
  const w1 = rail.locator('[data-pool-band="host:w-1"]');
  await expect(w1.locator('[data-segment]')).toHaveCount(3);
  await expect(w1.locator('[data-segment-labels]')).toHaveText("gates 8 · floating 16 · not engineering's 8");
  await expect(w1.locator('[data-segment="outside"]')).toHaveClass(/hatched/);
  await expect(rail.locator('[data-pools-caveat]')).toHaveCount(1);
  await expect(rail.locator('[data-pools-caveat]')).toContainText('In use is not recorded');
  await expect(rail).not.toContainText('room not stated');
  await expect(rail).not.toContainText('kept by');

  // Stuck and Needs you on the rail.
  await expect(rail.locator('[data-stuck="placeless"]')).toContainText('1 claim at no station');
  await expect(rail.locator('[data-need="st-sign"]')).toContainText('Sign off the dock change');

  // Parts on the left; Working by part with holder marks; no In, no Out, no queue read.
  await expect(page.locator('[data-part-card]')).toHaveCount(4);
  await expect(page.locator('[data-part-card="gates"] [data-station="gates"]')).toBeVisible();
  await expect(page.locator('[data-working-group]')).toHaveCount(3);
  await expect(page.locator('[data-working-group="gates"] [data-mark]')).toHaveText('■');
  await expect(page.locator('[data-working-group="engineering"] [data-mark]')).toHaveText('◆');
  // Every absolute time reads local and UTC side by side, the UTC half
  // its own secondary span (user-feedback 0b1fd59f, design b03ef2d4).
  // The pool row's time went with the per-pool notes (design 08d1530e).
  await expect(page.locator('[data-working-group="engineering"] [data-clock]')).toHaveText('23:00 PT · 07:00Z');
  await expect(page.locator('[data-working-group="engineering"] [data-clock-utc]')).toHaveText('· 07:00Z');
  await expect(page.locator('[data-unplaced]')).toContainText('1 open claim the record cannot place');
  await expect(page.locator('[data-column="in"]')).toHaveCount(0);
  await expect(page.locator('[data-column="out"]')).toHaveCount(0);
  await expect(page.locator('[data-out-unread]')).toHaveCount(0);
  expect(queues).toEqual([]);

  // The picture a reviewer opens.
  await page.screenshot({ path: testInfo.outputPath('station-page-answer-band.png'), fullPage: true });
});

test('every claim placed: an idle seat with a queue is stuck, first on the ladder and on the band', async ({ page }) => {
  await mocks(page, { held: HELD.slice(0, 3), binding: null });
  await page.goto('/it/tree/engineering');
  const rail = page.locator('[data-station-rail="engineering"]');
  await expect(rail.locator('[data-rung="loading-dock"]')).toHaveAttribute('data-rung-state', 'stuck');
  await expect(rail.locator('[data-rung="loading-dock"]')).toContainText('Its seat is idle while 30 wait.');
  await expect(page.locator('[data-answer="stuck"]')).toContainText('loading-dock: 30 not admitted, its seat idle');
  await expect(rail.locator('[data-stuck="idle:loading-dock"] a')).toHaveAttribute('href', '/it/tree/loading-dock');
  // Nothing refused: the binding says none, never a pool.
  await expect(page.locator('[data-binding="none"]')).toContainText('no reservation refused in 24 h');
});

test('/engineering is the same station page as /it/tree/engineering', async ({ page }) => {
  await mocks(page);
  await page.goto('/engineering');
  await expect(page.locator('[data-station-page="engineering"] h1')).toHaveText('engineering');
  await expect(page.locator('[data-answer="working"]')).toBeVisible();
  await expect(page.locator('[data-station-rail="engineering"]')).toBeVisible();
});

test('opening a part is a view swap to its own page', async ({ page }) => {
  await mocks(page);
  await page.goto('/it/tree/engineering');
  await page.locator('[data-part-card="forge"] [data-station="forge"] > header a.name').click();
  await expect(page).toHaveURL(/\/it\/tree\/forge$/);
  await expect(page.locator('[data-crumb]')).toHaveText(['company', 'engineering', 'forge']);
  await expect(page.locator('[data-part-card="forge-ci"]')).toBeVisible();
  // The four region views that filtered the columns retired with
  // REGION_AT (R3, backlog 23a4b4d7): no chip is drawn.
  await expect(page.locator('[data-view-chip]')).toHaveCount(0);
  // A station whose stations queue no protocol cannot scope Needs you: said, never zero.
  await expect(page.locator('[data-answer="needs you"]')).toHaveAttribute('data-answer-state', 'unread');
  await expect(page.locator('[data-needs-unread]')).toContainText('no station here queues a protocol');
});

test('two claims of one job at distinct steps render as two Working rows without a page error', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await mocks(page, { held: [
    heldClaim('same-job', 'gates', { step_id: 'step-a', holder: 'agent-claude' }),
    heldClaim('same-job', 'gates', { step_id: 'step-b', holder: 'agent-codex' }),
  ] });
  await page.goto('/it/tree/engineering');
  const gates = page.locator('[data-working-group="gates"]');
  await expect(gates.locator('[data-working-row]')).toHaveCount(2);
  await expect(gates).toContainText('agent-claude');
  await expect(gates).toContainText('agent-codex');
  expect(errors).toEqual([]);
});

test('a failed claims read draws the red rail, no in-use figure, and every claims figure unread with why', async ({ page }) => {
  await mocks(page, { claims: 'down' });
  await page.goto('/it/tree/engineering');
  await expect(page.locator('[data-station-page] header.head')).toHaveAttribute('data-rail', 'failed');
  await expect(page.locator('[data-plate-why]')).toContainText('503');
  await expect(page.locator('[data-answer="working"]')).toHaveAttribute('data-answer-state', 'unread');
  await expect(page.locator('[data-answer="working"]')).toContainText('the claims read failed');
  await expect(page.locator('[data-answer="stuck"]')).toHaveAttribute('data-answer-state', 'unread');
  await expect(page.locator('[data-binding="unread"]')).toContainText('the claims read failed');
  await expect(page.locator('[data-pools-caveat]')).toHaveText('In use is unread: the claims read failed.');
  await expect(page.locator('[data-working-failed]')).toContainText('503');
  await expect(page.locator('[data-working-row]')).toHaveCount(0);
});

test('a failed tree read draws the red rail and no figure at all', async ({ page }) => {
  await mocks(page, { tree: 'down' });
  await page.goto('/it/tree/engineering');
  await expect(page.locator('[data-page-failed]')).toContainText('503');
  await expect(page.locator('[data-pool-band]')).toHaveCount(0);
  await expect(page.locator('[data-answer]')).toHaveCount(0);
});
