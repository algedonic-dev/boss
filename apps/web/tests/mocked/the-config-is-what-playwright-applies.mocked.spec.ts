// WHAT playwright.mocked.config.ts STATES IS WHAT PLAYWRIGHT APPLIES
// (backlog ae162cb4).
//
// apps/web/bunfig.toml carried `[test] timeout = 30000` for seventeen
// days while bun 1.3.14 ignored the key — the file was read (its
// preload ran), the budget was not, and every unit file ran at 5 000 ms.
// A key that is read and ignored is a check that is not running. The
// pin that sat beside it, scripts/the-mocked-suite-states-its-budget.test.ts,
// reads the config OBJECT: it proves a number is stated, and would pass
// a number stated under a key the runner never consults.
//
// So this reads each budget back from INSIDE a running test, where the
// runner has already resolved it: the per-test timeout off testInfo, the
// viewport off the page, the use-block off the project Playwright built,
// the worker count off the config Playwright built — and the expect
// budget by its effect, a poll that must outlive Playwright's 5 000 ms
// default to pass at all.

import config from '../../playwright.mocked.config';
import { expect, test } from './_test';

/** Playwright's own expect default, the number an ignored
 *  `expect.timeout` would fall back to. */
const PLAYWRIGHT_DEFAULT_EXPECT_MS = 5_000;

test('the per-test timeout is the one the config states', () => {
  expect(test.info().timeout).toBe(config.timeout);
});

test('the use block reaches the page: budgets, origin, viewport, browser', async ({ page, browserName }) => {
  const use = test.info().project.use;
  // Numbers, not merely equal: an unstated key is undefined on both sides.
  expect(typeof use.actionTimeout).toBe('number');
  expect(typeof use.navigationTimeout).toBe('number');
  expect(use.actionTimeout).toBe(config.use?.actionTimeout);
  expect(use.navigationTimeout).toBe(config.use?.navigationTimeout);
  expect(use.baseURL).toBe(config.use?.baseURL);
  expect(page.viewportSize()).toEqual(config.use?.viewport ?? null);
  expect(browserName).toBe('chromium');
});

test('the workers, retries and test directory are the ones the config states', () => {
  const info = test.info();
  expect(info.config.workers).toBe(config.workers);
  expect(info.project.retries).toBe(config.retries);
  expect(info.project.testDir.endsWith('/tests/mocked')).toBe(true);
});

test('the expect budget is applied: a poll outlives the 5 000 ms default', async () => {
  // Stated above the default, or this test cannot tell the two apart.
  expect(config.expect?.timeout ?? 0).toBeGreaterThan(PLAYWRIGHT_DEFAULT_EXPECT_MS + 1_000);
  const t0 = Date.now();
  await expect
    .poll(() => Date.now() - t0 > PLAYWRIGHT_DEFAULT_EXPECT_MS + 1_000, {
      message: `a poll with no timeout of its own gave up before ${PLAYWRIGHT_DEFAULT_EXPECT_MS + 1_000} ms — ` +
        'the config\'s expect.timeout is not the budget Playwright applied',
    })
    .toBe(true);
});
