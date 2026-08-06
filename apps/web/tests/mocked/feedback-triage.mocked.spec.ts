// The triage board. What these pin is that columns are DERIVED from
// the triage step rather than from a status field the board keeps —
// a card that could disagree with its Job is the whole failure mode
// this design avoids — and that the agent hand-off records something
// durable rather than firing and forgetting.
//
// The hand-off shape matters beyond the button: an agent taking an
// automatic first pass later writes the same `agent_requested_at`
// record without a human clicking, so the board reads both the same
// way.

import { test, expect } from '@playwright/test';
import { mountPage } from '../smoke/_helpers';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

function job(
  id: string,
  message: string,
  triage: { status: string; metadata?: Record<string, unknown>; kind?: string },
) {
  return {
    id,
    kind: 'user-feedback',
    title: `Feedback on /ux/jobs`,
    status: triage.status === 'completed' ? 'closed' : 'open',
    subject_kind: 'custom',
    subject_id: '/ux/jobs',
    owner_id: 'emp-bootstrap-admin',
    metadata: { message, route: '/ux/jobs' },
    steps: [
      { id: `${id}-t`, kind: 'trigger', status: 'completed' },
      {
        id: `${id}-a`,
        // `authority_role` is what the JobKind puts on this step to
        // keep it waiting for a person, and it is how the board finds
        // the step — so the fixture carries it exactly as a real Job
        // does. Omitting it here would make every card read as
        // triaged, which is the failure the board must not have.
        kind: triage.kind ?? 'acknowledgment',
        status: triage.status,
        metadata: { authority_role: 'platform-admin', ...(triage.metadata ?? {}) },
      },
      { id: `${id}-o`, kind: 'outcome', status: 'pending' },
    ],
  };
}

const JOBS = [
  job('fb-waiting', 'Column picker forgets my choice', { status: 'ready' }),
  job('fb-agent', 'Typo on the vendors page', {
    status: 'ready',
    metadata: { agent_requested_at: '2026-08-06T10:00:00Z', agent_requested_by: 'emp-1' },
  }),
  job('fb-done', 'Already handled', { status: 'completed' }),
];

test.describe('feedback triage board', () => {
  test.beforeEach(async ({ page }) => {
    await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
    await page.route(/\/api\/jobs\?kind=user-feedback/, (r) =>
      r.fulfill({ json: { data: JOBS, total: JOBS.length } }),
    );
  });

  test('sorts each item by its triage step, not a stored column', async ({ page }) => {
    await mountPage(page, '/system/feedback', { titleMatch: /feedback triage/i });

    const waiting = page.locator('section[aria-label="Waiting on triage"]');
    const withAgent = page.locator('section[aria-label="With an agent"]');
    const done = page.locator('section[aria-label="Triaged"]');

    await expect(waiting).toContainText('Column picker forgets my choice');
    // Same step status as the first card — what moves it is the
    // recorded agent request, which is the only difference.
    await expect(withAgent).toContainText('Typo on the vendors page');
    await expect(done).toContainText('Already handled');
  });

  // What the board actually depends on is the authority gate, not the
  // spelling of the step kind. Kinds are registry data and a kind is a
  // bundle of properties, so the registry is free to re-author this
  // spec onto a different one; the board must keep sorting when it
  // does. Pinning it here is what stops the kind name creeping back
  // in as a lookup.
  test('sorts by the authority gate, not the step kind name', async ({ page }) => {
    const renamed = [
      job('fb-renamed', 'Still needs a person', { status: 'ready', kind: 'sign-off' }),
    ];
    await page.route(/\/api\/jobs\?kind=user-feedback/, (r) =>
      r.fulfill({ json: { data: renamed, total: renamed.length } }),
    );

    await mountPage(page, '/system/feedback', { titleMatch: /feedback triage/i });

    await expect(page.locator('section[aria-label="Waiting on triage"]')).toContainText(
      'Still needs a person',
    );
    await expect(page.locator('section[aria-label="Triaged"]')).not.toContainText(
      'Still needs a person',
    );
  });

  test('handing to an agent records a durable request on the step', async ({ page }) => {
    let put: { url: string; body: Record<string, unknown> } | null = null;
    await page.route(/\/api\/jobs\/[^/]+\/steps\/[^/]+$/, async (route) => {
      if (route.request().method() !== 'PUT') return route.fallback();
      put = {
        url: route.request().url(),
        body: route.request().postDataJSON() as Record<string, unknown>,
      };
      return route.fulfill({ json: {} });
    });

    await mountPage(page, '/system/feedback', { titleMatch: /feedback triage/i });
    const card = page.locator('article', { hasText: 'Column picker forgets my choice' });
    await card.getByRole('button', { name: /hand to agent/i }).click();

    await expect.poll(() => put !== null).toBe(true);
    const body = (put as unknown as { body: { metadata: Record<string, unknown> } }).body;
    // A recorded request, not a fired action — so a reload still shows
    // it and an automatic first pass can write the same shape.
    expect(body.metadata['agent_requested_at']).toBeTruthy();
    expect(body.metadata['agent_requested_by']).toBeTruthy();
    // Handing off is not a decision: the step must stay open.
    expect(body).not.toHaveProperty('status');
  });

  test('marking triaged completes the step', async ({ page }) => {
    let body: Record<string, unknown> | null = null;
    await page.route(/\/api\/jobs\/[^/]+\/steps\/[^/]+$/, async (route) => {
      if (route.request().method() !== 'PUT') return route.fallback();
      body = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ json: {} });
    });

    await mountPage(page, '/system/feedback', { titleMatch: /feedback triage/i });
    const card = page.locator('article', { hasText: 'Column picker forgets my choice' });
    await card.getByRole('button', { name: /mark triaged/i }).click();

    await expect.poll(() => body !== null).toBe(true);
    expect((body as unknown as { status: string }).status).toBe('completed');
  });

  test('offers no actions on an already-triaged item', async ({ page }) => {
    await mountPage(page, '/system/feedback', { titleMatch: /feedback triage/i });
    const done = page.locator('section[aria-label="Triaged"]');
    await expect(done.getByRole('button', { name: /mark triaged/i })).toHaveCount(0);
    await expect(done.getByRole('button', { name: /hand to agent/i })).toHaveCount(0);
  });
});
