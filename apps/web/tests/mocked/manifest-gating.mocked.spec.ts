import { expect, test } from './_test';
import { installSmokeMocks } from './_smokeMocks';
import { mountPage } from './_helpers';

// The surface under test is any page with no `module` in the nav catalog:
// App.svelte holds only a module-gated route behind the tenant manifest.
// It was the jobs list until step 1 of design 01fa5bdf removed that list
// (review df7aeb9c); the inbox is ungated the same way and is kept.
test('an unavailable manifest does not block an ungated surface (the inbox)', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/tenant\/manifest$/, (route) => route.fulfill({
    status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'unavailable' }),
  }));
  await mountPage(page, '/ux/inbox', { titleMatch: /^Inbox$/ });
  await expect(page.getByRole('heading', { name: 'Inbox', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Retry tenant manifest' })).toHaveCount(0);
  await expect(page.locator('.module-disabled')).toHaveCount(0);
});

// The control: the same 503 does hold a module-gated surface, so the test
// above passes because the inbox is ungated, not because the failed
// manifest read never reached the app.
test('the same unavailable manifest holds a module-gated surface (Finance)', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/tenant\/manifest$/, (route) => route.fulfill({
    status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'unavailable' }),
  }));
  await mountPage(page, '/finance');
  await expect(page.getByRole('button', { name: 'Retry tenant manifest' })).toBeVisible();
});
