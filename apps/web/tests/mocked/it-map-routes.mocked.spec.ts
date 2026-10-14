// THE MAP DRAWS ONLY SERVED ROUTES — design e765b3fc, car R3 on feedback
// 84cba7e2 (David, added_2026_09_25_david: "I am not sure our station
// lines are really accurate anymore ... like how the dock routes a train
// over to the gates before it departs onto the tracks").
//
// The car's plan names its test: a mocked spec where adding or removing
// a route in the fixture adds or removes it on the map. This pins, on
// the transit map (the Department Map's grammar, behind it-map-transit):
//
//  - every section, exit and entry drawn is one GET /api/yard/routes
//    served, and a route added to the answer arrives on the map while
//    one taken away leaves it — with no web change;
//  - the train's own line: from the dock back to the gates, then over to
//    the track — never dock → track, which no protocol takes;
//  - every packet that leaves the map leaves by a drawn exit naming its
//    terminals (added_2026_09_25_david_offramps);
//  - a route only the moves record supports is drawn dashed red and the
//    key says what that means;
//  - a routes read that fails draws the stations and no guessed track,
//    and says so;
//  - a drawn section with no rate yet opens a panel that says what
//    declares it.

import { expect, test, type Page, type Route } from './_test';
import { STATIONS } from '../../src/it/yard/transit';
import { ROUTES, routesPayload, type FixtureRoute } from '../fixtures/yard';
import { YARD_ROUTES, installSmokeMocks } from './_smokeMocks';

const FLIGHTS = /\/api\/flights\/mine(\?|$)/;
const MAP = 'section.transit[data-transit]';

const json = (r: Route, b: unknown): Promise<void> =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

async function mocks(page: Page, routes: ReadonlyArray<FixtureRoute> | 'fail' = ROUTES): Promise<void> {
  await installSmokeMocks(page);
  await page.route(FLIGHTS, (r) => json(r, { flights: ['it-map-transit'] }));
  await page.route(YARD_ROUTES, (r) =>
    routes === 'fail'
      ? r.fulfill({ status: 503, contentType: 'text/plain', body: 'no workflow registry is wired' })
      : json(r, routesPayload(routes)),
  );
}

const key = (r: FixtureRoute): string => `${r.from ?? ''}→${r.to ?? ''}`;
const drawn = (page: Page) => page.locator(`${MAP} [data-section]`);

test('the map draws exactly the routes served — one drawn element per route, no more and no fewer', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator(MAP)).toBeVisible();
  await expect(drawn(page)).toHaveCount(ROUTES.filter((r) => r.to !== null).length);
  const keys = await drawn(page).evaluateAll((els) => els.map((e) => e.getAttribute('data-section') ?? ''));
  expect([...keys].sort()).toEqual(ROUTES.filter((r) => r.to !== null).map(key).sort());
});

test('a route added to the answer arrives on the map, and one taken away leaves it', async ({ page }) => {
  const added: FixtureRoute = {
    from: 'track',
    to: 'garage',
    declared: true,
    sources: [{ source: 'workflow', workflow: 'pr-train', version: 4, step: 'struck', via: 'completed' }],
  };
  await mocks(page, [...ROUTES, added]);
  await page.goto('/it');
  await expect(page.locator(`${MAP} path.section[data-section="track→garage"]`)).toHaveCount(1);
  await expect(drawn(page)).toHaveCount(ROUTES.filter((r) => r.to !== null).length + 1);

  await page.unroute(YARD_ROUTES);
  await page.route(YARD_ROUTES, (r) => json(r, routesPayload(ROUTES.filter((x) => key(x) !== 'gates→track'))));
  await page.reload();
  await expect(drawn(page)).toHaveCount(ROUTES.filter((r) => r.to !== null).length - 1);
  await expect(page.locator(`${MAP} [data-section="gates→track"]`)).toHaveCount(0);
  await expect(page.locator(`${MAP} [data-section="track→garage"]`)).toHaveCount(0);
});

test('the train\'s own line: made up at the dock, back to the gates, over to the track — never dock → track', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator(`${MAP} path.section[data-section="dock→gates"]`)).toHaveCount(1);
  await expect(page.locator(`${MAP} path.section[data-section="gates→track"]`)).toHaveCount(1);
  await expect(page.locator(`${MAP} path.section[data-section="track→arrivals"]`)).toHaveCount(1);
  await expect(page.locator(`${MAP} [data-section="dock→track"]`)).toHaveCount(0);
  // Addressable by its two ends — the moves feed (car M2) finds the path
  // a move travels by the same pair.
  const train = page.locator(`${MAP} path.section[data-section="gates→track"]`);
  await expect(train).toHaveAttribute('data-from', 'gates');
  await expect(train).toHaveAttribute('data-to', 'track');
});

test('exits remain in the records but are not drawn as ramps; entries remain visible', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator(`${MAP} [data-ramp="exit"]`)).toHaveCount(0);
  const entries = ROUTES.filter((r) => r.from === null);
  await expect(page.locator(`${MAP} [data-ramp="entry"]`)).toHaveCount(entries.length);
  await expect(page.locator(`${MAP} [data-ramp="entry"][data-section="→receiving"] title`)).toHaveText('enters the map at receiving: filed');
});

// Backlog 3c8b5653 (design f0313eda; David chose E2, the exits band,
// 2026-09-26): every station's exits drop into ONE band under the line —
// a stub, a branch per terminal in name order, a buffer stop on the one
// rail with its count, the fan headed by its station and total.
test('exit terminals do not enlarge the map; station conservation counts remain', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  await expect(page.locator(`${MAP} [data-exits-band], ${MAP} [data-terminal]`)).toHaveCount(0);
  // The station's words stand off its lines, with its in · on · off.
  await expect(page.locator(`${MAP} [data-ledger="marshalling"]`)).toHaveText('in 20 · on 24 · off 45');
  // Still one drawn element per route: a branch is part of its exit.
  await expect(drawn(page)).toHaveCount(ROUTES.filter((r) => r.to !== null).length);
});

test('observed exit terminals do not restore exit ramps', async ({ page }) => {
  const withFinding = ROUTES.map((r) =>
    r.from === 'marshalling' && r.to === null
      ? { ...r, sources: [...r.sources, { source: 'observed' as const, moves: 19, last_at: '', kind: 'agent-run', terminal: 'landed' }] }
      : r,
  );
  await mocks(page, withFinding);
  await page.goto('/it');
  await expect(page.locator(`${MAP} [data-ramp="exit"], ${MAP} [data-terminal]`)).toHaveCount(0);
  await expect(page.locator(`${MAP} [data-ledger="marshalling"]`)).toHaveText('in 20 · on 24 · off 64');
});

test('many long exit terminal names do not add a fan or widen the map', async ({ page }) => {
  const many = Array.from({ length: 14 }, (_, i) => `a-rather-long-terminal-name-${String(i).padStart(2, '0')}`);
  const wide = ROUTES.map((r) =>
    r.from === 'marshalling' && r.to === null
      ? { ...r, sources: many.map((step) => ({ source: 'workflow' as const, workflow: 'backlog-item', version: 1, step, via: 'closed' })) }
      : r,
  );
  await mocks(page, wide);
  await page.goto('/it');
  await expect(page.locator(`${MAP} [data-ramp="exit"], ${MAP} [data-exits-band]`)).toHaveCount(0);
  const wideView = await page.locator(`${MAP} svg[viewBox]`).first().getAttribute('viewBox');
  await page.unroute(YARD_ROUTES);
  await page.route(YARD_ROUTES, (r) => json(r, routesPayload(ROUTES)));
  await page.reload();
  await expect(page.locator(`${MAP} svg[viewBox]`).first()).toHaveAttribute('viewBox', wideView!);
});

test('a route only the moves record supports is drawn dashed red, and the key says what that means', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const undeclared = page.locator(`${MAP} path.section[data-section="shed→arrivals"]`);
  await expect(undeclared).toHaveAttribute('data-declared', 'false');
  await expect(undeclared).toHaveClass(/undeclared/);
  await expect(page.locator(`${MAP} path.section[data-declared="false"]`)).toHaveCount(1);
  await expect(page.locator(`${MAP} [data-key-undeclared]`)).toContainText('no protocol or hand-off declares it');

  // With every route declared, the key does not carry the finding.
  await page.unroute(YARD_ROUTES);
  await page.route(YARD_ROUTES, (r) => json(r, routesPayload(ROUTES.filter((x) => x.declared))));
  await page.reload();
  await expect(page.locator(`${MAP} [data-declared="false"]`)).toHaveCount(0);
  await expect(page.locator(`${MAP} [data-key-undeclared]`)).toHaveCount(0);
});

// Backlog b9c88c61 (David 2026-09-26, watching the live map): a section
// with no reading was a dash of 2 6 on a round-capped stroke — beads.
// It is a hollow tube now: the section's own path as the casing and a
// pale core over it, neither dashed; the undeclared route keeps its dash.
test('a section with no reading is drawn as a hollow tube, and an undeclared one stays dashed', async ({ page }) => {
  await mocks(page);
  await page.goto('/it');
  const link = page.locator(`${MAP} [data-section-link="gates→track"]`);
  const casing = link.locator('path.section');
  await expect(casing).toHaveAttribute('data-ground', 'unknown');
  const core = link.locator('path.tube-core');
  await expect(core).toHaveCount(1);
  const dash = (l: typeof core) => l.evaluate((e) => getComputedStyle(e).strokeDasharray);
  const width = (l: typeof core) => l.evaluate((e) => parseFloat(getComputedStyle(e).strokeWidth));
  expect(await dash(casing)).toBe('none');
  expect(await dash(core)).toBe('none');
  expect(await width(core)).toBeLessThan(await width(casing));
  // The core is not a route: one drawn element per route still holds.
  await expect(core).not.toHaveAttribute('data-section', /.*/);

  const undeclared = page.locator(`${MAP} [data-section-link="shed→arrivals"]`);
  expect(await dash(undeclared.locator('path.section'))).not.toBe('none');
  await expect(undeclared.locator('path.tube-core')).toHaveCount(0);
  await expect(page.locator(`${MAP} [data-key-unconfirmed]`)).toContainText('no reading yet');
});

test('a routes read that fails draws the stations and no guessed track, and says so', async ({ page }) => {
  await mocks(page, 'fail');
  await page.goto('/it');
  await expect(page.locator(`${MAP} [data-station]`)).toHaveCount(STATIONS.length);
  await expect(drawn(page)).toHaveCount(0);
  await expect(page.locator('[data-routes-failed].load-failed')).toContainText('The routes cannot be read');
});

test('a drawn section with no rate yet opens a panel that says what declares it', async ({ page }) => {
  await mocks(page);
  await page.goto('/it?at=gates-%3Etrack');
  const panel = page.locator('section[data-map-panel]');
  await expect(panel).toHaveAttribute('data-kind', 'section');
  await expect(panel.locator('[data-field="route"] li').first()).toHaveText('pr-train v1, step merged (completed)');
  await expect(panel.locator('[data-field="rate"] li')).toHaveText([
    'no reading yet — this route is served from the protocols, and its rate comes with the moves record',
  ]);
  await expect(page.locator(`${MAP} [data-selected-mark="gates→track"]`)).toHaveCount(1);
});
