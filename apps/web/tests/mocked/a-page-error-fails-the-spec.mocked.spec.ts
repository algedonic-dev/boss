// THE GUARD IN _test.ts, PINNED BY ITS OWN EFFECT (backlog 9045a570).
//
// Each test here plants a throw in a blank page and says what the
// guard must do about it. `test.fail()` marks the tests the guard must
// FAIL: if the guard ever stops failing them, Playwright reports an
// unexpected pass and this file goes red — so the rule cannot quietly
// stop running the way the per-spec listeners it replaces could.
//
// No route of the app is opened: the guard is about the browser's
// error channel, not about any page, so the fixture is a blank document.

import { expect, test } from './_test';

const plant = (page: import('./_test').Page, how: 'throw' | 'reject', text: string) =>
  page.evaluate(
    ([h, t]) => {
      setTimeout(() => {
        if (h === 'throw') throw new TypeError(t);
        void Promise.reject(new Error(t));
      }, 0);
    },
    [how, text] as const,
  );

test('an uncaught throw fails a spec that asserts nothing about it', async ({ page, context }) => {
  test.fail(true, 'the guard must fail this test: the page threw and the spec did not declare it');
  await page.setContent('<p>blank</p>');
  const heard = context.waitForEvent('weberror');
  await plant(page, 'throw', "Cannot read properties of undefined (reading 'toLocaleString')");
  await heard;
  await expect(page.locator('p')).toHaveText('blank');
});

test('an unhandled rejection fails the spec too', async ({ page, context }) => {
  test.fail(true, 'the guard must fail this test: a rejected promise nobody handled is a page error');
  await page.setContent('<p>blank</p>');
  const heard = context.waitForEvent('weberror');
  await plant(page, 'reject', 'a planted rejection');
  await heard;
});

test.describe('a declared throw', () => {
  test.use({ expectedPageErrors: [{ name: 'the planted TypeError', pattern: /^TypeError: planted on purpose$/ }] });

  test('passes when it is the throw the spec declared', async ({ page, context }) => {
    await page.setContent('<p>blank</p>');
    const heard = context.waitForEvent('weberror');
    await plant(page, 'throw', 'planted on purpose');
    await heard;
  });

  test('still fails the spec when a DIFFERENT throw arrives beside it', async ({ page, context }) => {
    test.fail(true, 'the guard must fail this test: only the declared throw is excused');
    await page.setContent('<p>blank</p>');
    let heard = 0;
    context.on('weberror', () => void heard++);
    await plant(page, 'throw', 'planted on purpose');
    await plant(page, 'throw', 'a throw nobody declared');
    await expect.poll(() => heard).toBe(2);
  });

  test('fails the spec when it never arrives, so the declaration cannot outlive its throw', async ({ page }) => {
    test.fail(true, 'the guard must fail this test: a declared throw that never happened is a stale excuse');
    await page.setContent('<p>blank</p>');
  });
});
