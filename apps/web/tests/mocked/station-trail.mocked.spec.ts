// THE TOP BAR WALKS THE STATION TREE, THE SIDEBAR IS WHAT THE STATION
// HOSTS (backlog 474e2c1c; David 2026-11-19, and 2026-11-20: "A lot of the
// options are stale departments rather than the new stations").
//
// Measured the way a reader meets it: the bar on a station page, on the
// person's own pages and on /, a deep station on a phone, an unreadable
// tree, and no department of the registry anywhere in either bar. The
// tree is the shared fixture's (the server's read, standing in), whose
// root is named `company` so nothing here can pass by naming the real one.

import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { DEPARTMENTS, STATIONS_TREE, installSmokeMocks, signInWithPolicy } from './_smokeMocks';
import { ADDRESSES } from '../fixtures/station-tree';

const bar = (page: Page) => page.locator('.perspective-tabs [data-trail]');
const stops = (page: Page) => bar(page).locator('[data-trail-stop]');
const prefixOf = (name: string): string => {
  if (ADDRESSES.kind !== 'ready') throw new Error('the fixture tree did not read');
  return ADDRESSES.stations.find((s) => s.name === name)!.prefix;
};

async function open(page: Page, path: string): Promise<void> {
  await installSmokeMocks(page);
  await signInWithPolicy(page);
  await mountPage(page, path);
  await expect(bar(page)).toHaveAttribute('data-trail-state', 'ready');
}

test.describe('the top bar: Home, then the path to the station being viewed', () => {
  test('on a station page every ancestor is a link to its address, and the station is current', async ({ page }) => {
    await open(page, '/engineering/gates');
    await expect(bar(page).locator('[data-trail-home]')).toHaveAttribute('href', '/me');
    await expect(stops(page)).toHaveText(['company', 'engineering', 'gates']);
    await expect(bar(page).locator('[data-trail-stop="engineering"]')).toHaveAttribute('href', '/engineering');
    await expect(bar(page).locator('[data-trail-stop="gates"]')).toHaveAttribute('aria-current', 'page');
    // The ancestor link goes where it says, and the bar follows.
    await bar(page).locator('[data-trail-stop="engineering"]').click();
    await expect(page).toHaveURL(/\/engineering$/);
    await expect(stops(page)).toHaveText(['company', 'engineering']);
    await expect(bar(page).locator('[data-trail-stop="engineering"]')).toHaveAttribute('aria-current', 'page');
  });

  test("on /me Home is current and the company root is one link away", async ({ page }) => {
    await open(page, '/me');
    await expect(bar(page).locator('[data-trail-home]')).toHaveAttribute('aria-current', 'page');
    await expect(stops(page)).toHaveText(['company']);
    await expect(bar(page).locator('[data-trail-stop="company"]')).not.toHaveAttribute('aria-current', 'page');
  });

  test('on / the bar is the same as on My Day, which / renders in place', async ({ page }) => {
    await open(page, '/');
    await expect(bar(page).locator('[data-trail-home]')).toHaveAttribute('aria-current', 'page');
    await expect(stops(page)).toHaveText(['company']);
  });

  test('a page a station hosts draws that station as current', async ({ page }) => {
    await open(page, '/finance/vendors');
    await expect(stops(page)).toHaveText(['company', 'finance']);
    await expect(bar(page).locator('[data-trail-stop="finance"]')).toHaveAttribute('aria-current', 'page');
  });

  test('no department of the registry is in the bar', async ({ page }) => {
    await open(page, '/engineering/registry');
    // The departments registry still answers (the smoke floor serves it);
    // nothing in the bar reads it any longer.
    expect(DEPARTMENTS.length).toBeGreaterThan(0);
    await expect(page.locator('.perspective-tabs .perspective-tab')).toHaveCount(0);
    await expect(page.locator('.perspective-tabs .perspective-more-btn')).toHaveCount(0);
    // A department that is no station of the tree has no link at all; one
    // that is a station is there only as a stop on the path.
    const stations = ADDRESSES.kind === 'ready' ? ADDRESSES.stations.map((s) => s.prefix) : [];
    const stale = DEPARTMENTS.map((d) => `/${String(d.code)}`).filter((p) => !stations.includes(p));
    expect(stale.length).toBeGreaterThan(0);
    for (const path of stale) {
      await expect(page.locator(`.perspective-tabs a[href="${path}"]`), path).toHaveCount(0);
    }
  });
});

test.describe('the middle collapses on a phone', () => {
  test('a deep station shows Home, the root, an expander and the current station; the expander lists every hidden ancestor', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, `${prefixOf('forge-ci')}`);
    await expect(bar(page).locator('[data-trail-home]')).toBeVisible();
    await expect(stops(page)).toHaveText(['company', 'forge-ci']);
    const expander = bar(page).locator('[data-trail-expander]');
    await expect(expander).toHaveAttribute('aria-expanded', 'false');
    await expander.click();
    const hidden = bar(page).locator('[data-trail-hidden] [data-trail-stop]');
    await expect(hidden).toHaveText(['engineering', 'forge']);
    await expect(hidden.nth(0)).toHaveAttribute('href', '/engineering');
    await expect(hidden.nth(1)).toHaveAttribute('href', prefixOf('forge'));
    await hidden.nth(1).click();
    await expect(page).toHaveURL(new RegExp(`${prefixOf('forge')}$`));
    await expect(bar(page).locator('[data-trail-stop="forge"]')).toHaveAttribute('aria-current', 'page');
  });

  test('a wide screen draws the same station whole', async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await open(page, `${prefixOf('forge-ci')}`);
    await expect(stops(page)).toHaveText(['company', 'engineering', 'forge', 'forge-ci']);
    await expect(bar(page).locator('[data-trail-expander]')).toHaveCount(0);
  });
});

test.describe('an unreadable tree is said, never an empty bar', () => {
  test('a failed tree read names why, in the bar and in the sidebar', async ({ page }) => {
    await installSmokeMocks(page);
    await signInWithPolicy(page);
    await page.route(STATIONS_TREE, (r) => r.fulfill({ status: 503, body: '' }));
    await mountPage(page, '/me/inbox');
    await expect(bar(page)).toHaveAttribute('data-trail-state', 'failed');
    await expect(bar(page).locator('[data-trail-failed]')).toContainText('GET /api/stations answered 503');
    // Home needs no tree: the person's own pages stay reachable.
    await expect(bar(page).locator('[data-trail-home]')).toHaveAttribute('href', '/me');
    await expect(page.locator('.shell-sidebar a[href="/me/inbox"]')).toHaveCount(1);
  });

  test("a station page's sidebar says the tree failed, without wearing the page's failure marker", async ({ page }) => {
    // `.load-failed` marks a PAGE's primary read; the chrome wore it too
    // until gate-run 45fa3237, which made two on the station floor.
    await installSmokeMocks(page);
    await signInWithPolicy(page);
    await page.route(STATIONS_TREE, (r) => r.fulfill({ status: 503, body: '' }));
    await mountPage(page, '/tree');
    await expect(page.locator('.shell-sidebar [data-sidebar-failed]')).toContainText('GET /api/stations answered 503');
    await expect(page.locator('.shell-sidebar .load-failed')).toHaveCount(0);
    await expect(page.locator('.perspective-tabs .load-failed')).toHaveCount(0);
  });
});

test.describe('the sidebar holds only what this station hosts', () => {
  test("Finance's sidebar is its hosted pages, and no department row", async ({ page }) => {
    await open(page, '/finance');
    const rows = page.locator('.shell-sidebar a.shell-nav-item');
    await expect(rows).toHaveText(['Finance', 'Vendors']);
    await expect(page.locator('.shell-sidebar a[href$="/jobs"]')).toHaveCount(0);
  });

  test("Home's sidebar is the person's own pages", async ({ page }) => {
    await open(page, '/me');
    await expect(page.locator('.shell-sidebar a.shell-nav-item')).toHaveText(['My Day', 'My schedule', 'Inbox']);
  });

  test('a station that hosts no page says so', async ({ page }) => {
    await open(page, '/engineering/gates');
    await expect(page.locator('.shell-sidebar [data-sidebar-empty]')).toBeVisible();
    await expect(page.locator('.shell-sidebar a.shell-nav-item')).toHaveCount(0);
  });
});
