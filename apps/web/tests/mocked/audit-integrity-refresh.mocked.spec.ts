import { expect, test, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const OLD = '65dd7f6f-4c09-45f7-9065-7b4702707dcd';
const NEW = '235ae1ee-77fc-4e16-9713-be6a3c167bc6';
const packet = (id: string, second: number) => ({
  id, kind: 'maintenance-audit-integrity', partition: 'real', simulated: false,
  opened_at: `2026-10-03T03:30:0${second}Z`, status: 'open',
  steps: [{ spec_slug: 'run', status: 'active', metadata: {} }],
});
const answer = (route: Route, value: unknown) => route.fulfill({
  status: 200, contentType: 'application/json', body: JSON.stringify(value),
});

for (const newest of ['packet', 'unavailable'] as const) {
test(`a late prior refresh cannot replace the newest ${newest} result`, async ({ page }) => {
  await page.clock.install();
  await installSmokeMocks(page);
  let release: (() => void) | undefined;
  const held = new Promise<void>((resolve) => { release = resolve; });
  let reads = 0;
  await page.route(/\/api\/jobs\?/, async (route) => {
    if (new URL(route.request().url()).searchParams.get('kind') !== 'maintenance-audit-integrity') {
      return route.fallback();
    }
    reads += 1;
    if (reads === 1) {
      await held;
      return answer(route, { total: 1, data: [packet(OLD, 1)] });
    }
    if (newest === 'unavailable') return route.fulfill({ status: 503, body: 'unavailable' });
    return answer(route, { total: 2, data: [packet(OLD, 1), packet(NEW, 2)] });
  });
  // The newer read must be a complete counted population.
  await page.route(`/api/jobs/${NEW}`, (route) => answer(route, packet(NEW, 2)));
  await page.route(`/api/jobs/${OLD}`, (route) => answer(route, packet(OLD, 1)));
  try {
    await mountPage(page, '/it/operate/audit');
    await expect.poll(() => reads).toBe(1);
    await page.clock.fastForward(60_000);
    const region = page.getByRole('region', { name: 'Audit integrity' });
    const link = region.getByRole('link', { name: 'Open integrity check' });
    const assertNewest = async () => {
      if (newest === 'packet') await expect(link).toHaveAttribute('href', `/jobs/${NEW}`);
      else {
        await expect(region.getByRole('alert')).toHaveText('Integrity check unavailable: HTTP 503');
        await expect(link).toHaveCount(0);
      }
    };
    await assertNewest();
    const oldResponse = page.waitForResponse((response) => new URL(response.url()).pathname === `/api/jobs/${OLD}`);
    release?.();
    await (await oldResponse).finished();
    await page.clock.runFor(50);
    await assertNewest();
  } finally {
    release?.();
  }
});
}
