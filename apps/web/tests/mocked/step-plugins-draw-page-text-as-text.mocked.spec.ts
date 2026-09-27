// A step title, a metadata value and an error message carrying
// `<img src=x onerror=alert(1)>` are drawn as those characters by every
// step plugin that shows them — never parsed into an element (backlog
// 4a359b51, 2026-09-27).
//
// GitHub CodeQL failed publish PR #245 on two high-severity alerts in
// these bundles: sign-off.js:84 (DOM and exception text reinterpreted
// as HTML) and diagnostic-call.js:137 (the Join button's href set from
// the URL box verbatim, so a `javascript:` join_url in step metadata ran
// in the operator's session on a click). They render the steps a
// passkey signs. src/steps/stepPluginHtmlSinks.test.ts holds the source
// to the rule; this drives the real bundles in a real browser, because
// only a real DOM says whether a string became an element.
//
// Each bundle mounts on a blank page with the plugin host's contract
// and nothing else: `__boss_register_step_plugin`, a `fetch` answering
// from the case's own routes, and an `alert` that records rather than
// shows. No SPA and no dev-server read is involved.

import { expect, test, type Page } from './_test';
import { readFileSync } from 'fs';

const PAYLOAD = '<img src=x onerror=alert(1)>';

type Routes = Readonly<Record<string, unknown>>;

/// A route answering `{ __throw: msg }` makes that fetch REJECT with
/// Error(msg), so the plugin's exception text carries the payload.
const THROWS = (msg: string) => ({ __throw: msg });

async function mountPlugin(
  page: Page,
  file: string,
  props: Readonly<{ step: unknown; jobId: string }>,
  routes: Routes,
): Promise<string[]> {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  await page.setContent('<div id="host"></div>');
  await page.evaluate((r: Routes) => {
    const w = window as unknown as Record<string, unknown>;
    const alerts: string[] = [];
    w['__alerts'] = alerts;
    w['alert'] = (m: unknown) => alerts.push(String(m));
    w['__boss_register_step_plugin'] = (kind: string, mount: unknown) => {
      w['__plugin'] = { kind, mount };
    };
    w['fetch'] = async (url: unknown) => {
      const path = String(url);
      if (!Object.prototype.hasOwnProperty.call(r, path)) {
        return new Response('not routed', { status: 404 });
      }
      const answer = r[path] as { __throw?: string } | null;
      if (answer && typeof answer.__throw === 'string') throw new Error(answer.__throw);
      return new Response(JSON.stringify(answer), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    };
  }, routes);
  await page.addScriptTag({
    content: readFileSync(new URL(`../../../../infra/step-plugins/${file}`, import.meta.url), 'utf8'),
  });
  await page.evaluate((p) => {
    const w = window as unknown as Record<string, unknown>;
    const plugin = w['__plugin'] as { mount: (c: Element, props: unknown) => unknown } | undefined;
    if (!plugin) throw new Error('the bundle registered no plugin');
    const host = document.getElementById('host');
    if (!host) throw new Error('no host');
    plugin.mount(host, { ...p, onUpdate() {} });
  }, props);
  return errs;
}

/// The payload is on screen as its characters, no element was made of
/// it, and nothing it would have run has run.
async function drawnAsText(page: Page, errs: readonly string[], needle: string = PAYLOAD) {
  await expect(page.locator('#host')).toContainText(needle);
  await expect(page.locator('#host img')).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __alerts: string[] }).__alerts)).toEqual([]);
  expect(errs, `plugin threw: ${errs.join(' | ')}`).toEqual([]);
}

test('sign-off draws the title, every signed key, the case and a refusal as text', async ({ page }) => {
  const step = {
    id: 'step-1',
    kind: 'sign-off',
    title: `Approve ${PAYLOAD}`,
    status: 'ready',
    assurance_required: 'presence',
    sign_offs_required: [],
    sign_offs: [],
    fields: [{ name: 'plan', field_type: 'string', required: true }],
    metadata: { context_md: `the case ${PAYLOAD}`, plan: `run ${PAYLOAD}` },
  };
  const errs = await mountPlugin(page, 'sign-off.js', { step, jobId: 'job-1' }, {
    '/api/jobs/job-1/steps/step-1/metadata': THROWS(`refused ${PAYLOAD}`),
  });
  await drawnAsText(page, errs, `Approve ${PAYLOAD}`);
  await drawnAsText(page, errs, `the case ${PAYLOAD}`);
  await drawnAsText(page, errs, `run ${PAYLOAD}`);

  // The exception text CodeQL traced to h(): a refused save, drawn.
  await page.getByRole('button', { name: 'Approve', exact: true }).click();
  await drawnAsText(page, errs, `Could not record the decision: Error: refused ${PAYLOAD}`);
});

test('diagnostic-call draws its title as text and links only an http(s) join URL', async ({ page }) => {
  const step = {
    id: 'step-call',
    kind: 'diagnostic-call',
    title: `Call ${PAYLOAD}`,
    status: 'active',
    metadata: { join_url: 'javascript:alert(1)', notes_md: PAYLOAD },
  };
  const errs = await mountPlugin(page, 'diagnostic-call.js', { step, jobId: 'job-1' }, {});
  await drawnAsText(page, errs, `Call ${PAYLOAD}`);

  const join = page.locator('#host a', { hasText: 'Join call' });
  const box = page.locator('#host input[type=url]').first();
  // A join_url from metadata that is not http(s) is no link at all.
  await expect(box).toHaveValue('javascript:alert(1)');
  await expect(join).not.toHaveAttribute('href', /.*/);
  await expect(join).toBeHidden();

  // Typed, the same: every spelling a browser would still run.
  for (const typed of ['JavaScript:alert(1)', ' javascript:alert(1)', 'data:text/html,<b>x</b>', 'vbscript:x']) {
    await box.fill(typed);
    await expect(join).not.toHaveAttribute('href', /.*/);
    await expect(join).toBeHidden();
  }

  // An http(s) meeting link is the link it was, as it always rendered.
  await box.fill('https://zoom.us/j/123?pwd=abc');
  await expect(join).toHaveAttribute('href', 'https://zoom.us/j/123?pwd=abc');
  await expect(join).toBeVisible();
  await box.fill('  http://meet.example/room  ');
  await expect(join).toHaveAttribute('href', 'http://meet.example/room');
  expect(await page.evaluate(() => (window as unknown as { __alerts: string[] }).__alerts)).toEqual([]);
});

test('correction-verdict draws the evidence, the corrected title and a refusal as text', async ({ page }) => {
  const step = { id: 'step-v', kind: 'correction-verdict', status: 'ready', fields: [], metadata: {} };
  const errs = await mountPlugin(page, 'correction-verdict.js', { step, jobId: 'job-1' }, {
    '/api/jobs/job-1': {
      metadata: { corrects: 'job-x', corrects_title: `corrected ${PAYLOAD}` },
      steps: [
        {
          spec_slug: 'evidence',
          metadata: {
            claim: `claimed ${PAYLOAD}`,
            measured: `measured ${PAYLOAD}`,
            method: `method ${PAYLOAD}`,
            where: `where ${PAYLOAD}`,
          },
        },
      ],
    },
    '/api/jobs/job-1/steps/step-v/metadata': THROWS(`refused ${PAYLOAD}`),
  });
  for (const drawn of ['claimed', 'measured', 'corrected']) {
    await drawnAsText(page, errs, `${drawn} ${PAYLOAD}`);
  }
  // The link to the corrected packet is still the link it was.
  await expect(page.getByRole('link', { name: `corrected ${PAYLOAD}` })).toHaveAttribute(
    'href',
    '/ux/jobs/job-x',
  );

  const record = page.getByRole('button', { name: 'Record verdict' });
  await expect(record).toBeDisabled();
  await page.getByText('Accept — the correction is right').click();
  await expect(record).toBeEnabled();
  await record.click();
  await drawnAsText(page, errs, `refused ${PAYLOAD}`);
});

test('correction-verdict draws a recorded verdict as text', async ({ page }) => {
  const step = {
    id: 'step-v',
    kind: 'correction-verdict',
    status: 'completed',
    fields: [],
    metadata: { verdict: PAYLOAD },
  };
  const errs = await mountPlugin(page, 'correction-verdict.js', { step, jobId: 'job-1' }, {});
  await drawnAsText(page, errs, `Verdict recorded: ${PAYLOAD}.`);
});

test('scope-declaration draws the branch, the item, the receipt and both halves as text', async ({ page }) => {
  const step = {
    id: 'step-scope',
    kind: 'scope-declaration',
    status: 'ready',
    metadata: { summary: `does ${PAYLOAD}`, excludes: `not ${PAYLOAD}` },
  };
  const errs = await mountPlugin(page, 'scope-declaration.js', { step, jobId: 'job-1' }, {
    '/api/jobs/job-1': {
      subject: { subject_kind: 'custom', id: `fix/${PAYLOAD}` },
      metadata: { backlog_text: `item ${PAYLOAD}` },
      steps: [
        {
          spec_slug: 'gate',
          status: 'completed',
          metadata: { verified: `verified ${PAYLOAD}`, receipt: `receipt ${PAYLOAD}` },
        },
      ],
    },
    '/api/jobs/job-1/steps/step-scope/metadata': THROWS(`refused ${PAYLOAD}`),
  });
  await drawnAsText(page, errs, `fix/${PAYLOAD}`);
  await drawnAsText(page, errs, `answers item ${PAYLOAD}`);
  await page.getByText(/The gate has already run/).click();
  await drawnAsText(page, errs, `verified ${PAYLOAD}`);
  await drawnAsText(page, errs, `receipt ${PAYLOAD}`);
  // The two halves come back into their boxes as the characters stored.
  await expect(page.locator('#ssd-summary')).toHaveValue(`does ${PAYLOAD}`);
  await expect(page.locator('#ssd-excludes')).toHaveValue(`not ${PAYLOAD}`);

  await page.getByRole('button', { name: 'Declare the boundary' }).click();
  await drawnAsText(page, errs, `refused ${PAYLOAD}`);
});

test('scope-declaration draws a declared boundary as text', async ({ page }) => {
  const step = {
    id: 'step-scope',
    kind: 'scope-declaration',
    status: 'completed',
    metadata: { summary: `does ${PAYLOAD}`, excludes: `not ${PAYLOAD}` },
  };
  const errs = await mountPlugin(page, 'scope-declaration.js', { step, jobId: 'job-1' }, {});
  await drawnAsText(page, errs, `does ${PAYLOAD}`);
  await drawnAsText(page, errs, `not ${PAYLOAD}`);
});

test('incident-review draws the findings, each step and a refusal as text', async ({ page }) => {
  const step = { id: 'step-review', kind: 'incident-review', status: 'ready', metadata: {} };
  const errs = await mountPlugin(page, 'incident-review.js', { step, jobId: 'job-1' }, {
    '/api/jobs/job-1': {
      title: `incident ${PAYLOAD}`,
      metadata: { incident_at: `when ${PAYLOAD}`, summary: `summary ${PAYLOAD}` },
      steps: [
        {
          id: 'step-t',
          title: `timeline ${PAYLOAD}`,
          status: 'completed',
          assignee_id: `by ${PAYLOAD}`,
          metadata: { found: `found ${PAYLOAD}` },
        },
      ],
    },
    '/api/jobs/job-1/steps': THROWS(`refused ${PAYLOAD}`),
  });
  for (const drawn of ['incident', 'when', 'summary', 'timeline', 'by', 'found']) {
    await drawnAsText(page, errs, `${drawn} ${PAYLOAD}`);
  }
  await page.getByRole('button', { name: 'Findings reviewed — complete review' }).click();
  await drawnAsText(page, errs, `refused ${PAYLOAD}`);
});

test('checklist, sr-triage and answer-question draw a title and metadata as text', async ({ page }) => {
  let errs = await mountPlugin(
    page,
    'checklist.js',
    {
      step: {
        id: 's',
        kind: 'checklist',
        title: `list ${PAYLOAD}`,
        status: 'active',
        metadata: { items: [{ label: `item ${PAYLOAD}`, checked: false }] },
      },
      jobId: 'job-1',
    },
    {},
  );
  await drawnAsText(page, errs, `list ${PAYLOAD}`);
  await drawnAsText(page, errs, `item ${PAYLOAD}`);

  errs = await mountPlugin(
    page,
    'sr-triage.js',
    { step: { id: 's', kind: 'sr-triage', title: `triage ${PAYLOAD}`, status: 'active', metadata: {} }, jobId: 'job-1' },
    {},
  );
  await drawnAsText(page, errs, `triage ${PAYLOAD}`);

  errs = await mountPlugin(
    page,
    'answer-question.js',
    {
      step: {
        id: 's',
        kind: 'answer-question',
        title: 'q',
        status: 'ready',
        metadata: { question: `asked ${PAYLOAD}`, context_md: `context ${PAYLOAD}` },
      },
      jobId: 'job-1',
    },
    { '/api/jobs/job-1': { metadata: {}, steps: [] } },
  );
  await drawnAsText(page, errs, `asked ${PAYLOAD}`);
  // The context goes through the bundle's escape-first markdown — the
  // one named HTML site in this file — and still reads as characters.
  await drawnAsText(page, errs, `context ${PAYLOAD}`);
});
