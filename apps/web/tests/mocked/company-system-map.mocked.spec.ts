import { expect, test, type Page, type Route } from './_test';
import { installSmokeMocks, DEPARTMENTS_ENDPOINT, YARD_REGIONS, YARD_ROUTES } from './_smokeMocks';
import { routesPayload } from '../fixtures/yard';
import { TERRITORIES } from '../../src/it/yard/world';

const json = (route: Route, body: unknown) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });
const roster = [
  { code: 'it', display_name: 'Information Technology' },
  { code: 'hosting', display_name: 'Cloud Operations' },
];
async function setup(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(DEPARTMENTS_ENDPOINT, route => json(route, { data: roster, total: roster.length }));
  await page.route(YARD_REGIONS, route => json(route, {
    now: '2026-10-02T15:00:00Z', window_hours: 24,
    regions: TERRITORIES.map(({ name }) => ({ name, count: name === 'gates' ? 2 : 0,
      unit: 'packets', state: name === 'gates' ? 'troubled' : 'clear',
      why: name === 'gates' ? 'A recorded validation failed' : 'No waiting work',
      trend: { metric: 'crossings', unit: 'per day', current: 0, previous: 0, samples: 0, previous_samples: 0 },
    })),
  }));
}

test('company regions use the real roster; IT leads to an overview before a detailed board', async ({ page }) => {
  await setup(page);
  await page.goto('/map');
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('System Network Map');
  await expect(page.locator('[data-department]')).toHaveCount(2);
  const stub = page.locator('[data-department="hosting"]');
  await expect(stub).toContainText('Cloud Operations');
  await expect(stub).toContainText('Not connected');
  await expect(stub.locator('a')).toHaveCount(0);
  await page.getByRole('link', { name: /Information Technology/ }).click();
  await expect(page).toHaveURL(/\/it\?view=overview$/);
  await expect(page.locator('[data-transit][data-overview="true"]')).toBeVisible();
  const mapNode = await page.locator('[data-transit] .board > svg').elementHandle();
  await expect(page.locator('[data-ledger]')).toHaveCount(0);
  await page.locator('[data-station="gates"]').click();
  await expect(page.locator('[data-map-overview]')).toContainText('A recorded validation failed');
  await expect(page.locator('[data-contents="gates"]')).toHaveCount(0);
  await page.getByRole('link', { name: 'Open detailed board' }).click();
  await expect(page).toHaveURL(/\/it\?at=gates&view=overview&detail=1$/);
  await expect(page.locator('[data-contents="gates"]')).toBeVisible();
  expect(await mapNode!.evaluate(node => node.isConnected)).toBe(true);
  await page.goBack();
  await expect(page.locator('[data-map-overview]')).toBeVisible();
});

test('registry failure and incomplete rows never become an empty or invented company map', async ({ page }) => {
  await setup(page);
  await page.route(DEPARTMENTS_ENDPOINT, route => route.fulfill({ status: 503, body: 'unavailable' }));
  await page.goto('/map');
  await expect(page.locator('[data-company-map] .load-failed')).toContainText('HTTP 503');
  await expect(page.locator('[data-department]')).toHaveCount(0);
  await page.route(DEPARTMENTS_ENDPOINT, route => json(route, { data: roster, total: 3 }));
  await page.reload();
  await expect(page.locator('[data-company-map] .load-failed')).toContainText('incomplete');
  await expect(page.locator('[data-department]')).toHaveCount(0);
});

test('phone and keyboard navigate through orientation, with reduced motion and a way back', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await setup(page);
  await page.goto('/map');
  await page.getByRole('link', { name: /Information Technology/ }).focus();
  await page.keyboard.press('Enter');
  const station = page.locator('.strip-row[data-region="gates"]');
  await expect(station).toBeVisible();
  await station.focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('[data-map-overview]')).toBeVisible();
  await expect(page.locator('[data-contents]')).toHaveCount(0);
  await expect(page.locator('animateMotion')).toHaveCount(0);
  await page.getByRole('link', { name: 'Company System Map' }).click();
  await expect(page.locator('[data-company-map]')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test('a region outage stays visible while its independently read detailed board remains reachable', async ({ page }) => {
  await setup(page);
  await page.route(YARD_REGIONS, route => route.fulfill({ status: 503, body: 'unavailable' }));
  await page.goto('/it?at=gates&view=overview');
  await expect(page.locator('.load-failed').first()).toContainText('HTTP 503');
  await expect(page.locator('[data-contents]')).toHaveCount(0);
  await expect(page.locator('[data-close]')).toHaveAttribute('href', '/it?view=overview');
  await page.getByRole('link', { name: 'Open detailed board' }).click();
  await expect(page.locator('[data-contents="gates"]')).toBeVisible();
  await expect(page.locator('.load-failed').first()).toContainText('HTTP 503');
});

test('overview preserves served routes and recorded move identity; selections do not substitute another map', async ({ page }) => {
  await setup(page);
  const payload = routesPayload();
  await page.route(YARD_ROUTES, route => json(route, payload));
  const recorded = { seq: 1, at: '2026-10-02T15:00:00Z', packet: 'recorded-packet', kind: 'ship-a-change',
    label: 'A recorded change', from: 'shop-floor', to: 'gates', declared: true,
    cause_event_id: '00000000-0000-0000-0000-000000000001', cause_seq: 1001, cause_kind: 'jobs.step.updated',
    handoff_from: null, lineage: null, aboard: [] };
  await page.route(/\/api\/yard\/moves\/stream(\?|$)/, route => route.fulfill({
    status: 200, headers: { 'content-type': 'text/event-stream' },
    body: `retry: 3600000\n\nevent: resync\ndata: ${JSON.stringify({ frame: 'resync', seq: 0, reason: 'current placement' })}\n\nid: 1\nevent: move\ndata: ${JSON.stringify(recorded)}\n\n`,
  }));
  await page.goto('/it?view=overview');
  const map = page.locator('[data-transit]');
  await expect(map.locator('path.section[data-section]')).toHaveCount(payload.routes.filter(route => route.from !== null && route.to !== null).length);
  await expect(map.locator('[data-dot="1"]')).toHaveCount(1);
  await expect(map.locator('[data-dot="1"] title')).toContainText('A recorded change');
  await expect(map.locator('[data-train]')).toHaveCount(0);
  await expect(map.locator('[data-ledger]')).toHaveCount(0);
  await map.locator('[data-section-link="shop-floor→gates"]').focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('[data-map-overview]')).toHaveAttribute('data-kind', 'section');
  await expect(map.locator('[data-dot="1"]')).toHaveCount(1);
});

for (const viewport of [{ width: 1280, height: 800 }, { width: 1440, height: 1000 }, { width: 390, height: 844 }]) {
  test(`company and IT orientation are readable at ${viewport.width}×${viewport.height}`, async ({ page }, info) => {
    await page.setViewportSize(viewport);
    await setup(page);
    await page.goto('/map');
    await expect(page.locator('[data-department]')).toHaveCount(2);
    await page.screenshot({ path: info.outputPath('company.png'), fullPage: true });
    await page.getByRole('link', { name: /Information Technology/ }).click();
    if (viewport.width > 720) {
      const allFit = await page.locator('section[data-transit]').evaluate(root => {
        const board = root.querySelector('.board')!.getBoundingClientRect();
        return [...root.querySelectorAll('[data-station]')].every(station => {
          const bounds = station.getBoundingClientRect();
          return bounds.left >= board.left && bounds.right <= board.right;
        });
      });
      expect(allFit).toBe(true);
      await expect(page.locator('[data-motion-status]')).toBeVisible();
    }
    const station = viewport.width < 721 ? page.locator('.strip-row[data-region="gates"]') : page.locator('[data-station="gates"]');
    await station.click();
    await expect(page.locator('[data-map-overview]')).toBeVisible();
    await page.screenshot({ path: info.outputPath('it-overview.png'), fullPage: true });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });
}
