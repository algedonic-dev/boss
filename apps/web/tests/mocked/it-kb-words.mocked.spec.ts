// /it/kb — the words and the doors page audit 8cd38edd decided on
// (2026-09-25), pinned by the car that carries them. Each test names
// the backlog item its claim came from, so a later change to the page
// can find the decision it would be overturning.
//
//   839a7f0f  the eyebrow says IT, not "System Model" — and the three
//             other IT headers that still said it (/it/design's
//             fallback, the System atlas, Observability)
//   846f0b51  the breadcrumb names the page's parent, /it
//   200d474c  the jump-nav link that lands on /it/registry says Registry
//   d133ebf0  the OUT third is reachable: a link to /it/design
//   6ff3c347  §2 names CLAUDE.md's three supporting concepts, no Composite
//   b8d1b453  §3 says one example tenant
//   34717528  §4 draws the quickstart and links this instance's estate
//   4718d918  gap 1: each diagram is drawn from its Mermaid source on
//             the page, so it cannot show a service the source retired
//
// The page makes no read at all (controls inventory, 8cd38edd), so the
// smoke mocks are enough to mount it.

import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const PATH = '/it/kb';

async function open(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await mountPage(page, PATH, { titleMatch: /Knowledge Base/ });
}

const section = (page: Page, id: string) => page.locator(`section#${id}`);

test.describe('/it/kb — the words the audit decided', () => {
  test('839a7f0f: the eyebrow names the IT department', async ({ page }) => {
    await open(page);
    await expect(page.locator('.exec-eyebrow').first()).toHaveText('IT · Knowledge Base');
    await expect(page.getByText('System Model', { exact: false })).toHaveCount(0);
  });

  test('846f0b51: the breadcrumb goes to the IT world, not My Day', async ({ page }) => {
    await open(page);
    const crumb = page.getByRole('link', { name: '← The IT world' });
    await expect(crumb).toHaveAttribute('href', '/it');
    await expect(page.getByRole('link', { name: '← Home' })).toHaveCount(0);
  });

  test('200d474c + d133ebf0: the jump nav promises what each link lands on', async ({ page }) => {
    await open(page);
    const nav = page.getByRole('navigation', { name: 'Knowledge Base jump nav' });
    await expect(nav.getByRole('link', { name: 'Registry ↗' })).toHaveAttribute('href', '/it/registry');
    await expect(nav.getByRole('link', { name: 'Design decisions ↗' })).toHaveAttribute('href', '/it/design');
    await expect(nav.getByRole('link', { name: 'Workflows ↗' })).toHaveCount(0);
  });

  test('6ff3c347: §2 names the three supporting concepts CLAUDE.md names', async ({ page }) => {
    await open(page);
    const s2 = section(page, 'it-primitives');
    for (const concept of ['Class registry', 'StepPlugins', 'Policy']) {
      await expect(s2.locator('strong', { hasText: concept })).toHaveCount(1);
    }
    await expect(s2).not.toContainText('Composite');
  });

  test('b8d1b453: §3 says one example tenant', async ({ page }) => {
    await open(page);
    const s3 = section(page, 'it-service-map');
    await expect(s3).toContainText('the one example tenant');
    await expect(s3).not.toContainText('two example tenants');
  });

  test("34717528: §4 describes the quickstart and links this instance's estate", async ({ page }) => {
    await open(page);
    const s4 = section(page, 'it-deployment');
    await expect(s4).toContainText('infra/oss-quickstart');
    await expect(s4).not.toContainText('systemd');
    await expect(s4.getByRole('link', { name: "This instance's estate, live ↗" })).toHaveAttribute('href', '/it/estate');
  });

  test('4718d918: every diagram is rendered from its Mermaid source, not a committed picture', async ({ page }) => {
    await open(page);
    for (const id of ['it-framing', 'it-primitives', 'it-service-map', 'it-deployment']) {
      await expect(section(page, id).locator('.arch-diagram > svg')).toHaveCount(1);
      await expect(section(page, id).locator('.arch-diagram img')).toHaveCount(0);
    }
    // The service map's source names every port-registry row, and
    // boss-observability was retired from it (467175e7 car B) while the
    // committed renders went on drawing it.
    const map = section(page, 'it-service-map').locator('.arch-diagram > svg');
    await expect(map).toContainText('boss-gateway');
    await expect(map).not.toContainText('boss-observability');
    // The caption names the one file a reader edits to change the picture.
    await expect(section(page, 'it-service-map')).toContainText('docs/architecture/02-service-map.mmd');
  });
});

test.describe('839a7f0f: the other IT headers that still said System Model', () => {
  test('the System atlas', async ({ page }) => {
    await installSmokeMocks(page);
    await mountPage(page, '/it/operate/atlas');
    await expect(page.locator('.exec-eyebrow').first()).toHaveText('IT · System atlas');
  });

  test('Observability', async ({ page }) => {
    await installSmokeMocks(page);
    await mountPage(page, '/it/operate/perf');
    await expect(page.locator('.exec-eyebrow').first()).toHaveText('IT · Observability');
  });
});
