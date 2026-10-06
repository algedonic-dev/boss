// Whole control completion for page audit d2e86594. The existing
// views-page and views-sources specs retain the landed gap repairs;
// this file exercises the composer, write effects, every result link
// and Back, and honest empty/malformed answers against private data.
import { expect, test, type Page, type Route } from './_test';
import { installApiFloor, servePeopleRows, VIEW_SOURCES_FIXTURE } from './_smokeMocks';
import { mountPage, recordPageRequests, openedRequests } from './_helpers';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import type { View, ViewInput, ViewResults } from '../../src/views/types';
import { parseRoute } from '../../src/router';

const PATH = ROUTE_CATALOG.views.path;
const ME = {
  id: 'emp-views', name: 'Views Operator', email: null, role: 'platform-admin', department: 'it',
  hire_date: null, status: 'active', location: null, employment_type: null,
  skills: [], certifications: [],
};
const json = (r: Route, body: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
const view = (extra: Partial<View> = {}): View => ({
  id: 'view-private', owner_id: ME.id, title: 'Private question', source: 'jobs',
  filter: '', columns: [], layout: 'table', visibility: 'private',
  created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-01T00:00:00Z', ...extra,
});
const result = (extra: Partial<ViewResults> = {}): ViewResults => ({
  view_id: 'view-private', source: 'jobs', layout: 'table', rows: [],
  matched: 0, pushed_down: 0, truncated: false, scope: 'all', ...extra,
});
const row = (page: Page, id = 'view-private') => page.locator(`[data-view="${id}"]`);
const form = (page: Page) => page.locator('.v-form');
type Write = Readonly<{ method: string; path: string; body: unknown }>;
async function install(page: Page, initial: ReadonlyArray<View> = [view()]) {
  await installApiFloor(page);
  await servePeopleRows(page, [ME]);
  await page.route(/\/api\/session$/, (r) => json(r, {
    username: 'operator', employee_id: ME.id, role: 'platform-admin',
  }));
  let saved = [...initial];
  const writes: Write[] = [];
  const reads: string[] = [];
  await page.route(/\/api\/views(?:\/[^/?]+)?$/, (r) => {
    const req = r.request();
    const path = new URL(req.url()).pathname;
    if (req.method() === 'GET') {
      reads.push(path);
      return path === '/api/views' ? json(r, saved) : r.fallback();
    }
    const body: unknown = req.method() === 'DELETE' ? null : req.postDataJSON();
    writes.push({ method: req.method(), path, body });
    if (req.method() === 'DELETE') {
      saved = saved.filter((v) => `/api/views/${v.id}` !== path);
      return r.fulfill({ status: 204 });
    }
    const input = body as ViewInput;
    const next = view({ ...input, id: path === '/api/views' ? 'view-created' : path.split('/').pop()! });
    saved = path === '/api/views' ? [...saved, next] : saved.map((v) => v.id === next.id ? next : v);
    return json(r, next);
  });
  return { writes, reads };
}

for (const [shape, body] of [['object', {}], ['null', null], ['envelope', { data: [] }]] as const) {
  test(`a malformed successful Views list (${shape}) says the read failed instead of disappearing`, async ({ page }) => {
    await install(page);
    await page.route(/\/api\/views$/, (r) => json(r, body));
    await mountPage(page, PATH);
    await expect(page.locator('.v-list-failed.load-failed[role="alert"]')).toContainText('body');
    await expect(page.locator('.v-view')).toHaveCount(0);
    await expect(page.getByText('No views yet.', { exact: false })).toHaveCount(0);
  });
}

test('composer inventory, every source and column chip, layout and visibility submit the actual draft', async ({ page }) => {
  const seen = await install(page, []);
  await mountPage(page, PATH);
  await expect(page.getByRole('heading', { name: 'Views', exact: true })).toBeVisible();
  await expect(form(page).locator('input')).toHaveCount(2);
  await expect(form(page).locator('select')).toHaveCount(3);
  await expect(form(page).locator('form')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Save view', exact: true })).toBeDisabled();
  await expect(page.getByText('No views yet.', { exact: false })).toContainText('A view is a saved question');
  await page.getByLabel('Title', { exact: true }).fill('  Shared event question  ');
  await page.getByRole('textbox', { name: 'Filter optional' }).fill('  kind = "jobs.job.opened"  ');
  for (const source of VIEW_SOURCES_FIXTURE.sources) {
    await page.getByRole('combobox', { name: 'Source', exact: true }).selectOption(source.source);
    const chips = form(page).locator('.v-chips');
    await expect(chips.getByRole('button')).toHaveText([...source.fields]);
    for (const field of source.fields) {
      const chip = chips.getByRole('button', { name: field, exact: true });
      await chip.click();
      await expect(chip).toHaveAttribute('aria-pressed', 'true');
      await chip.click();
      await expect(chip).toHaveAttribute('aria-pressed', 'false');
    }
  }
  await form(page).getByRole('button', { name: 'id', exact: true }).click();
  await page.getByRole('combobox', { name: 'Layout', exact: true }).selectOption('list');
  await page.getByRole('combobox', { name: 'Visibility', exact: true }).selectOption('shared');
  await page.getByRole('button', { name: 'Save view', exact: true }).click();
  await expect(row(page, 'view-created').getByRole('heading')).toHaveText('Shared event question');
  expect(seen.writes).toEqual([{ method: 'POST', path: '/api/views', body: {
    title: 'Shared event question', source: 'events', filter: 'kind = "jobs.job.opened"',
    columns: ['id'], layout: 'list', visibility: 'shared',
  } }]);
  await expect(page.getByLabel('Title', { exact: true })).toHaveValue('');
  await expect(page.getByRole('button', { name: 'Save view', exact: true })).toBeDisabled();
});

test('Edit fills all draft fields, Cancel changes nothing, replacement clears the stale result, and Delete removes only its row', async ({ page }) => {
  const initial = view({ filter: 'status = "open"', columns: ['id'], layout: 'count', visibility: 'shared' });
  const seen = await install(page, [initial, view({ id: 'view-second', title: 'Second question' })]);
  await page.route(/\/api\/views\/view-private\/results\?limit=100$/, (r) => json(r, result({ layout: initial.layout, matched: 3 })));
  await mountPage(page, PATH);
  await row(page).getByRole('button', { name: 'Run', exact: true }).click();
  await expect(row(page).locator('.v-count')).toContainText('3 matches');
  await row(page).getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(page.getByRole('combobox', { name: 'Source', exact: true })).toHaveValue('jobs');
  await expect(page.getByRole('combobox', { name: 'Layout', exact: true })).toHaveValue('count');
  await expect(page.getByRole('combobox', { name: 'Visibility', exact: true })).toHaveValue('shared');
  await expect(form(page).getByRole('button', { name: 'id', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await page.getByLabel('Title', { exact: true }).fill('Discard this');
  await page.getByRole('button', { name: 'Cancel edit', exact: true }).click();
  expect(seen.writes).toEqual([]);
  await expect(row(page).getByRole('heading')).toHaveText(initial.title);
  await row(page).getByRole('button', { name: 'Edit', exact: true }).click();
  await page.getByLabel('Title', { exact: true }).fill('Replaced question');
  await page.getByRole('combobox', { name: 'Layout', exact: true }).selectOption('table');
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await expect(row(page).getByRole('heading')).toHaveText('Replaced question');
  await expect(row(page).locator('.v-count')).toHaveCount(0);
  expect(seen.writes[0]).toEqual({ method: 'PUT', path: '/api/views/view-private', body: {
    title: 'Replaced question', source: 'jobs', filter: 'status = "open"',
    columns: ['id'], layout: 'table', visibility: 'shared',
  } });
  await row(page).getByRole('button', { name: 'Delete', exact: true }).click();
  await expect(row(page)).toHaveCount(0);
  await expect(row(page, 'view-second')).toBeVisible();
  expect(seen.writes[1]).toEqual({ method: 'DELETE', path: '/api/views/view-private', body: null });
});

for (const layout of ['table', 'list', 'count'] as const) {
  test(`${layout} Run paints genuine empty data and rerun replaces the previous answer`, async ({ page }) => {
    await install(page, [view({ layout })]);
    let runs = 0;
    await page.route(/\/api\/views\/view-private\/results\?limit=100$/, (r) => {
      runs += 1;
      return json(r, result({ layout, matched: runs === 1 ? 1 : 0,
        rows: runs === 1 ? [{ title: 'First answer', nullable: null, metadata: { department: 'it' } }] : [] }));
    });
    await recordPageRequests(page);
    await mountPage(page, PATH);
    await row(page).getByRole('button', { name: 'Run', exact: true }).click();
    await expect(row(page).locator('.v-count')).toContainText('1 match');
    if (layout !== 'count') await expect(row(page)).toContainText('First answer');
    else await expect(row(page).locator('table,ul')).toHaveCount(0);
    await row(page).getByRole('button', { name: 'Run', exact: true }).click();
    await expect(row(page).locator('.v-count')).toContainText('0 matches');
    await expect(row(page).locator('.v-shown')).toHaveText(/^showing 0 of 0 · read \d\d:\d\d$/);
    await expect(row(page).locator('table,ul,.load-failed')).toHaveCount(0);
    expect(runs).toBe(2);
    expect((await openedRequests(page)).filter((r) => r.method !== 'GET' && r.path !== '/api/surface-opens')).toEqual([]);
  });
}

for (const layout of ['table', 'list'] as const) {
  test(`${layout} result Job, step and subject links open their catalogued surfaces and Back restores Views`, async ({ page }) => {
    const jobId = '12345678-1234-4234-8234-123456789abc';
    const stepId = 'step-linked';
    const step = { id: stepId, job_id: jobId, kind: 'task', title: 'Linked step',
      status: 'ready', sort_order: 0, assignee_id: null, blocked_by: [],
      fields: [], sign_offs_required: [], sign_offs: [], metadata: {}, completed_on: null };
    const packet = { id: jobId, kind: 'ad-hoc', workflow_version: 1, title: 'Linked Views packet',
      status: 'open', priority: 'standard', owner_id: ME.id,
      subject: { subject_kind: 'employee', id: ME.id }, opened_on: '2026-09-01',
      due_on: null, closed_on: null, metadata: {}, steps: [step] };
    const views = [
      view({ id: 'jobs-view', source: 'jobs', layout }),
      view({ id: 'steps-view', source: 'steps', layout }),
      view({ id: 'subjects-view', source: 'subjects', layout }),
    ];
    await install(page, views);
    await recordPageRequests(page);
    await page.route(new RegExp(`/api/jobs/${jobId}$`), (r) => json(r, packet));
    await page.route(/\/api\/views\/[^/]+\/results\?limit=100$/, (r) => {
      const id = new URL(r.request().url()).pathname.split('/')[3];
      if (!id) throw new Error('private View result fixture missing id');
      const source = id === 'jobs-view' ? 'jobs' : id === 'steps-view' ? 'steps' : 'subjects';
      const data = source === 'jobs' ? { id: jobId, title: packet.title }
        : source === 'steps' ? { id: stepId, job_id: jobId, title: step.title }
          : { kind: 'employee', id: ME.id, name: ME.name };
      return json(r, result({ view_id: id, source, layout, rows: [data], matched: 1 }));
    });
    await mountPage(page, PATH);
    const links = [
      { view: 'jobs-view', label: jobId, href: `${ROUTE_CATALOG.jobs.path}/${jobId}`, kind: 'jobDetail' },
      { view: 'steps-view', label: stepId, href: `${ROUTE_CATALOG.jobs.path}/${jobId}/steps/${stepId}?from=%2Fux%2Fviews&from_label=Views`, kind: 'stepFocus' },
      { view: 'steps-view', label: jobId, href: `${ROUTE_CATALOG.jobs.path}/${jobId}`, kind: 'jobDetail' },
      { view: 'subjects-view', label: ME.id, href: `${ROUTE_CATALOG.people.path}/${ME.id}`, kind: 'employee' },
    ];
    for (const link of links) {
      await row(page, link.view).getByRole('button', { name: 'Run', exact: true }).click();
      const anchor = row(page, link.view).getByRole('link', { name: link.label, exact: true });
      await expect(anchor).toHaveAttribute('href', link.href);
      const url = new URL(link.href, page.url());
      expect(parseRoute(url.pathname, url.search).kind).toBe(link.kind);
      await anchor.click();
      await expect.poll(() => new URL(page.url()).pathname + new URL(page.url()).search).toBe(link.href);
      if (link.kind === 'employee') await expect(page.locator('h1.detail-title')).toHaveText(ME.name);
      else if (link.kind === 'stepFocus') await expect(page.locator('.step-focus-title')).toHaveText(step.title);
      else await expect(page.getByRole('heading', { name: packet.title, exact: true })).toBeVisible();
      await page.goBack();
      await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
      await expect(page.getByRole('heading', { name: 'Views', exact: true })).toBeVisible();
      await expect(row(page, link.view)).toBeVisible();
    }
    expect((await openedRequests(page)).filter((r) => r.method !== 'GET' && r.path !== '/api/surface-opens')).toEqual([]);
  });
}

test('pending list, Run and Save state names the actual outstanding request and returns to its settled surface', async ({ page }) => {
  await install(page);
  let listRead: Route | undefined;
  await page.route(/\/api\/views$/, (r) => {
    if (r.request().method() === 'GET') listRead = r;
    else return r.fallback();
  });
  await mountPage(page, PATH);
  await expect(page.getByText('Loading views…', { exact: true })).toBeVisible();
  await expect.poll(() => Boolean(listRead)).toBe(true);
  if (!listRead) throw new Error('private list request never arrived');
  await json(listRead, [view()]);
  let runRead: Route | undefined;
  await page.route(/\/api\/views\/view-private\/results\?limit=100$/, (r) => { runRead = r; });
  await row(page).getByRole('button', { name: 'Run', exact: true }).click();
  await expect(row(page).getByRole('button', { name: 'Running…', exact: true })).toBeDisabled();
  await expect.poll(() => Boolean(runRead)).toBe(true);
  if (!runRead) throw new Error('private Run request never arrived');
  await json(runRead, result());
  await expect(row(page).getByRole('button', { name: 'Run', exact: true })).toBeEnabled();
  let saveWrite: Route | undefined;
  await page.route(/\/api\/views$/, (r) => {
    if (r.request().method() === 'POST') saveWrite = r;
    else return json(r, [view(), view({ id: 'view-created', title: 'New saved question' })]);
  });
  await page.getByLabel('Title').fill('New saved question');
  await page.getByRole('button', { name: 'Save view', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Saving…', exact: true })).toBeDisabled();
  await expect.poll(() => Boolean(saveWrite)).toBe(true);
  if (!saveWrite) throw new Error('private Save request never arrived');
  await json(saveWrite, view({ id: 'view-created', title: 'New saved question' }));
  await expect(row(page, 'view-created').getByRole('heading')).toHaveText('New saved question');
});

for (const [shape, body] of [
  ['object', {}], ['null', null], ['invalid count', result({ matched: -1 })],
] as const) {
  test(`a malformed successful Run (${shape}) answers as failure instead of throwing or claiming zero matches`, async ({ page }) => {
    await install(page);
    await page.route(/\/api\/views\/view-private\/results\?limit=100$/, (r) => json(r, body));
    await mountPage(page, PATH);
    await row(page).getByRole('button', { name: 'Run', exact: true }).click();
    await expect(row(page).locator('.load-failed[role="alert"]')).toContainText('body');
    await expect(row(page).locator('.v-count')).toHaveCount(0);
  });
}

for (const [method, action, label] of [
  ['POST', 'new', 'Save view'], ['PUT', 'edit', 'Save changes'], ['DELETE', 'delete', 'Delete'],
] as const) {
  test(`${method} network refusal stays at its own control and preserves draft and rows`, async ({ page }) => {
    await install(page);
    await page.route(/\/api\/views(?:\/view-private)?$/, (r) =>
      r.request().method() === method ? r.abort('failed') : r.fallback());
    await mountPage(page, PATH);
    if (action === 'new') await page.getByLabel('Title').fill('Keep this draft');
    if (action === 'edit') await row(page).getByRole('button', { name: 'Edit', exact: true }).click();
    await (action === 'delete' ? row(page) : form(page)).getByRole('button', { name: label, exact: true }).click();
    const failed = (action === 'delete' ? row(page) : form(page)).locator('.load-failed[role="alert"]');
    await expect(failed).toContainText(`${action === 'delete' ? 'delete' : 'save'} refused:`);
    await expect(row(page)).toBeVisible();
    await expect(page.locator('.v-list-failed')).toHaveCount(0);
    if (action !== 'delete') await expect(page.getByLabel('Title')).toHaveValue(action === 'new' ? 'Keep this draft' : 'Private question');
  });
}

for (const source of ['jobs', 'steps', 'subjects', 'events'] as const) {
  test(`${source} weak scan names its own served ceiling and stronger truncation states a floor`, async ({ page }) => {
    await install(page, [view({ source, filter: 'label = "needle"', layout: 'count' })]);
    let runs = 0;
    await page.route(/\/api\/views\/view-private\/results\?limit=100$/, (r) => {
      runs += 1;
      return json(r, result({ source, layout: 'count', matched: 2, truncated: true, pushed_down: runs === 1 ? 0 : 1 }));
    });
    await mountPage(page, PATH);
    await row(page).getByRole('button', { name: 'Run', exact: true }).click();
    await expect(row(page).locator('.v-trunc')).toContainText(`newest 5,000 ${source} were examined`);
    await expect(row(page).locator('.v-trunc')).toContainText('Older matches are not counted.');
    await row(page).getByRole('button', { name: 'Run', exact: true }).click();
    await expect(row(page).locator('.v-trunc')).toHaveText('— more than this matched; the count is a floor, not a total');
    await expect(row(page).locator('.v-trunc')).not.toContainText('were examined');
  });
}

test('a shared owner name failure stays attributed to that identity and exposes no owner write controls', async ({ page }) => {
  await install(page, [view({ owner_id: 'emp-unread', visibility: 'shared' })]);
  await page.route(/\/api\/people\/emp-unread$/, (r) => json(r, { error: 'no people read' }, 403));
  await mountPage(page, PATH);
  await expect(row(page)).toContainText('shared by emp-unread (name unread:');
  await expect(row(page)).toContainText('HTTP 403');
  await expect(row(page).getByRole('button')).toHaveText(['Run']);
});

test('unreachable list and sources reads remain failures while successful empty data stays separate', async ({ page }) => {
  await install(page, []);
  await page.route(/\/api\/views$/, (r) => r.abort('failed'));
  await page.route(/\/api\/views\/sources$/, (r) => r.abort('failed'));
  await mountPage(page, PATH);
  await expect(page.locator('.v-list-failed[role="alert"]')).toBeVisible();
  await expect(form(page).locator('.load-failed[role="alert"]')).toContainText('fields could not be read');
  await expect(form(page).locator('.v-chips')).toHaveCount(0);
  await expect(page.getByText('No views yet.', { exact: false })).toHaveCount(0);
});
