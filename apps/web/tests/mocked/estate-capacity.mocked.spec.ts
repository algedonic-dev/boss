import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const assignment = { packet: '32e7cf87-7d8e-4764-94a3-3b0cfa8548c4', step: 'a40668dd-27d0-467b-822c-10f5d28b657a', question: 'Q1', decided_by: 'emp-david', decided_at: '2026-10-02T23:53:23.553088Z' };
async function install(page: Page, verdict: 'match' | 'drift' | 'legacy'): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/estate\/comparisons\?scope=instance-volumes&/, (route) => route.fulfill({
    contentType: 'application/json', body: JSON.stringify({ data: [{ payload: {
      scope: 'instance-volumes', observed_at: new Date().toISOString(), observer: 'fixture', volumes: [{
        id: 'boss/pgdata-postgres-0', namespace: 'boss', claim: 'pgdata-postgres-0', requested: verdict === 'drift' ? '20Gi' : '30Gi',
        capacity_bytes: 31526436864, free_bytes: 11408822272, floor_bytes: 6305287373, tight: false,
        ...(verdict === 'legacy' ? {} : { capacity_intent: { verdict, desired_bytes: 32212254720, requested_bytes: verdict === 'drift' ? 21474836480 : 32212254720, assignment, reason: null } }),
      }],
    } }], total: 1 }),
  }));
}

test('PVC declared request match is displayed with assignment while filesystem overhead remains independent', async ({ page }) => {
  await install(page, 'match');
  await mountPage(page, '/it/estate');
  const row = page.locator('[data-volume="boss/pgdata-postgres-0"]');
  await expect(row).toHaveCount(1);
  await expect(row.locator('[data-capacity-intent="match"]')).toContainText('desired 32212254720 bytes · requested 30Gi');
  await expect(row).toHaveAttribute('data-state', 'ok');
  await expect(row.getByRole('link', { name: 'assignment Q1' })).toHaveAttribute('href', '/ux/jobs/32e7cf87-7d8e-4764-94a3-3b0cfa8548c4');
});

test('PVC drift stays explicit and legacy comparison has unknown intent rather than an inferred target', async ({ page }) => {
  await install(page, 'drift');
  await mountPage(page, '/it/estate');
  await expect(page.locator('[data-capacity-intent="drift"]')).toContainText('requested 20Gi (21474836480 bytes)');
  await page.unroute(/\/api\/estate\/comparisons\?scope=instance-volumes&/);
  await install(page, 'legacy');
  await page.reload();
  await expect(page.locator('[data-capacity-intent="unknown"]')).toContainText('no declared capacity comparison recorded');
  await expect(page.getByRole('link', { name: 'assignment Q1' })).toHaveCount(0);
});
