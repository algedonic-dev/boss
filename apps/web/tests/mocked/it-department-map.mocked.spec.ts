// THE DEPARTMENT MAP AFTER THE TRANSIT MAP (R3 of the retirement, backlog
// 23a4b4d7; conductor orders 20261108T1500Z and 20261108T1523Z). /it
// draws what David reads and uses, all of it real, current activity: the
// needs-you board, the alarms board, the company station's page (R3b: the
// root of the station tree, found by its shape — `company` in the fixture,
// `algedonic` on the live registry), and a station's boards when `?at=`
// selects one. No flight decides it — the
// `it-tree-floor` fork is gone — and the transit map, the HUD frame's
// other rows, the flow counts and the band under the floor are not drawn.

import { expect, test, type Page, type Route } from './_test';
import { claimsRead, stationList, stationLoad, treeRead } from '../fixtures/station-tree';
import { STATIONS_LOAD, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';
import { REGION_NAMES } from '../../src/it/yard/regions';

const STATIONS = /\/api\/stations(\?|$)/;
const CLAIMS = /\/api\/claims(\?|$)/;
const NEEDS = /\/api\/jobs\?status=open&department=it&human_required_view=true/;
const NOW = '2026-11-08T15:00:00Z';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

/** Every region clear but the given states, as the regions read says. */
const regionsPayload = (states: Readonly<Record<string, string>>) => ({
  window_hours: 24,
  now: NOW,
  regions: REGION_NAMES.map((name) => ({
    name, count: 1, unit: 'things', state: states[name] ?? 'clear',
    why: name === 'gates' ? 'a bay holds a corpse' : name === 'receiving' ? '80 packets standing, the oldest 5 days' : 'fine',
    trend: { metric: 'crossings', unit: 'per day', current: 4, previous: 4, samples: 4, previous_samples: 4 },
  })),
});

const needsPayload = {
  human_required_view: 1, total: 1, limit: 500, offset: 0,
  data: [{
    id: 'j-ops', kind: 'ops-request', title: 'Grow the gate volume', status: 'open', priority: 'standard', opened_on: '2026-11-08',
    steps: [{ id: 's-approve', kind: 'sign-off', title: 'Approve the request', status: 'ready', human_required: true }],
  }],
};

async function mocks(page: Page, states: Readonly<Record<string, string>> = {}): Promise<void> {
  await installSmokeMocks(page);
  // No flight is on: what /it draws must not depend on one.
  await page.addInitScript('window.__BOSS_FLIGHTS__ = { flights: [] }');
  await page.route(STATIONS, (r) => {
    const under = new URL(r.request().url()).searchParams.get('under');
    if (under === null) return json(r, stationList());
    return json(r, treeRead(under.startsWith('id-') ? under.slice(3) : under));
  });
  await page.route(STATIONS_LOAD, (r) => json(r, stationLoad()));
  await page.route(CLAIMS, (r) => json(r, claimsRead()));
  await page.route(YARD_REGIONS, (r) => json(r, regionsPayload(states)));
  await page.route(NEEDS, (r) => json(r, needsPayload));
}

test('/it draws the company station page with no flight set, and no transit map or HUD frame', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const root = page.locator('[data-station-page="company"]');
  await expect(root).toBeVisible();
  // Its departments stand as station cards, and its motion line is drawn.
  await expect(root.locator('[data-part-card="engineering"]')).toBeVisible();
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Department Map');
  await expect(page.locator('[data-tree-floor]')).toHaveCount(0);
  await expect(page.locator('section.transit')).toHaveCount(0);
  await expect(page.locator('[data-hud]')).toHaveCount(0);
  await expect(page.locator('[data-flow-state]')).toHaveCount(0);
  await expect(page.locator('[data-console-signals]')).toHaveCount(0);
});

test('the needs-you board lists the human-required IT work, approvals and ops requests among it', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const board = page.locator('[data-needs-you]');
  await expect(board).toBeVisible();
  const need = board.locator('[data-need="s-approve"]');
  await expect(need).toContainText('ops-request');
  await expect(need).toContainText('Grow the gate volume');
  await expect(need.locator('a')).toHaveAttribute('href', /\/ux\/jobs\/j-ops\/steps\/s-approve\?/);
});

test('the alarms board: every station not clear, troubled first, each a door to its boards', async ({ page }) => {
  await mocks(page, { gates: 'troubled', receiving: 'attention' });
  await page.goto('/it');
  const alarms = page.locator('[data-alarms] [data-alarm]');
  await expect(alarms).toHaveCount(2);
  await expect(alarms.nth(0)).toHaveAttribute('data-alarm', 'gates');
  await expect(alarms.nth(0)).toHaveAttribute('data-state', 'troubled');
  await expect(alarms.nth(0)).toHaveAttribute('href', '/it?at=gates');
  await expect(alarms.nth(0)).toContainText('a bay holds a corpse');
  await expect(alarms.nth(1)).toHaveAttribute('data-alarm', 'receiving');
  await alarms.nth(0).click();
  await expect(page).toHaveURL(/\/it\?at=gates$/);
  await expect(page.locator('[data-map-panel][data-selection="gates"] [data-contents="gates"]')).toBeVisible();
});

test('an all-clear board says so', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator('[data-alarms] [data-alarms-clear]')).toBeVisible();
  await expect(page.locator('[data-alarms] [data-alarm]')).toHaveCount(0);
});

test('a selection that names no station is said, never an empty panel', async ({ page }) => {
  await mocks(page);
  await page.goto('/it?at=gatez');
  const panel = page.locator('[data-map-panel][data-selection="gatez"]');
  await expect(panel).toContainText('No station here is named');
  await expect(panel.locator('[data-contents]')).toHaveCount(0);
});
