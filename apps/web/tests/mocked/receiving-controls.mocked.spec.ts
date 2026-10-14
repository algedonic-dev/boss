// Audit b0333631: test the board's own reads independently of the map.
// A healthy region cannot excuse a failed registry or packet listing.
// The two historical aliases were retired; exercise the canonical map
// selection, rather than restoring routes solely to fit the old audit.
import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { regionHref } from '../../src/it/yard/regions';

const json = (r: Route, body: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
const board = (page: Page) => page.locator('.ry-root');

const packet = (n: number) => ({
  id: `10000000-0000-4000-8000-${String(n).padStart(12, '0')}`,
  kind: 'backlog-item', title: `audit packet ${n}`, status: 'open', priority: 'standard',
  opened_on: new Date().toISOString().slice(0, 10), closed_on: null,
  lane: { lane: 'operator', basis: 'recorded' },
  origin: { source: { basis: 'missing' }, area: null },
  steps: [{ kind: 'task', status: 'ready', assignee_id: null }],
});

async function install(page: Page, fail: 'registry' | 'packets' | null) {
  await installSmokeMocks(page);
  await page.route(/\/api\/workflows$/, (r) => fail === 'registry'
    ? json(r, { error: 'audit registry refusal' }, 503)
    : json(r, [{ kind: 'backlog-item', category: 'platform', status: 'active' }]));
  await page.route(/\/api\/jobs\?kind=backlog-item&/, (r) => fail === 'packets'
    ? json(r, { error: 'audit packet refusal' }, 503)
    : json(r, { data: [], total: 0, limit: 500, offset: 0 }));
}

test.describe('receiving audit: empty and failed are distinct board states', () => {
  for (const failure of ['registry', 'packets'] as const) {
    test(`a healthy map cannot hide the ${failure} failure`, async ({ page }) => {
      await install(page, failure);
      await mountPage(page, regionHref('receiving'), { titleMatch: /Department Map/ });
      const own = board(page);
      await expect(own.locator('.load-failed')).toContainText(failure === 'registry'
        ? 'The workflow registry did not answer'
        : 'A packet read did not answer');
      await expect(own.locator('.ry-tracks')).toHaveCount(0);
      await expect(own.locator('.ry-table')).toHaveCount(0);
      await expect(own).not.toContainText('The inbound track is clear.');
      await expect(own).not.toContainText('No backlog-item is standing untriaged.');
    });
  }

  test('a successful empty listing says the track is clear and keeps the unclassified lane', async ({ page }) => {
    await install(page, null);
    await mountPage(page, regionHref('receiving'), { titleMatch: /Department Map/ });
    const own = board(page);
    await expect(own).toContainText('The inbound track is clear.');
    await expect(own).toContainText('No backlog-item is standing untriaged.');
    await expect(own.locator('.load-failed')).toHaveCount(0);
    await expect(own.locator('.ry-track .tk-name')).toHaveText('unclassified');
    await expect(own.locator('.ry-track .tk-num')).toHaveText('0 in · 0 standing');
    await expect(own.locator('.ry-track .ry-empty')).toHaveText('nothing standing');
    await expect(own.locator('.ry-table')).toHaveCount(0);
    await expect(own.locator('.ry-strip .v').nth(2)).toHaveText('0');
  });

  for (const failTail of [false, true]) {
    test(`the second page ${failTail ? 'failure invalidates the whole board' : 'contributes its packet to the manifest'}`, async ({ page }) => {
      await install(page, null);
      const offsets: number[] = [];
      await page.route(/\/api\/jobs\?kind=backlog-item&/, (r) => {
        const offset = Number(new URL(r.request().url()).searchParams.get('offset'));
        offsets.push(offset);
        if (offset > 0 && failTail) return json(r, { error: 'audit tail refusal' }, 503);
        const data = offset === 0 ? Array.from({ length: 500 }, (_, n) => packet(n)) : [packet(500)];
        return json(r, { data, total: 501, limit: 500, offset });
      });
      await mountPage(page, regionHref('receiving'), { titleMatch: /Department Map/ });
      const own = board(page);
      if (failTail) {
        await expect(own.locator('.load-failed')).toContainText('A packet read did not answer');
        await expect(own.locator('.ry-table')).toHaveCount(0);
        await expect(own.locator('.ry-tracks')).toHaveCount(0);
        await expect(own).not.toContainText('The inbound track is clear.');
      } else {
        await expect(own.locator('.ry-table tbody tr')).toHaveCount(501);
        await expect(own.locator('.ry-table')).toContainText('audit packet 500');
        await expect(own.locator('.ry-strip .v').nth(2)).toHaveText('501');
        await expect(own.locator('.load-failed')).toHaveCount(0);
      }
      expect(offsets).toContain(0);
      expect(offsets).toContain(500);
    });
  }

  test('each board packet link opens its recorded packet and browser back restores receiving', async ({ page }) => {
    await install(page, null);
    await page.route(/\/api\/jobs\?kind=backlog-item&/, (r) =>
      json(r, { data: [packet(1)], total: 1, limit: 500, offset: 0 }));
    await mountPage(page, regionHref('receiving'), { titleMatch: /Department Map/ });
    const own = board(page);
    await expect(own.locator('.ry-table tbody tr')).toHaveCount(1);
    // Lane track, no-source track and manifest are separate controls.
    const links = own.locator('a');
    await expect(links).toHaveCount(3);
    for (let i = 0; i < 3; i += 1) {
      await expect(links.nth(i)).toHaveAttribute('href', `/jobs/${packet(1).id}`);
      await links.nth(i).click();
      await expect(page).toHaveURL(new RegExp(`/jobs/${packet(1).id}$`));
      await page.goBack();
      await expect(page).toHaveURL(/\/it\?at=receiving$/);
      await expect(own.locator('.ry-table')).toContainText('audit packet 1');
    }
  });

  test('recorded provenance and a failed verb retain their source and alert navigation', async ({ page }) => {
    await install(page, null);
    const source = packet(2).id;
    const alert = packet(3).id;
    await page.route(/\/api\/jobs\?kind=backlog-item&/, (r) => json(r, {
      data: [{ ...packet(1),
        origin: { source: { basis: 'recorded', kind: 'packet', key: `packet:${source}`, id: source }, area: 'delivery' },
        steps: [{ kind: 'task', status: 'ready', assignee_id: 'agent-codex',
          failed_verb: { failed: 'original diagnostic refused', failed_exit: 75, alert } }],
      }], total: 1, limit: 500, offset: 0,
    }));
    await mountPage(page, regionHref('receiving'), { titleMatch: /Department Map/ });
    const own = board(page);
    await expect(own.locator('.ry-table [role="alert"]')).toContainText('FAILED (exit 75) · original diagnostic refused');
    await expect(own.locator('.ry-table .hold')).toHaveText('the agent');
    await expect(own.locator('a.car').first()).toHaveAttribute('title', /FAILED \(exit 75\) · original diagnostic refused/);
    for (const target of [source, alert]) {
      const link = own.locator(`a[href="/jobs/${target}"]`);
      await expect(link).toHaveCount(1);
      await link.click();
      await expect(page).toHaveURL(new RegExp(`/jobs/${target}$`));
      await page.goBack();
      await expect(own.locator('.ry-table')).toContainText('audit packet 1');
    }
  });

  test('arrivals, departures, age bands and intake use different recorded facts', async ({ page }) => {
    await install(page, null);
    await page.route(/\/api\/jobs\?kind=backlog-item&/, (r) => {
      const today = new Date().toISOString().slice(0, 10);
      const old = new Date(Date.parse(`${today}T00:00:00Z`) - 20 * 86_400_000).toISOString().slice(0, 10);
      const data = [
        { ...packet(1), opened_on: old },
        { ...packet(2), lane: { lane: 'unclassified', basis: 'unclassified' } },
        { ...packet(3), status: 'closed', closed_on: today },
        { ...packet(4), steps: [{ kind: 'task', status: 'completed', assignee_id: 'agent-codex' }] },
      ];
      return json(r, { data, total: data.length, limit: 500, offset: 0 });
    });
    await mountPage(page, regionHref('receiving'), { titleMatch: /Department Map/ });
    const own = board(page);
    const figures = own.locator('.ry-strip .v');
    await expect(figures.nth(0)).toHaveText('3');
    await expect(figures.nth(1)).toHaveText('1');
    await expect(figures.nth(2)).toHaveText('2');
    await expect(figures.nth(3)).toContainText('1');
    await expect(figures.nth(4)).toHaveText('1');
    await expect(own.locator('.ry-table tbody tr')).toHaveCount(2);
    await expect(own.locator('.ry-table tbody tr').first()).toContainText('audit packet 1');
    await expect(own.locator('.ry-table tbody tr').first().locator('td').first()).toHaveText('20 d');
    await expect(own.locator('.ry-table')).not.toContainText('audit packet 3');
    await expect(own.locator('.ry-table')).not.toContainText('audit packet 4');
    await expect(own.locator('a.car.stale')).toHaveCount(2);
    await expect(own.locator('.ry-chart')).toHaveAttribute('aria-label', 'inbound packets per day');
    await expect(own.locator('.ry-chart line.left')).toHaveCount(8);
    await expect(own.locator('.ry-findings').first()).toContainText('1 of 3 arrivals record no lane.');
  });
});
