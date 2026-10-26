import { expect, test } from './_test';
import { holdClock } from './_clock';
import { TERRITORIES } from '../../src/it/yard/world';
import { installSmokeMocks, YARD_REGIONS } from './_smokeMocks';

const FLOW = /\/api\/yard\/flow(\?|$)/;
const pending = { state: 'pending', as_of: null, window_hours: 24, why: 'flow counts pending' };
const instant = '2026-10-23T12:34:56Z';
const regions = { window_hours: 24, now: instant, regions: TERRITORIES.map(({name}) => ({
  name, count: 7, state: 'clear', why: 'seven from the server',
  trend: { metric: 'crossings', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 },
})) };

test('an unresolved flow read holds only itself; base map and its polls still draw server counts', async ({ page }) => {
  await installSmokeMocks(page);
  await holdClock(page, new Date(instant));
  let flowCalls = 0, regionCalls = 0;
  const independent = new Set<string>();
  const endpoints = ['/api/yard/regions', '/api/yard/borders', '/api/yard/routes', '/api/jobs/assignments'];
  page.on('response', (response) => {
    const path = new URL(response.url()).pathname;
    if (endpoints.includes(path) && response.ok()) independent.add(path);
  });
  let release = () => {};
  const blocked = new Promise<void>((resolve) => { release = resolve; });
  await page.route(FLOW, async (route) => { flowCalls++; await blocked; await route.fulfill({ status: 202, json: pending }); });
  await page.route(YARD_REGIONS, (route) => { regionCalls++; return route.fulfill({ json: regions }); });
  try {
    await page.goto('/it');
    const station = page.locator('section.transit svg[role="group"] [data-station="dock"]');
    await expect(station).toHaveAttribute('aria-label', /7/);
    await expect.poll(() => [...independent].sort()).toEqual([...endpoints].sort());
    await expect.poll(() => page.locator('path.section[data-section]').count()).toBeGreaterThan(0);
    await expect(page.locator('[data-row="needs-you"]')).toBeVisible();
    await expect(page.locator('[data-flow-state="loading"]')).toContainText('Reading flow counts');
    await page.clock.runFor(20_000);
    await expect.poll(() => regionCalls).toBeGreaterThanOrEqual(3);
    expect(flowCalls).toBe(1);
    await expect(station).toHaveAttribute('aria-label', /7/);
  } finally { release(); }
});

test('202 pending becomes a separately stamped ready reading on the next poll', async ({ page }) => {
  await installSmokeMocks(page);
  await holdClock(page, new Date(instant));
  let ready = false;
  await page.route(FLOW, (route) => route.fulfill({ status: ready ? 200 : 202,
    json: ready ? { state: 'ready', as_of: instant, window_hours: 24, why: null } : pending }));
  await page.goto('/it');
  await expect(page.locator('[data-flow-state="pending"]')).toHaveText('Flow counts pending');
  await expect(page.locator('section.transit')).toBeVisible();
  ready = true;
  await page.clock.runFor(10_000);
  await expect(page.locator('[data-flow-state="ready"] time')).toHaveAttribute('datetime', instant);
  await expect(page.locator('[data-flow-state="ready"]')).toContainText(instant);
});

for (const state of ['withheld', 'unread', 'failed'] as const) {
  test(`flow ${state} is named without taking down the map`, async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(FLOW, (route) => route.fulfill(state !== 'failed'
      ? { json: { ...pending, state, why: state === 'withheld' ? 'history is outside your policy scope' : 'native membership history unavailable' } }
      : { status: 524, body: 'gateway timeout' }));
    await page.goto('/it');
    await expect(page.locator('section.transit svg[role="group"] [data-station="dock"]')).toBeVisible();
    await expect(page.locator(`[data-flow-state="${state}"]`)).toContainText(state === 'withheld'
      ? 'Flow counts withheld — history is outside your policy scope'
      : state === 'unread' ? 'Flow counts unread — native membership history unavailable'
      : 'Flow counts unread — /api/yard/flow: HTTP 524');
  });
}


test('a later failed refresh keeps the real reading instant, server counts, and exact failure visible', async ({ page }) => {
  await installSmokeMocks(page);
  await holdClock(page, new Date(instant));
  let why: string | null = null;
  await page.route(FLOW, (route) => route.fulfill({ json: { state: 'ready', as_of: instant, window_hours: 24, why } }));
  await page.route(YARD_REGIONS, (route) => route.fulfill({ json: regions }));
  await page.goto('/it');
  const reading = page.locator('[data-flow-state="ready"]');
  const station = page.locator('section.transit svg[role="group"] [data-station="dock"]');
  await expect(reading.locator('time')).toHaveAttribute('datetime', instant);
  await expect(reading).not.toContainText('last refresh failed');
  why = 'storage failure: history statement timed out';
  await page.clock.runFor(10_000);
  await expect(reading).toContainText(`last refresh failed — ${why}`);
  await expect(reading.locator('time')).toHaveAttribute('datetime', instant);
  await expect(station).toHaveAttribute('aria-label', /7/);
});
