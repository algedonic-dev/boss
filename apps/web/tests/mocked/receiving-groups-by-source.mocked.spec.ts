// The receiving board, embedded under the Department Map's receiving
// selection (/it?at=receiving), groups its untriaged backlog items by
// WHERE THEY CAME FROM and by area — design 3036296f mechanism D,
// backlog 9b473d4a car 2.
//
// Until this car the board drew one flat pile: measured 2026-09-27, 105
// untriaged of 359 open backlog items, none with a structured source.
// The server now reads each row's source and area (`origin=true`,
// `boss_jobs::origin::origin_of`); the board groups by the key it was
// handed. What this spec pins, through the page a person opens:
//   - the read asks for `origin=true`
//   - the design's own sentence, "N items from M car reviews, K urgent"
//   - one track per source, largest first, each car a link to its packet
//   - the unrecorded — prose, an ad-hoc key, nothing — in ONE visible
//     "no recorded source" group, counted by basis, never dropped
//   - the areas, with "no area" always said

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { regionHref } from '../../src/it/yard/regions';

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const recorded = (kind: string, key: string, extra: Record<string, unknown> = {}) => ({
  basis: 'recorded', kind, key, id: null, branch: null, ...extra,
});
const unrecorded = (basis: string) => ({ basis, kind: null, key: null, id: null, branch: null });

const item = (
  today: string,
  id: string,
  source: Record<string, unknown>,
  area: string | null,
  priority = 'standard',
) => ({
  id,
  kind: 'backlog-item',
  title: `item ${id}`,
  status: 'open',
  priority,
  opened_on: today,
  closed_on: null,
  metadata: {},
  lane: { lane: 'review-finding', basis: 'recorded' },
  origin: { source, area },
  steps: [
    { kind: 'trigger', status: 'completed', assignee_id: null },
    { kind: 'task', status: 'ready', assignee_id: 'claude@algedonic.dev' },
  ],
});

/** The rows, dated the day the handler answers — never the day the file
 *  loaded, so no age grows while the worker runs. */
const rows = () => {
  const today = new Date().toISOString().slice(0, 10);
  return [
    item(today, '00000000-0000-4000-8000-0000000000a1', recorded('review', 'review:fix/a', { branch: 'fix/a' }), 'jobs', 'urgent'),
    item(today, '00000000-0000-4000-8000-0000000000a2', recorded('review', 'review:fix/a', { branch: 'fix/a' }), 'jobs'),
    item(today, '00000000-0000-4000-8000-0000000000a3', recorded('review', 'review:fix/b', { branch: 'fix/b' }), 'web', 'urgent'),
    item(today, '00000000-0000-4000-8000-0000000000a4', recorded('agent-run', 'agent-run:665c7419-0000-4000-8000-000000000001', { id: '665c7419-0000-4000-8000-000000000001' }), null),
    item(today, '00000000-0000-4000-8000-0000000000a5', unrecorded('prose'), 'jobs'),
    item(today, '00000000-0000-4000-8000-0000000000a6', unrecorded('ad-hoc'), null),
    item(today, '00000000-0000-4000-8000-0000000000a7', unrecorded('missing'), null),
  ];
};

async function install(page: Page): Promise<string[]> {
  await installSmokeMocks(page);
  const asked: string[] = [];
  await page.route(/\/api\/workflows$/, (r) =>
    json(r, [{ kind: 'backlog-item', category: 'platform', status: 'active' }]),
  );
  await page.route(/\/api\/jobs\?kind=backlog-item&/, (r) => {
    asked.push(r.request().url());
    const data = rows();
    return json(r, { data, total: data.length, limit: 500, offset: 0 });
  });
  return asked;
}

const board = (page: Page) => page.locator('.ry-root');

test.describe('the receiving board groups the untriaged by source and area', () => {
  test('the read asks for the origin, and the design\'s sentence is drawn per source kind', async ({ page }) => {
    const asked = await install(page);
    await mountPage(page, regionHref('receiving'), { titleMatch: /Department Map/ });
    const said = board(page).locator('[data-testid="ry-provenance"]');
    await expect(said).toContainText('3 items from 2 car reviews, 2 urgent.');
    await expect(said).toContainText('1 item from 1 agent run.');
    await expect(said).toContainText('3 of 7 with no recorded source.');
    await expect(said).toContainText('1 carries a sentence.');
    // The board's own read (real work, over its window); another
    // reader on the map lists backlog items too.
    const mine = asked.filter((u) => u.includes('simulated=false&closed_within='));
    expect(mine.length).toBeGreaterThan(0);
    expect(mine.filter((u) => !(u.includes('origin=true') && u.includes('lane=true')))).toEqual([]);
    // SLIM rows (backlog ea80b5fd): the failed-verb note is the server's
    // reading, so the board never asks every step's metadata for it.
    expect(mine.filter((u) => !u.includes('failed_verbs=true') || u.includes('full=true'))).toEqual([]);
  });

  test('one track per source, largest first, and the unrecorded stand in one visible group', async ({ page }) => {
    await install(page);
    await mountPage(page, regionHref('receiving'), { titleMatch: /Department Map/ });
    const section = board(page).locator('.ry-section', { hasText: 'WHERE THE UNTRIAGED BACKLOG ITEMS CAME FROM' });
    await expect(section).toBeVisible();
    const tracks = board(page).locator('.ry-tracks').nth(1).locator('.ry-track');
    await expect(tracks).toHaveCount(4);
    await expect(tracks.nth(0).locator('.tk-name')).toContainText('fix/a');
    await expect(tracks.nth(0).locator('.tk-num')).toHaveText('2 standing · 1 urgent');
    await expect(tracks.nth(0).locator('a.car')).toHaveCount(2);
    // A source whose reading carries an id links to that packet, named
    // by its first eight.
    await expect(tracks.nth(1).locator('.tk-name a')).toHaveText('665c7419');
    await expect(tracks.nth(1).locator('.tk-name a')).toHaveAttribute(
      'href',
      /\/jobs\/665c7419-0000-4000-8000-000000000001$/,
    );
    const none = tracks.nth(3);
    await expect(none.locator('.tk-name')).toHaveText('no recorded source');
    await expect(none.locator('a.car')).toHaveCount(3);
  });

  test('the areas are counted, and no area is always said', async ({ page }) => {
    await install(page);
    await mountPage(page, regionHref('receiving'), { titleMatch: /Department Map/ });
    const areas = board(page).locator('.ry-areas .pill');
    await expect(areas).toHaveText(['jobs 3 · 1 urgent', 'web 1 · 1 urgent', 'no area 3']);
  });
});
