import type { Page } from './_test';

// a4896fd2: install alone advances while HTTP or the host scheduler waits.
// Set the held instant before navigation; later time moves only by runFor.
// The lead covers the blank page's install/pause exchange, not a domain budget.
export async function holdClock(page: Page, now: Date): Promise<void> {
  if (page.url() !== 'about:blank') throw new Error('holdClock must precede navigation');
  await page.clock.install({ time: now });
  await page.clock.pauseAt(now.getTime() + 60_000);
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
