import { expect, test } from '@playwright/test';

import { installApiFloor } from '../mocked/_smokeMocks';

test('the browser reads the requested server tree', async ({ page }) => {
  await installApiFloor(page);
  await page.goto('/');
  const tree = await page.evaluate(async () => {
    const response = await fetch('/__boss_tree');
    return (await response.json()).tree as string;
  });
  expect(tree).toBe(process.env['BOSS_EXPECTED_TREE']);
});
