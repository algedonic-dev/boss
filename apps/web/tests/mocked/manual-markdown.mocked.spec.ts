import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

async function readBody(page: Page, body: string): Promise<void> {
  await installSmokeMocks(page);
  const section = {
    id: 'ms-welcome', slug: 'welcome', parent_slug: null, sort_order: 0,
    title: 'Welcome', body, audience: { all: true }, current_version: 1,
    published: true, created_at: '2026-09-16T12:00:00Z', updated_at: '2026-09-16T12:00:00Z',
  };
  await page.route(/\/api\/content\/manual$/, (r) => r.fulfill({ json: [section] }));
  await page.route(/\/api\/content\/manual\/welcome$/, (r) => r.fulfill({ json: section }));
  await page.route(/\/api\/people\/emp-001$/, (r) => r.fulfill({ json: { id: 'emp-001', name: 'Rhea <HR>' } }));
  await mountPage(page, '/ux/manual/welcome', { titleMatch: /Company manual/ });
}

test('the authored seed shape and ordinary markdown render as semantic content', async ({ page }) => {
  await readBody(page, '# Policy\n\n_TBD, owner: HR._\n\n**Important**: ask emp-001.\n\n- First\n- Second\n\n> Remember\n\n`literal_underscore`');
  const body = page.locator('.manual-article-body');
  await expect(body.getByRole('heading', { name: 'Policy' })).toBeVisible();
  await expect(body.locator('em')).toHaveText('TBD, owner: HR.');
  await expect(body.locator('strong')).toHaveText('Important');
  await expect(body.locator('li')).toHaveCount(2);
  await expect(body.locator('blockquote')).toHaveText('Remember');
  await expect(body.locator('code')).toHaveText('literal_underscore');
  await expect(body.getByRole('link', { name: 'Rhea <HR>' })).toHaveAttribute('href', '/ux/people/emp-001');
});

test('hostile prose and labels stay text; code and authored links do not become nested entity links', async ({ page }) => {
  await readBody(page, '<img src=x onerror="window.manualInjected=true">\n\n[x](javascript:alert(1))\n\n`emp-001`\n\n[emp-001](/ux/people/emp-001)\n\nAsk emp-001.');
  const body = page.locator('.manual-article-body');
  await expect(body.locator('img, script')).toHaveCount(0);
  await expect(body.locator('a[href^="javascript:"]')).toHaveCount(0);
  await expect(body.locator('code')).toHaveText('emp-001');
  await expect(body.locator('code a, a a')).toHaveCount(0);
  await expect(body.locator('a')).toHaveCount(2);
  await expect(body.getByRole('link', { name: 'Rhea <HR>' })).toBeVisible();
  expect(await page.evaluate(() => Reflect.get(window, 'manualInjected'))).toBeUndefined();
});

test('plain bodies retain readable text and shortcode navigation', async ({ page }) => {
  await readBody(page, 'Questions go to emp-001.');
  const link = page.locator('.manual-article-body').getByRole('link', { name: 'Rhea <HR>' });
  await link.click();
  await expect(page).toHaveURL(/\/ux\/people\/emp-001$/);
  await page.goBack();
  await expect(page.locator('.manual-article-body')).toContainText('Questions go to');
  await expect(page.locator('.manual-article-body').getByRole('link', { name: 'Rhea <HR>' })).toBeVisible();
});
