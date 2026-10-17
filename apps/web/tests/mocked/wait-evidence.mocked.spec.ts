import { expect, test, type Page } from './_test';
import { installSmokeMocks, YARD_BORDERS, YARD_REGIONS } from './_smokeMocks';
import { TERRITORIES } from '../../src/it/yard/world';
import { BORDERS } from '../fixtures/yard';

const wait = {
  packet: 'synthetic-packet', owner: 'agent-reviewer', condition_owner: 'world',
  title: 'Independent review', condition: 'receipt for the exact reviewed head',
  next_check_at: '2026-10-15T14:30:00.123456789Z',
  evidence: { kind: 'review', run: 'synthetic-run', head: 'a'.repeat(40), workflow_version: 7,
    started_at: '2026-10-15T12:30:00Z' },
};

async function mocks(page: Page, state: 'attention' | 'troubled') {
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, route => route.fulfill({ json: {
    now: '2026-10-15T13:00:00Z', window_hours: 24,
    regions: TERRITORIES.map(({ name }) => ({ name, count: 1, trend: {},
      state: name === 'garage' ? state : 'clear', why: 'Recorded server judgment',
      ...(name === 'garage' ? { waits: [wait] } : {}),
    })),
  } }));
  await page.route(YARD_BORDERS, route => route.fulfill({ json: {
    now: '2026-10-15T13:00:00Z', window_hours: 24,
    borders: BORDERS.map(({ from, to }) => ({ from, to, rate: {}, machine: {}, state: 'attention',
      why: 'Recorded hold', waiting: from === 'dock' && to === 'track' ? 1 : 0,
      holds: from === 'dock' && to === 'track' ? [{ what: 'change', why: 'held for review', wait }] : [],
    })),
  } }));
}

for (const state of ['attention', 'troubled'] as const) {
  test(`qualified waits stay visible beside the ${state} station verdict`, async ({ page }) => {
    await mocks(page, state);
    await page.goto('/it?at=garage');
    const panel = page.locator('[data-map-panel][data-selection="garage"]');
    await expect(panel).toHaveAttribute('data-state', state);
    await expect(panel.locator('.panel-lamp')).toHaveClass(state === 'attention' ? /warn/ : /err/);
    const waiting = panel.locator('[data-field="waiting"]');
    await expect(waiting).toContainText(wait.owner);
    await expect(waiting).toContainText(wait.condition);
    await expect(waiting).toContainText(wait.next_check_at);
  });
}

test('opening the inline crossing shows its recorded owner and next check without writes', async ({ page }) => {
  await mocks(page, 'attention');
  const writes: string[] = [];
  page.on('request', request => {
    if (!['GET', 'HEAD', 'OPTIONS'].includes(request.method()) && new URL(request.url()).pathname.startsWith('/api/')) writes.push(request.url());
  });
  await page.goto('/it');
  await expect(page.locator('.crossing[data-crossing="dock→track"]')).toBeVisible();
  // Surface-open telemetry belongs to navigation; measure the crossing action.
  writes.length = 0;
  await page.locator('.crossing[data-crossing="dock→track"]').click();
  const crossing = page.locator('.crossing-panel');
  await expect(crossing).toContainText(wait.owner);
  await expect(crossing).toContainText(wait.next_check_at);
  expect(writes).toEqual([]);
});
