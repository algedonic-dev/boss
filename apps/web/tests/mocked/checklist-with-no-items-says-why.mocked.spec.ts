// A checklist step with no items says so, names who writes them, and
// shows the procedure that will write them — it never renders blank
// (backlog 82cd3da2, 2026-09-27).
//
// David opened the publish review step of 246d597a (kind checklist,
// ready, its agent run not yet started) on its full page and it showed
// nothing at all: the bundle drew an empty list, a 0/0 and a Save
// button, and the full page mounts the plugin without the procedure
// panel StepSurface puts above it. An empty surface must say why — the
// empty answer and the unread one look the same unless it does.
//
// The bundle mounts on a blank page with the plugin host's contract and
// nothing else, the way step-plugins-draw-page-text-as-text drives it.

import { expect, test, type Page } from './_test';
import { readFileSync } from 'fs';

async function mountChecklist(page: Page, step: unknown): Promise<string[]> {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  await page.setContent('<div id="host"></div>');
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    w['__boss_register_step_plugin'] = (kind: string, mount: unknown) => {
      w['__plugin'] = { kind, mount };
    };
  });
  await page.addScriptTag({
    content: readFileSync(new URL('../../../../infra/step-plugins/checklist.js', import.meta.url), 'utf8'),
  });
  await page.evaluate((s) => {
    const w = window as unknown as Record<string, unknown>;
    const plugin = w['__plugin'] as { mount: (c: Element, props: unknown) => unknown } | undefined;
    if (!plugin) throw new Error('the bundle registered no plugin');
    const host = document.getElementById('host');
    if (!host) throw new Error('no host');
    plugin.mount(host, { step: s, jobId: 'job-1', onUpdate() {} });
  }, step);
  return errs;
}

const PROCEDURE = 'READ EACH NEWLY PUBLIC FILE; a count is not a review.';

test('an unrun checklist an agent writes names the agent run and shows its procedure', async ({ page }) => {
  const errs = await mountChecklist(page, {
    id: 'step-review',
    kind: 'checklist',
    title: 'Review the newly-public surface',
    status: 'ready',
    assignee_id: null,
    metadata: {
      // The live metadata of 246d597a's review step, minus its items.
      agent_profile: 'analyst',
      agent_model: 'opus-5[1m]',
      agent_budget_usd: 2.0,
      agent_effort: 'medium',
      human_only: 'False',
      authority_role: 'platform-admin',
      procedure: PROCEDURE,
    },
  });
  const empty = page.getByTestId('checklist-empty');
  await expect(empty).toContainText('No items yet');
  await expect(empty).toContainText('an agent run (profile analyst)');
  await expect(empty).toContainText('nothing has run');
  await expect(page.getByTestId('checklist-procedure')).toContainText(PROCEDURE);
  // No 0/0 and no Save with nothing to save.
  await expect(page.locator('.step-checklist-progress')).toHaveText('');
  await expect(page.getByRole('button', { name: 'Save' })).toHaveCount(0);
  expect(errs, `plugin threw: ${errs.join(' | ')}`).toEqual([]);
});

test('an unrun checklist a person writes names the holder, or says whoever takes it', async ({ page }) => {
  await mountChecklist(page, {
    id: 's', kind: 'checklist', title: 'Walk the floor', status: 'active',
    assignee_id: 'emp-david', metadata: {},
  });
  await expect(page.getByTestId('checklist-empty')).toContainText('emp-david');
  await expect(page.getByTestId('checklist-procedure')).toHaveCount(0);

  await mountChecklist(page, {
    id: 's', kind: 'checklist', title: 'Walk the floor', status: 'ready',
    assignee_id: null,
    // A block the protocol still marks human-only is a person's step.
    metadata: { agent_profile: 'analyst', agent_model: 'm', agent_budget_usd: 1, agent_effort: 'low', human_only: true },
  });
  const empty = page.getByTestId('checklist-empty');
  await expect(empty).toContainText('whoever takes this step');
  await expect(empty).not.toContainText('agent run');
});

test('a completed checklist with no items says it recorded none', async ({ page }) => {
  await mountChecklist(page, {
    id: 's', kind: 'checklist', title: 'Done', status: 'completed', metadata: { items: [] },
  });
  await expect(page.getByTestId('checklist-empty')).toContainText('completed with no items recorded');
});

test('a checklist with items renders them and no empty state', async ({ page }) => {
  await mountChecklist(page, {
    id: 's', kind: 'checklist', title: 'Walk the floor', status: 'active',
    metadata: { items: [{ label: 'one', checked: true }, { label: 'two', checked: false }], procedure: PROCEDURE },
  });
  await expect(page.getByTestId('checklist-empty')).toHaveCount(0);
  await expect(page.locator('.step-checklist-progress')).toHaveText('1/2');
  await expect(page.getByRole('button', { name: 'Save' })).toBeVisible();
});

test('the profile, holder and procedure are drawn as text', async ({ page }) => {
  const PAYLOAD = '<img src=x onerror=alert(1)>';
  const errs = await mountChecklist(page, {
    id: 's', kind: 'checklist', title: 't', status: 'ready',
    metadata: {
      agent_profile: PAYLOAD, agent_model: 'm', agent_budget_usd: 1, agent_effort: 'low',
      procedure: `do ${PAYLOAD}`,
    },
  });
  await expect(page.getByTestId('checklist-empty')).toContainText(PAYLOAD);
  await expect(page.getByTestId('checklist-procedure')).toContainText(`do ${PAYLOAD}`);
  await expect(page.locator('#host img')).toHaveCount(0);
  expect(errs).toEqual([]);
});
