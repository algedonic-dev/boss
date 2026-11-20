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

// /ux/jobs left this list with the jobs list (step 1 of design 01fa5bdf).
for (const path of ['/me/inbox', '/', '/me/calendar']) {
  test(`remaining Home page ${path} keeps rendering`, async ({ page }) => {
    await installSmokeMocks(page);
    await signInWithPolicy(page);
    await mountPage(page, path);
    await expect(page.locator('h1').first()).toBeVisible();
    await expect(page.getByRole('heading', { name: 'No page at this address' })).toHaveCount(0);
    expect(new URL(page.url()).pathname).toBe(path);
  });
}

// The Bottlenecks tab, the views API's last reader of fleet and stage
// results, went with step 1 of design 01fa5bdf; its reads went with it.
