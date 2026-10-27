import { expect, test } from './_test';
import { installSmokeMocks } from './_smokeMocks';

test('the transit map has no exit ramps and scrolls away to give the shop floor room', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 1000 });
  await installSmokeMocks(page);
  await page.route(/\/api\/flights\/mine(\?|$)/, (route) => route.fulfill({
    status: 200, contentType: 'application/json', body: JSON.stringify({ flights: ['it-map-transit'] }),
  }));
  await page.goto('/it');
  const map = page.locator('section.transit[data-transit]');
  await expect(map).toBeVisible();
  await expect(map.locator('[data-ramp="exit"], [data-exits-band], [data-key-exit]')).toHaveCount(0);
  await expect(page.locator('[data-map-pin]')).toHaveAttribute('data-map-pin', 'scrolls');
  await map.locator('[data-station="shop-floor"]').click();
  await expect(page).toHaveURL(/at=shop-floor/);
  await expect(page.locator('[data-map-panel]')).toHaveAttribute('data-selection', 'shop-floor');
  await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
  await expect.poll(async () => map.evaluate((el) => el.getBoundingClientRect().bottom)).toBeLessThanOrEqual(44);
  expect(await page.evaluate(() => document.documentElement.style.scrollPaddingTop)).toBe('');
  await page.evaluate(() => window.scrollTo(0, 0));
  await expect(map).toBeInViewport();
});
