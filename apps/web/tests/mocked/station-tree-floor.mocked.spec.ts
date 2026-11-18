// THE FLOOR SPEAKS THE TREE — T7's drawing (backlog 94a9dad4, design
// 4b27a65a), against a MOCKED tree read. The pure rules are pinned in
// src/it/tree/*.test.ts; this pins the page: cards nested inside cards,
// the pools and gaps the read carries, the station rows' edges drawn
// ring to ring, the depth control collapsing a department, a subtree
// read from its own top, and a failed read said rather than drawn as an
// empty company. The moves that rode the floor, the served routes and
// the alarms board's place on it went with the transit map (R3, backlog
// 23a4b4d7): motion is the station page's (station-motion.mocked.spec.ts)
// and the alarms board is the Department Map's (it-department-map).

import { expect, test, type Page, type Route } from './_test';
import { claimsRead, stationList, stationLoad, treeRead } from '../fixtures/station-tree';
import { STATIONS_LOAD, installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const STATIONS = /\/api\/stations(\?|$)/;
const CLAIMS = /\/api\/claims(\?|$)/;

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

/** The tree read answers by its `under`; the bare list is what the page
 *  finds the root in. Every station read is counted, so a test can say
 *  which reads the page made. */
async function mocks(
  page: Page,
  tree: 'ok' | 'down' = 'ok',
  claims: 'ok' | 'down' = 'ok',
): Promise<string[]> {
  const asked: string[] = [];
  await installSmokeMocks(page);
  await page.route(STATIONS, (r) => {
    const url = new URL(r.request().url());
    asked.push(url.search);
    const under = url.searchParams.get('under');
    if (under === null) return json(r, stationList());
    if (tree === 'down') return r.fulfill({ status: 503, body: 'the station registry is not configured' });
    const name = under.startsWith('id-') ? under.slice(3) : under;
    return json(r, treeRead(name));
  });
  await page.route(STATIONS_LOAD, (r) => json(r, stationLoad()));
  await page.route(CLAIMS, (r) => (claims === 'down' ? r.fulfill({ status: 503, body: 'claims console down' }) : json(r, claimsRead())));
  return asked;
}

const card = (page: Page, name: string) => page.locator(`[data-station="${name}"]`);

test('one card drawn recursively: five rows on every station — rail, plate, pools as cells, parts — and the gaps beside the floor', async ({ page }) => {
  const asked = await mocks(page);
  await page.goto('/it/tree');
  // The root is found by the tree's shape, then read by its id.
  await expect(card(page, 'company')).toBeVisible();
  expect(asked).toContain('');
  expect(asked).toContain('?under=id-company');

  // Nested: a room is drawn inside its department, inside the company.
  await expect(card(page, 'company').locator('[data-station="engineering"] [data-station="gates"]')).toBeVisible();
  await expect(card(page, 'operations').locator('[data-station="repair"]')).toBeVisible();
  await expect(page.locator('[data-station]')).toHaveCount(16);
  // The same two body rows on every card (design c4854fd4, card-rows).
  await expect(page.locator('[data-card-row="pools"]')).toHaveCount(16);
  await expect(page.locator('[data-card-row="parts"]')).toHaveCount(16);

  // The rail is the department's line, inherited inside it; the root's is night ink.
  await expect(card(page, 'engineering')).toHaveAttribute('data-line', 'it');
  await expect(card(page, 'forge-ci')).toHaveAttribute('data-line', 'it');
  await expect(card(page, 'repair')).toHaveAttribute('data-line', 'hosting');
  await expect(card(page, 'company')).toHaveAttribute('data-line', 'none');

  // The root holds the pools whole: one cell per unit, all open, because
  // no claim records an allocation — said, never drawn as free.
  const root = card(page, 'company').locator(':scope > [data-card-body]');
  await expect(root.locator('[data-pool="host:w-1"]')).toContainText('32 vCPU held');
  await expect(root.locator('[data-cells="host:w-1"] .c')).toHaveCount(32);
  await expect(root.locator('[data-cells="host:w-1"] .c.u')).toHaveCount(0);
  await expect(root.locator('[data-pool="disk:ci-workspaces"]')).toContainText('capacity unknown');
  await expect(root.locator('[data-pool="people:platform-admin"]')).toBeVisible();
  await expect(root.locator('[data-pool="robots:opus"]')).toBeVisible();
  await expect(root.locator('[data-pools-note]')).toHaveText('in use not recorded');

  // A room: its share, its plate, its seats, its machine, the load's count.
  const gates = card(page, 'gates').locator(':scope > [data-card-body]');
  await expect(gates.locator('[data-pool="host:w-1"]')).toContainText('8 vCPU held');
  await expect(gates.locator('[data-cells="host:w-1"] .c')).toHaveCount(8);
  await expect(gates.locator('[data-pool-group="people"]')).toContainText('none held');
  await expect(gates.locator('[data-part="seats"]')).toContainText('busy not recorded · 2 bays');
  await expect(gates.locator('[data-part="machines"]')).toContainText('1');
  await expect(gates.locator('[data-part="standing"]')).toContainText('3');
  await expect(card(page, 'gates').locator(':scope > header [data-plate]')).toHaveText('clear');

  // Every gap the server named is said, beside the floor.
  const gaps = page.locator('[data-tree-gaps]');
  await expect(gaps).toContainText('live node `forge` has no host pool');
  await expect(gaps).toContainText('outside the tree: my-watchlist');
  await expect(gaps).toContainText('host:w-2: no estate node `w-2` is declared');
  // The address comes from the read, never from the page.
  await expect(page.locator('[data-tree-address]')).toHaveText('/company');
});

test('a station row edge is drawn ring to ring, and nothing moves on the floor', async ({ page }) => {
  await mocks(page);
  await page.goto('/it/tree');
  const edge = page.locator('[data-tree-route="gates→loading-dock"]');
  await expect(edge).toHaveCount(1);
  const ring = page.locator('[data-ring="gates"]');
  const d = (await edge.getAttribute('d')) ?? '';
  expect(d.startsWith(`M${Number(await ring.getAttribute('cx'))} ${Number(await ring.getAttribute('cy'))}`)).toBe(true);
  // The transit map's motion layer retired with it (R3): no dots, no pings.
  await expect(page.locator('[data-tree-floor] [data-live]')).toHaveCount(0);
});

test('past the depth asked a department collapses, and its parts row counts the stations it holds', async ({ page }) => {
  await mocks(page);
  await page.goto('/it/tree?depth=1');
  await expect(card(page, 'engineering')).toBeVisible();
  await expect(card(page, 'gates')).toHaveCount(0);
  await expect(card(page, 'engineering').locator('[data-part="sub-stations"]')).toContainText('4');
  await expect(card(page, 'engineering').locator('[data-part="ad-hoc"]')).toContainText('0');
  await expect(page.locator('[data-depth-choice="1"]')).toHaveAttribute('aria-current', 'true');
});

test('a claims read that fails turns every rail red and draws no in-use figure, and is said beside the floor', async ({ page }) => {
  await mocks(page, 'ok', 'down');
  await page.goto('/it/tree');
  await expect(card(page, 'gates')).toBeVisible();
  await expect(card(page, 'gates')).toHaveAttribute('data-rail', 'failed');
  await expect(card(page, 'gates').locator(':scope > header [data-plate]')).toHaveText('read failed');
  await expect(card(page, 'gates').locator(':scope > [data-card-body] [data-pools-note]')).toHaveText('in use unread');
  await expect(card(page, 'gates').locator(':scope > [data-card-body] [data-part="seats"]')).toContainText('busy unread');
  await expect(page.locator('[data-claims-failed]')).toContainText('503');
});

test('a subtree is read from its own top, with no list read and the address the read derived', async ({ page }) => {
  const asked = await mocks(page);
  await page.goto('/it/tree?under=engineering');
  await expect(card(page, 'engineering')).toBeVisible();
  await expect(card(page, 'company')).toHaveCount(0);
  await expect(page.locator('[data-station]')).toHaveCount(6);
  expect(asked).toEqual(['?under=engineering']);
  await expect(page.locator('[data-tree-address]')).toHaveText('/company/engineering');
});

test('a tree read that fails is said, not drawn as an empty company', async ({ page }) => {
  await mocks(page, 'down');
  await page.goto('/it/tree');
  await expect(page.locator(FAILURE_MARKER)).toContainText('503');
  await expect(page.locator('[data-station]')).toHaveCount(0);
});
