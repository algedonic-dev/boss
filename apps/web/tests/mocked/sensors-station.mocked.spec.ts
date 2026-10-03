import { expect, test, type Page } from './_test';
import { installSmokeMocks, YARD_REGIONS } from './_smokeMocks';

const NOW = '2026-10-02T12:00:00.000Z';
const SENSOR = { id: 'mail-support', source: 'mail', credential: 'shared-mail', every_minutes: 5,
  opens_kind: 'receive-message', enabled: true, selector: 'support@example.test', last_polled_at: null };
async function mount(page: Page, mode = 'normal') {
  await installSmokeMocks(page);
  await page.clock.install({ time: new Date(NOW) });
  await page.route(YARD_REGIONS, (r) => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({
    window_hours: 24, now: NOW, regions: [{ name: 'sensors', count: 1, state: 'clear', why: 'one declared sensor',
      trend: { metric: 'arriving readings', unit: 'readings', current: 3, previous: 1, samples: 3, previous_samples: 1 } }],
  }) }));
  await page.route(/\/api\/sensors$/, (r) => r.fulfill({ status: mode === 'forbidden' ? 403 : 200, contentType: 'application/json', body: JSON.stringify({ data: [SENSOR], total: 1 }) }));
  await page.route(/\/api\/sensors\/mail-support\/readings\?/, (r) => {
    const url = new URL(r.request().url());
    return r.fulfill({ status: mode === 'failed-window' ? 503 : 200, contentType: 'application/json', body: JSON.stringify({ sensor_id: SENSOR.id,
      since: url.searchParams.get('since'), until: url.searchParams.get('until'), arrived: 3, stamped: 2, unstamped: 1, packets: ['packet-one'] }) });
  });
  await page.goto('/it?at=sensors');
  return page.getByRole('region', { name: 'Sensor readings' });
}
test('Sensors opens below the existing map with source receipts and unavailable outbound', async ({ page }) => {
  const panel = await mount(page);
  await expect(panel).toBeVisible();
  await expect(panel).toContainText('shared-mail');
  await expect(panel).toContainText('support@example.test');
  await expect(panel).toContainText('Leaving: no outbound transport');
  await expect(panel.getByRole('link', { name: 'packet-one' })).toHaveAttribute('href', '/ux/jobs/packet-one');
  await expect(page.locator('[data-contents="sensors"]')).toBeVisible();
});
test('forbidden registry and failed windows never render a healthy empty or zero row', async ({ page }) => {
  const panel = await mount(page, 'forbidden');
  await expect(panel.getByRole('alert')).toContainText('HTTP 403');
  await expect(panel).not.toContainText('No sensors declared');
  await page.route(/\/api\/sensors$/, (r) => r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ data: [SENSOR], total: 1 }) }));
  await page.route(/\/api\/sensors\/mail-support\/readings\?/, (r) => r.fulfill({ status: 503, body: 'unavailable' }));
  await panel.getByRole('button', { name: 'Refresh readings' }).click();
  await expect(panel).toContainText('Readings unavailable');
  await expect(panel.locator('tbody td')).toHaveCount(4);
});
