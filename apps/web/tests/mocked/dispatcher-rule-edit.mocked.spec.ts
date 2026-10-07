import { isPageWrite } from './_smokeMocks';
// /it/registry/rules/:ruleName — the rule editor's four writes, pinned
// (backlog 91e42a8b, page-audit bd5018e1 gap 5), with the gaps that same
// audit filed against them.
//
// WHY. Validate, Save draft, Publish draft and Retire were exercised by
// no mocked spec; the ready state was pinned by one assertion. Each
// write painted its refusal (ensureOk → the action error line), except
// the one escape below, and nothing held that in place — a refusal
// nothing pins is one edit away from being swallowed. So this spec
// pins, per write, what is sent and what is painted:
//   - Validate ok and error; the body is the spec and never a source;
//   - Save draft 201 (the versions are read again), 400 and 403 (each
//     painted as "HTTP <code>: <body>");
//   - Publish disabled with no draft, and Publish 200;
//   - Retire with the confirm cancelled (no POST) and accepted.
// And the gaps it carries:
//   gap 1 (b38360ed) a schedule rule serves no on_event; the form seeded
//                    undefined, Save threw outside its try, and all four
//                    buttons stayed "Saving…" until a reload — FIXED: the
//                    form carries an event OR a schedule and the save keeps
//                    the schedule; the throw would also fail this spec
//                    through ./_test's page-error guard;
//   gap 2 (636b9868) Save sent no source, so the draft door refused every
//                    tenant rule — FIXED: the draft restates the live row's
//                    source, and the banner names boss tenant export;
//   gap 3 (0034d5ef) no provenance or activity — FIXED: a read-only summary,
//                    each of its three reads with its own failure line;
//   gap 4 (9550ac95) the history hid source and authorship and could not
//                    show an old version — FIXED: Source and By columns
//                    (null is "unrecorded") and rows that expand;
//   gap 7 (3a42c67b) the When placeholder taught a syntax no live rule uses,
//                    and the Name placeholder sat on a read-only field —
//                    FIXED.
//
// Fixtures: three rules shaped like live rows on 2026-09-27 — an event
// rule, a schedule rule (no on_event key at all, as the server omits
// it), and a tenant rule. Versions, authors and activity are invented.

import { expect, test, type Page, type Route } from './_test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const EVENT_RULE = 'auto-park-on-gate-green';
const SCHEDULE_RULE = 'cadence-silence-sweep-daily';
const TENANT_RULE = 'complete-site-live-on-converge-closed';
const DEAD_LETTER_JOB = 'a1b2c3d4-0000-4000-8000-000000000002';

const editor = (rule: string) => `/it/registry/rules/${rule}`;
const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

const RULES = /\/api\/dispatcher\/rules$/;
const VALIDATE = /\/api\/dispatcher\/rules\/_validate$/;
const versionsOf = (rule: string) => new RegExp(`/api/dispatcher/rules/${esc(rule)}/versions$`);
const publishOf = (rule: string) => new RegExp(`/api/dispatcher/rules/${esc(rule)}/publish$`);
const retireOf = (rule: string) => new RegExp(`/api/dispatcher/rules/${esc(rule)}/retire$`);
const FIRINGS = /\/api\/yard\/rule-firings$/;
const SCHEDULE = /\/api\/dispatcher\/schedule$/;
/// The shell's own write (App.svelte's surface-open), not this page's.

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const CREATED = '2026-09-20T10:00:00Z';

/// The event rule: v1 retired long ago (no door recorded any act of it),
/// v2 active and drafted/published through the doors.
const EVENT_V1 = {
  name: EVENT_RULE, version: 1, status: 'retired', on_event: 'step.done.gate-verdict',
  when: null, do: [{ handler: 'jobs.auto-park', args: { verdict: 'metadata.verdict' } }], delay: null,
  created_by: null, published_by: null, retired_by: null, created_at: '2026-09-01T09:00:00Z',
};
const EVENT_V2 = {
  name: EVENT_RULE, version: 2, status: 'active', on_event: 'step.done.gate-verdict',
  when: 'verdict = "green"', do: [{ handler: 'jobs.auto-park', args: {} }], delay: null,
  created_by: 'agent-claude', published_by: 'emp-david', retired_by: null, created_at: CREATED,
};
const EVENT_V3_DRAFT = { ...EVENT_V2, version: 3, status: 'draft', published_by: null, created_at: '2026-09-27T11:00:00Z' };

/// The schedule rule, as the server serves it: no `on_event` key.
const SCHEDULE_V1 = {
  name: SCHEDULE_RULE, version: 1, status: 'active',
  schedule: { cadence: 'daily', anchor_date: '2026-09-01' },
  when: null, do: [{ handler: 'cadence.silence-sweep', args: {} }], delay: null,
  created_by: null, published_by: null, retired_by: null, created_at: CREATED,
};

/// The tenant rule: an event rule its tenant declared.
const TENANT_V1 = {
  name: TENANT_RULE, version: 1, status: 'active', on_event: 'jobs.job.closed',
  when: 'kind = "converge"', do: [{ handler: 'jobs.complete-step', args: {} }], delay: null,
  source: 'tenant:algedonic', created_by: 'agent-claude', published_by: 'agent-claude', retired_by: null,
  created_at: CREATED,
};

/// The rules read the list page makes: the three active rows with their
/// provenance, and the handler roster.
const RULES_PAYLOAD = {
  rules: [
    { name: EVENT_RULE, on_event: 'step.done.gate-verdict', when: 'verdict = "green"', do: [{ handler: 'jobs.auto-park', args: {} }], version: 2, status: 'active', why: 'A CROSS-PROTOCOL REACTOR — the gate and the dock are two protocols.', authored: true, source: 'product' },
    { name: SCHEDULE_RULE, schedule: { cadence: 'daily', anchor_date: '2026-09-01' }, when: null, do: [{ handler: 'cadence.silence-sweep', args: {} }], version: 1, status: 'active', why: 'A TIMER — a day passing is no packet.', authored: true, source: 'product' },
    { name: TENANT_RULE, on_event: 'jobs.job.closed', when: 'kind = "converge"', do: [{ handler: 'jobs.complete-step', args: {} }], version: 1, status: 'active', why: null, authored: false, source: 'tenant:algedonic' },
  ],
  handler_emits: { 'jobs.auto-park': ['jobs.car.filed'], 'cadence.silence-sweep': [], 'jobs.complete-step': ['jobs.step.completed'] },
  system_edges: [],
  authored_registry: { dir: '/opt/boss/infra/dispatcher/rules', rules: 2, error: null },
};

const FIRINGS_PAYLOAD = {
  now: '2026-09-27T12:00:00Z',
  retention_days: 30,
  firings: [
    { rule: EVENT_RULE, fired_on: 'step.done.gate-verdict', fired_at: '2026-09-27T11:00:00Z' },
    { rule: TENANT_RULE, fired_on: 'jobs.job.closed', fired_at: '2026-09-25T12:00:00Z' },
  ],
  firings_error: null,
  dead_letters: [
    { rule: TENANT_RULE, packets: 2, unrouted: 0, newest_at: '2026-09-27T11:30:00Z', newest_job_id: DEAD_LETTER_JOB },
  ],
  dead_letters_error: null,
};

const SCHEDULE_PAYLOAD = {
  now: '2026-09-27T12:00:00Z',
  simulated: false,
  retention_days: 30,
  schedule: [
    {
      name: SCHEDULE_RULE, version: 1, cadence: 'daily', anchor_date: '2026-09-01', business_calendar: null,
      when: null, last_fired: null, last_fired_why: 'no firing recorded', next_due: '2026-09-27T15:00:00Z', next_due_why: null,
    },
  ],
  schedule_error: null,
};

type Sent = { method: string; path: string; body: unknown };
type Answer = { status: number; body: unknown };

type Setup = {
  rule: string;
  versions: ReadonlyArray<unknown>;
  /** What the versions read answers after a write succeeds. */
  after?: ReadonlyArray<unknown>;
  draft?: Answer;
  validate?: Answer;
  publish?: Answer;
  retire?: Answer;
};

type Harness = { sent: Sent[]; versionReads: () => number };

async function install(page: Page, setup: Setup): Promise<Harness> {
  const sent: Sent[] = [];
  let versionReads = 0;
  let served = setup.versions;
  page.on('request', (req) => {
    const path = new URL(req.url()).pathname;
    if (path.startsWith('/api/') && req.method() !== 'GET' && isPageWrite(req.method(), path)) {
      const raw = req.postData();
      sent.push({ method: req.method(), path, body: raw ? JSON.parse(raw) : null });
    }
  });
  // A write that succeeds moves the versions read to `after`; Validate
  // persists nothing, so it never does.
  const answer = async (r: Route, a: Answer | undefined, persists = true) => {
    const got = a ?? { status: 500, body: 'no answer planted for this write' };
    if (persists && got.status < 300 && setup.after) served = setup.after;
    if (typeof got.body === 'string') {
      await r.fulfill({ status: got.status, contentType: 'text/plain', body: got.body });
    } else {
      await json(r, got.body, got.status);
    }
  };

  await installSmokeMocks(page);
  await page.route(RULES, (r) => (r.request().method() === 'GET' ? json(r, RULES_PAYLOAD) : answer(r, setup.draft)));
  await page.route(versionsOf(setup.rule), (r) => {
    versionReads += 1;
    return json(r, served);
  });
  await page.route(VALIDATE, (r) => answer(r, setup.validate, false));
  await page.route(publishOf(setup.rule), (r) => answer(r, setup.publish));
  await page.route(retireOf(setup.rule), (r) => answer(r, setup.retire));
  await page.route(FIRINGS, (r) => json(r, FIRINGS_PAYLOAD));
  await page.route(SCHEDULE, (r) => json(r, SCHEDULE_PAYLOAD));
  return { sent, versionReads: () => versionReads };
}

async function open(page: Page, rule: string): Promise<void> {
  await mountPage(page, editor(rule), { titleMatch: new RegExp(esc(rule)) });
  await expect(page.getByRole('button', { name: 'Save draft' })).toBeEnabled();
}

const button = (page: Page, name: string) => page.getByRole('button', { name, exact: true });
const actionError = (page: Page) => page.locator('.catalog .action-error');

/// All four lifecycle buttons, idle: none reads "…ing…" and Validate and
/// Save take a click. The wedge of gap 1 left all four disabled.
async function idle(page: Page): Promise<void> {
  await expect(button(page, 'Validate')).toBeEnabled();
  await expect(button(page, 'Save draft')).toBeEnabled();
  await expect(page.getByText(/Saving…|Validating…|Publishing…|Retiring…/)).toHaveCount(0);
}

test.describe('/it/registry/rules/:ruleName — Validate', () => {
  test('ok paints Valid, and the body is the spec with no source', async ({ page }) => {
    const h = await install(page, { rule: TENANT_RULE, versions: [TENANT_V1], validate: { status: 200, body: { ok: true, error: null } } });
    await open(page, TENANT_RULE);

    await button(page, 'Validate').click();
    await expect(page.getByText('✓ Valid')).toHaveCount(1);
    await idle(page);
    expect(h.sent).toEqual([
      {
        method: 'POST',
        path: '/api/dispatcher/rules/_validate',
        body: { name: TENANT_RULE, on_event: 'jobs.job.closed', when: 'kind = "converge"', do: [{ handler: 'jobs.complete-step', args: {} }], delay: null },
      },
    ]);
  });

  test('a parse error paints the parser’s words', async ({ page }) => {
    await install(page, { rule: EVENT_RULE, versions: [EVENT_V1, EVENT_V2], validate: { status: 200, body: { ok: false, error: 'rule `auto-park-on-gate-green`: bad when' } } });
    await open(page, EVENT_RULE);

    await button(page, 'Validate').click();
    await expect(page.getByText('✗ rule `auto-park-on-gate-green`: bad when')).toHaveCount(1);
    await idle(page);
  });
});

test.describe('/it/registry/rules/:ruleName — Save draft', () => {
  test('201 posts the product rule with no source, and the versions are read again', async ({ page }) => {
    const h = await install(page, {
      rule: EVENT_RULE,
      versions: [EVENT_V1, EVENT_V2],
      after: [EVENT_V1, EVENT_V2, EVENT_V3_DRAFT],
      draft: { status: 201, body: EVENT_V3_DRAFT },
    });
    await open(page, EVENT_RULE);
    await expect(button(page, 'Publish draft')).toBeDisabled();
    expect(await settledReads(page, h.versionReads, 1)).toBe(1);

    await button(page, 'Save draft').click();
    await expect(page.locator('.catalog header.exec-header p')).toHaveText('3 versions · active v2');
    expect(h.versionReads()).toBe(2);
    expect(h.sent).toHaveLength(1);
    expect(h.sent[0]!.path).toBe('/api/dispatcher/rules');
    expect(h.sent[0]!.body).toEqual({
      name: EVENT_RULE, on_event: 'step.done.gate-verdict', when: 'verdict = "green"',
      do: [{ handler: 'jobs.auto-park', args: {} }], delay: null,
    });
    await expect(button(page, 'Publish draft')).toBeEnabled();
    await expect(actionError(page)).toHaveCount(0);
    await idle(page);
  });

  for (const [status, body] of [
    [400, 'rule `auto-park-on-gate-green` is owned by tenant:algedonic; a draft from product cannot supersede it'],
    [403, 'emp-guest may not create dispatcher_rule'],
  ] as const) {
    test(`${status} is painted as HTTP ${status} and its body, and nothing is re-read`, async ({ page }) => {
      const h = await install(page, { rule: EVENT_RULE, versions: [EVENT_V1, EVENT_V2], draft: { status, body } });
      await open(page, EVENT_RULE);

      await button(page, 'Save draft').click();
      await expect(actionError(page)).toHaveText(`HTTP ${status}: ${body}`);
      expect(h.versionReads()).toBe(1);
      await idle(page);
    });
  }

  // GAP 1 (backlog b38360ed): the wedge. The row serves no on_event; on
  // origin/main before this car the click threw "Cannot read properties
  // of undefined (reading 'trim')" outside the try — an unhandled
  // rejection ./_test fails on — and the buttons stayed "Saving…".
  test('a schedule rule seeds the schedule, saves it, and names no topic', async ({ page }) => {
    const draft = { ...SCHEDULE_V1, version: 2, status: 'draft' };
    const h = await install(page, {
      rule: SCHEDULE_RULE,
      versions: [SCHEDULE_V1],
      after: [SCHEDULE_V1, draft],
      draft: { status: 201, body: draft },
      validate: { status: 200, body: { ok: true, error: null } },
    });
    await open(page, SCHEDULE_RULE);

    await expect(page.getByLabel('A schedule')).toBeChecked();
    await expect(page.getByLabel('Cadence')).toHaveValue('daily');
    await expect(page.getByLabel('Anchor date')).toHaveValue('2026-09-01');
    await expect(page.getByLabel('On event')).toHaveCount(0);

    await button(page, 'Validate').click();
    await expect(page.getByText('✓ Valid')).toHaveCount(1);
    await button(page, 'Save draft').click();
    await expect(page.locator('.catalog header.exec-header p')).toHaveText('2 versions · active v1');
    await idle(page);

    const expected = {
      name: SCHEDULE_RULE, schedule: { cadence: 'daily', anchor_date: '2026-09-01' }, when: null,
      do: [{ handler: 'cadence.silence-sweep', args: {} }], delay: null,
    };
    expect(h.sent.map((s) => [s.path, s.body])).toEqual([
      ['/api/dispatcher/rules/_validate', expected],
      ['/api/dispatcher/rules', expected],
    ]);
  });

  test('a topic typed on the event side is not sent while the trigger is the schedule', async ({ page }) => {
    const h = await install(page, { rule: SCHEDULE_RULE, versions: [SCHEDULE_V1], draft: { status: 201, body: SCHEDULE_V1 } });
    await open(page, SCHEDULE_RULE);

    await page.getByLabel('An event').check();
    await page.getByLabel('On event').fill('typed.by.mistake');
    await page.getByLabel('A schedule').check();
    await button(page, 'Save draft').click();
    await idle(page);
    expect(h.sent).toHaveLength(1);
    expect(h.sent[0]!.body).not.toHaveProperty('on_event');
    expect(h.sent[0]!.body).toHaveProperty('schedule', { cadence: 'daily', anchor_date: '2026-09-01' });
  });

  test('a schedule with no cadence is refused on the page, and nothing is sent', async ({ page }) => {
    const h = await install(page, { rule: SCHEDULE_RULE, versions: [SCHEDULE_V1] });
    await open(page, SCHEDULE_RULE);

    await page.getByLabel('Cadence').fill('');
    await button(page, 'Save draft').click();
    await expect(actionError(page)).toHaveText('A schedule needs a cadence.');
    await idle(page);
    expect(h.sent).toEqual([]);
  });

  // GAP 2 (backlog 636b9868): the draft restates the live row's source.
  test('a tenant rule’s draft carries its tenant source, and the banner names the export', async ({ page }) => {
    const draft = { ...TENANT_V1, version: 2, status: 'draft', published_by: null };
    const h = await install(page, {
      rule: TENANT_RULE,
      versions: [TENANT_V1],
      after: [TENANT_V1, draft],
      draft: { status: 201, body: draft },
    });
    await open(page, TENANT_RULE);

    await expect(page.locator('.catalog .publish-banner')).toContainText('boss tenant export');
    await expect(page.locator('.catalog .publish-banner')).toContainText('tenant:algedonic');
    await button(page, 'Save draft').click();
    await expect(page.locator('.catalog header.exec-header p')).toHaveText('2 versions · active v1');
    expect(h.sent).toHaveLength(1);
    expect(h.sent[0]!.body).toHaveProperty('source', 'tenant:algedonic');
    await idle(page);
  });

  test('a product rule’s banner names its file under infra/dispatcher/rules/', async ({ page }) => {
    await install(page, { rule: EVENT_RULE, versions: [EVENT_V1, EVENT_V2] });
    await open(page, EVENT_RULE);
    await expect(page.locator('.catalog .publish-banner')).toContainText(`infra/dispatcher/rules/${EVENT_RULE}.toml`);
    await expect(page.locator('.catalog .publish-banner')).not.toContainText('boss tenant export');
  });
});

test.describe('/it/registry/rules/:ruleName — Publish draft', () => {
  test('with no draft the button is disabled and says why', async ({ page }) => {
    await install(page, { rule: EVENT_RULE, versions: [EVENT_V1, EVENT_V2] });
    await open(page, EVENT_RULE);
    await expect(button(page, 'Publish draft')).toBeDisabled();
    await expect(button(page, 'Publish draft')).toHaveAttribute('title', 'No draft to publish');
  });

  test('200 posts to the publish door and the versions are read again', async ({ page }) => {
    const published = [
      EVENT_V1,
      { ...EVENT_V2, status: 'retired', retired_by: 'emp-david' },
      { ...EVENT_V3_DRAFT, status: 'active', published_by: 'emp-david' },
    ];
    const h = await install(page, {
      rule: EVENT_RULE,
      versions: [EVENT_V1, EVENT_V2, EVENT_V3_DRAFT],
      after: published,
      publish: { status: 200, body: published[2] },
    });
    await open(page, EVENT_RULE);

    await button(page, 'Publish draft').click();
    await expect(page.locator('.catalog header.exec-header p')).toHaveText('3 versions · active v3');
    expect(h.sent).toEqual([{ method: 'POST', path: `/api/dispatcher/rules/${EVENT_RULE}/publish`, body: null }]);
    expect(h.versionReads()).toBe(2);
    await expect(button(page, 'Publish draft')).toBeDisabled();
    await idle(page);
  });

  test('a refusal is painted', async ({ page }) => {
    await install(page, { rule: EVENT_RULE, versions: [EVENT_V1, EVENT_V2, EVENT_V3_DRAFT], publish: { status: 422, body: 'rule would not load: x' } });
    await open(page, EVENT_RULE);
    await button(page, 'Publish draft').click();
    await expect(actionError(page)).toHaveText('HTTP 422: rule would not load: x');
    await idle(page);
  });
});

test.describe('/it/registry/rules/:ruleName — Retire', () => {
  test('cancelling the confirm sends nothing', async ({ page }) => {
    const h = await install(page, { rule: EVENT_RULE, versions: [EVENT_V1, EVENT_V2] });
    await open(page, EVENT_RULE);

    const asked: string[] = [];
    page.once('dialog', (d) => {
      asked.push(d.message());
      void d.dismiss();
    });
    await button(page, 'Retire').click();
    await expect.poll(() => asked.length).toBe(1);
    expect(asked[0]).toContain(`Retire rule "${EVENT_RULE}"?`);
    await idle(page);
    expect(h.sent).toEqual([]);
  });

  test('accepting posts to the retire door and the versions are read again', async ({ page }) => {
    const h = await install(page, {
      rule: EVENT_RULE,
      versions: [EVENT_V1, EVENT_V2],
      after: [EVENT_V1, { ...EVENT_V2, status: 'retired', retired_by: 'emp-david' }],
      retire: { status: 204, body: '' },
    });
    await open(page, EVENT_RULE);

    page.once('dialog', (d) => void d.accept());
    await button(page, 'Retire').click();
    await expect(page.locator('.catalog header.exec-header p')).toHaveText('2 versions · no active version');
    expect(h.sent).toEqual([{ method: 'POST', path: `/api/dispatcher/rules/${EVENT_RULE}/retire`, body: null }]);
    await expect(button(page, 'Retire')).toBeDisabled();
    await idle(page);
  });
});

// GAP 3 (backlog 0034d5ef): what the list knows about a rule, on the
// page where Retire is decided.
test.describe('/it/registry/rules/:ruleName — the summary', () => {
  const lines = async (page: Page) => {
    const block = page.locator('.catalog .rule-summary');
    const dt = (await block.locator('dt').allTextContents()).map((s) => s.trim());
    const dd = (await block.locator('dd').allTextContents()).map((s) => s.trim().replace(/\s+/g, ' '));
    return Object.fromEntries(dt.map((k, i) => [k, dd[i]]));
  };

  test('an event rule: why, who declared it, when it last fired, dead-letters, what it emits', async ({ page }) => {
    await install(page, { rule: EVENT_RULE, versions: [EVENT_V1, EVENT_V2] });
    await open(page, EVENT_RULE);
    await expect(page.locator('.catalog .rule-summary dd.last-fired')).toHaveText('1h ago');

    expect(await lines(page)).toEqual({
      Why: 'A CROSS-PROTOCOL REACTOR — the gate and the dock are two protocols.',
      'Declared by': 'file — authored under infra/dispatcher/rules/ — a restart keeps it',
      'Last fired': '1h ago',
      'Dead-letters': 'none',
      Emits: 'jobs.car.filed',
    });
    await expect(page.locator(`.catalog ${FAILURE_MARKER}`)).toHaveCount(0);
  });

  test('a schedule rule says when it is next due', async ({ page }) => {
    await install(page, { rule: SCHEDULE_RULE, versions: [SCHEDULE_V1] });
    await open(page, SCHEDULE_RULE);
    await expect(page.locator('.catalog .rule-summary dd.next-due')).toHaveText('in 3h');
    await expect(page.locator('.catalog .rule-summary dd.next-due')).toHaveAttribute('title', 'next due 2026-09-27T15:00:00Z');
    expect((await lines(page))['Emits']).toBe('nothing — its handler is a sink');
  });

  test('a failing tenant rule says failing and links the packet holding its newest dead-letter', async ({ page }) => {
    await install(page, { rule: TENANT_RULE, versions: [TENANT_V1] });
    await open(page, TENANT_RULE);
    const dead = page.locator('.catalog .rule-summary dd.dead-letters');
    await expect(dead).toHaveClass(/failing/);
    await expect(dead).toHaveText('2, newest 30m ago — failing since its last firing');
    await expect(dead.locator('a')).toHaveAttribute('href', `/jobs/${DEAD_LETTER_JOB}`);
    expect((await lines(page))['Declared by']).toContain('tenant:algedonic');
    // A tenant's why lives in its seeds, which the dispatcher does not
    // read — said so, rather than printing nothing.
    expect((await lines(page))['Why']).toBe('kept in the tenant’s seeds/rules.toml, which this dispatcher does not read');
  });

  test('each read that fails paints its own marked line, and the editor still paints', async ({ page }) => {
    await install(page, { rule: SCHEDULE_RULE, versions: [SCHEDULE_V1] });
    await page.route(RULES, (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'dispatcher down' }));
    await page.route(FIRINGS, (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'jobs down' }));
    await page.route(SCHEDULE, (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'dispatcher down' }));
    await open(page, SCHEDULE_RULE);

    await expect(page.locator(`.catalog .rule-summary ${FAILURE_MARKER}[role=alert]`)).toHaveText([
      'Couldn’t read the rule registry: HTTP 503: dispatcher down',
      'Couldn’t read the firing record: /api/yard/rule-firings: HTTP 503',
      'Couldn’t read the schedule: /api/dispatcher/schedule: HTTP 503',
    ]);
    await expect(page.locator('.catalog .rule-summary dd.last-fired')).toHaveText('unknown');
    await expect(page.locator('.catalog .rule-summary dd.next-due')).toHaveText('unknown');
    await expect(page.getByLabel('Cadence')).toHaveValue('daily');
  });
});

// GAP 4 (backlog 9550ac95) and GAP 7 (backlog 3a42c67b).
test.describe('/it/registry/rules/:ruleName — the history and the form', () => {
  test('the history names source and authorship, null as unrecorded, and a row expands to its content', async ({ page }) => {
    await install(page, { rule: EVENT_RULE, versions: [EVENT_V1, EVENT_V2] });
    await open(page, EVENT_RULE);

    const history = page.locator('.catalog table.version-history');
    const heads = (await history.locator('thead th').allTextContents()).map((s) => s.trim());
    expect(heads).toEqual(['Version', 'Status', 'Source', 'By', 'Created', '']);
    const row = (v: number) => history.locator(`tr.version-row[data-version="${v}"]`);
    await expect(row(1).locator('td.source')).toHaveText('product');
    await expect(row(1).locator('td.by')).toHaveText(
      'drafted by (unrecorded) · published by (unrecorded) · retired by (unrecorded)',
    );
    await expect(row(2).locator('td.by')).toHaveText('drafted by agent-claude · published by emp-david');

    await expect(history.locator('tr.version-detail')).toHaveCount(0);
    await row(1).getByRole('button', { name: 'Show' }).click();
    const detail = history.locator('tr.version-detail');
    await expect(detail).toHaveCount(1);
    await expect(detail).toContainText('on step.done.gate-verdict');
    await expect(detail).toContainText('jobs.auto-park');
    await expect(detail).toContainText('verdict = metadata.verdict');
    await row(1).getByRole('button', { name: 'Hide' }).click();
    await expect(history.locator('tr.version-detail')).toHaveCount(0);
  });

  test('the When placeholder is a live-shaped predicate, and the read-only Name has none', async ({ page }) => {
    await install(page, { rule: EVENT_RULE, versions: [EVENT_V1, EVENT_V2] });
    await open(page, EVENT_RULE);
    await expect(page.getByLabel('When')).toHaveAttribute('placeholder', 'kind = "ship-a-change" AND outcome = "merged"');
    await expect(page.getByLabel('Name')).not.toHaveAttribute('placeholder');
    await expect(page.getByLabel('Name')).toHaveAttribute('readonly', '');
  });
});
