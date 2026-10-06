import { expect, test, type Page } from './_test';
import { installSmokeMocks } from './_smokeMocks';

const NOW = '2026-10-02T19:30:00.000Z';
const ID = '11111111-1111-4111-8111-111111111111';
function capture(complete = true, ended = '2026-10-02T19:29:00.000Z') {
  const step = {
    spec_slug: 'capture', status: 'completed', completed_by: 'agent-codex', metadata: { snapshot: {
      capture_id: ID, namespace: '01a0f9e6-4d1c-7c81-859d-5033e0b4418a', source: 'codex.collaboration.list_agents',
      collected_from: ended, collected_to: ended, received_at: NOW, complete, result: complete ? 'captured' : 'failed',
      rows: complete ? [{ path: '/root/worker', status: 'running', native_id: null, current_task: null, observed_model: null }] : [],
    } },
  };
  return { id: ID, kind: 'runtime-roster-capture', partition: 'real', metadata: {}, steps: [step] as [typeof step] };
}
async function mount(page: Page, value: unknown, mode = 'normal') {
  await installSmokeMocks(page);
  await page.clock.install({ time: new Date(NOW) });
  await page.route(/\/api\/jobs\?kind=runtime-roster-capture&/, (route) => route.fulfill({
    status: mode === 'unreadable' ? 503 : 200, contentType: 'application/json', body: JSON.stringify({data:[value],total:1}),
  }));
  await page.route(new RegExp(`/api/jobs/${ID}$`), (route) => route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(value)}));
  await page.goto('/it?at=shop-floor');
  return page.getByRole('region',{name:'Observed native runtime roster'});
}

test('copied runtime status is shown beside unknown task and separate workflow facts', async ({ page }) => {
  const panel = await mount(page,capture());
  await expect(panel).toBeVisible();
  await expect(panel).toContainText('/root/worker · observed running · current registered task unknown');
  await expect(panel).toContainText('Five-minute expiry; no continuous collection or heartbeat');
  await expect(panel.getByRole('link',{name:'BOSS capture receipt'})).toHaveAttribute('href',`/ux/jobs/${ID}`);
  await page.clock.fastForward(5 * 60_000);
  await expect(panel).toContainText('/root/worker · stale observation');
  await expect(panel).not.toContainText('/root/worker · observed running');
});

test('failed and unreadable captures cannot render an empty healthy roster', async ({ page }) => {
  const panel = await mount(page,capture(false));
  await expect(panel).toContainText('Latest capture failed');
  await expect(panel).not.toContainText('No agents in this complete snapshot');
  await page.route(/\/api\/jobs\?kind=runtime-roster-capture&/, (route) => route.fulfill({status:503,body:'unreadable'}));
  await panel.getByRole('button',{name:'Read recorded snapshots'}).click();
  await expect(panel.getByRole('alert')).toContainText('runtime state unknown');
});

test('native path markup is text and finished never claims delivered work', async ({ page }) => {
  const v = capture();
  v.steps[0].metadata.snapshot.rows[0]!.path = '/root/<img onerror=alert(1)>';
  v.steps[0].metadata.snapshot.rows[0]!.status = 'finished';
  const panel = await mount(page,v);
  await expect(panel).toContainText('/root/<img onerror=alert(1)> · observed finished');
  await expect(panel.locator('img')).toHaveCount(0);
  await expect(panel).toContainText('A finished executor does not establish delivery or gate success');
});
