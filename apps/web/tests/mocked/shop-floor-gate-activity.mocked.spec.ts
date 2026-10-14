import { expect, test } from './_test';
import { installSmokeMocks } from './_smokeMocks';

const run = '342b709d-4c41-4e4f-8033-b8dadc1b1fb7';
async function workstation(page: import('@playwright/test').Page) {
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\?kind=agent-run&/, (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ data: [{ id: run, title: 'Builder run', metadata: { packet: 'job-gate', agent: 'agent-codex' }, steps: [{ spec_slug: 'building', title: 'Building', status: 'active' }] }], total: 1 }) }));
  await page.route(/\/api\/jobs\/job-gate$/, (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ id: 'job-gate', title: 'Observe the gate activity', steps: [] }) }));
}

test('latest exactly linked gate shows its verdict, event time and evidence on a phone', async ({ page }, testInfo) => {
  await workstation(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.route(/\/api\/jobs\?kind=gate-run&/, (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ data: [
    { id: 'old-pass', status: 'closed', metadata: { agent_run: run, opened_at: '2026-10-13T20:00:00Z', closed_at: '2026-10-13T20:10:00Z', outcome: 'completed' } },
    { id: 'new-failure', status: 'closed', metadata: { agent_run: run, opened_at: '2026-10-13T21:00:00Z', closed_at: '2026-10-13T21:10:00Z', outcome: 'failed' } },
  ], total: 2 }) }));
  await page.goto('/it?at=shop-floor');
  const floor = page.getByRole('region', { name: 'Recorded workstations' });
  await expect(floor.getByRole('link', { name: 'Gate Failed' })).toHaveAttribute('href', '/ux/jobs/new-failure');
  await expect(floor.locator('time[datetime="2026-10-13T21:10:00Z"]')).toBeVisible();
  await expect(floor).not.toContainText('Gate Passed');
  await floor.screenshot({ path: testInfo.outputPath('shop-floor-gate-failure-phone.png') });
});

test('an alien gate is never linked to a workstation', async ({ page }) => {
  await workstation(page);
  await page.route(/\/api\/jobs\?kind=gate-run&/, (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ data: [{ id: 'alien', status: 'closed', metadata: { agent_run: 'different-run', branch: 'same-branch', outcome: 'completed', opened_at: '2026-10-13T21:00:00Z' } }], total: 1 }) }));
  await page.goto('/it?at=shop-floor');
  const floor = page.getByRole('region', { name: 'Recorded workstations' });
  await expect(floor).toContainText('No gate linked to this run');
  await expect(floor).not.toContainText('Gate Passed');
});

test('a failed gate read says activity is unknown', async ({ page }) => {
  await workstation(page);
  await page.route(/\/api\/jobs\?kind=gate-run&/, (route) => route.fulfill({ status: 503, body: 'unavailable' }));
  await page.goto('/it?at=shop-floor');
  const floor = page.getByRole('region', { name: 'Recorded workstations' });
  await expect(floor).toContainText('Gate activity unknown:');
  await expect(floor).not.toContainText('No gate linked');
});
