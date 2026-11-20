// /engineering/registry/agents — the agents registry as a directory (backlog
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

const PAGE = '/engineering/registry/agents';
const RUN = '7ab302e1-d820-484f-95ae-5925cf1d7575';
const BUILT = '94cd0c23-0ceb-408a-8d41-83bef395d003';

const json = (r: Route, body: unknown): Promise<void> =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });

/// How the seat door answers: `seat` is the door's compare-and-set (the
/// seats move only while they are the ones the page sent as `expected`,
/// else 409); any other answer is that status and body.
type Door = Readonly<{ status: number; body: string }> | 'seat';

async function install(
  page: import('@playwright/test').Page,
  opts: Readonly<{
    runsFail?: boolean;
    door?: Door;
    sent?: unknown[];
    /// Seats another writer moves the row to after the page has read it.
    movedTo?: ReadonlyArray<string>;
  }> = {},
): Promise<void> {
  await installSmokeMocks(page);
  // The registry's seats, moved only by a write the door applies.
  let seats: ReadonlyArray<string> = ['build'];
  const row = () => ({
    aliases: ['claude@algedonic.dev'],
    default_model: 'opus-5[1m]',
    department: 'engineering',
    display_name: 'Claude (engineering)',
    factory_rotation: true,
    hourly_budget_usd_micros: 40000000,
    id: 'agent-claude',
    max_concurrent_runs: null,
    model_class: null,
    role: 'engineering-agent',
    stations: seats,
  });
  await page.route(/\/api\/agents(\?.*)?$/, (r) => json(r, { data: [row()], total: 1 }));
  await page.route(/\/api\/agents\/agent-claude\/stations$/, (r) => {
    const asked = r.request().postDataJSON() as Readonly<{ expected: string[]; stations: string[] }>;
    opts.sent?.push({ url: r.request().url(), method: r.request().method(), body: asked });
    if (opts.movedTo) seats = opts.movedTo;
    const door = opts.door ?? 'seat';
    if (door !== 'seat') {
      return r.fulfill({ status: door.status, contentType: 'text/plain', body: door.body });
    }
    if (JSON.stringify(asked.expected) !== JSON.stringify(seats)) {
      return r.fulfill({
        status: 409,
        contentType: 'text/plain',
        body: `agent agent-claude: its seats changed since they were read — read ${JSON.stringify(asked.expected)}, the registry holds ${JSON.stringify(seats)} now; nothing was written`,
      });
    }
    const before = row();
    seats = asked.stations;
    return json(r, {
      id: 'agent-claude',
      before,
      after: row(),
      changes: [{ field: 'stations', from: before.stations, to: seats }],
    });
  });
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

test.describe('/engineering/registry/agents', () => {
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
    // The row's retired run cap is not drawn as a limit (backlog 3e927996).
    await expect(facts.locator('dt:has-text("Concurrent runs")')).toHaveCount(0);
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

  // Backlog cdbebc22: David places an agent at a station from here.
  test('seating an agent sends only its seats to the seat door and draws it before and after', async ({ page }) => {
    const sent: unknown[] = [];
    await install(page, { sent });
    await mountPage(page, PAGE, { titleMatch: /Agents/ });

    const card = page.locator('section.ag-card');
    await expect(card.locator('dt:has-text("Seats") + dd')).toHaveText('build');
    const form = card.locator('form.ag-seat');
    await expect(form.locator('input[name="stations"]')).toHaveValue('build');
    await form.locator('input[name="stations"]').fill('algedonic');
    await form.locator('button[type="submit"]').click();

    const result = card.locator('.ag-seat-result');
    await expect(result).toContainText('Before: build');
    await expect(result).toContainText('After: algedonic');
    await expect(result).toContainText('stations: ["build"] → ["algedonic"]');
    await expect(result).toContainText('jobs.agent.updated');
    await expect(card.locator('dt:has-text("Seats") + dd')).toHaveText('algedonic');
    await expect(page.locator('.load-failed')).toHaveCount(0);

    // One write, carrying the seats the card showed and the seats asked
    // for — no other field (review 1a49c3a7 finding 1).
    expect(sent).toHaveLength(1);
    const [write] = sent as ReadonlyArray<{ url: string; method: string; body: unknown }>;
    expect(write?.method).toBe('PUT');
    expect(write?.url).toMatch(/\/api\/agents\/agent-claude\/stations$/);
    expect(write?.body).toEqual({ expected: ['build'], stations: ['algedonic'] });
  });

  test('seats another writer moved since the page read them are refused, and the card shows what is held', async ({
    page,
  }) => {
    await install(page, { movedTo: ['design-review'] });
    await mountPage(page, PAGE, { titleMatch: /Agents/ });

    const card = page.locator('section.ag-card');
    await expect(card.locator('dt:has-text("Seats") + dd')).toHaveText('build');
    const form = card.locator('form.ag-seat');
    await form.locator('input[name="stations"]').fill('algedonic');
    await form.locator('button[type="submit"]').click();

    const refusal = card.locator('.ag-seat-refused');
    await expect(refusal).toContainText('Nothing changed');
    await expect(refusal).toContainText('the seats moved since this page read them');
    await expect(card.locator('.ag-seat-result')).toHaveCount(0);
    await expect(card.locator('dt:has-text("Seats") + dd')).toHaveText('design-review');
  });

  test('a seat the door refuses shows the door’s reason, and the row is unchanged', async ({ page }) => {
    const why = 'agent agent-claude: station `nowhere` is not an active station in the station registry';
    await install(page, { door: { status: 422, body: why } });
    await mountPage(page, PAGE, { titleMatch: /Agents/ });

    const card = page.locator('section.ag-card');
    const form = card.locator('form.ag-seat');
    await form.locator('input[name="stations"]').fill('nowhere');
    await form.locator('button[type="submit"]').click();

    const refusal = card.locator('.ag-seat-refused');
    await expect(refusal).toContainText('HTTP 422');
    await expect(refusal).toContainText(why);
    await expect(refusal).toContainText('Nothing changed');
    await expect(card.locator('.ag-seat-result')).toHaveCount(0);
    await expect(card.locator('dt:has-text("Seats") + dd')).toHaveText('build');
  });

  test('a session below the operator tier is told the passkey opens this write', async ({ page }) => {
    await install(page, { door: { status: 403, body: '' } });
    await mountPage(page, PAGE, { titleMatch: /Agents/ });

    const form = page.locator('form.ag-seat');
    await form.locator('input[name="stations"]').fill('algedonic');
    await form.locator('button[type="submit"]').click();

    const refusal = page.locator('.ag-seat-refused');
    await expect(refusal).toContainText('HTTP 403');
    await expect(refusal).toContainText('operator passkey');
  });

  test('a failed run read is said on its card, and the directory still stands', async ({ page }) => {
    await install(page, { runsFail: true });
    await mountPage(page, PAGE, { titleMatch: /Agents/ });

    await expect(page.locator('section.ag-card h2')).toContainText('Claude (engineering)');
    await expect(page.locator('section.ag-card .load-failed')).toContainText('agent-run record did not answer');
    await expect(page.locator('table.ag-table')).toHaveCount(0);
  });
});
