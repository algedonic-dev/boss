// THE ALARMS BOARD — what /it draws since step 1 of design 01fa5bdf
// (David, 2026-11-17: "IT disappears, the old map is pulled out"). The
// Department Map's needs-you board (the person's queue moves to /me) and
// its embedded company station page (it stands at /it/tree) went with
// it; what stays is the alarms board, and under `?at=` the boards with
// the release and cancel controls (it-department-map-stations pins
// those). No flight decides what it draws.

import { expect, test, type Page, type Route } from './_test';
import { YARD_REGIONS, installSmokeMocks } from './_smokeMocks';
import { REGION_NAMES } from '../../src/it/yard/regions';

const NOW = '2026-11-08T15:00:00Z';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

/** Every region clear but the given states, as the regions read says. */
const regionsPayload = (states: Readonly<Record<string, string>>) => ({
  window_hours: 24,
  now: NOW,
  regions: REGION_NAMES.map((name) => ({
    name, count: 1, unit: 'things', state: states[name] ?? 'clear',
    why: name === 'dock' ? 'a car stands gated, then changed' : name === 'receiving' ? '80 packets standing, the oldest 5 days' : 'fine',
    trend: { metric: 'crossings', unit: 'per day', current: 4, previous: 4, samples: 4, previous_samples: 4 },
  })),
});

async function mocks(page: Page, states: Readonly<Record<string, string>> = {}): Promise<void> {
  await installSmokeMocks(page);
  // No flight is on: what /it draws must not depend on one.
  await page.addInitScript('window.__BOSS_FLIGHTS__ = { flights: [] }');
  await page.route(YARD_REGIONS, (r) => json(r, regionsPayload(states)));
}

test('/it draws the alarms board with no flight set, and neither the needs-you board nor the company station page', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Alarms');
  await expect(page.locator('[data-alarms]')).toBeVisible();
  await expect(page.locator('[data-needs-you]')).toHaveCount(0);
  await expect(page.locator('[data-station-page]')).toHaveCount(0);
  await expect(page.locator('[data-tree-floor]')).toHaveCount(0);
  await expect(page.locator('section.transit')).toHaveCount(0);
  await expect(page.locator('[data-hud]')).toHaveCount(0);
});

test('the alarms board: every station not clear, troubled first, each a door to its boards', async ({ page }) => {
  await mocks(page, { dock: 'troubled', receiving: 'attention' });
  await page.goto('/it');
  const alarms = page.locator('[data-alarms] [data-alarm]');
  await expect(alarms).toHaveCount(2);
  await expect(alarms.nth(0)).toHaveAttribute('data-alarm', 'dock');
  await expect(alarms.nth(0)).toHaveAttribute('data-state', 'troubled');
  await expect(alarms.nth(0)).toHaveAttribute('href', '/it?at=dock');
  await expect(alarms.nth(0)).toContainText('a car stands gated, then changed');
  await expect(alarms.nth(1)).toHaveAttribute('data-alarm', 'receiving');
  await alarms.nth(0).click();
  await expect(page).toHaveURL(/\/it\?at=dock$/);
  await expect(page.locator('[data-map-panel][data-selection="dock"] [data-contents="dock"]')).toBeVisible();
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
