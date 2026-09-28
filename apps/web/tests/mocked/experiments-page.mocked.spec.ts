// /it/design/experiments — what the Running and Concluded panels render
// (backlog da547804, gap 5 of page-audit ec8351f4).
//
// Before this spec the page was pinned only by the route smoke (it
// renders), the outage crawl (failure marker), the tab spec and the
// router test. Nothing held the running/concluded split on job status,
// the v5 `control` / `candidate` arms with their `*_version` fallback,
// "waiting on" through standingOf, the outcome label, the card links, or
// the truncated-read refusal. Gaps 1, 2 and 4 of the same audit change
// exactly what those render, so the spec rides with them:
//   * a concluded card shows `measure.source`, `state.sample_floor` beside
//     n, and `decide.against_stated_rule` (3633a918);
//   * an abandoned card shows its terminal's `reason`, not dashes
//     (baf0c973);
//   * the empty Running panel states the threshold David decided on
//     2026-09-23 (design d8771dec) instead of calling zero a lapse
//     (071ffd8b).

import { test, expect, type Page, type Route } from './_test';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const LIST = /\/api\/jobs\?kind=protocol-experiment&/;

// ---- fixtures ---------------------------------------------------------

type StepRow = Readonly<{
  slug: string;
  status: string;
  sort_order: number;
  metadata?: Record<string, unknown>;
}>;

const packet = (
  id: string,
  title: string,
  status: 'open' | 'closed',
  steps: ReadonlyArray<StepRow>,
  metadata: Record<string, unknown> = {},
  closed_on: string | null = null,
) => ({
  id,
  kind: 'protocol-experiment',
  workflow_version: 1,
  subject: { subject_kind: 'custom', id: `exp-${id}` },
  title,
  owner_id: 'emp-david',
  status,
  priority: 'standard',
  opened_on: '2026-09-20',
  due_on: null,
  closed_on,
  tags: [],
  metadata: { protocol: title, ...metadata },
  steps: steps.map((s) => ({
    id: `${id}-${s.slug}`,
    job_id: id,
    kind: 'task',
    title: s.slug,
    spec_slug: s.slug,
    assignee_id: null,
    status: s.status,
    sort_order: s.sort_order,
    blocked_by: [],
    completed_on: s.status === 'completed' ? '2026-09-21' : null,
    metadata: s.metadata ?? {},
  })),
});

const V5_STATE = {
  hypothesis: 'A 6-wide gate finishes sooner than a 4-wide one',
  metric: 'gate wall clock, minutes',
  control: '4 cargo jobs',
  candidate: '6 cargo jobs',
  decision_rule: 'candidate at least 10% faster',
  sample_floor: '5 gates per arm',
};

const done = (slug: string, sort_order: number, metadata: Record<string, unknown> = {}): StepRow => ({
  slug, status: 'completed', sort_order, metadata,
});

/// Running, stated under the v5 body: the arms are free text.
const RUNNING_V5 = packet('exp-run-v5', 'Experiment: gate width', 'open', [
  done('opened', 0),
  done('state', 1, V5_STATE),
  { slug: 'run', status: 'ready', sort_order: 2 },
  { slug: 'measure', status: 'pending', sort_order: 3 },
]);

/// Running, stated under a v1–v4 body: the arms are `*_version`.
const RUNNING_OLD = packet('exp-run-old', 'Experiment: backlog-item v3 vs v4', 'open', [
  done('opened', 0),
  done('state', 1, {
    hypothesis: 'v4 closes items sooner',
    metric: 'cycle days',
    control_version: 'v3',
    candidate_version: 'v4',
    decision_rule: 'v4 not worse on cycle days',
  }),
  { slug: 'run', status: 'active', sort_order: 2 },
]);

const concludedOn = (
  id: string,
  title: string,
  verdict: string,
  outcome: string,
  closed_on: string,
) =>
  packet(
    id,
    title,
    'closed',
    [
      done('opened', 0),
      done('state', 1, V5_STATE),
      done('run', 2, { started_at: '2026-09-21T10:00:00Z', method: 'alternate gates' }),
      done('measure', 3, {
        control_result: '11m',
        candidate_result: `${id}-candidate`,
        samples: '6',
        source: `${id} gate receipts`,
      }),
      done('decide', 4, {
        verdict,
        decision: `${id}: ${verdict}`,
        against_stated_rule: `${id} judged against the 10% rule`,
        confounds: 'one warm target',
        elapsed: '40m',
      }),
      done(outcome, 5),
    ],
    { outcome },
    closed_on,
  );

const PROMOTED = concludedOn('exp-promoted', 'Experiment: promoted one', 'promote', 'promoted', '2026-09-26');
const RETIRED = concludedOn('exp-retired', 'Experiment: retired one', 'retire', 'retired', '2026-09-25');
const INCONCLUSIVE = concludedOn('exp-inconclusive', 'Experiment: inconclusive one', 'inconclusive', 'inconclusive', '2026-09-24');

/// Abandoned after `state`: no `measure`, no `decide`, only the reason.
const ABANDONED = packet(
  'exp-abandoned',
  'Experiment: abandoned one',
  'closed',
  [
    done('opened', 0),
    done('state', 1, V5_STATE),
    done('abandoned', 7, { reason: 'the gate pool was resized mid-run, so the arms stopped sharing a machine' }),
  ],
  { outcome: 'abandoned', abandoned: 'true' },
  '2026-09-27',
);

const ALL = [RUNNING_V5, RUNNING_OLD, PROMOTED, RETIRED, INCONCLUSIVE, ABANDONED];

async function mocks(page: Page, answer: (r: Route) => Promise<void>) {
  await installSmokeMocks(page);
  await page.route(LIST, answer);
}

const card = (page: Page, title: string) =>
  page.locator('article.exp-card').filter({ has: page.getByRole('link', { name: title, exact: true }) });

// ---- specs ------------------------------------------------------------

test('running and concluded split on job status, each card linking to its packet', async ({ page }) => {
  await mocks(page, (r) => json(r, { data: ALL, total: ALL.length, limit: 1000, offset: 0 }));
  await page.goto('/it/design/experiments');

  const running = page.getByRole('region', { name: 'Running experiments' });
  const concluded = page.getByRole('region', { name: 'Concluded experiments' });
  await expect(running.locator('article.exp-card')).toHaveCount(2);
  await expect(concluded.locator('article.exp-card')).toHaveCount(4);

  for (const j of ALL) {
    await expect(page.getByRole('link', { name: j.title, exact: true })).toHaveAttribute(
      'href',
      new RegExp(`/ux/jobs/${j.id}$`),
    );
  }
  // Concluded newest-closed first.
  await expect(concluded.locator('a.exp-title')).toHaveText([
    ABANDONED.title, PROMOTED.title, RETIRED.title, INCONCLUSIVE.title,
  ]);
});

test('a running card reads the v5 arms, falls back to *_version, and says what it waits on', async ({ page }) => {
  await mocks(page, (r) => json(r, { data: ALL, total: ALL.length, limit: 1000, offset: 0 }));
  await page.goto('/it/design/experiments');

  const v5 = card(page, RUNNING_V5.title);
  await expect(v5).toContainText('4 cargo jobs vs 6 cargo jobs');
  await expect(v5).toContainText(V5_STATE.hypothesis);
  await expect(v5).toContainText(V5_STATE.decision_rule);
  await expect(v5.locator('.exp-waiting')).toHaveText('waiting on: run (ready, not yet done)');

  const old = card(page, RUNNING_OLD.title);
  await expect(old).toContainText('v3 vs v4');
  await expect(old.locator('.exp-waiting')).toHaveText('waiting on: run (active, not yet done)');
});

test('a concluded card shows source, floor and the decision against the stated rule', async ({ page }) => {
  await mocks(page, (r) => json(r, { data: ALL, total: ALL.length, limit: 1000, offset: 0 }));
  await page.goto('/it/design/experiments');

  for (const [j, outcome, verdict] of [
    [PROMOTED, 'promoted', 'promote'],
    [RETIRED, 'retired', 'retire'],
    [INCONCLUSIVE, 'inconclusive', 'inconclusive'],
  ] as const) {
    const c = card(page, j.title);
    await expect(c.locator('.exp-outcome')).toHaveText(outcome);
    await expect(c.locator('.exp-measured')).toHaveText(
      `control 11m · candidate ${j.id}-candidate · n=6 (floor 5 gates per arm) · source: ${j.id} gate receipts`,
    );
    await expect(c.locator('.exp-decided')).toHaveText(`${j.id}: ${verdict}`);
    await expect(c.locator('.exp-against-rule')).toHaveText(`${j.id} judged against the 10% rule`);
    await expect(c.locator('.exp-confounds')).toHaveText('one warm target');
  }
});

test('an abandoned card shows its reason in place of Measured and Decided', async ({ page }) => {
  await mocks(page, (r) => json(r, { data: ALL, total: ALL.length, limit: 1000, offset: 0 }));
  await page.goto('/it/design/experiments');

  const c = card(page, ABANDONED.title);
  await expect(c.locator('.exp-outcome')).toHaveText('abandoned');
  await expect(c.locator('.exp-abandoned-reason')).toHaveText(
    'the gate pool was resized mid-run, so the arms stopped sharing a machine',
  );
  await expect(c).toContainText(V5_STATE.hypothesis);
  await expect(c.locator('.exp-measured')).toHaveCount(0);
  await expect(c.locator('.exp-decided')).toHaveCount(0);
  await expect(c).not.toContainText('control —');
});

test('no experiment running states the threshold for the first one, not a lapse', async ({ page }) => {
  await mocks(page, (r) => json(r, { data: [], total: 0, limit: 1000, offset: 0 }));
  await page.goto('/it/design/experiments');

  const empty = page.getByRole('region', { name: 'Running experiments' }).locator('.exp-empty');
  await expect(empty).toContainText('No experiment is running.');
  await expect(empty).toContainText('first draft workflow version');
  await expect(empty).toContainText('20+ terminals a day');
  await expect(empty).toContainText('adds, removes or reorders a step');
  await expect(empty).not.toContainText('judgement alone');
});

test('a read that stops short of its total refuses, naming how many of how many', async ({ page }) => {
  // Every page hands back the same two rows against a total of five:
  // fetchEvery stops on the repeat, and the page must say so rather than
  // render two experiments as the whole archive.
  await mocks(page, (r) => json(r, { data: [RUNNING_V5, PROMOTED], total: 5, limit: 1000, offset: 0 }));
  await page.goto('/it/design/experiments');

  await expect(page.locator('.exp-failed')).toContainText('read 2 of 5 rows and stopped');
  await expect(page.locator('article.exp-card')).toHaveCount(0);
});
