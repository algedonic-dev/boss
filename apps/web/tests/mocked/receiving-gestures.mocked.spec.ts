// b0333631: remaining inherited gestures/drafts, independent of car92's
// failure marker. A stateful in-memory backend makes the second paint
// evidence of the recorded transition, not merely a request body.
import { test, expect, type Page, type Locator } from './_test';
import { installSmokeMocks } from './_smokeMocks';
import { recordPageRequests, openedRequests } from './_helpers';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { parseRoute } from '../../src/router';

const KINDS = ['user-feedback', 'backlog-item'] as const;
const workflow = (kind: string) => ({
  kind, version: 1, status: 'active', category: 'platform', steps: [
    { title: 'opened', kind: 'trigger', ready_when: 'true', fields: [] },
    { title: 'triage', kind: 'task', title_template: 'Triage synthetic work',
      ready_when: 'steps.opened.done', fields: [{ name: 'disposition', field_type: 'build|decline', required: true }] },
    { title: 'build', kind: 'task', title_template: 'Build synthetic work',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "build"', fields: [] },
    { title: 'declined', kind: 'task', title_template: 'Decline synthetic work',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "decline"', fields: [] },
  ],
});
type FixtureStep = { id: string; title: string; spec_slug: string; kind: string; status: string;
  fields: { name: string; field_type: string; required: boolean }[]; metadata: Record<string, unknown> };
const packet = (kind: string, suffix = 'one') => ({
  id: `fixture-${kind}-${suffix}`, kind, title: `Synthetic ${kind} ${suffix}`, status: 'open',
  priority: 'standard', opened_on: '2026-01-01',
  metadata: { message: `Synthetic ${kind} ${suffix}`, route: ROUTE_CATALOG['company-map'].path },
  steps: [
    { id: `fixture-${suffix}-opened`, title: 'opened', spec_slug: 'opened', kind: 'trigger', status: 'completed', fields: [], metadata: {} },
    { id: `fixture-${suffix}-triage`, title: 'triage', spec_slug: 'triage', kind: 'task', status: 'ready',
      fields: [{ name: 'disposition', field_type: 'build|decline', required: true }],
      metadata: { authority_role: 'platform-admin', conserved: 'untouched' } },
    { id: `fixture-${suffix}-build`, title: 'build', spec_slug: 'build', kind: 'task', status: 'pending', fields: [], metadata: {} },
  ] as FixtureStep[],
});
type Packet = ReturnType<typeof packet>;
type Write = { method: string; body: Record<string, unknown> };

async function backend(page: Page, kind: string, rows: Packet[] = [packet(kind)]): Promise<Write[]> {
  await recordPageRequests(page);
  await installSmokeMocks(page);
  await page.route(/\/api\/workflows$/, (r) => r.fulfill({ json: KINDS.map(workflow) }));
  await page.route(/\/api\/workflows\/(user-feedback|backlog-item)$/, (r) =>
    r.fulfill({ json: workflow(new URL(r.request().url()).pathname.split('/').at(-1)!) }));
  await page.route(/\/api\/views\/fleet\//, (r) => r.fulfill({ json: { nodes: [] } }));
  await page.route(/\/api\/jobs\?/, (r) => {
    const data = new URL(r.request().url()).searchParams.get('kind') === kind ? rows : [];
    return r.fulfill({ json: { data, total: data.length } });
  });
  await page.route(/\/api\/jobs\/fixture-[^/?]+(\/steps)?$/, (r) => {
    const url = new URL(r.request().url());
    const found = rows.find((j) => url.pathname === `/api/jobs/${j.id}` || url.pathname === `/api/jobs/${j.id}/steps`);
    return found ? r.fulfill({ json: url.pathname.endsWith('/steps') ? found.steps : found }) : r.fallback();
  });
  const writes: Write[] = [];
  await page.route(/\/api\/jobs\/fixture-[^/]+\/steps\/fixture-[^/]+(\/metadata)?$/, (r) => {
    const method = r.request().method();
    const body = r.request().postDataJSON() as Record<string, unknown>;
    writes.push({ method, body });
    const path = new URL(r.request().url()).pathname;
    const step = rows.flatMap((j) => j.steps).find((s) => path.includes(`/steps/${s.id}`));
    if (!step) return r.fulfill({ status: 404, body: 'synthetic step absent' });
    if (method === 'PATCH') {
      for (const [k, v] of Object.entries(body)) {
        if (v === null) delete step.metadata[k]; else step.metadata[k] = v;
      }
    }
    if (method === 'PUT') {
      step.status = String(body.status);
      if (step.status === 'completed') {
        const parent = rows.find((j) => j.steps.includes(step))!;
        parent.steps[2]!.status = 'ready';
      }
    }
    return r.fulfill({ json: {} });
  });
  return writes;
}
async function board(page: Page, kind: string): Promise<Locator> {
  await page.goto('/it?at=receiving');
  const panel = page.locator(`[data-station-board="${kind === 'user-feedback' ? 'feedback' : 'backlog'}"]`);
  await expect(panel.locator('.tb-title')).toBeVisible();
  return panel;
}
async function flow(panel: Locator): Promise<void> {
  await panel.getByRole('button', { name: 'Flow', exact: true }).click();
  await expect(panel.locator('.tf-title')).toBeVisible();
}
async function noWrites(page: Page): Promise<void> {
  // App records navigation independently (surface-opens.ts). Allow
  // only that exact POST; any control/packet mutation still fails.
  expect((await openedRequests(page)).filter((r) =>
    r.method !== 'GET' && !(r.method === 'POST' && r.path === '/api/surface-opens'),
  )).toEqual([]);
}

for (const kind of KINDS) {
  test(`${kind} empty finding draft cannot save, and Cancel writes nothing`, async ({ page }) => {
    const writes = await backend(page, kind);
    const panel = await board(page, kind);
    const card = panel.locator('article');
    await card.getByRole('button', { name: 'Record finding', exact: true }).click();
    await expect(card.getByRole('button', { name: 'Save finding', exact: true })).toBeDisabled();
    await card.getByLabel('What did you find?').fill('  ');
    await expect(card.getByRole('button', { name: 'Save finding', exact: true })).toBeDisabled();
    await card.getByLabel('What did you find?').fill('Draft not recorded');
    await card.getByRole('button', { name: 'Cancel', exact: true }).click();
    await expect(card.locator('textarea')).toHaveCount(0);
    expect(writes).toEqual([]);
    await noWrites(page);
  });

  test(`${kind} saved finding is reread with provenance, and cancelling an edit keeps it`, async ({ page }) => {
    const rows = [packet(kind)];
    const writes = await backend(page, kind, rows);
    const panel = await board(page, kind);
    const card = panel.locator('article');
    await card.getByRole('button', { name: 'Record finding', exact: true }).click();
    await card.getByLabel('What did you find?').fill('  Recorded synthetic finding  ');
    await card.getByRole('button', { name: 'Save finding', exact: true }).click();
    await expect(card.locator('.tb-finding-text')).toHaveText('Recorded synthetic finding');
    await expect(card.locator('.tb-finding-by')).toContainText('found by');
    await card.getByRole('button', { name: 'Edit finding', exact: true }).click();
    await expect(card.getByLabel('What did you find?')).toHaveValue('Recorded synthetic finding');
    await card.getByLabel('What did you find?').fill('Cancelled replacement');
    await card.getByRole('button', { name: 'Cancel', exact: true }).click();
    await expect(card.locator('.tb-finding-text')).toHaveText('Recorded synthetic finding');
    expect(writes.map((w) => w.method)).toEqual(['PATCH']);
    expect(rows[0]!.steps[1]!.metadata).toMatchObject({ conserved: 'untouched', finding: 'Recorded synthetic finding' });
    expect(rows[0]!.steps[1]!.status).toBe('ready');
  });

  test(`${kind} handoff and recall repaint from conserved durable annotations, not a disposition`, async ({ page }) => {
    const rows = [packet(kind)];
    const writes = await backend(page, kind, rows);
    const panel = await board(page, kind);
    const card = panel.locator('article');
    await card.getByRole('button', { name: 'Hand to agent', exact: true }).click();
    await expect(card.locator('.tb-agent')).toContainText('With an agent');
    await card.getByRole('button', { name: 'Take back', exact: true }).click();
    await expect(card.locator('.tb-agent')).toHaveCount(0);
    await expect(card.getByRole('button', { name: 'Hand to agent', exact: true })).toBeVisible();
    expect(writes.map((w) => w.method)).toEqual(['PATCH', 'PATCH']);
    expect(writes[1]!.body).toEqual({ agent_requested_at: null, agent_requested_by: null });
    expect(rows[0]!.steps[1]!.metadata).toEqual({ authority_role: 'platform-admin', conserved: 'untouched' });
    expect(rows[0]!.steps[1]!.status).toBe('ready');
  });

  for (const close of ['Close', 'Escape', 'backdrop'] as const) {
    test(`${kind} card double-click opens the complete packet and ${close} dismisses without writes`, async ({ page }) => {
      await backend(page, kind);
      const panel = await board(page, kind);
      await panel.locator('article').dblclick();
      const modal = page.getByRole('dialog', { name: `Synthetic ${kind} one` });
      await expect(modal).toBeVisible();
      await expect(modal).toContainText('Steps');
      await expect(modal).toContainText('triage');
      await expect(modal).toContainText('Metadata');
      if (close === 'Close') await modal.getByRole('button', { name: 'Close', exact: true }).click();
      else if (close === 'Escape') await page.keyboard.press('Escape');
      // The fixed Perspective bar owns the top 44px. Use the exposed
      // backdrop below it, outside the centered packet body.
      else await page.locator('.pm-back').click({ position: { x: 8, y: 90 } });
      await expect(modal).toHaveCount(0);
      await expect(panel.locator('article')).toContainText(`Synthetic ${kind} one`);
      await noWrites(page);
    });
  }

  test(`${kind} modal opens the full recorded job and Back restores the Receiving board`, async ({ page }) => {
    await backend(page, kind);
    const panel = await board(page, kind);
    await panel.locator('article').dblclick();
    const link = page.getByRole('dialog').getByRole('link', { name: 'Open the full job', exact: true });
    await expect(link).toHaveAttribute('href', `${ROUTE_CATALOG.jobs.path}/fixture-${kind}-one`);
    await link.click();
    await expect(page).toHaveURL(new RegExp(`/ux/jobs/fixture-${kind}-one$`));
    await page.goBack();
    await expect(page).toHaveURL(/\/it\?at=receiving$/);
    await expect(panel.locator('article')).toContainText(`Synthetic ${kind} one`);
    await noWrites(page);
  });

  for (const key of ['Enter', 'Space'] as const) {
    test(`${kind} Flow ${key} on a registry edge records and reloads the chosen successor`, async ({ page }) => {
      const rows = [packet(kind)];
      const writes = await backend(page, kind, rows);
      const panel = await board(page, kind); await flow(panel);
      await panel.locator('.node').filter({ hasText: 'Triage synthetic work' }).click();
      const edge = panel.locator('.dag-edge-hit[aria-label="route: build"]');
      await edge.focus(); await edge.press(key);
      await expect(panel).toContainText('Nothing waiting at this step.');
      await panel.locator('.node').filter({ hasText: 'Build synthetic work' }).click();
      await expect(panel.locator('.tf-item')).toContainText(`Synthetic ${kind} one`);
      expect(writes).toEqual([{ method: 'PATCH', body: { disposition: 'build' } }, { method: 'PUT', body: { status: 'completed' } }]);
      expect(rows[0]!.steps[1]!.metadata).toMatchObject({ conserved: 'untouched', authority_role: 'platform-admin' });
    });
  }

  test(`${kind} Flow item keyboard toggles selection; an unselected edge explains and writes nothing`, async ({ page }) => {
    await backend(page, kind, [packet(kind), packet(kind, 'two')]);
    const panel = await board(page, kind); await flow(panel);
    await panel.locator('.node').filter({ hasText: 'Triage synthetic work' }).click();
    await expect(panel.locator('.tf-item')).toHaveCount(2);
    await expect(panel.locator('.tf-route')).toHaveCount(0);
    const item = panel.locator('.tf-item').first();
    await item.press('Space'); await expect(item).toHaveClass(/selected/);
    await expect(panel.locator('.tf-route')).toHaveCount(2);
    await item.press('Enter'); await expect(item).not.toHaveClass(/selected/);
    const edge = panel.locator('.dag-edge-hit[aria-label="route: build"]');
    await edge.press('Enter');
    await expect(panel.locator('.tf-err')).toHaveText('Select an item first — an edge routes the selected item.');
    await noWrites(page);
  });

  test(`${kind} a refused fleet enhancement retains actual local queue and counts`, async ({ page }) => {
    await backend(page, kind, [packet(kind), packet(kind, 'two')]);
    await page.route(/\/api\/views\/fleet\//, (r) => r.fulfill({ status: 503, body: 'synthetic aggregate unavailable' }));
    const panel = await board(page, kind); await flow(panel);
    const node = panel.locator('.node').filter({ hasText: 'Triage synthetic work' });
    await expect(node.locator('.node-badge')).toHaveText('2 waiting');
    await node.click();
    await expect(panel.locator('.tf-queue-n')).toHaveText('2 waiting');
    await expect(panel.locator('.tf-item')).toHaveCount(2);
    await noWrites(page);
  });

  test(`${kind} a genuine non-fork registry uses Mark done without inventing a disposition`, async ({ page }) => {
    const rows = [packet(kind)];
    const writes = await backend(page, kind, rows);
    const plain = { ...workflow(kind), steps: workflow(kind).steps.map((s) => ({ ...s, fields: [] })) };
    await page.route(/\/api\/workflows$/, (r) => r.fulfill({ json: [plain] }));
    const panel = await board(page, kind);
    await panel.getByRole('button', { name: 'Mark done', exact: true }).click();
    await expect(panel.locator('section[aria-label="Closed"]')).toContainText(`Synthetic ${kind} one`);
    expect(writes).toEqual([{ method: 'PUT', body: { status: 'completed' } }]);
    expect(rows[0]!.steps[1]!.metadata).toEqual({ authority_role: 'platform-admin', conserved: 'untouched' });
    expect(rows[0]!.status).toBe('open');
  });

  test(`${kind} a refused Mark done stays a refusal and leaves the genuine non-fork step ready`, async ({ page }) => {
    const rows = [packet(kind)];
    await backend(page, kind, rows);
    const plain = { ...workflow(kind), steps: workflow(kind).steps.map((s) => ({ ...s, fields: [] })) };
    await page.route(/\/api\/workflows$/, (r) => r.fulfill({ json: [plain] }));
    const writes: Write[] = [];
    await page.route(/\/api\/jobs\/fixture-[^/]+\/steps\/fixture-[^/]+(\/metadata)?$/, (r) => {
      writes.push({ method: r.request().method(), body: r.request().postDataJSON() });
      return r.fulfill({ status: 403, body: 'synthetic completion refused' });
    });
    const panel = await board(page, kind);
    await panel.getByRole('button', { name: 'Mark done', exact: true }).click();
    await expect(panel.locator('.tb-err.load-failed')).toContainText('synthetic completion refused');
    expect(writes).toEqual([{ method: 'PUT', body: { status: 'completed' } }]);
    expect(rows[0]!.steps[1]!.status).toBe('ready');
    expect(rows[0]!.steps[1]!.metadata).toEqual({ authority_role: 'platform-admin', conserved: 'untouched' });
  });

  test(`${kind} clicking a rendered routing edge records the same chosen successor`, async ({ page }) => {
    const rows = [packet(kind)];
    const writes = await backend(page, kind, rows);
    const panel = await board(page, kind); await flow(panel);
    await panel.locator('.node').filter({ hasText: 'Triage synthetic work' }).click();
    await panel.locator('.dag-edge-label.clickable').filter({ hasText: /^build$/ }).click();
    await expect(panel).toContainText('Nothing waiting at this step.');
    await panel.locator('.node').filter({ hasText: 'Build synthetic work' }).click();
    await expect(panel.locator('.tf-item')).toContainText(`Synthetic ${kind} one`);
    expect(writes).toEqual([{ method: 'PATCH', body: { disposition: 'build' } }, { method: 'PUT', body: { status: 'completed' } }]);
  });

  test(`${kind} older-items link says what the window omits and opens the catalogued job list`, async ({ page }) => {
    await backend(page, kind);
    await page.route(/\/api\/jobs\?/, (r) => {
      const u = new URL(r.request().url());
      return u.searchParams.get('kind') === kind && u.searchParams.get('limit') === '1'
        ? r.fulfill({ json: { data: [packet(kind)], total: 3 } }) : r.fallback();
    });
    const panel = await board(page, kind);
    const older = panel.getByRole('link', { name: '2 older items', exact: true });
    const href = await older.getAttribute('href');
    expect(href).toBeTruthy();
    const u = new URL(href!, 'http://fixture.invalid');
    expect(parseRoute(u.pathname, u.search).kind).toBe(parseRoute(ROUTE_CATALOG.jobs.path).kind);
    expect(u.searchParams.get('kind')).toBe(kind);
    expect(u.searchParams.get('status')).toBe('closed');
    await older.click();
    await expect(page).toHaveURL(new RegExp(`kind=${kind}.*status=closed`));
    await page.goBack();
    await expect(panel.locator('.tb-note')).toContainText('have closed and aged off the board.');
    await noWrites(page);
  });
}

test('Feedback recorded-page button follows the catalog and Back preserves Receiving', async ({ page }) => {
  await backend(page, 'user-feedback');
  const panel = await board(page, 'user-feedback');
  await panel.getByRole('button', { name: ROUTE_CATALOG['company-map'].path, exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`${ROUTE_CATALOG['company-map'].path}$`));
  await page.goBack();
  await expect(panel.locator('article')).toContainText('Synthetic user-feedback one');
  await noWrites(page);
});

test('Receiving section supports native keyboard navigation, close and Back', async ({ page }) => {
  await backend(page, 'user-feedback');
  await board(page, 'user-feedback');
  const crossing = page.locator('[data-section-link="receiving→marshalling"]');
  const panel = page.locator('[data-map-panel]');
  await crossing.focus(); await crossing.press('Enter');
  await expect(panel).toBeVisible();
  await expect(panel).toHaveAttribute('data-selection', 'receiving->marshalling');
  await panel.locator('a[data-close]').click();
  await expect(panel).toHaveCount(0);
  await expect(page).toHaveURL(/\/it$/);
  await page.goBack();
  await expect(panel).toHaveAttribute('data-selection', 'receiving->marshalling');
  await page.goBack();
  await expect(page).toHaveURL(/\/it\?at=receiving$/);
  await noWrites(page);
});

test('Company-map keyboard link and selection-close keep native catalog navigation and Back', async ({ page }) => {
  await backend(page, 'user-feedback');
  await board(page, 'user-feedback');
  const company = page.getByRole('link', { name: 'Company System Map', exact: true });
  await expect(company).toHaveAttribute('href', ROUTE_CATALOG['company-map'].path);
  await company.press('Enter');
  await expect(page).toHaveURL(new RegExp(`${ROUTE_CATALOG['company-map'].path}$`));
  await page.goBack();
  await page.getByRole('link', { name: 'close the receiving selection', exact: true }).click();
  await expect(page).toHaveURL(/\/it$/);
  await expect(page.locator('[data-map-panel]')).toHaveCount(0);
  await page.goBack();
  await expect(page.locator('[data-map-panel]')).toHaveAttribute('data-selection', 'receiving');
  await noWrites(page);
});

test('Feedback short card retains its complete multi-line report in the condensed modal', async ({ page }) => {
  const rows = [packet('user-feedback')];
  const full = 'A synthetic inquiry about a confusing route.\nThe second line is deliberately longer than the card summary and remains complete in the packet record. Only the full report contains this conclusion.';
  rows[0]!.metadata.message = full;
  await backend(page, 'user-feedback', rows);
  const panel = await board(page, 'user-feedback');
  const card = panel.locator('article');
  await expect(card.locator('.fb-card-msg')).toHaveText('A synthetic inquiry about a confusing route.');
  await expect(card.locator('.fb-more')).toHaveText('more…');
  await card.dblclick();
  await expect(page.getByRole('dialog').locator('.pm-message')).toHaveText(full);
  await noWrites(page);
});

test('Receiving transit key opens and its complete timing choice persists without a protocol write', async ({ page }) => {
  await backend(page, 'user-feedback');
  await page.route(/\/api\/flights\/mine(\?|$)/, (r) => r.fulfill({ json: { flights: ['it-map-transit', 'it-map-live'] } }));
  await page.route('**/api/yard/moves/stream', (r) => r.fulfill({
    contentType: 'text/event-stream',
    body: 'retry: 3600000\nevent: resync\ndata: {"seq":0,"reason":"synthetic baseline"}\n\n',
  }));
  await page.route(/\/api\/yard\/moves\?/, (r) => r.fulfill({ json: { moves: [], latest_seq: 0 } }));
  await board(page, 'user-feedback');
  const key = page.locator('section.transit details.map-key');
  await expect(key).toHaveAttribute('open', '');
  await key.locator('summary').click();
  await expect(key).not.toHaveAttribute('open', '');
  await key.locator('summary').press('Enter');
  await expect(key).toHaveAttribute('open', '');
  const choice = key.locator('[data-transit-choice]');
  for (const seconds of [6, 15, 30, 60]) {
    await choice.getByRole('radio', { name: `${seconds} s`, exact: true }).check();
    await expect(choice).toHaveAttribute('data-transit-choice', String(seconds * 1000));
    await expect(key.locator('[data-replay]')).toHaveText(`each dot is one packet that moved at the time on its label; the crossing is shown over ${seconds} s`);
  }
  await page.reload();
  await expect(page.locator('[data-transit-choice]')).toHaveAttribute('data-transit-choice', '60000');
  await expect(page.locator('[data-transit-choice] input[value="60000"]')).toBeChecked();
  await noWrites(page);
});
