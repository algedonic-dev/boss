// b0333631 / 6fd3dda9: Receiving includes two registry-shaped boards.
// Keep the map healthy so its failure paint cannot satisfy a Flow's
// refusal. Every write below is intercepted; no live packet is routed.
import { test, expect, type Page, type Locator } from './_test';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const KINDS = ['user-feedback', 'backlog-item'] as const;
const spec = (kind: string) => ({
  kind, version: 1, status: 'active', category: 'platform',
  steps: [
    { title: 'opened', kind: 'trigger', ready_when: 'true', fields: [] },
    { title: 'triage', kind: 'task', title_template: 'Triage the synthetic packet',
      ready_when: 'steps.opened.done', fields: [
        { name: 'disposition', field_type: 'build|decline', required: true },
      ] },
    { title: 'build', kind: 'task', title_template: 'Build the synthetic change',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "build"', fields: [] },
    { title: 'declined', kind: 'task', title_template: 'Decline the synthetic change',
      ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "decline"', fields: [] },
  ],
});
const packet = (kind: string) => ({
  id: `fixture-${kind}`, kind, title: `Synthetic ${kind} packet`, status: 'open',
  priority: 'standard', opened_on: '2026-01-01', metadata: { message: `Synthetic ${kind} packet` },
  steps: [
    { id: 'fixture-opened', spec_slug: 'opened', title: 'opened', kind: 'trigger', status: 'completed', metadata: {} },
    { id: 'fixture-triage', spec_slug: 'triage', title: 'triage', kind: 'task', status: 'ready',
      fields: [{ name: 'disposition', field_type: 'build|decline', required: true }],
      metadata: { authority_role: 'platform-admin', untouched: 'conserved' } },
  ],
});

async function install(page: Page, kind: string, rows = [packet(kind)]): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/workflows$/, (r) => r.fulfill({ json: KINDS.map(spec) }));
  await page.route(/\/api\/workflows\/(user-feedback|backlog-item)$/, (r) => {
    const k = new URL(r.request().url()).pathname.split('/').at(-1)!;
    return r.fulfill({ json: spec(k) });
  });
  await page.route(/\/api\/jobs\?/, (r) => {
    const k = new URL(r.request().url()).searchParams.get('kind');
    const data = k === kind ? rows : [];
    return r.fulfill({ json: { data, total: data.length } });
  });
  await page.route(/\/api\/views\/fleet\//, (r) => r.fulfill({ json: { nodes: [] } }));
}
async function flow(page: Page, kind: string): Promise<Locator> {
  await page.goto('/it?at=receiving');
  const panel = page.locator(`[data-station-board="${kind === 'user-feedback' ? 'feedback' : 'backlog'}"]`);
  await panel.getByRole('button', { name: 'Flow', exact: true }).click();
  await expect(panel.locator('.tf-title')).toBeVisible();
  return panel;
}
async function routeBuild(panel: Locator): Promise<void> {
  await panel.locator('.node').filter({ hasText: 'Triage the synthetic packet' }).click();
  await expect(panel.locator('.tf-item')).toHaveCount(1);
  await panel.getByRole('button', { name: 'build', exact: true }).click();
}

for (const kind of KINDS) {
  test(`${kind} Flow names a workflow-only refusal at its own marker`, async ({ page }) => {
    await install(page, kind);
    await page.route(`**/api/workflows/${kind}`, (r) => r.fulfill({ status: 503, body: 'registry unavailable' }));
    const panel = await flow(page, kind);
    await expect(panel.locator('.tf-err')).toHaveText(`workflow ${kind}: HTTP 503`);
    await expect(panel.locator(FAILURE_MARKER)).toContainText(`workflow ${kind}: HTTP 503`);
    await expect(panel.locator('.tf-item')).toHaveCount(0);
    await expect(panel).not.toContainText('Nothing waiting at this step.');
  });

  test(`${kind} Flow names a jobs-only refusal while the map and workflow answer`, async ({ page }) => {
    await install(page, kind);
    await page.route(/\/api\/jobs\?/, (r) => {
      const u = new URL(r.request().url());
      return u.searchParams.get('kind') === kind && u.searchParams.get('status') === 'open'
        ? r.fulfill({ status: 503, body: 'packet read unavailable' }) : r.fallback();
    });
    const panel = await flow(page, kind);
    await expect(panel.locator('.tf-err')).toContainText('HTTP 503');
    await expect(panel.locator(FAILURE_MARKER)).toContainText('HTTP 503');
    await expect(panel.locator('.tf-item')).toHaveCount(0);
    await expect(panel).not.toContainText('Nothing waiting at this step.');
  });

  test(`${kind} Flow has an honest empty queue and returns to Board`, async ({ page }) => {
    await install(page, kind, []);
    const panel = await flow(page, kind);
    await panel.locator('.node').filter({ hasText: 'Triage the synthetic packet' }).click();
    await expect(panel).toContainText('Nothing waiting at this step.');
    await expect(panel.locator(FAILURE_MARKER)).toHaveCount(0);
    await panel.getByRole('button', { name: 'Board', exact: true }).click();
    await expect(panel.locator('.tb-title')).toBeVisible();
    await expect(panel.locator('.tf-title')).toHaveCount(0);
  });

  for (const refused of ['PATCH', 'PUT'] as const) {
    test(`${kind} Flow keeps the selected packet and shows a refused ${refused}`, async ({ page }) => {
      await install(page, kind);
      const writes: { method: string; body: unknown }[] = [];
      await page.route(/\/api\/jobs\/fixture-[^/]+\/steps\/fixture-triage(\/metadata)?$/, (r) => {
        const method = r.request().method();
        writes.push({ method, body: r.request().postDataJSON() });
        return method === refused
          ? r.fulfill({ status: 403, body: `fixture ${refused} refused` })
          : r.fulfill({ json: {} });
      });
      const panel = await flow(page, kind);
      await routeBuild(panel);
      await expect(panel.locator('.tf-err')).toContainText(`fixture ${refused} refused`);
      await expect(panel.locator(FAILURE_MARKER)).toContainText(`fixture ${refused} refused`);
      expect(writes.map((w) => w.method)).toEqual(refused === 'PATCH' ? ['PATCH'] : ['PATCH', 'PUT']);
      expect(writes[0]?.body).toEqual({ disposition: 'build' });
      if (refused === 'PUT') expect(writes[1]?.body).toEqual({ status: 'completed' });
      await expect(panel.locator('.tf-item.selected')).toContainText(`Synthetic ${kind} packet`);
      await expect(panel.getByRole('button', { name: 'build', exact: true })).toBeEnabled();
    });
  }

  test(`${kind} Flow routes only after conserved metadata and status writes, then reloads actual state`, async ({ page }) => {
    const rows = [packet(kind)];
    await install(page, kind, rows);
    const writes: { method: string; body: unknown }[] = [];
    await page.route(/\/api\/jobs\/fixture-[^/]+\/steps\/fixture-triage(\/metadata)?$/, (r) => {
      const method = r.request().method();
      const body = r.request().postDataJSON();
      writes.push({ method, body });
      const step = rows[0]!.steps[1]!;
      if (method === 'PATCH') Object.assign(step.metadata, body);
      if (method === 'PUT') step.status = 'completed';
      return r.fulfill({ json: {} });
    });
    const panel = await flow(page, kind);
    await routeBuild(panel);
    await expect(panel).toContainText('Nothing waiting at this step.');
    expect(writes).toEqual([
      { method: 'PATCH', body: { disposition: 'build' } },
      { method: 'PUT', body: { status: 'completed' } },
    ]);
    expect(rows[0]!.steps[1]!.metadata).toMatchObject({ untouched: 'conserved', authority_role: 'platform-admin', disposition: 'build' });
    await expect(panel.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  for (const action of ['route', 'handoff', 'recall', 'finding'] as const) {
    test(`${kind} Board shows a refused ${action} beside its unchanged packet`, async ({ page }) => {
      const row = packet(kind);
      if (action === 'recall') Object.assign(row.steps[1]!.metadata, {
        agent_requested_at: '2026-01-01T00:00:00Z', agent_requested_by: 'fixture-actor',
      });
      await install(page, kind, [row]);
      const writes: { method: string; body: Record<string, unknown> }[] = [];
      await page.route(/\/api\/jobs\/fixture-[^/]+\/steps\/fixture-triage(\/metadata)?$/, (r) => {
        writes.push({ method: r.request().method(), body: r.request().postDataJSON() });
        return r.fulfill({ status: 403, body: `fixture ${action} refused` });
      });
      await page.goto('/it?at=receiving');
      const panel = page.locator(`[data-station-board="${kind === 'user-feedback' ? 'feedback' : 'backlog'}"]`);
      const card = panel.locator('article', { hasText: `Synthetic ${kind} packet` });
      if (action === 'route') {
        await card.getByLabel('Route this item').selectOption('build');
        await card.getByRole('button', { name: 'Route', exact: true }).click();
      } else if (action === 'handoff') {
        await card.getByRole('button', { name: 'Hand to agent', exact: true }).click();
      } else if (action === 'recall') {
        await card.getByRole('button', { name: 'Take back', exact: true }).click();
      } else {
        await card.getByRole('button', { name: 'Record finding', exact: true }).click();
        await card.getByLabel('What did you find?').fill('Synthetic refusal finding');
        await card.getByRole('button', { name: 'Save finding', exact: true }).click();
      }
      await expect(panel.locator(FAILURE_MARKER)).toContainText(`fixture ${action} refused`);
      expect(writes.map((w) => w.method)).toEqual(['PATCH']);
      const body = writes[0]!.body;
      if (action === 'route') expect(body).toEqual({ disposition: 'build' });
      if (action === 'handoff') {
        expect(body.agent_requested_at).toBeTruthy();
        expect(body.agent_requested_by).toBeTruthy();
      }
      if (action === 'recall') expect(body).toEqual({ agent_requested_at: null, agent_requested_by: null });
      if (action === 'finding') {
        expect(body.finding).toBe('Synthetic refusal finding');
        expect(body.finding_by).toBeTruthy();
        expect(body.finding_at).toBeTruthy();
      }
      expect(body.authority_role).toBeUndefined();
      expect(row.steps[1]!.status).toBe('ready');
      expect(row.steps[1]!.metadata).toMatchObject({ authority_role: 'platform-admin', untouched: 'conserved' });
    });
  }
}
