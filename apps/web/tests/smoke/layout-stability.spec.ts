// Layout-stability smoke — proves that lazy-loaded content doesn't
// push existing click targets around. The scenario that matters:
// open a JobDetailPage with plugin-backed steps, snapshot the y-
// coordinate of step #2 before the React runtime + plugins hydrate,
// wait for hydration, snapshot again. Positions must be stable
// within a few pixels (sub-pixel rounding is the only legitimate
// delta).
//
// The fix this guards: StepPluginMount reserves `min-height: 300px`
// via .step-plugin-mount, and React/ReactDOM load from their own
// lazy chunks only when a plugin is actually mounted.

import { test, expect, type Page } from '@playwright/test';

async function firstJobHref(page: Page, listUrl: string): Promise<string> {
  await page.goto(listUrl);
  const firstRow = page.locator('table tbody tr').first();
  await expect(firstRow).toBeVisible({ timeout: 10_000 });
  const href = await firstRow.locator('a').first().getAttribute('href');
  if (!href) throw new Error(`no job href on ${listUrl}`);
  return href;
}

test.describe('Layout stability', () => {
  test('plugin-backed steps do not push siblings down when hydrating', async ({
    page,
  }) => {
    // Slow the React runtime chunk so the skeleton → hydrated
    // transition is observable (and testable). Without this, the
    // chunk hits cache and races with the first layout read.
    await page.route('**/chunk-*.js', async (route) => {
      const url = route.request().url();
      // Delay anything that looks like a lazy chunk (sqmydbxd /
      // cx91jj5g from the current build). The main chunk is
      // already loaded by the time the route handler is attached
      // on navigation.
      if (/\/chunk-[a-z0-9]{8}\.js$/.test(url)) {
        await new Promise((r) => setTimeout(r, 250));
      }
      await route.continue();
    });

    // Also stall plugin script loads so the skeleton is visible
    // for a measurable window.
    await page.route('**/plugins/**', async (route) => {
      await new Promise((r) => setTimeout(r, 400));
      await route.continue();
    });

    const href = await firstJobHref(page, '/service');
    await page.goto(href);

    // Wait for at least one step-surface card (any flavour).
    const firstStep = page.locator('.step-surface').first();
    await expect(firstStep).toBeVisible({ timeout: 10_000 });

    // Count the steps; need at least 2 to measure sibling drift.
    const stepCount = await page.locator('.step-surface').count();
    if (stepCount < 2) {
      test.skip(true, 'service job has only one step — nothing to measure');
    }

    const second = page.locator('.step-surface').nth(1);
    const before = await second.boundingBox();
    expect(before).not.toBeNull();

    // Wait for all skeletons to disappear (plugin mounts hydrated).
    await expect(page.locator('.step-plugin-skeleton')).toHaveCount(0, {
      timeout: 15_000,
    });

    const after = await second.boundingBox();
    expect(after).not.toBeNull();

    // Allow 4px of slop for sub-pixel rounding / font metrics. A
    // real shift (plugin hydrates into 500px from a 40px loading
    // stub) would be hundreds of px.
    const dy = Math.abs((after!.y) - (before!.y));
    expect(dy).toBeLessThan(4);
  });

  test('plugin mount reserves min-height before hydration', async ({ page }) => {
    // Stall plugin script so the skeleton is guaranteed to be on
    // screen when we measure.
    await page.route('**/plugins/**', async (route) => {
      await new Promise((r) => setTimeout(r, 500));
      await route.continue();
    });

    const href = await firstJobHref(page, '/service');
    await page.goto(href);

    const skeleton = page.locator('.step-plugin-skeleton').first();
    await expect(skeleton).toBeVisible({ timeout: 10_000 });

    // The outer .step-plugin-mount reserves min-height:300px.
    const mount = page.locator('.step-plugin-mount').first();
    const box = await mount.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.height).toBeGreaterThanOrEqual(300);
  });
});
