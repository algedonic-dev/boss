import { expect, test } from './_test';
import { installSmokeMocks, servePeopleRows } from './_smokeMocks';
import { mountPage } from './_helpers';

const row = (id: string, location: string | null) => ({
  id, name: id, email: `${id}@test`, role: 'platform-admin', department: 'it',
  status: 'active', location, hire_date: '2020-01-01', skills: ['skill'],
  certifications: [{ name: 'Training', expires_on: '2030-01-01' }],
});

for (const failed of [false, true]) {
  test(`location registry ${failed ? 'refusal' : 'names and unknown ids'} stays distinct from missing location`, async ({ page }) => {
    await installSmokeMocks(page);
    await servePeopleRows(page, [row('emp-known', 'loc-known'), row('emp-missing', null), row('emp-unknown', 'loc-unknown')]);
    await page.route(/\/api\/people$/, r => r.fulfill({ contentType: 'application/json', body: JSON.stringify([row('emp-known', 'loc-known'), row('emp-missing', null), row('emp-unknown', 'loc-unknown')]) }));
    await page.route(/\/api\/locations\/[^/?]+$/, r => {
      const known = r.request().url().endsWith('/loc-known');
      return r.fulfill({ status: failed ? 503 : known ? 200 : 404,
        contentType: 'application/json', body: JSON.stringify(failed ? 'unavailable' : known ? { id: 'loc-known', name: 'Recorded place' } : 'not found') });
    });
    await mountPage(page, '/ux/people');
    const cell = (id: string) => page.locator('tbody tr').filter({ hasText: id }).locator('td').nth(4);
    await expect(cell('emp-missing')).toHaveText('Unspecified');
    await expect(cell('emp-known')).toHaveText(failed ? 'Location unavailable — loc-known' : 'Recorded place');
    await expect(cell('emp-unknown')).toHaveText(failed ? 'Location unavailable — loc-unknown' : 'Unresolved location — loc-unknown');
    await expect(page.locator('thead')).not.toContainText(/Tenure|Skills/);
    await expect(page.locator('header.exec-header')).not.toContainText(/certification/i);
  });
}

test('location loading never guesses a name and malformed success becomes unavailable', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/people$/, r => r.fulfill({ contentType: 'application/json', body: JSON.stringify([row('emp-known', 'loc-known'), row('emp-missing', null)]) }));
  let release: () => void = () => {};
  const held = new Promise<void>(resolve => { release = resolve; });
  await page.route(/\/api\/locations\/loc-known$/, async r => {
    await held;
    await r.fulfill({ contentType: 'application/json', body: JSON.stringify({ id: 'loc-known', display_name: 'Guessed field' }) });
  });
  await mountPage(page, '/ux/people');
  const cell = (id: string) => page.locator('tbody tr').filter({ hasText: id }).locator('td').nth(4);
  await expect(cell('emp-known')).toHaveText('Loading location — loc-known');
  await expect(cell('emp-missing')).toHaveText('Unspecified');
  release();
  await expect(cell('emp-known')).toHaveText('Location unavailable — loc-known');
  await expect(cell('emp-missing')).toHaveText('Unspecified');
});

test('HR keeps supported headcount without unsupported summaries', async ({ page }) => {
  await installSmokeMocks(page);
  await servePeopleRows(page, [row('emp-known', 'loc-known')]);
  await page.route(/\/api\/people$/, r => r.fulfill({ contentType: 'application/json', body: JSON.stringify([row('emp-known', 'loc-known')]) }));
  await mountPage(page, '/hr');
  await expect(page.getByRole('tab')).toHaveText(['Overview', 'Workflows', 'Headcount']);
  await expect(page.getByText('Total headcount', { exact: true })).toBeVisible();
  await expect(page.locator('.catalog.theme-exec')).not.toContainText(/Avg tenure|Nothing urgent|certifications expiring/i);
});
