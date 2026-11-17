import { expect, type Page } from './_test';

// a4896fd2: install alone advances while HTTP or the host scheduler waits.
// Set the held instant before navigation; later time moves only by runFor.
// The lead covers the blank page's install/pause exchange, not a domain budget.
export async function holdClock(page: Page, now: Date): Promise<void> {
  if (page.url() !== 'about:blank') throw new Error('holdClock must precede navigation');
  await page.clock.install({ time: now });
  await page.clock.pauseAt(now.getTime() + 60_000);
}

// 0f1fe453: one runFor across several poll periods is not a barrier. A poll
// that skips a read still out (map-polling.ts) fires its next tick inside the
// same runFor whether or not the last answer is back through the route, and a
// held clock fires nothing after it, so a count awaited afterwards never
// grows: train 234819ee's gate waited out its budget at 2 of 3 reads. Advance
// one period per attempt until the mock has counted what the spec needs; the
// wait is the mock's progress, and more host time cannot strand it. A red
// still names the count it stopped at.
export async function advanceUntil(page: Page, periodMs: number, count: () => number, atLeast: number): Promise<void> {
  await expect.poll(async () => {
    if (count() < atLeast) await page.clock.runFor(periodMs);
    return count();
  }).toBeGreaterThanOrEqual(atLeast);
}

// Arrival is a route fact. Waiting a few milliseconds and assuming a request
// started is not a barrier. Release is idempotent so teardown cannot strand it.
export function responseBarrier(): Readonly<{
  arrived: Promise<void>;
  wait: () => Promise<void>;
  release: () => void;
}> {
  let enter: () => void = () => {};
  let release: () => void = () => {};
  const arrived = new Promise<void>(resolve => { enter = resolve; });
  const held = new Promise<void>(resolve => { release = resolve; });
  return { arrived, wait: () => { enter(); return held; }, release };
}
