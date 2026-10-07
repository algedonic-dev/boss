import { readFileSync } from 'node:fs';
import { expect, test } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG['system-estate'].path;
const declaration = readFileSync(new URL('../../../../infra/estate/dev-door.json', import.meta.url), 'utf8');
const mount = async (page: import('@playwright/test').Page) => {
  await installSmokeMocks(page);
  await mountPage(page, PATH, { titleMatch: /The estate/ });
};

test('the real packaged dev-server default declares none without a fixture response', async ({ page }) => {
  await mount(page);
  await expect(page.locator('.estate-door')).toContainText('This install declares no dev door.');
  await expect(page.locator('pre.estate-snippet')).toHaveCount(0);
  await expect(page.locator('.estate-door')).not.toContainText('dev.algedonic.dev');
});

for (const [label, body] of [['current', declaration], ['second', declaration.replaceAll('dev.algedonic.dev', 'workspace.example.test')]] as const) {
  test(`${label} deployment serves its own three commands on a phone`, async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.route('**/instance-config/dev-door.json', (route) => route.fulfill({ contentType: 'application/json', body }));
    await mount(page);
    const host = label === 'current' ? 'dev.algedonic.dev' : 'workspace.example.test';
    await expect(page.locator('pre.estate-snippet')).toHaveCount(3);
    await expect(page.locator('pre.estate-snippet').last()).toHaveText(`ssh root@${host}`);
    if (label === 'second') await expect(page.locator('.estate-door')).not.toContainText('dev.algedonic.dev');
  });
}

for (const [label, body, status] of [['unread', '{}', 503], ['malformed', '{bad', 200], ['incomplete', '{"host":"workspace.example.test"}', 200]] as const) {
  test(`${label} declaration is a failed read without setup commands`, async ({ page }) => {
    await page.route('**/instance-config/dev-door.json', (route) => route.fulfill({ contentType: 'application/json', body, status }));
    await mount(page);
    await expect(page.locator('.estate-door .estate-fail')).toBeVisible();
    await expect(page.locator('pre.estate-snippet')).toHaveCount(0);
    await expect(page.locator('.estate-door')).not.toContainText('This install declares no dev door.');
  });
}

test('the existing refresh reads the declaration again and removes obsolete commands', async ({ page }) => {
  await page.clock.install();
  let body = declaration;
  await page.route('**/instance-config/dev-door.json', (route) => route.fulfill({ contentType: 'application/json', body }));
  await mount(page);
  await expect(page.locator('pre.estate-snippet')).toHaveCount(3);
  body = '{}';
  await page.clock.runFor(60_001);
  await expect(page.locator('.estate-door')).toContainText('This install declares no dev door.');
  await expect(page.locator('pre.estate-snippet')).toHaveCount(0);
});
