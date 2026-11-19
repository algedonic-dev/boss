// THE STATIONS' PANELS THAT STAY — step 1 of design 01fa5bdf (David,
// 2026-11-17: "IT disappears, the old map is pulled out"; when unsure,
// remove).
//
// The Department Map's panels (design e765b3fc, car N3) carried every
// retired page's content under its station. Step 1 pulled the map out
// and kept, at /it, only the alarms board and the boards that hold a
// write no other page offers yet: the track's deck, where a troubled
// train is cancelled, and the yard status board's lanes, where a held
// car is released (panel.ts `STATION_BOARDS`). The station page takes
// both in that design's step 4. This pins that the three stations still
// draw them, that every other station's panel says its boards went, and
// that every retired path renders not-found with the one door the
// router names for it.

import { expect, test, type Page, type Route } from './_test';
import { RETIRED_ROUTES } from './_routes';
import { YARD_REGIONS, YARD_STATUS, installSmokeMocks } from './_smokeMocks';
import { notFoundBack } from '../../src/router';

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const trend = (metric: string, unit: string) => ({
  metric, unit, current: 1, previous: 1, samples: 3, previous_samples: 3,
});

const REGIONS = {
  window_hours: 24,
  now: '2026-09-19T05:00:00Z',
  regions: [
    { name: 'dock', count: 2, bound: 5, state: 'clear', why: '2 cars parked', trend: trend('dock wait', 'hours') },
    { name: 'gates', count: 0, bound: 3, state: 'clear', why: 'no bay in use', trend: trend('gate duration', 'minutes') },
    { name: 'track', count: 0, bound: 1, state: 'clear', why: 'no train in transit', trend: trend('time at CI', 'minutes') },
    { name: 'shed', count: 0, state: 'clear', why: 'nothing awaits a probe', trend: trend('time to proven', 'hours') },
    { name: 'arrivals', count: 0, state: 'clear', why: 'nothing landed', trend: trend('arrivals', 'per day') },
    { name: 'garage', count: 0, state: 'clear', why: 'nothing gated red', trend: trend('reds', 'per day') },
    { name: 'receiving', count: 0, state: 'clear', why: 'nothing inbound', trend: trend('inbound', 'per day') },
    { name: 'marshalling', count: 0, state: 'clear', why: 'nothing waiting', trend: trend('served', 'per day') },
    { name: 'shop-floor', count: 0, bound: 6, state: 'clear', why: 'no run in flight, no crew on the floor', trend: trend('build duration', 'minutes') },
    { name: 'publish', count: 0, state: 'clear', why: 'no pull request awaiting a merge', trend: trend('publishes', 'per day') },
  ],
};

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
}

const PANEL = 'section[data-map-panel]';
const board = (page: Page, name: string) => page.locator(`${PANEL} [data-station-board="${name}"]`);

test('a station retries a transient render failure on the next successful regions read', async ({ page }) => {
  await mocks(page);
  let reads = 0;
  await page.route(YARD_REGIONS, (r) => {
    reads += 1;
    return json(r, REGIONS);
  });
  // Throw once from an actual DOM operation inside the station boundary.
  // The failed snippet must catch it; there is no production test hook.
  await page.addInitScript(() => {
    const original = Element.prototype.setAttribute;
    let thrown = false;
    Element.prototype.setAttribute = function (name: string, value: string): void {
      if (!thrown && name === 'data-station-board' && value === 'yard-status') {
        thrown = true;
        throw new Error('one-shot station render fault');
      }
      original.call(this, name, value);
    };
  });
  await page.goto('/it?at=dock');
  const failure = page.locator(`${PANEL} [data-region="dock"].load-failed`);
  await expect(failure).toContainText('one-shot station render fault');
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Alarms');
  await expect.poll(() => reads).toBeGreaterThan(1);
  await expect(failure).toHaveCount(0);
  await expect(board(page, 'yard-status')).toBeVisible();
  await expect(page).toHaveURL(/\/it\?at=dock$/);
});

test('a successful regions poll preserves a healthy station subtree', async ({ page }) => {
  await mocks(page);
  let reads = 0;
  await page.route(YARD_REGIONS, (r) => {
    reads += 1;
    return json(r, REGIONS);
  });
  await page.goto('/it?at=dock');
  const status = board(page, 'yard-status');
  await expect(status).toBeVisible();
  const original = await status.elementHandle();
  expect(original).not.toBeNull();
  await expect.poll(() => reads).toBeGreaterThan(1);
  await expect(status).toBeVisible();
  expect(await original!.evaluate((node) => node.isConnected)).toBe(true);
});

test('a persistent station render failure retries once per successful poll', async ({ page }) => {
  await mocks(page);
  let reads = 0;
  await page.route(YARD_REGIONS, (r) => {
    reads += 1;
    return json(r, REGIONS);
  });
  await page.addInitScript(() => {
    const original = Element.prototype.setAttribute;
    Element.prototype.setAttribute = function (name: string, value: string): void {
      if (name === 'data-station-board' && value === 'yard-status') {
        const attempts = Number(document.documentElement.dataset['stationAttempts'] ?? '0') + 1;
        document.documentElement.dataset['stationAttempts'] = String(attempts);
        throw new Error('persistent station render fault');
      }
      original.call(this, name, value);
    };
  });
  await page.goto('/it?at=dock');
  const failure = page.locator(`${PANEL} [data-region="dock"].load-failed`);
  await expect(failure).toContainText('persistent station render fault');
  await expect(page.locator('html')).toHaveAttribute('data-station-attempts', '1');
  await expect.poll(() => reads).toBeGreaterThan(1);
  await expect(page.locator('html')).toHaveAttribute('data-station-attempts', '2');
  await expect(failure).toContainText('persistent station render fault');
});

// ---------------------------------------------------------------------
// The retired pages: not found, each with the door the router names
// ---------------------------------------------------------------------

test.describe('the retired pages', () => {
  for (const path of RETIRED_ROUTES) {
    const door = notFoundBack(path);
    test(`${path} is not found, and its one door is ${door.href}`, async ({ page }) => {
      await mocks(page);
      await page.goto(path);
      await expect(page.getByRole('heading', { level: 1 })).toHaveText('No page at this address');
      await expect(page.locator('code', { hasText: path })).toHaveCount(1);
      const link = page.getByRole('link', { name: `← ${door.label}` });
      await expect(link).toHaveAttribute('href', door.href);
      if (door.href === '/it') {
        // In the IT chrome, whose landing the alarms board is.
        await expect(page.locator('.perspective-tabs [aria-current="page"]')).toContainText('IT');
        await link.click();
        await expect(page).toHaveURL(/\/it$/);
        await expect(page.getByRole('heading', { level: 1 })).toHaveText('Alarms');
      }
    });
  }

  test('Operate keeps its desk work and Design its reviews — the station tabs are gone', async ({ page }) => {
    await mocks(page);
    await page.goto('/it/operate/perf');
    await expect(page.locator('nav.it-tabs a')).toHaveText(['Audit Log', 'Performance']);
    await page.goto('/it/design');
    await expect(page.locator('nav.it-tabs a')).toHaveText(['Reviews']);
  });
});

// ---------------------------------------------------------------------
// The release and cancel controls, in the panels that keep them
// ---------------------------------------------------------------------

const TRAIN = {
  id: 'train-1', title: 'PR train 2026-09-19 04:30', phase: 'awaiting-ci', at_step: 'ci', block: null,
  ci_result: null, pr_url: null, car_count: 2, channel: null, boarded_at: '2026-09-19T04:30:00Z',
  eta: { kind: 'unknown', reason: 'too few arrivals to say' },
};

const STATUS = {
  trains: [TRAIN],
  dock: [{ id: 'car-1111', title: 'Teach the dock to breathe', branch: 'fix/dock-breathes', parked_since: '2026-09-19T04:00:00Z', red_trains: 0 }],
  held_cars: [{ id: 'car-9', branch: 'fix/held-for-reds', reason: 'two red trains', red_trains: 2, parked_since: '2026-09-18T20:00:00Z' }],
  recent: [{ id: 'train-0', title: 'PR train 2026-09-19 03:00', outcome: 'arrived', journey_seconds: 1500 }],
  stranded: [{ branch: 'fix/green-with-no-car' }],
  held: [{ branch: 'fix/held-green', reason: 'waiting on the migration', since: '2026-09-18T22:00:00Z' }],
  boarding: {
    dock_threshold: 4, cooldown_minutes: 45, at_times: [], dock_depth: 1, threshold_met: false,
    summary: 'Boards at 4 parked cars; 1 car parked now — below the dock threshold.',
  },
  gates: { capacity: 3, active: [] },
  garage: [],
  policy: { stall_hours: 2, max_red_trains: 2 },
  now: '2026-09-19T05:00:00Z',
};

test.describe('the boards that hold the release and cancel controls', () => {
  test('the track carries its deck and the trains in flight and the recent trains', async ({ page }) => {
    await mocks(page);
    await page.route(YARD_STATUS, (r) => json(r, STATUS));
    await page.goto('/it?at=track');
    await expect(board(page, 'deck').locator('.yard-deck').first()).toBeVisible();
    const status = board(page, 'yard-status');
    await expect(status.locator('.ys-section')).toHaveText(['IN FLIGHT', 'RECENT TRAINS']);
    await expect(status).toContainText('PR train 2026-09-19 04:30');
    await expect(status).toContainText('PR train 2026-09-19 03:00');
    // The dock's and the garage's lanes are theirs, not the track's.
    await expect(status).not.toContainText('HELD CARS');
    await expect(status).not.toContainText('STRANDED GREENS');
  });

  test('the dock carries its cars, its boarding sentence and the held cars', async ({ page }) => {
    await mocks(page);
    await page.route(YARD_STATUS, (r) => json(r, STATUS));
    await page.goto('/it?at=dock');
    const status = board(page, 'yard-status');
    await expect(status.locator('.ys-section')).toHaveText(['THE DOCK', 'HELD CARS']);
    await expect(status.locator('.ys-boarding')).toHaveText(STATUS.boarding.summary);
    await expect(status).toContainText('fix/dock-breathes');
    await expect(status).toContainText('fix/held-for-reds');
    await expect(status).toContainText('two red trains');
    await expect(board(page, 'deck')).toHaveCount(0);
  });

  test('the garage carries the stranded and the held greens', async ({ page }) => {
    await mocks(page);
    await page.route(YARD_STATUS, (r) => json(r, STATUS));
    await page.goto('/it?at=garage');
    const status = board(page, 'yard-status');
    await expect(status.locator('.ys-section')).toHaveText(['STRANDED GREENS', 'HELD GREENS']);
    await expect(status).toContainText('fix/green-with-no-car');
    await expect(status).toContainText('waiting on the migration');
  });

  for (const station of ['gates', 'shed', 'arrivals', 'receiving', 'marshalling', 'shop-floor', 'sensors']) {
    test(`${station}'s boards went with the map, and its panel says so`, async ({ page }) => {
      await mocks(page);
      await page.goto(`/it?at=${station}`);
      await expect(page.locator(`${PANEL} [data-station-board]`)).toHaveCount(0);
      await expect(page.locator(`${PANEL} [data-no-boards]`)).toBeVisible();
    });
  }
});
