// The chrome bar is the one piece of furniture every app shares, and
// it was rendered from three call sites that repeated their props by
// hand. They drifted twice: the step-focus bar shipped without
// `searchAppKinds`, so global search lost its app scoping on exactly
// the surface built for focused reading, and the Simulator's bar
// carried a hardcoded brand that a second tenant would have rendered
// as someone else's name.
//
// These pin the property the user actually notices: the same bar,
// with the same tenant identity, on every surface — including the two
// that render outside AppShell.

import { test, expect } from '@playwright/test';
import { mountPage } from '../smoke/_helpers';

/// Surfaces that render the chrome through different code paths:
/// a normal AppShell route, the full-page step route (rendered
/// OUTSIDE AppShell), and an IT surface.
const SURFACES = ['/ux/jobs', '/system', '/ux/views'] as const;

const MANIFEST = {
  display_name: 'Algedonic Ales',
  tenant_id: 'brewery',
  modules: {},
  labels: {},
};

test.describe('chrome bar', () => {
  test.beforeEach(async ({ page }) => {
    // `mountPage` does not install the smoke-mock backend, so the
    // manifest is mocked here — the brand comes from it now.
    await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  });

  for (const path of SURFACES) {
    test(`shows the tenant's own name on ${path}`, async ({ page }) => {
      await mountPage(page, path);
      // The brand is split into wordmark + suffix, so assert on the
      // bar's text rather than a single node.
      const bar = page.locator('.perspective-tabs').first();
      await expect(bar).toContainText('Algedonic');
      await expect(bar).toContainText('Ales');
    });
  }

  test('offers the same app tabs everywhere', async ({ page }) => {
    // The tab list comes from APPS, so a surface rendering a different
    // set would mean a second, drifted bar.
    const counts: number[] = [];
    for (const path of SURFACES) {
      await mountPage(page, path);
      counts.push(await page.locator('.perspective-tabs a[href]').count());
    }
    expect(new Set(counts).size, `tab counts differed across surfaces: ${counts}`).toBe(
      1,
    );
    expect(counts[0]).toBeGreaterThan(4);
  });

  test('falls back to BOSS when the tenant has not named itself', async ({ page }) => {
    // A deployment with no [meta] in tenant.toml should read "BOSS",
    // not blank and not a brewery's name.
    await page.route(/\/api\/tenant\/manifest$/, (r) =>
      r.fulfill({ json: { modules: {}, labels: {} } }),
    );
    await mountPage(page, '/ux/jobs');
    const bar = page.locator('.perspective-tabs').first();
    await expect(bar).toContainText('BOSS');
    await expect(bar).not.toContainText('Algedonic');
  });
});
