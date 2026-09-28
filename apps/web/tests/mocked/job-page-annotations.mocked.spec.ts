// The job page renders a packet's operator annotations (backlog
// 1abb9928, 2026-09-27). Until this spec's car, JobDetailPage read
// job.metadata only for the keys a workflow declares as link fields,
// so a plan of action, an ETA, what needs David and every recorded
// decision were readable only through the API — David asked whether
// steps_for_david was visible in the UI, and it was not. End to end
// against a mocked packet:
//   1. the alarm keys lead, in order, then decided_*, then the rest;
//   2. prose keeps its newlines; a uuid is a link to that packet;
//   3. a nested object reads as pretty JSON, key by key;
//   4. a declared link field stays in Linked Jobs, and the corrections
//      list stays with the step it corrects — neither is drawn twice.

import { expect, test, type Page, type Route } from './_test';

const JOB_ID = '1abb9928-67d2-4aa1-a950-f8049f88b8fa';
const CAR_A = 'd6267b8e-c0f6-4dd3-b8eb-426482191505';
const CAR_B = '817b1b84-0000-4000-8000-000000000001';
const LINKED = '6f49076a-e493-427d-b698-eae82ff76212';

const PLAN = 'Rebase the car onto main.\nRe-gate it with the park flags.';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

async function mocks(page: Page) {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  const job = {
    id: JOB_ID, kind: 'backlog-item', title: 'Annotated packet', status: 'open',
    opened_on: '2026-09-27', due_on: null, closed_on: null, owner_id: 'emp-001',
    priority: 'urgent', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'fixture' },
    metadata: {
      area: 'web/jobs',
      review_verdict: { verdict: 'approve', by: 'emp-david' },
      landed_cars: [CAR_A, CAR_B],
      'decided_2026-09-27_david': 'Build it; no design needed.',
      steps_for_david: 'Sign the release step.',
      eta_utc: '2026-09-28T02:00:00Z',
      needs_david: 'Nothing until the car lands.',
      plan_of_action: PLAN,
      owner: 'agent-claude',
      remaining: `The car behind ${CAR_A} still owes its probe.`,
      related_job: LINKED,
      corrections: [{ step: 'triage', field: 'evidence', reads: 'x', should_read: 'y' }],
    },
    steps: [],
  };
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: 'emp-001', role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/job-edges$/, (r) =>
    json(r, [{ source_kind: 'backlog-item', field_path: 'related_job', field_kind: 'job_id', description: 'a related job' }]));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, job));
}

test('every operator annotation renders, alarm keys first, prose intact, ids linked', async ({ page }) => {
  await mocks(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  await expect(page.locator('h1')).toContainText('Annotated packet');

  const section = page.locator('dl.jd-annotations');
  await expect(section).toBeVisible();
  await expect(section.locator('dt')).toHaveText([
    'owner',
    'plan_of_action',
    'eta_utc',
    'needs_david',
    'steps_for_david',
    'decided_2026-09-27_david',
    'area',
    'landed_cars',
    'remaining',
    'review_verdict',
  ]);

  // Prose keeps its line break — a plan is read as the steps it was
  // written in, not one flattened wall.
  const plan = section.locator('[data-key="plan_of_action"] p.jd-ann-prose');
  await expect(plan).toHaveText(PLAN);
  expect(await plan.evaluate((el) => (el as HTMLElement).innerText)).toContain('\n');
  await expect(section.locator('[data-key="steps_for_david"] dd')).toHaveText('Sign the release step.');
  await expect(section.locator('[data-key="eta_utc"] dd')).toHaveText('2026-09-28T02:00:00Z');

  // A list of ids is a list of links to those packets; a uuid inside
  // prose is a link too.
  const cars = section.locator('[data-key="landed_cars"] li a');
  await expect(cars).toHaveCount(2);
  await expect(cars.first()).toHaveAttribute('href', `/ux/jobs/${CAR_A}`);
  await expect(section.locator('[data-key="remaining"] a')).toHaveAttribute('href', `/ux/jobs/${CAR_A}`);

  // A nested object reads key by key.
  await expect(section.locator('[data-key="review_verdict"] pre')).toContainText('"verdict": "approve"');

  // The declared link field stays where it was, and only there; the
  // corrections list is not re-drawn as an annotation.
  await expect(section.locator('[data-key="related_job"]')).toHaveCount(0);
  await expect(section.locator('[data-key="corrections"]')).toHaveCount(0);
  await expect(page.locator('.jd-info-label', { hasText: 'related_job' })).toBeVisible();
});
