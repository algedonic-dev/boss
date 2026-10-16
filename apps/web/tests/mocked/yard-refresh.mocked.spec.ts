import { expect, test } from './_test';
import { installSmokeMocks } from './_smokeMocks';
import { holdClock, responseBarrier } from './_clock';

const NOW = new Date('2026-10-15T01:00:00Z');

const trains = (closed: boolean) => Array.from({ length: 5 }, (_, i) => ({
  id: `refresh-train-${i}`, kind: 'pr-train', title: `Refresh train ${i}`, status: closed ? 'closed' : 'open',
  opened_on: '2026-10-14', metadata: { outcome: closed ? 'arrived' : undefined, cars: [] },
  steps: ['pr', 'ci', 'merged', 'deployed', 'converged', 'arrived'].map(slug => ({
    spec_slug: slug, title: slug, status: closed || !['converged', 'arrived'].includes(slug) ? 'completed' : 'pending', metadata: {},
  })),
}));

test('consistently slow readings appear while each next poll is still pending', async ({ page }) => {
  await installSmokeMocks(page);
  await holdClock(page, NOW);
  let calls = 0;
  const releases: (() => void)[] = [];
  const release = (index: number) => {
    const pending = releases[index];
    if (!pending) throw new Error(`No pending yard read ${index}`);
    pending();
  };
  await page.route('**/api/jobs?**', async route => {
    const url = new URL(route.request().url());
    if (url.searchParams.get('kind') !== 'pr-train' || url.searchParams.get('limit') !== '40') return route.fallback();
    const call = ++calls;
    await new Promise<void>(resolve => { releases.push(resolve); });
    return route.fulfill({ json: { data: trains(call > 1), total: 5 } });
  });
  try {
    await page.goto('/it?at=track');
    await expect.poll(() => calls).toBe(1);
    await page.clock.runFor(11_000);
    await expect.poll(() => calls).toBe(2);
    release(0);
    await expect(page.locator('.yard-trainname')).toHaveCount(5);
    await page.clock.runFor(10_000);
    await expect.poll(() => calls).toBe(3);
    release(1);
    await expect(page.getByText('Nothing in transit.', { exact: true })).toBeVisible();
    await page.clock.runFor(10_000);
    await expect.poll(() => calls).toBe(4);
    const thirdResponse = page.waitForResponse(response => {
      const url = new URL(response.url());
      return url.searchParams.get('kind') === 'pr-train' && url.searchParams.get('limit') === '40';
    });
    release(2);
    await (await thirdResponse).json();
    await expect(page.locator('.yard-trainname')).toHaveCount(0);
  } finally {
    for (const release of releases) release();
  }
});

test('a failed refresh marks five retained trains with the last successful age, then recovers', async ({ page }) => {
  await installSmokeMocks(page);
  await holdClock(page, NOW);
  let failed = false;
  let closed = false;
  const response = responseBarrier();
  await page.route('**/api/jobs?**', async route => {
    const url = new URL(route.request().url());
    if (url.searchParams.get('kind') === 'pr-train' && url.searchParams.get('limit') === '40') {
      return route.fulfill({ json: { data: trains(closed), total: 5 } });
    }
    if (url.searchParams.get('kind') === 'ship-a-change' && failed) {
      await response.wait();
      return route.fulfill({ status: 403, body: 'synthetic denied car read' });
    }
    return route.fallback();
  });
  try {
    await page.goto('/it?at=track');
    await expect(page.locator('.yard-trainname')).toHaveCount(5);
    await expect(holdClock(page, NOW)).rejects.toThrow('holdClock must precede navigation');
    closed = true;
    failed = true;
    await page.clock.runFor(11_000);
    await response.arrived;
    const beforeDelay = await page.evaluate(() => Date.now());
    // Deliberately delay an observed response; this is an adverse scheduling
    // input, not a guessed wait for completion. The route supplies arrival.
    await new Promise(resolve => setTimeout(resolve, 3_000));
    expect(await page.evaluate(() => Date.now())).toBe(beforeDelay);
    response.release();
    const warning = page.locator('[data-yard-read="failed"]');
    await expect(warning).toContainText('HTTP 403');
    await expect(warning).toContainText(/Last successful reading: 1[01]s ago/);
    await expect(page.locator('.yard-trainname')).toHaveCount(5);
    failed = false;
    await page.clock.runFor(10_000);
    await expect(warning).toHaveCount(0);
    await expect(page.getByText('Nothing in transit.', { exact: true })).toBeVisible();
    await expect(page.locator('.yard-trainname')).toHaveCount(0);
  } finally {
    response.release();
  }
});

test('an older slow refresh cannot restore trains after a newer arrival reading', async ({ page }) => {
  await installSmokeMocks(page);
  await holdClock(page, NOW);
  let calls = 0;
  let release: () => void = () => {};
  const held = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/api/jobs?**', async route => {
    const url = new URL(route.request().url());
    if (url.searchParams.get('kind') !== 'pr-train' || url.searchParams.get('limit') !== '40') return route.fallback();
    const call = ++calls;
    const captured = trains(call >= 3);
    if (call === 2) await held;
    return route.fulfill({ json: { data: captured, total: 5 } });
  });
  await page.goto('/it?at=track');
  await expect(page.locator('.yard-trainname')).toHaveCount(5);
  await page.clock.runFor(10_000);
  await expect.poll(() => calls).toBe(2);
  await page.clock.runFor(10_000);
  await expect(page.getByText('Nothing in transit.', { exact: true })).toBeVisible();
  const olderResponse = page.waitForResponse(response => {
    const url = new URL(response.url());
    return url.searchParams.get('kind') === 'pr-train' && url.searchParams.get('limit') === '40';
  });
  release();
  await (await olderResponse).json();
  await page.clock.runFor(100);
  await expect(page.locator('.yard-trainname')).toHaveCount(0);
  await expect(page.locator('[data-yard-read="failed"]')).toHaveCount(0);
});
