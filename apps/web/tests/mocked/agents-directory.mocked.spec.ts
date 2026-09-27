// /it/registry/agents — the agents registry as a directory (backlog
// 62988516; David 2026-09-26: agents get a page in IT, never the People
// roster).
//
// The crawls cover the empty registry (the mock floor's `[]`) and the
// outage. This spec draws the ONE live row as the system of record served
// it on 2026-09-27, and pins what a reader of it must be able to do:
// read every registry field, see a cap the registry does not declare as
// "not declared" rather than a zero, open each finished run's own
// packet, and be told when one card's read failed without the directory
// going blank.

import { expect, test, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const PAGE = '/it/registry/agents';
const RUN = '7ab302e1-d820-484f-95ae-5925cf1d7575';
const BUILT = '94cd0c23-0ceb-408a-8d41-83bef395d003';

const json = (r: Route, body: unknown): Promise<void> =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });

async function install(
  page: import('@playwright/test').Page,
  opts: Readonly<{ runsFail?: boolean }> = {},
): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/agents(\?.*)?$/, (r) =>
    json(r, {
      data: [
        {
          aliases: ['claude@algedonic.dev'],
          default_model: 'opus-5[1m]',
          department: 'engineering',
          display_name: 'Claude (engineering)',
          hourly_budget_usd_micros: 40000000,
          id: 'agent-claude',
          max_concurrent_runs: null,
          role: 'engineering-agent',
        },
      ],
      total: 1,
    }),
  );
  await page.route(/\/api\/agent-runs\?/, (r) =>
    opts.runsFail
      ? r.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"down"}' })
      : json(r, [
          {
            run_id: RUN,
            actor_id: 'agent-claude',
            model: 'opus-5-5',
            outcome: 'success',
            started_at: '2026-09-27T07:50:47Z',
            finished_at: '2026-09-27T09:05:57Z',
            usd_micros: 5630399,
            job_id: BUILT,
            branch: 'fix/a-busy-scratch-target-is-not-freed-at-green',
            detail: { effort: 'high' },
          },
        ]),
  );
  await page.route(/\/api\/jobs\/assignments\?/, (r) =>
    json(r, {
      data: r.request().url().includes('assignee_id=agent-claude')
        ? [{ step: { id: 's1' } }, { step: { id: 's2' } }]
        : [{ step: { id: 's3' } }],
    }),
  );
}

test.describe('/it/registry/agents', () => {
  test('draws every registry field, an undeclared cap in words, and the held steps under every alias', async ({ page }) => {
    await install(page);
    await mountPage(page, PAGE, { titleMatch: /Agents/ });

    const card = page.locator('section.ag-card');
    await expect(card).toHaveCount(1);
    await expect(card.locator('h2')).toContainText('Claude (engineering)');
    await expect(card.locator('h2')).toContainText('agent-claude');
    const facts = card.locator('dl.ag-facts');
    await expect(facts).toContainText('claude@algedonic.dev');
    await expect(facts).toContainText('engineering-agent');
    await expect(facts).toContainText('opus-5[1m]');
    await expect(facts).toContainText('$40.00 an hour');
    // NULL in the registry is not a zero.
    await expect(facts.locator('dt:has-text("Concurrent runs") + dd')).toHaveText('not declared');
    // Two steps under the id, one under the login: three.
    await expect(facts.locator('dt:has-text("Open steps held") + dd')).toHaveText('3');
    await expect(facts.locator('dt:has-text("Effort in these runs") + dd')).toHaveText('high ×1');
    await expect(page.locator('.load-failed')).toHaveCount(0);
  });

  test('each finished run opens its own packet, and the work it built opens that one', async ({ page }) => {
    await install(page);
    await mountPage(page, PAGE, { titleMatch: /Agents/ });

    const row = page.locator('table.ag-table tbody tr');
    await expect(row).toHaveCount(1);
    await expect(row.locator('a').first()).toHaveAttribute('href', `/ux/jobs/${RUN}`);
    await expect(row.locator('a').last()).toHaveAttribute('href', `/ux/jobs/${BUILT}`);
    await expect(row.locator('a').last()).toHaveText('fix/a-busy-scratch-target-is-not-freed-at-green');
    await expect(row).toContainText('$5.63');
  });

  test('a failed run read is said on its card, and the directory still stands', async ({ page }) => {
    await install(page, { runsFail: true });
    await mountPage(page, PAGE, { titleMatch: /Agents/ });

    await expect(page.locator('section.ag-card h2')).toContainText('Claude (engineering)');
    await expect(page.locator('section.ag-card .load-failed')).toContainText('agent-run record did not answer');
    await expect(page.locator('table.ag-table')).toHaveCount(0);
  });
});
