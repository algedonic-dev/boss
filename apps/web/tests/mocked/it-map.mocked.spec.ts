// THE IT WORLD MAP — design d2154293, car 1 (over 0524fc95's regions
// read, GET /api/yard/regions). /it is ONE SVG world: the territories
// laid out along the packet flow (world.ts), each drawn
// in the yard's strokes with the region's count, state and trend
// inside its outline, the why on a troubled one, and a door to its
// floor. This pins what the packet names: the world paints one
// territory per declared region from a fixture payload, a troubled one
// carries its why where it is, a click opens the floor — and the
// floor's panel opens with the region's own state and why at its head
// (a106309c), from the same read, so clicking in cannot contradict the
// map.
//
// THE ARITY IS READ FROM THE LAYOUT, NEVER TYPED HERE (backlog
// 94c6ffd0). These counts were the literals 8 and 8 until the shop
// floor became the ninth region, and then five specs failed for
// arithmetic rather than for anything they were built to protect. The
// layout declares how many territories and rails there are, world.ts
// is pinned to the server's own list by world.test.ts, and this reads
// it — so a tenth region is a fixture row here and nothing else.

import { expect, test, type Page, type Route } from './_test';
import { TERRITORIES } from '../../src/it/yard/world';
import { REGION_NAMES } from '../../src/it/yard/regions';
import { BORDERS } from '../fixtures/yard';
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

const trend = (metric: string, unit: string, current: number | null, previous: number | null) => ({
  metric, unit, current, previous, samples: current === null ? 0 : 5, previous_samples: previous === null ? 0 : 4,
});

/** The wire shape the server serialises, in map order, with the gates
 *  troubled — a bay holding a corpse — and marshalling unread. */
const REGIONS = {
  window_hours: 24,
  now: '2026-09-19T05:00:00Z',
  regions: [
    { name: 'dock', count: 3, bound: 5, state: 'clear', why: '3 cars parked', trend: trend('dock wait', 'hours', 4.25, 3) },
    { name: 'gates', count: 1, bound: 3, state: 'troubled', why: '1 bay holds a corpse — a gate-run past its own deadline', trend: trend('gate duration', 'minutes', 11, 9.5) },
    { name: 'track', count: 0, bound: 1, state: 'clear', why: 'no train in transit', trend: trend('time at CI', 'minutes', null, 14) },
    { name: 'shed', count: 2, state: 'attention', why: '2 landed cars await their probe', trend: trend('time to proven', 'hours', 1, 1.5) },
    { name: 'arrivals', count: 17, state: 'clear', why: '17 trains arrived in the window', trend: trend('arrivals', 'per day', 17, 12) },
    { name: 'garage', count: 0, state: 'clear', why: 'nothing gated red', trend: trend('reds', 'per day', 0, 2) },
    // Design 62de32ae car A: a non-clear state names the declared band
    // that decided it and how long the record says it has held, and
    // the count and the KPI carry their units.
    {
      name: 'receiving', count: 4, unit: 'packets standing', state: 'attention', why: '4 inbound, oldest 5 days',
      band: { id: 'receiving-aging', reads: 'oldest 5d > the 3-day triage band', hold_minutes: 0, since: '2026-09-18T00:00:00+00:00', held_minutes: 2160, held: '36h' },
      kpi: [{ name: 'oldest untriaged', value: 5, unit: 'days', text: 'oldest untriaged 5 days' }],
      trend: trend('inbound', 'per day', 4, 6),
    },
    { name: 'marshalling', count: null, state: 'troubled', why: 'the station registry could not be read', trend: trend('served', 'per day', null, null) },
    { name: 'shop-floor', count: 2, bound: 6, state: 'clear', why: '2 runs in flight, 1 crew on the floor', trend: trend('build duration', 'minutes', 64, 58) },
    { name: 'publish', count: 0, state: 'clear', why: 'no pull request awaiting a merge', trend: trend('publishes', 'per day', 1, 1) },
    { name: 'sensors', count: 2, unit: 'declared sensors', state: 'clear', why: '2 declared sensors; readings stamped', trend: trend('sensor readings', 'per day', 4, 3) },
  ],
  // THE HUD'S BLOCK (design 00774ca8): the server's rows — one reading a
  // floor, one whose edges were unread, one balanced at a true zero with
  // a troubled stuck count — and its machine cell.
  thirds: [
    {
      third: 'queue-management', regions: ['sensors', 'receiving', 'marshalling'],
      balance: { unit: 'inbound packets', in_means: 'an inbound packet opened', out_means: 'an inbound packet taken off the queue',
        in: 40, out: 28, net: 12, in_count: 40, out_count: 28 },
      stuck: { third: 'queue-management', stuck: 3, waiting: 0, oldest_hours: 170, regions: ['receiving', 'marshalling'],
        unknown: ['station q.platform-admin.task: 305 standing — the flow cube is blind to its predicate'] },
    },
    {
      third: 'actors-building', regions: ['shop-floor', 'gates', 'garage'],
      balance: { unit: 'runs', in_means: 'a run opened', out_means: 'a run closed',
        in: null, out: null, net: null, in_count: null, out_count: null, why: 'the agent-run packets could not be read' },
      stuck: { third: 'actors-building', stuck: 0, waiting: 0, unknown: [], oldest_hours: null, regions: [] },
    },
    {
      third: 'delivery', regions: ['dock', 'track', 'arrivals', 'shed', 'publish'],
      balance: { unit: 'cars', in_means: 'a green gate parked as a car', out_means: 'a car closed',
        in: 9, out: 9, net: 0, in_count: 9, out_count: 9 },
      stuck: { third: 'delivery', stuck: 1, waiting: 2, unknown: [], oldest_hours: 30, regions: ['shed'] },
    },
  ],
  machines: {
    running: 12, idle: 11, failed: 0, unknown: 2, total: 25,
    failed_or_unknown: [
      { region: 'marshalling', id: 'station:design-review', name: 'design-review', state: 'unknown', why: 'the flow cube is blind to its predicate' },
      { region: 'receiving', id: 'runner:forge', name: 'forge runner', state: 'unknown', why: 'no ops-request answered in the window' },
    ],
  },
};

/** The rails (design d2154293, car 2): one busy with a machine silent
 *  past its own cadence, one the server could not read at all, the rest
 *  quiet. The shapes are the server's — `boss_jobs::borders`. */
const rail = (
  from: string,
  to: string,
  over: Record<string, unknown> = {},
): Record<string, unknown> => ({
  from, to,
  crossing: 'a packet crossed',
  rate: { metric: 'crossings', unit: 'per day', current: 2, previous: 3, samples: 2, previous_samples: 3 },
  last_crossed: '2026-09-19T04:00:00Z',
  waiting: 0,
  holds: [],
  machine: { name: 'the receiving desk', kind: 'actors', last_fired: null, silent_for_minutes: null,
    expected_every_minutes: null, silent: null, why: 'no machine moves this hop' },
  state: 'clear',
  why: 'nothing waiting; 2 crossings in 24h',
  ...over,
});

const BORDERS_PAYLOAD = {
  window_hours: 24,
  now: '2026-09-19T05:00:00Z',
  borders: [
    // The rate could not be computed and neither could the queue: the
    // rail must read UNKNOWN, not empty.
    rail('receiving', 'marshalling', {
      rate: { metric: 'crossings', unit: 'per day', current: null, previous: null, samples: 0, previous_samples: 0 },
      last_crossed: null, waiting: null, state: 'troubled',
      why: 'the workflow registry that names the inbound kinds could not be read',
    }),
    // The shop floor split this hop in two (backlog 94c6ffd0): a run
    // OPENS on a packet, and its branch takes a bay once built — then
    // parks on green (design 62de32ae decision 3).
    rail('marshalling', 'shop-floor'),
    rail('shop-floor', 'gates'),
    rail('gates', 'dock'),
    // Traffic waiting and the machine silent past its declared cadence.
    rail('dock', 'track', {
      crossing: 'a car boarded a train',
      rate: { metric: 'crossings', unit: 'per day', current: 24, previous: 18, samples: 24, previous_samples: 18 },
      waiting: 3,
      holds: [{ what: 'fix/a-car', why: 'parked, waiting for the boarding depth or the clock rule' }],
      machine: { name: 'train-board-on-dock-depth', kind: 'cadence', last_fired: '2026-09-19T02:00:00Z',
        silent_for_minutes: 180, expected_every_minutes: 30, silent: true,
        why: 'its own firing in cadence_firings' },
      state: 'troubled',
      why: '3 packets waiting and train-board-on-dock-depth silent for 180m — it declares every 30m',
    }),
    rail('track', 'arrivals'),
    rail('arrivals', 'shed'),
    rail('arrivals', 'publish'),
    rail('gates', 'garage'),
    rail('track', 'garage'),
  ],
};

async function mocks(page: Page): Promise<void> {
  await installSmokeMocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
  await page.route(YARD_BORDERS, (r) => json(r, BORDERS_PAYLOAD));
}

test('every territory selects its own region, and publish opens publish — not the track', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator('section.transit svg[role="group"]');
  await expect(svg.locator('[data-station]')).toHaveCount(TERRITORIES.length);
  for (const t of TERRITORIES) {
    await expect(svg.locator(`[data-station="${t.name}"]`)).toHaveAttribute('href', `/it?at=${t.name}`);
  }
  await svg.locator('[data-station="publish"]').click();
  await expect(page).toHaveURL(/\/it\?at=publish$/);
  await expect(page.locator('section[data-map-panel]')).toHaveAttribute('data-selection', 'publish');
  await expect(page.locator('section[data-map-panel] .panel-title')).toHaveText('Publish');
  await expect(page.locator('section[data-map-panel] [data-contents="publish"]')).toHaveCount(1);
  await expect(page.locator('section[data-map-panel] [data-contents="track"]')).toHaveCount(0);
});

test('a territory click selects it, and its panel carries the floor with the region\'s state and why at its head', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const svg = page.locator('section.transit svg[role="group"]');
  await expect(svg.locator('[data-station]')).toHaveCount(TERRITORIES.length);

  // A troubled yard region: selected on the map, its panel carries the
  // map's own state, and the floor in it opens on the gates' entity
  // panel headed by the same verdict — the floor cannot contradict the
  // map. It is IN the panel since car N3 retired the floor pages.
  // The pulse changes its link bounds; click the stable painted door.
  await svg.locator('[data-station="gates"] circle.ring').click();
  await expect(page).toHaveURL(/\/it\?at=gates$/);
  await expect(page.locator('section[data-map-panel]')).toHaveAttribute('data-state', 'troubled');
  await expect(page.locator('section[data-map-panel] a[data-floor]')).toHaveCount(0);
  await expect(page.locator('section[data-map-panel] .yard-panel-h', { hasText: 'Entity · approach' })).toBeVisible();
  const head = page.locator('.yard-region-head');
  await expect(head).toHaveAttribute('data-region', 'gates');
  await expect(head).toHaveAttribute('data-state', 'troubled');
  await expect(head).toContainText('1 bay holds a corpse');
  await expect(head.locator('.yard-lamp-dot.err')).toHaveCount(1);

  // A clear region's floor says so at its head, too.
  await page.locator('section.transit svg[role="group"] [data-station="dock"]').click();
  await expect(page).toHaveURL(/\/it\?at=dock$/);
  await expect(page.locator('.yard-panel-h', { hasText: 'Entity · loading dock' })).toBeVisible();
  await expect(page.locator('.yard-region-head')).toContainText('dock · clear — 3 cars parked');

  // Back returns to the previous selection — it is on the history.
  await page.goBack();
  await expect(page).toHaveURL(/\/it\?at=gates$/);
  await expect(page.locator('section.transit svg[role="group"] [data-station]')).toHaveCount(TERRITORIES.length);

  // A region whose floor is a QUEUE BOARD. Its page retired on car 4
  // of design d2154293: its selection opens its board in the panel like
  // every other floor.
  await page.goto('/it');
  await page.locator('section.transit svg[role="group"] [data-station="receiving"]').click();
  await expect(page).toHaveURL(/\/it\?at=receiving$/);
  await expect(page.locator('section[data-map-panel] .ry-root')).toHaveCount(1);
});

// THE HUD FRAME (design 00774ca8), redrawn as the top board by design
// ea906603: four rows of things to act on, above the map. The three-
// thirds rows this test pinned were removed (David, Q5, 2026-09-27);
// it-top-board.mocked.spec.ts pins each row's three pictures. Here: the
// frame stands above the map on the map's own fixture — which predates
// the board's fields, so those rows read unread, never empty — and the
// machine cell still opens its region.
test('the HUD frame stands above the map, four rows, the machine cell the server’s', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const hud = page.locator('[data-hud]');
  await expect(hud).toHaveAttribute('data-read', 'ok');
  await expect(hud.locator('.hud-age')).toHaveText(/^read \d+s ago$/);
  // Above the map: the frame comes first on the page.
  const hudTop = (await hud.boundingBox())!.y;
  const mapTop = (await page.locator('section.transit svg[role="group"]').boundingBox())!.y;
  expect(hudTop).toBeLessThan(mapTop);

  const rows = hud.locator('.hud-row');
  expect(await rows.evaluateAll((els) => els.map((el) => el.getAttribute('data-row')))).toEqual([
    'outranks', 'needs-you', 'next-up', 'machines',
  ]);
  await expect(hud.locator('[data-third]')).toHaveCount(0);
  // This payload carries no `outranks` and no `next_up`: unread, with why.
  await expect(hud.locator('.hud-row[data-row="outranks"] [data-state="unread"] [data-fig="unread"]')).toHaveText('?');
  await expect(hud.locator('.hud-row[data-row="next-up"] [data-state="unread"] [data-fig="unread"]')).toHaveText('?');

  // The machine cell: failed and unjudged against a total, each
  // unjudged machine a door to its region's map.
  const machines = hud.locator('[data-machines]');
  const line = machines.locator('.hud-machine-line');
  await expect(line.locator('[data-fig]')).toHaveText(['0', '2', '25']);
  await expect(line.locator('[data-fig="zero"]')).toHaveCount(1);
  await expect(line).toContainText('unjudged of');
  await expect(machines.locator('a[data-machine]')).toHaveCount(2);
  await machines.locator('a[data-machine="station:design-review"]').click();
  await expect(page).toHaveURL(/\/it\?at=marshalling$/);
  await expect(page.locator('section[data-map-panel]')).toHaveAttribute('data-selection', 'marshalling');
  // The same frame stands over the selection.
  await expect(page.locator('[data-hud] .hud-row')).toHaveCount(4);
  await expect(page.locator('[data-hud] [data-strip]')).toHaveCount(1);
});

test('a regions read that fails turns every HUD cell to ? and says when, keeping no value', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify('the backend is down') }),
  );
  await page.goto('/it');
  const hud = page.locator('[data-hud]');
  await expect(hud).toHaveAttribute('data-read', 'failed');
  await expect(hud.locator('.hud-age')).toContainText(/read failed \d\d:\d\dZ · no good read yet/);
  await expect(hud.locator('.hud-row')).toHaveCount(4);
  await expect(hud.locator('[data-fig="value"], [data-fig="zero"], [data-fig="floor"]')).toHaveCount(0);
  expect(await hud.locator('[data-fig="unread"]').count()).toBeGreaterThan(0);
});

test('a regions read that fails is said, never drawn as a clear world', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify('the backend is down') }),
  );
  await page.goto('/it');
  await expect(page.locator('.load-failed')).toContainText('The regions cannot be read');
  await expect(page.locator('[data-station]')).toHaveCount(0);
});

test('a borders read that fails is said, and the territories still paint', async ({ page }) => {
  await installSmokeMocks(page);
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route(YARD_REGIONS, (r) => json(r, REGIONS));
  await page.route(YARD_BORDERS, (r) =>
    r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify('the backend is down') }),
  );
  await page.goto('/it');
  await expect(page.locator('.load-failed')).toContainText('The borders cannot be read');
  // The world is still a world: every territory stands. A rail is a
  // border the server answered (car R3 of design e765b3fc), so with the
  // read out none is drawn — the failure line says why, and no rail is
  // guessed in its place.
  await expect(page.locator('section.transit svg[role="group"] [data-station]')).toHaveCount(TERRITORIES.length);
  await expect(page.locator('section.transit svg[role="group"] [data-section][data-ground="unknown"]')).not.toHaveCount(0);
});

// An older server remains an unread source, never a healthy zero.
test('a partial regions payload retains missing Sensors once with the desktop unread reason', async ({ page }) => {
  await mocks(page);
  await page.route(YARD_REGIONS, (r) => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({
    ...REGIONS, regions: REGIONS.regions.filter((region) => region.name !== 'sensors'),
    thirds: REGIONS.thirds.map((third) => ({ ...third, regions: third.regions.filter((name) => name !== 'sensors') })),
  }) }));
  await page.goto('/it');
  const sensors = page.locator('section.transit svg[role="group"] [data-station="sensors"]');
  await expect(sensors).toHaveCount(1);
  await expect(sensors).toHaveAttribute('data-state', 'troubled');
  await expect(sensors.locator('title').first()).toContainText('no reading');
  await expect(page.locator('section.transit svg[role="group"] [data-station]')).toHaveCount(REGION_NAMES.length);
});

test('withheld firing evidence stays a scope refusal in the section panel', async ({ page }) => {
  await mocks(page);
  await page.route(YARD_BORDERS, (r) => r.fulfill({ json: {
    ...BORDERS_PAYLOAD,
    borders: BORDERS_PAYLOAD.borders.map((b) => b.from === 'dock' && b.to === 'track' ? {
      ...b,
      machine: { ...(typeof b.machine === 'object' && b.machine !== null ? b.machine : {}), withheld: true, last_fired: null, silent: null,
        why: 'the firing record is withheld from this caller' },
    } : b),
  } }));
  await page.goto('/it?at=dock-%3Etrack');
  const machines = page.locator('[data-map-panel] [data-field="machines"]');
  await expect(machines).toContainText('not in your policy scope');
  await expect(machines).toContainText('the firing record is withheld from this caller');
});

test('an unread section keeps its reason and never becomes a zero in the panel', async ({ page }) => {
  await mocks(page);
  await page.goto('/it?at=receiving-%3Emarshalling');
  const panel = page.locator('[data-map-panel]');
  await expect(panel.locator('[data-field="rate"]')).toContainText('no reading');
  await expect(panel.locator('[data-field="waiting"]')).toContainText('waiting: no reading');
  await expect(panel).toContainText('the workflow registry that names the inbound kinds could not be read');
});
