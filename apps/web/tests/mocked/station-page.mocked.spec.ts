// THE STATION PAGE (design c4854fd4 E2; backlog 72d58e00; the answer band
// of design 08d1530e car A, backlog 950ad972) WITH ITS LIVE FLOOR (backlog
// dd0eb058, David's decisions on the prototype canvas 2026-11-19), against
// MOCKED live reads: the tree, the load, the open claims, the
// human-required view, each part's queue, the flow and the live events
// stream. The pure rules are pinned in src/it/tree/station-floor.test.ts,
// station-page.test.ts and station-answer.test.ts; this pins the page —
// the answer band, the floor's admission machine, its parts at their
// places with the head as tokens, the age-banded pile and the working
// slots, the routes dashed where nothing left, the Outbound record naming
// a PEER, motion on a stream frame, a part opened one level down, and
// each failed read said where it would have drawn. The card grid, the
// rail and the station line it replaced are gone.

import { expect, test, type Page, type Route } from './_test';
import { TREE_ROWS, claimsRead, heldClaim, nameOf, queueRead, stationList, stationLoad, treeRead } from '../fixtures/station-tree';
import { EVENTS_STREAM, STATIONS_FLOW, STATIONS_LOAD, installSmokeMocks } from './_smokeMocks';

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

const day = (d: number): string => new Date(Date.now() - d * 86_400_000).toISOString();

/** Each part's queue: three at the gates, thirty at the dock across
 *  every age band, and the forge's subtree empty. A function the queue
 *  handler calls, so its ages are read when it answers, never when the
 *  file loads (backlog de205627). */
const queueReads = (): Readonly<Record<string, ReturnType<typeof queueRead>>> => ({
  gates: queueRead('gates', [
    { id: 'j-gate-1', title: 'Seated at the gates', openedAt: day(0.1) },
    { id: 'j-g-2', title: 'Next for the gates', openedAt: day(0.2) },
    { id: 'j-g-3', title: 'Waiting long', openedAt: day(12) },
  ]),
  'loading-dock': queueRead('loading-dock', Array.from({ length: 30 }, (_, i) => ({
    id: `j-dock-${i + 1}`, title: `Car ${i + 1}`, openedAt: day([0.2, 3, 12, 40][i % 4]!),
  }))),
  forge: queueRead('forge', []),
});

const flowRead = () => ({
  basis: 'step-window-v1', window_hours: 24, as_of: new Date().toISOString(), total: 6,
  data: [['engineering', 5, 4], ['gates', 3, 2], ['loading-dock', 2, 0], ['design', 0, 0], ['forge', 1, 1], ['probe-shed', 0, 0]].map(([station, arrived, departed]) => ({
    station, kind: 'group', basis: 'step-window-v1', arrived, departed, served: departed, net: Number(arrived) - Number(departed), unavailable_reason: null,
  })),
});

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

type Opts = Readonly<{
  claims?: 'ok' | 'down';
  tree?: 'ok' | 'down';
  held?: ReadonlyArray<Readonly<Record<string, unknown>>>;
  binding?: string | null;
  queueDown?: string;
  /** Frames the events stream sends once the test releases it. */
  frames?: ReadonlyArray<Readonly<Record<string, unknown>>>;
  /** The tree's rows, when a test serves other than the fixture's. */
  rows?: Parameters<typeof treeRead>[1];
}>;

async function mocks(page: Page, o: Opts = {}): Promise<Readonly<{ queues: string[]; release: () => void }>> {
  const queues: string[] = [];
  let release: () => void = () => {};
  const released = new Promise<void>((resolve) => { release = resolve; });
  await installSmokeMocks(page);
  await page.route(STATIONS, (r) => {
    const under = new URL(r.request().url()).searchParams.get('under');
    if (under === null) return json(r, stationList());
    if (o.tree === 'down') return r.fulfill({ status: 503, body: 'the station registry is not configured' });
    return json(r, treeRead(nameOf(under), o.rows));
  });
  await page.route(STATIONS_LOAD, (r) => json(r, stationLoad()));
  await page.route(STATIONS_FLOW, (r) => json(r, flowRead()));
  await page.route(QUEUE, (r) => {
    const station = decodeURIComponent(new URL(r.request().url()).pathname.split('/')[3] ?? '');
    queues.push(station);
    if (station === o.queueDown) return r.fulfill({ status: 500, body: 'queue read deadline exhausted' });
    return json(r, queueReads()[station] ?? queueRead(station, []));
  });
  await page.route(NEEDS, (r) => json(r, needsRead(r.request().url())));
  await page.route(CLAIMS, (r) => (o.claims === 'down'
    ? r.fulfill({ status: 503, body: 'claims history unavailable: read deadline exhausted' })
    : json(r, {
      ...claimsRead(o.held ?? HELD),
      window_hours: 24,
      binding: o.binding === undefined ? 'host:w-1' : o.binding,
      pools: [{ pool_key: 'host:w-1', pool: { name: 'host:w-1', unit: 'vCPU', capacity: { state: 'configured', amount: '32' } }, reserved: '7.5', failed_reservations: 1, releases: 3, state: 'attention', band: null }],
    })));
  await page.route(EVENTS_STREAM, async (r) => {
    // A frame before the floor holds the packet is dropped, as a live one
    // would be: the stream answers when the test has seen the floor drawn.
    if (o.frames !== undefined) await released;
    const body = (o.frames ?? []).map((f) => `data: ${JSON.stringify(f)}\n\n`).join('');
    return r.fulfill({ status: 200, headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' }, body: `retry: 3600000\n\n${body}` });
  });
  return { queues, release };
}

test('opening a station: the answer band, then the live floor in place of the cards, the rail and the line', async ({ page }, testInfo) => {
  const { queues } = await mocks(page);
  await page.goto('/tree/engineering');
  const root = page.locator('[data-station-page="engineering"]');
  await expect(root.locator('h1')).toHaveText('engineering');
  await expect(root.locator('[data-subtitle]')).toHaveText('a station');
  await expect(page.locator('[data-crumb]')).toHaveText(['company', 'engineering']);

  // THE ANSWER BAND stays, and car C's Work / Resources toggle (backlog
  // 87575d69) stays in the crumbs row, Work the view drawn.
  const working = page.locator('[data-answer="working"]');
  await expect(working.locator('[data-answer-count]')).toHaveText('3');
  await expect(working).toContainText('working: 1 at engineering itself, 1 at forge, 1 at gates');
  await expect(working).toHaveAttribute('href', '#floor');
  await expect(page.locator('[data-answer="stuck"] [data-answer-count]')).toHaveText('1');
  await expect(page.locator('[data-answer="needs you"] [data-answer-count]')).toHaveText('1');
  await expect(page.locator('[data-binding="pool"] [data-binding-pool]')).toHaveText('host:w-1 · 7.5 of 32 vCPU reserved');
  await expect(root.locator('[data-crumbs] [data-view-toggle] .on')).toHaveText('Work');
  await expect(root.locator('[data-crumbs] [data-view-toggle] a[data-view="resources"]')).toHaveAttribute('href', '/tree/engineering/resources');

  // THE FLOOR: one admission edge, each part at its place.
  const floor = page.locator('[data-floor="engineering"]');
  await expect(floor.locator('[data-admission="engineering"]')).toContainText('one edge in');
  await expect(floor.locator('[data-floor-station]')).toHaveCount(5);
  await expect(floor.locator('[data-floor-station="gates"]')).toHaveAttribute('data-placed', 'grid');
  // The gates: the seated packet in a working slot, the next two as the head.
  const gates = floor.locator('[data-floor-station="gates"]');
  await expect(gates.locator('[data-flow]')).toHaveText('in 3/d · out 2/d');
  await expect(floor.locator('[data-token="j-gate-1"]')).toHaveAttribute('data-token-place', 'working');
  await expect(floor.locator('[data-token="j-g-2"]')).toHaveAttribute('data-token-place', 'waiting');
  await expect(floor.locator('[data-token="j-g-3"]')).toHaveAttribute('data-token-past', 'true');
  // The dock: four as the head, the other 26 as the pile in all four bands.
  const dock = floor.locator('[data-floor-station="loading-dock"]');
  await expect(floor.locator('[data-token^="j-dock-"]')).toHaveCount(4);
  await expect(dock.locator('[data-pile]')).toHaveAttribute('data-pile', '26');
  await expect(dock.locator('[data-age-band]')).toHaveCount(4);
  // The routes: gates -> dock crossed; dock -> probe shed dashed, nothing left the dock.
  await expect(floor.locator('[data-route="gates>loading-dock"]')).toHaveAttribute('data-route-state', 'crossed');
  await expect(floor.locator('[data-route="loading-dock>probe-shed"]')).toHaveAttribute('data-route-state', 'none');
  // Outbound is a record, empty until something leaves this session.
  await expect(floor.locator('[data-outbound="engineering"] [data-outbound-empty]')).toContainText('Nothing has left since this page opened.');
  // The claim at engineering itself is said, never placed in a part.
  await expect(floor.locator('[data-floor-note]').first()).toContainText('1 open claim held at engineering itself');
  await expect(floor.locator('[data-floor-legend]')).toContainText('still past its bound');
  // The empty leaves take no queue read; the parts that hold work do.
  expect([...new Set(queues)].sort()).toEqual(['forge', 'gates', 'loading-dock']);

  // What the floor replaced is gone.
  await expect(page.locator('[data-part-card]')).toHaveCount(0);
  await expect(page.locator('[data-station-rail]')).toHaveCount(0);
  await expect(page.locator('[data-pool-band]')).toHaveCount(0);
  await expect(page.locator('[data-band="line"]')).toHaveCount(0);
  await expect(page.locator('[data-working-group]')).toHaveCount(0);

  // Stuck and Needs you, which the band counts, listed under the floor.
  await expect(page.locator('#stuck [data-stuck="placeless"]')).toContainText('1 claim at no station');
  await expect(page.locator('#needs-you [data-need="st-sign"]')).toContainText('Sign off the dock change');

  await page.screenshot({ path: testInfo.outputPath('station-floor-engineering.png'), fullPage: true });
});

test('a move on the stream: the token and its station move, and a packet claimed outside is recorded as sent to the peer', async ({ page }, testInfo) => {
  const at = new Date().toISOString();
  const { release } = await mocks(page, {
    frames: [
      { event_id: 'e-1', timestamp: at, source: 'boss-jobs-api', kind: 'jobs.step.updated', payload: { job_id: 'j-dock-2', _station: 'gates' } },
      { event_id: 'e-2', timestamp: at, source: 'boss-jobs-api', kind: 'jobs.step.updated', payload: { job_id: 'j-dock-1', _station: 'converge' } },
      { event_id: 'e-3', timestamp: at, source: 'boss-people-api', kind: 'people.employee.updated', payload: { id: 'emp-1' } },
    ],
  });
  await page.goto('/tree/engineering');
  const floor = page.locator('[data-floor="engineering"]');
  await expect(floor.locator('[data-token="j-dock-1"]')).toBeVisible();
  release();
  await expect(floor.locator('[data-feed]')).toHaveAttribute('data-feed', 'live');
  await expect(floor.locator('[data-outbound-row="e-2"]')).toContainText('Car 1: sent to operations');
  // The peer at engineering's own tier, never converge inside it.
  await expect(floor.locator('[data-outbound-row="e-2"]')).not.toContainText('converge');
  await expect(floor.locator('[data-outbound-row]')).toHaveCount(1);
  await expect(floor.locator('[data-floor-station="gates"]')).toHaveClass(/moving/);
  await floor.screenshot({ path: testInfo.outputPath('station-floor-moving.png') });
});

test('a part whose row serves a placement stands where it says; the rest keep the fallback grid', async ({ page }) => {
  // Car A (backlog e3a927c3) serves the box as `placement` on the row.
  await mocks(page, { rows: TREE_ROWS.map((r) => (r.name === 'gates' ? { ...r, placement: { x: 590, y: 120, w: 230, h: 170 } } : r)) });
  await page.goto('/tree/engineering');
  const floor = page.locator('[data-floor="engineering"]');
  await expect(floor.locator('[data-floor-station="gates"]')).toHaveAttribute('data-placed', 'declared');
  await expect(floor.locator('[data-floor-station="loading-dock"]')).toHaveAttribute('data-placed', 'grid');
});

test('opening a part opens the same view one level down', async ({ page }) => {
  await mocks(page);
  await page.goto('/tree/engineering');
  await page.locator('[data-floor-station="forge"] a.sname').click();
  await expect(page).toHaveURL(/\/tree\/forge$/);
  await expect(page.locator('[data-crumb]')).toHaveText(['company', 'engineering', 'forge']);
  await expect(page.locator('[data-floor="forge"] [data-floor-station="forge-ci"]')).toBeVisible();
  // A station whose stations queue no protocol cannot scope Needs you: said, never zero.
  await expect(page.locator('[data-answer="needs you"]')).toHaveAttribute('data-answer-state', 'unread');
  await expect(page.locator('[data-needs-unread]')).toContainText('no station here queues a protocol');
});

test('a leaf’s floor is its own admission, its working slots and its Outbound record', async ({ page }) => {
  await mocks(page);
  await page.goto('/tree/gates');
  const floor = page.locator('[data-floor="gates"]');
  await expect(floor.locator('[data-floor-station]')).toHaveCount(1);
  await expect(floor.locator('[data-floor-station="gates"] a.sname')).toHaveCount(0);
  await expect(floor.locator('[data-token="j-gate-1"]')).toHaveAttribute('data-token-place', 'working');
  await expect(floor.locator('[data-outbound="gates"]')).toBeVisible();
});

test('/engineering is the same station page as /tree/engineering', async ({ page }) => {
  await mocks(page);
  await page.goto('/engineering');
  await expect(page.locator('[data-station-page="engineering"] h1')).toHaveText('engineering');
  await expect(page.locator('[data-answer="working"]')).toBeVisible();
  await expect(page.locator('[data-floor="engineering"]')).toBeVisible();
});

test('every claim placed: an idle seat with a queue is stuck, on the band and in the list', async ({ page }) => {
  await mocks(page, { held: HELD.slice(0, 3), binding: null });
  await page.goto('/tree/engineering');
  await expect(page.locator('[data-answer="stuck"]')).toContainText('loading-dock: 30 not admitted, its seat idle');
  await expect(page.locator('#stuck [data-stuck="idle:loading-dock"] a')).toHaveAttribute('href', '/tree/loading-dock');
  await expect(page.locator('[data-binding="none"]')).toContainText('no reservation refused in 24 h');
});

test('a failed claims read draws the red rail, fills no slot, and says why', async ({ page }) => {
  await mocks(page, { claims: 'down' });
  await page.goto('/tree/engineering');
  await expect(page.locator('[data-station-page] header.head')).toHaveAttribute('data-rail', 'failed');
  await expect(page.locator('[data-plate-why]')).toContainText('503');
  await expect(page.locator('[data-answer="working"]')).toHaveAttribute('data-answer-state', 'unread');
  await expect(page.locator('[data-binding="unread"]')).toContainText('the claims read failed');
  await expect(page.locator('[data-floor-note]').first()).toContainText('The claims read failed, so no working slot is filled');
  await expect(page.locator('[data-token-place="working"]')).toHaveCount(0);
  // The queues still draw their heads.
  await expect(page.locator('[data-token="j-g-2"]')).toBeVisible();
});

test('a failed queue read is said in its station, and the others still draw', async ({ page }) => {
  await mocks(page, { queueDown: 'loading-dock' });
  await page.goto('/tree/engineering');
  const dock = page.locator('[data-floor-station="loading-dock"]');
  await expect(dock.locator('[data-queue-unread]')).toContainText('the queue read failed');
  await expect(dock.locator('[data-queue-unread]')).toContainText('HTTP 500');
  await expect(page.locator('[data-token="j-g-2"]')).toBeVisible();
});

// The page is the root's since backlog 92bfcb04, and a hosted page is
// placed by the tree (backlog 0bc2eb68): with the tree unread, nothing can
// place the address, and the page says why rather than guessing one. At
// /it/tree/<name> it was placed by hand and drew its own red rail.
test('a failed tree read is said, naming the read, and no figure is drawn', async ({ page }) => {
  await mocks(page, { tree: 'down' });
  await page.goto('/tree/engineering');
  await expect(page.locator('[data-not-found-why]')).toContainText('the station tree could not be read');
  await expect(page.locator('[data-not-found-why]')).toContainText('503');
  await expect(page.locator('[data-floor]')).toHaveCount(0);
  await expect(page.locator('[data-answer]')).toHaveCount(0);
});
