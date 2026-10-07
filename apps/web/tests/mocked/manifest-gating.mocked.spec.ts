import { expect, test } from './_test';
import { installSmokeMocks } from './_smokeMocks';
import { mountPage } from './_helpers';

test('an unavailable manifest does not block the ungated Jobs surface', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/tenant\/manifest$/, (route) => route.fulfill({
    status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'unavailable' }),
  }));
  await mountPage(page, '/jobs?status=all');
  await expect(page.getByRole('heading', { name: 'Filtered jobs', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Retry tenant manifest' })).toHaveCount(0);
  await expect(page.locator('.module-disabled')).toHaveCount(0);
});
