import { isPageWrite } from './_smokeMocks';
import { expect, test } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

test('empty plugin registry names the declared delivery and offers a keyboard-readable reference', async ({page}) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\/step-plugins$/, (r) => r.fulfill({
    status: 200, contentType: 'application/json', body: '[]',
  }));
  await page.route(/\/api\/workflows$/, (r) => r.fulfill({
    status: 200, contentType: 'application/json', body: '[]',
  }));
  const writes: string[] = [];
  page.on('request', (r) => {
    const path = new URL(r.url()).pathname;
    if (isPageWrite(r.method(), path)) {
      writes.push(`${r.method()} ${path}`);
    }
  });
  await mountPage(page, '/it/registry/step-plugins', {titleMatch: /Step UX plugins/});
  const empty = page.locator('.catalog p.empty');
  await expect(empty).toContainText('infra/platform/step-plugins/');
  await expect(empty).toContainText('boss-platform-workflow-seed');
  await expect(empty).toContainText('Land the car');
  await expect(empty).not.toContainText('POST /api/jobs/step-plugins');
  const reference = empty.getByRole('link', {name: 'infra/step-plugins/README.md', exact: true});
  await expect(reference).toHaveAttribute('href', 'https://github.com/algedonic-dev/boss/blob/main/infra/step-plugins/README.md');
  await reference.focus();
  await expect(reference).toBeFocused();
  await expect(reference).toHaveAttribute('target', '_blank');
  await expect(reference).toHaveAttribute('rel', 'noopener');
  expect(writes).toEqual([]);
});

test('the rendered public landing link keeps the declared source destination', async ({page}) => {
  await installSmokeMocks(page);
  await page.goto('/system-model');
  const source = page.getByRole('link', {name: 'Source on GitHub', exact: true});
  await expect(source).toHaveAttribute('href', 'https://github.com/algedonic-dev/boss');
  await expect(source).toHaveAttribute('data-claim', 'source.repo');
  await expect(source).toHaveAttribute('target', '_blank');
  await expect(source).toHaveAttribute('rel', 'noopener');
});
