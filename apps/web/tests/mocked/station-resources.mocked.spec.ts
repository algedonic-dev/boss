// A STATION'S RESOURCES VIEW (backlog 87575d69, car C of David's station
// visuals, 2026-11-19), against MOCKED live reads: the tree, the pools,
// the open claims, the load and each counted pool's claim line. The pure
// rules are pinned in src/it/tree/resources/resources.test.ts; this pins
// the page — the Work / Resources toggle both ways, units drawn where the
// claims hold them and idle ones in the rail as lendable, the gate seats
// inside their station, each station's pressure in words, the robots'
// budget said plainly unread, and a stream that ends said rather than
// silent.

import { expect, test, type Page, type Route } from './_test';
import { TREE_ROWS, claimsRead, heldClaim, stationList, stationLoad, treeRead } from '../fixtures/station-tree';
import { STATIONS_LOAD, installSmokeMocks } from './_smokeMocks';

const STATIONS = /\/api\/stations(\?|$)/;
const CLAIMS = /\/api\/claims(\?|$)/;
const WAITING = /\/api\/claims\/waiting\?/;
const POOLS = /\/api\/pools$/;
const STREAM = /\/api\/events\/stream/;

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

/** The fixture tree with engineering holding three opus slots and a
 *  `build` room holding all three. */
const ROWS = [
  ...TREE_ROWS.map((r) => (r.name === 'engineering' ? { ...r, allocation: { ...r.allocation, 'robots:opus': '3' } } : r)),
  { name: 'build', parent: 'engineering', allocation: { 'robots:opus': '3' } },
];

const POOL_ROWS = [
  { name: 'robots:opus', version: 2, status: 'active', unit: 'slot', kind: 'flow', shape: 'capacity', capacity: { state: 'configured', amount: '6' } },
  { name: 'host:w-1', version: 3, status: 'active', unit: 'vCPU', kind: 'flow', shape: 'capacity', capacity: { state: 'configured', amount: '32' } },
  { name: 'budget:robots', version: 2, status: 'active', unit: 'USD/h', kind: 'flow', shape: 'rate', capacity: { state: 'unknown' },
    capacity_from: 'no reader sums the robots hourly budgets yet' },
];

const RUN = heldClaim('j-run', 'build', {
  allocations: [{ pool: 'robots:opus', version: 2, amount: '1' }, { pool: 'host:w-1', version: 3, amount: '2.5' }],
  resource_acquisition: { workflow_kind: 'agent-run', bill: { host: { node: 'w-1', station: 'engineering' } } },
});

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(STATIONS, (r) => {
    const under = new URL(r.request().url()).searchParams.get('under');
    return under === null ? json(r, stationList()) : json(r, treeRead('company', ROWS));
  });
  await page.route(STATIONS_LOAD, (r) => json(r, stationLoad()));
  await page.route(POOLS, (r) => json(r, POOL_ROWS));
  await page.route(CLAIMS, (r) => json(r, claimsRead([RUN, heldClaim('j-gate', 'gates', { holder: 'automation:gate-runner', executor_kind: 'automaton' })])));
  await page.route(WAITING, (r) => json(r, { pool: new URL(r.request().url()).searchParams.get('pool'), line: [] }));
  // 204 tells an EventSource never to reconnect: the stream has ended.
  await page.route(STREAM, (r) => r.fulfill({ status: 204, body: '' }));
}

test('the toggle opens a station\'s resources: units where the claims hold them, seats in their station, pressure in words', async ({ page }) => {
  await mocks(page);
  await page.goto('/it/tree/engineering');
  const toggle = page.locator('[data-station-page="engineering"] [data-view-toggle] a[data-view="resources"]');
  await expect(toggle).toHaveAttribute('href', '/it/tree/engineering/resources');
  await toggle.click();
  await expect(page).toHaveURL(/\/it\/tree\/engineering\/resources$/);

  const root = page.locator('[data-resources="engineering"]');
  await expect(root.locator('h1')).toHaveText('engineering');

  // Three opus units, engineering's allocation: one held at build, two idle and lendable in the rail.
  await expect(root.locator('[data-unit^="robots:opus#"]')).toHaveCount(3);
  await expect(root.locator('[data-unit="robots:opus#0"]')).toHaveAttribute('data-unit-box', 'build');
  await expect(root.locator('[data-unit-box="pool"]')).toHaveCount(2);
  await expect(root.locator('[data-unit][data-lendable="true"]')).toHaveCount(3);
  await expect(root.locator('[data-pool="robots:opus"]')).toContainText('3 slots · 1 held · 2 lendable free');

  // The robots' budget says plainly that its rate is unread.
  await expect(root.locator('[data-pool="budget:robots"] [data-rate]')).toHaveText('rate not read: no reader sums the robots hourly budgets yet');

  // Pressure: build works; gates' seats are its own and one is claimed; loading-dock has 30 open while units sit idle.
  await expect(root.locator('[data-box="build"]')).toHaveAttribute('data-pressure', 'working');
  await expect(root.locator('[data-box="gates"] [data-pressure-words]')).toHaveText('working: 1 of 2 seats claimed');
  await expect(root.locator('[data-seat^="gates#"]')).toHaveCount(2);
  await expect(root.locator('[data-box="loading-dock"] [data-pressure-words]')).toHaveText('starved: 30 open here while 2 units sit idle');
  await expect(root.locator('[data-box="forge"]')).toHaveAttribute('data-pressure', 'quiet');

  // The claim line and the record.
  await expect(root.locator('[data-line-empty="robots:opus"]')).toHaveText('robots:opus: nothing waits.');
  await expect(root.locator('[data-moves-none]')).toBeVisible();

  // A stream that ended is said, and the view reads on a clock.
  await expect(root.locator('[data-feed="down"]')).toContainText('the event stream is down');

  // And back: Work is the station page.
  await root.locator('[data-view-toggle] a[data-view="work"]').click();
  await expect(page).toHaveURL(/\/it\/tree\/engineering$/);
  await expect(page.locator('[data-station-page="engineering"]')).toBeVisible();
});

test('a failed claims read draws no unit and says why', async ({ page }) => {
  await mocks(page);
  await page.route(CLAIMS, (r) => r.fulfill({ status: 503, body: 'claims history unavailable' }));
  await page.goto('/it/tree/engineering/resources');
  await expect(page.locator('[data-resources-failed="claims"]')).toContainText('where each unit is held is unknown');
  await expect(page.locator('[data-unit]')).toHaveCount(0);
});
