import { test, expect } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks, signInWithPolicy } from './_smokeMocks';

for (const path of ['/ux/views', '/views']) {
  test(`retired Views address ${path} remains explicitly unanswered`, async ({ page }) => {
    await installSmokeMocks(page);
    await signInWithPolicy(page);
    await mountPage(page, path, { titleMatch: /No page at this address/ });
    expect(new URL(page.url()).pathname).toBe(path);
    await expect(page.locator('code').filter({ hasText: path })).toBeVisible();
    await expect(page.locator('.v-page')).toHaveCount(0);
    await expect(page.getByRole('link', { name: 'Views', exact: true })).toHaveCount(0);
  });
}

test('Home navigation retains Inbox and schedule while retiring Views', async ({ page }) => {
  await installSmokeMocks(page);
  await signInWithPolicy(page);
  await mountPage(page, '/');
  await expect(page.getByRole('link', { name: 'Views', exact: true })).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'Inbox', exact: true }).first()).toBeVisible();
  await expect(page.getByRole('link', { name: 'My schedule', exact: true }).first()).toBeVisible();
});

for (const path of ['/ux/jobs', '/ux/inbox', '/', '/ux/calendar/me']) {
  test(`remaining Home page ${path} keeps rendering`, async ({ page }) => {
    await installSmokeMocks(page);
    await signInWithPolicy(page);
    await mountPage(page, path);
    await expect(page.locator('h1').first()).toBeVisible();
    await expect(page.getByRole('heading', { name: 'No page at this address' })).toHaveCount(0);
    expect(new URL(page.url()).pathname).toBe(path);
  });
}

test('independent Bottlenecks query consumer still reads fleet and stage results', async ({ page }) => {
  await installSmokeMocks(page);
  await signInWithPolicy(page);
  let fleetReads = 0;
  let stageReads = 0;
  await page.route(/\/api\/views\/fleet\/seasonal-release$/, (route) => {
    fleetReads += 1;
    return route.fulfill({ json: { workflow_kind: 'seasonal-release', open_jobs: 0, nodes: [], as_of: '2026-10-12T00:00:00Z' } });
  });
  await page.route(/\/api\/views\/stage-durations\/seasonal-release\?days=7$/, (route) => {
    stageReads += 1;
    return route.fulfill({ json: { stages: [] } });
  });
  await mountPage(page, '/it/operate/bottlenecks?kind=seasonal-release', { titleMatch: /Bottlenecks/ });
  await expect.poll(() => fleetReads).toBeGreaterThan(0);
  await expect.poll(() => stageReads).toBeGreaterThan(0);
  await expect(page.locator('.fleet-scope')).toContainText('0 open');
});
