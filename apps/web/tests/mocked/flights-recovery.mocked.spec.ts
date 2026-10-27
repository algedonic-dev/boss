// 04fcbecd: no inline answer is unread, not a decision to turn flights
// off. A first failed browser read must recover without a page reload.
import { expect, test, type Route } from './_test';
import { holdClock, responseBarrier } from './_clock';
import { installSmokeMocks } from './_smokeMocks';
import { scaled } from '../../src/dev-load';

const FLIGHTS = /\/api\/flights\/mine(\?|$)/;
const unread = '[data-flights-unread]';
const answer = (route: Route, flights: ReadonlyArray<string>) => route.fulfill({
  status: 200, contentType: 'application/json', body: JSON.stringify({ flights }),
});

for (const inline of ['absent', 'null'] as const) {
  for (const failure of ['503', 'network', 'malformed'] as const) {
    test(`${inline} inline flights recover after ${failure} without a reload`, async ({ page }) => {
      await installSmokeMocks(page);
      if (inline === 'null') await page.addInitScript('window.__BOSS_FLIGHTS__ = null');
      const second = responseBarrier();
      let requests = 0;
      await page.route(FLIGHTS, async route => {
        requests += 1;
        if (requests === 1) {
          if (failure === 'network') return route.abort('failed');
          if (failure === '503') return route.fulfill({ status: 503, body: 'rolling' });
          return route.fulfill({ status: 200, contentType: 'application/json', body: '{"flights":"bad"}' });
        }
        await second.wait();
        return answer(route, ['it-map-transit', 'it-map-live']);
      });
      try {
        await page.goto('/it');
        // This is the defect's witness: old App/loader makes only ONE
        // request. Hold the retry response so its unread mark is observable.
        await expect.poll(() => requests, { timeout: scaled(15_000) }).toBe(2);
        await expect(page.locator(unread)).toContainText('Flights unread, retrying');
        second.release();
        await expect(page.locator(unread)).toHaveCount(0);
      } finally {
        second.release();
      }
    });
  }
}

test('a valid inline empty list is known OFF and never starts recovery', async ({ page }) => {
  await holdClock(page, new Date('2026-10-23T00:00:00Z'));
  await installSmokeMocks(page);
  await page.addInitScript('window.__BOSS_FLIGHTS__ = { flights: [] }');
  let requests = 0;
  await page.route(FLIGHTS, route => { requests += 1; return answer(route, ['it-map-transit']); });
  await page.goto('/it');
  await expect(page.locator(unread)).toHaveCount(0);
  await page.clock.runFor(60_000);
  expect(requests).toBe(0);
});

test('a fetched empty list is known OFF and stops recovery', async ({ page }) => {
  await holdClock(page, new Date('2026-10-23T00:00:00Z'));
  await installSmokeMocks(page);
  let requests = 0;
  await page.route(FLIGHTS, route => { requests += 1; return answer(route, []); });
  await page.goto('/it');
  await expect.poll(() => requests).toBe(1);
  await expect(page.locator(unread)).toHaveCount(0);
  await page.clock.runFor(60_000);
  expect(requests).toBe(1);
});

test('unread reads back off from one second to a thirty-second cap, with one request at a time', async ({ page }) => {
  await holdClock(page, new Date('2026-10-23T00:00:00Z'));
  await installSmokeMocks(page);
  let requests = 0;
  await page.route(FLIGHTS, route => {
    requests += 1;
    return route.fulfill({ status: 503, body: 'rolling' });
  });
  await page.goto('/it');
  const retrying = page.locator(`${unread}[data-retrying="true"]`);
  await expect(retrying).toBeVisible();
  for (const delay of [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]) {
    const before = requests;
    await page.clock.runFor(delay - 1);
    expect(requests).toBe(before);
    await page.clock.runFor(1);
    await expect.poll(() => requests).toBe(before + 1);
    // The rendered retrying state witnesses response handling and timer
    // installation before advancing time again; host sleep proves neither.
    await expect(retrying).toBeVisible();
  }
});
