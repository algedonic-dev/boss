import { expect, test, type Page } from './_test';
import { readFileSync } from 'node:fs';

const JOB = 'question-job';
const DESIGN = '5da17ace-7e35-4eae-a7c6-20e063c1791a';
type Reply = Readonly<{ body: unknown; status?: number }>;
type Replies = Readonly<Record<string, Reply | readonly Reply[]>>;
type Call = Readonly<{ url: string; method: string; body: string | null }>;
type Props = Readonly<{ step: unknown; jobId: string }>;

async function mount(page: Page, props: Props, replies: Replies): Promise<void> {
  await page.setContent('<div id="host"></div>');
  await page.evaluate((routes) => {
    const w = window as unknown as {
      __calls: Call[];
      __updates: number;
      __boss_register_step_plugin: (kind: string, plugin: unknown) => void;
      __plugin: unknown;
      fetch: (url: unknown, init?: Readonly<{ method?: string; body?: string }>) => Promise<Response>;
    };
    const counters: Record<string, number> = {};
    w.__calls = [];
    w.__updates = 0;
    w.__boss_register_step_plugin = (_kind, plugin) => { w.__plugin = plugin; };
    w.fetch = async (url, init) => {
      const path = String(url);
      w.__calls.push({ url: path, method: init?.method ?? 'GET', body: init?.body ?? null });
      const value = routes[path];
      if (!value) return new Response('not routed', { status: 404 });
      const queue = Array.isArray(value) ? value as readonly Reply[] : [value as Reply];
      const index = counters[path] ?? 0;
      counters[path] = index + 1;
      const reply = queue[Math.min(index, queue.length - 1)];
      if (!reply) return new Response('no reply', { status: 500 });
      return new Response(JSON.stringify(reply.body), {
        status: reply.status ?? 200,
        headers: { 'content-type': 'application/json' },
      });
    };
  }, replies);
  await page.addScriptTag({
    content: readFileSync(new URL('../../../../infra/step-plugins/answer-question.js', import.meta.url), 'utf8'),
  });
  await page.evaluate((p) => {
    const w = window as unknown as {
      __plugin: (host: Element, args: Props & { onUpdate: () => void }) => unknown;
      __updates: number;
    };
    const host = document.getElementById('host');
    if (!host) throw new Error('plugin host missing');
    w.__plugin(host, { ...p, onUpdate: () => { w.__updates += 1; } });
  }, props);
}

const question = (metadata: Readonly<Record<string, unknown>> = {}, status = 'ready') => ({
  id: 'question-step', kind: 'answer-question', title: 'Answer the question', status,
  fields: [
    { name: 'verdict', field_type: 'answered', required: true },
    { name: 'answer', field_type: 'string', required: true },
  ],
  metadata: { question: 'Which maintenance window should we use?', context_md: 'The customer needs an evening window.', ...metadata },
});

test('the question leads, context and source work are visible, and only the declared answer is offered', async ({ page }) => {
  await mount(page, { step: question(), jobId: JOB }, {
    [`/api/jobs/${JOB}`]: { body: { title: 'Service visit', metadata: { parent_job_id: 'source-job' }, steps: [] } },
    [`/api/jobs/${JOB}/steps/question-step/metadata`]: { body: {} },
    [`/api/jobs/${JOB}/steps/question-step`]: { body: {} },
  });
  await expect(page.getByRole('heading', { name: 'Which maintenance window should we use?' })).toBeVisible();
  await expect(page.getByText('The customer needs an evening window.')).toBeVisible();
  await expect(page.getByRole('link', { name: 'View related work' })).toHaveAttribute('href', '/ux/jobs/source-job');
  await expect(page.getByRole('button', { name: 'Approve', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Answer', exact: true }).click();
  await page.getByRole('textbox').fill('Tuesday evening');
  await page.getByRole('button', { name: 'Record the answer' }).click();
  await expect.poll(() => page.evaluate(() => (window as unknown as { __updates: number }).__updates)).toBe(1);
  const calls = await page.evaluate(() => (window as unknown as { __calls: Call[] }).__calls);
  expect(calls.filter((call) => call.method !== 'GET')).toEqual([
    { url: `/api/jobs/${JOB}/steps/question-step/metadata`, method: 'PATCH', body: JSON.stringify({ verdict: 'answered', answer: 'Tuesday evening' }) },
    { url: `/api/jobs/${JOB}/steps/question-step`, method: 'PUT', body: JSON.stringify({ status: 'completed' }) },
  ]);
});

const linkedJob = {
  title: 'Old alarm report', metadata: {},
  steps: [{ id: 'draft', status: 'completed', metadata: { design_id: DESIGN } }],
};

test('an already-recorded linked decision is not presented as another approval', async ({ page }) => {
  await mount(page, { step: question({ question: 'Decide the alarm design', context_md: '' }), jobId: JOB }, {
    [`/api/jobs/${JOB}`]: { body: linkedJob },
    [`/api/jobs/${DESIGN}`]: { body: { title: 'Alarm meanings', metadata: { no_open_questions: true, questions: [], markdown: 'Red requires immediate action.' } } },
  });
  await expect(page.getByRole('heading', { name: 'No answer needed' })).toBeVisible();
  await expect(page.getByText('This decision is already recorded.')).toBeVisible();
  await expect(page.getByRole('link', { name: 'View decision' })).toHaveAttribute('href', `/ux/jobs/${DESIGN}`);
  await expect(page.getByText('Red requires immediate action.')).toBeVisible();
  await expect(page.getByRole('textbox')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Record the answer' })).toHaveCount(0);
});

// The flag as the system of record actually holds it: `boss design
// --no-questions` writes the STRING "true" (design.rs, "the flag is always
// written"), and design 195276f0 carried exactly that. A boolean-only
// check read it as a design with questions and sent David to a review
// that does not exist (backlog ac141c99).
test('the recorded flag in its stored string shape reads as recorded too', async ({ page }) => {
  await mount(page, { step: question({ question: 'Decide the alarm design', context_md: '' }), jobId: JOB }, {
    [`/api/jobs/${JOB}`]: { body: linkedJob },
    [`/api/jobs/${DESIGN}`]: { body: { title: 'Alarm meanings', metadata: { no_open_questions: 'true', questions: [], markdown: 'Red requires immediate action.' } } },
  });
  await expect(page.getByRole('heading', { name: 'No answer needed' })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Review the linked design' })).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'View decision' })).toHaveAttribute('href', `/ux/jobs/${DESIGN}`);
});

test('a linked design with a real open question directs the answer to its own review', async ({ page }) => {
  await mount(page, { step: question({ question: 'Decide the design', context_md: '' }), jobId: JOB }, {
    [`/api/jobs/${JOB}`]: { body: linkedJob },
    [`/api/jobs/${DESIGN}`]: { body: { title: 'Transport choice', metadata: { questions: [{ anchor: 'transport', title: 'Enable roaming?', proposal: 'Keep SSH and add roaming.' }], markdown: 'The session needs to survive connection loss.' } } },
  });
  await expect(page.getByRole('heading', { name: 'Review the linked design' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'View decision' })).toHaveAttribute('href', `/ux/jobs/${DESIGN}`);
  await expect(page.getByRole('button', { name: 'Record the answer' })).toHaveCount(0);
});

test('a failed details read shows a retry instead of allowing an answer without context', async ({ page }) => {
  await mount(page, { step: question(), jobId: JOB }, {
    [`/api/jobs/${JOB}`]: [{ body: {}, status: 503 }, { body: { title: 'Service visit', metadata: {}, steps: [] } }],
  });
  await expect(page.getByText('Could not load the question details.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Record the answer' })).toHaveCount(0);
  await page.getByRole('button', { name: 'Retry' }).click();
  await expect(page.getByRole('heading', { name: 'Which maintenance window should we use?' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Record the answer' })).toBeDisabled();
});

test('a completed answer stays visible even when related details cannot be read', async ({ page }) => {
  await mount(page, { step: question({ verdict: 'answered', answer: 'Tuesday evening' }, 'completed'), jobId: JOB }, {
    [`/api/jobs/${JOB}`]: { body: {}, status: 503 },
  });
  await expect(page.getByText('Answered — answered. Tuesday evening')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Record the answer' })).toHaveCount(0);
});

test('a contradictory no-questions flag does not conceal an actual question', async ({ page }) => {
  await mount(page, { step: question(), jobId: JOB }, {
    [`/api/jobs/${JOB}`]: { body: linkedJob },
    [`/api/jobs/${DESIGN}`]: { body: { title: 'Transport choice', metadata: { no_open_questions: true, questions: [{ anchor: 'transport', title: 'Enable roaming?', proposal: 'Keep SSH.' }] } } },
  });
  await expect(page.getByRole('heading', { name: 'Review the linked design' })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'No answer needed' })).toHaveCount(0);
});

test('missing question text is explicit and cannot record an invented answer', async ({ page }) => {
  await mount(page, { step: question({ question: '' }), jobId: JOB }, {
    [`/api/jobs/${JOB}`]: { body: { title: 'A vague task title', metadata: {}, steps: [] } },
  });
  await expect(page.getByText('No question has been recorded. The asker needs to add the question before it can be answered.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Record the answer' })).toHaveCount(0);
});
