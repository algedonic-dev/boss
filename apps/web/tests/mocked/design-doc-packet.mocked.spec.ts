// A design doc that carries its own questions is reviewable the moment
// the packet exists — no file on deployed main, no reindex, no round
// trip.
//
// This is the property the whole change is for. `review-design.js`
// carries a 404 message apologising that "review Jobs are instant data
// but docs ride trains, so a review can exist before its doc reaches
// deployed main"; David, 2026-08-16: "our lack of good protocol around
// design docs, and the plumbing being broken too, is causing major
// slowdowns in my design review handling speed."
//
// Rendered rather than reasoned about, because nothing else mounts this
// bundle: nine step-plugin bundles ship and CI mounts exactly one, in
// the live-backend suite (workflow-ux-as-data Q4). A change to plugin
// JS is otherwise verified by a human opening the page.

import { test, expect } from '@playwright/test';
import { readFileSync } from 'fs';
import { mountPage } from '../smoke/_helpers';
import { installSmokeMocks } from './_smokeMocks';

const PLUGIN = readFileSync(
  new URL('../../../../infra/step-plugins/review-design.js', import.meta.url),
  'utf8',
);

const STEP = {
  id: 'step-1',
  spec_slug: 'review',
  title: 'review',
  kind: 'review-design',
  status: 'ready',
  assignee_id: 'emp-david',
  blocked_by: [],
  metadata: {
    title: 'Design: the physical layer, virtualized',
    markdown: '# The claim\n\nBOSS models its own physical world nowhere.',
    questions: [
      { anchor: 'Q1', title: 'New Subject kinds, or Classes of asset?', proposal: 'New kinds.' },
      { anchor: 'Q2', title: 'Tree, or database?', proposal: 'Declared in the tree.' },
    ],
  },
};

const JOB = {
  id: 'job-dd-1',
  kind: 'design-doc',
  workflow_version: 1,
  subject: { subject_kind: 'custom', id: 'bossnet-physical-topology' },
  title: 'Design: the physical layer, virtualized',
  owner_id: 'emp-david',
  status: 'open',
  priority: 'standard',
  opened_on: '2026-08-16',
  due_on: null,
  closed_on: null,
  metadata: {},
  steps: [STEP],
};

test('a self-carried design doc renders its questions without touching the docs API', async ({
  page,
}) => {
  const docsApiCalls: string[] = [];
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));

  // The shared installer first: it registers the catch-all and the
  // chrome's reads, and Playwright matches routes in REVERSE
  // registration order, so everything below overrides it.
  await installSmokeMocks(page);
  // The assertion that matters: this must never be called.
  await page.route('**/api/design/**', (r) => {
    docsApiCalls.push(r.request().url());
    return r.fulfill({ status: 500, body: 'the packet must not need me' });
  });
  await page.route('**/api/jobs/job-dd-1', (r) => r.fulfill({ json: JOB }));
  await page.route('**/api/jobs/step-plugins', (r) =>
    r.fulfill({
      json: [
        {
          kind: 'review-design',
          label: 'Review Design',
          category: 'platform',
          version: 1,
          frontend_url: '/plugins/review-design.js',
          owning_team: 'platform',
        },
      ],
    }),
  );
  await page.route('**/plugins/review-design.js', (r) =>
    r.fulfill({ contentType: 'application/javascript', body: PLUGIN }),
  );

  // The step surface deliberately renders OUTSIDE `.app-shell` to take
  // the whole viewport, so it passes its own root.
  await mountPage(page, '/jobs/job-dd-1/steps/step-1', { root: '.step-focus' });
  await page.waitForTimeout(2000);

  expect(errs, `plugin threw: ${errs.join(' | ')}`).toEqual([]);
  // Both questions on the surface, by their own text.
  await expect(page.getByText('New Subject kinds, or Classes of asset?')).toBeVisible();
  await expect(page.getByText('Tree, or database?')).toBeVisible();
  // The prose rode with the packet.
  await expect(page.getByText(/BOSS models its own physical world nowhere/)).toBeVisible();
  // And it said what it is, rather than three `undefined`s where a
  // file's path/status/word-count would go.
  await expect(page.getByText('carried by this packet · not yet a file')).toBeVisible();
  // The whole point: no docs API, so no dependence on the doc having shipped.
  expect(docsApiCalls, 'the packet reached for the docs API').toEqual([]);
});

// BACKWARD COMPATIBILITY, asserted rather than asserted-to-David. Ten
// design-doc-review Jobs are in flight, none of which carries
// `metadata.questions`; they must still take the docs-API path exactly
// as before. A change that fixes the new shape by breaking the old one
// would be worse than the round trip.
test('a step with no carried questions still reads the docs API', async ({ page }) => {
  const docsApiCalls: string[] = [];
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));

  await installSmokeMocks(page);
  await page.route('**/api/design/docs/**', (r) => {
    docsApiCalls.push(r.request().url());
    return r.fulfill({
      json: {
        path: 'docs/design/legacy.md',
        title: 'A doc that lives in git',
        status: 'in-review',
        word_count: 900,
        content_html: '<h1>A doc that lives in git</h1>',
        questions: [{ anchor: 'Q1', title: 'Fetched from the file, not the packet' }],
      },
    });
  });
  const legacyStep = {
    ...STEP,
    metadata: { doc_path: 'docs/design/legacy.md' }, // no `questions`
  };
  await page.route('**/api/jobs/job-dd-1', (r) =>
    r.fulfill({ json: { ...JOB, kind: 'design-doc-review', steps: [legacyStep] } }),
  );
  await page.route('**/api/jobs/step-plugins', (r) =>
    r.fulfill({
      json: [
        {
          kind: 'review-design',
          label: 'Review Design',
          category: 'platform',
          version: 1,
          frontend_url: '/plugins/review-design.js',
          owning_team: 'platform',
        },
      ],
    }),
  );
  await page.route('**/plugins/review-design.js', (r) =>
    r.fulfill({ contentType: 'application/javascript', body: PLUGIN }),
  );

  await mountPage(page, '/jobs/job-dd-1/steps/step-1', { root: '.step-focus' });
  await page.waitForTimeout(2000);

  expect(errs, `plugin threw: ${errs.join(' | ')}`).toEqual([]);
  await expect(page.getByText('Fetched from the file, not the packet')).toBeVisible();
  // The file's own meta line, not the packet's.
  await expect(page.getByText(/docs\/design\/legacy\.md/)).toBeVisible();
  expect(docsApiCalls.length, 'the legacy path stopped reading the docs API').toBeGreaterThan(0);
});

