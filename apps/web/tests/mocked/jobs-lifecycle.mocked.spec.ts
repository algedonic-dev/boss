import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.jobs.path;
const ID = 'a1111111-1111-4111-8111-111111111111';
const row = {
  id: ID, kind: 'review-a-widget', title: 'Review the widget',
  subject: { subject_kind: 'custom', id: 'widget-1' }, owner_id: 'emp-001',
  status: 'open', priority: 'standard', opened_on: '2026-09-01',
  opened_at: '2026-09-01T10:00:00Z', closed_on: null, due_on: null,
  metadata: {}, tags: [],
  steps: [
    { id: 'active', job_id: ID, kind: 'task', title: 'Inspect the widget',
      status: 'active', assignee_id: 'agent-codex', sort_order: 2,
      blocked_by: [], completed_on: null, slim: true },
    { id: 'ready', job_id: ID, kind: 'sign-off', title: 'Review the inspection',
      status: 'ready', assignee_id: null, sort_order: 3,
      blocked_by: [], completed_on: null, slim: true },
    { id: 'pending', job_id: ID, kind: 'task', title: 'Publish the inspection',
      status: 'pending', assignee_id: 'emp-owner', sort_order: 4,
      blocked_by: ['ready'], completed_on: null, slim: true },
  ],
};

async function install(page: Page, rows: readonly unknown[]): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\?/, route => route.fulfill({
    contentType: 'application/json', body: JSON.stringify({ data: rows, total: rows.length }),
  }));
}

test('a slim listed packet shows every ready or active step and its actual holder', async ({ page }) => {
  await install(page, [row]);
  await mountPage(page, PATH);
  const packet = page.locator('table.data-table tbody tr');
  await expect(packet).toContainText('Inspect the widget');
  await expect(packet).toContainText('active');
  await expect(packet).toContainText('agent-codex');
  await expect(packet).toContainText('Review the inspection');
  await expect(packet).toContainText('ready');
  await expect(packet).toContainText('Unassigned');
  await expect(packet).not.toContainText('Publish the inspection');
  await expect(packet).not.toContainText('emp-owner');
});

test('a closed packet shows its recorded closed date without inventing a current holder', async ({ page }) => {
  await install(page, [{ ...row, status: 'closed', closed_on: '2026-09-30',
    steps: row.steps.map(step => ({ ...step, status: 'completed', completed_on: '2026-09-30' })) }]);
  await mountPage(page, `${PATH}?status=closed`);
  const packet = page.locator('table.data-table tbody tr');
  await expect(packet).toContainText('2026-09-30');
  await expect(packet).not.toContainText('agent-codex');
  await expect(packet).not.toContainText('Unassigned');
});

const registry = [
  { kind: 'ordinary-looking', label: 'A machine', subject_kinds: ['custom'], metadata: { list_group: { code: 'machine', label: 'Machine activity' } } },
  { kind: 'maintenance-human', label: 'A human task', subject_kinds: ['custom'], metadata: {} },
];
const groupControl = (page: Page) => page.locator('.job-filters label').filter({ has: page.locator('span', { hasText: /^Kind group$/ }) }).locator('select');
const orderControl = (page: Page) => page.locator('.job-filters label').filter({ has: page.locator('span', { hasText: /^Order$/ }) }).locator('select');

async function grouped(page: Page): Promise<URLSearchParams[]> {
  await installSmokeMocks(page);
  await page.route(/\/api\/workflows$/, route => route.fulfill({ contentType: 'application/json', body: JSON.stringify(registry) }));
  const reads: URLSearchParams[] = [];
  const rows = Array.from({ length: 203 }, (_, index) => ({
    ...row, id: `a1111111-1111-4111-8111-${String(index).padStart(12, '0')}`,
    kind: index < 201 ? 'ordinary-looking' : index === 201 ? 'maintenance-human' : 'historic-unknown',
    title: `Packet ${index}`, steps: [],
  }));
  await page.route(/\/api\/jobs\?/, route => {
    const query = new URL(route.request().url()).searchParams;
    reads.push(query);
    const includes: string[] | null = query.has('kinds') ? JSON.parse(query.get('kinds') ?? '') : null;
    const excludes: string[] = query.has('exclude_kinds') ? JSON.parse(query.get('exclude_kinds') ?? '') : [];
    const matches = rows.filter(packet => (!includes || includes.includes(packet.kind)) && !excludes.includes(packet.kind));
    const ordered = query.get('order') === 'oldest' ? matches.slice().reverse() : matches;
    const offset = Number(query.get('offset') ?? 0);
    return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ data: ordered.slice(offset, offset + 200), total: matches.length }) });
  });
  return reads;
}

test('registry groups narrow the whole counted collection and page, with unknown kinds kept in Other', async ({ page }) => {
  const reads = await grouped(page);
  await mountPage(page, PATH);
  await groupControl(page).selectOption('group:machine');
  await expect.poll(() => reads.at(-1)?.get('kinds')).toBe('["ordinary-looking"]');
  await expect(page.locator('header.exec-header p')).toHaveText('201 open');
  await expect(page.locator('table.data-table tbody tr')).toHaveCount(200);
  await page.getByRole('button', { name: 'Next', exact: true }).click();
  await expect.poll(() => reads.at(-1)?.get('offset')).toBe('200');
  await expect(page.locator('table.data-table tbody tr')).toHaveCount(1);
  await expect(page.locator('nav.job-pager p')).toHaveText('Showing 201–201 of 201, newest first');
  await groupControl(page).selectOption('other');
  await expect.poll(() => reads.at(-1)?.get('exclude_kinds')).toBe('["ordinary-looking"]');
  await expect.poll(() => reads.at(-1)?.has('offset')).toBe(false);
  await expect(page.locator('header.exec-header p')).toHaveText('2 open');
  await expect(page.locator('table.data-table tbody tr')).toHaveCount(2);
  await expect(page.locator('table.data-table')).toContainText('maintenance-human');
  await expect(page.locator('table.data-table')).toContainText('historic-unknown');
  await expect(page.locator('table.data-table')).not.toContainText('ordinary-looking');
});

test('group/order deep links survive reload, are removable and Clear preserves unrelated query', async ({ page }) => {
  const reads = await grouped(page);
  await mountPage(page, `${PATH}?kind_group=other&order=oldest&keep=yes`);
  await expect.poll(() => reads.at(-1)?.get('exclude_kinds')).toBe('["ordinary-looking"]');
  await expect.poll(() => reads.at(-1)?.get('order')).toBe('oldest');
  await expect(page.locator('table.data-table tbody tr').first()).toContainText('historic-unknown');
  await page.reload();
  await expect(groupControl(page)).toHaveValue('other');
  await expect(orderControl(page)).toHaveValue('oldest');
  await page.getByRole('button', { name: 'Remove kind group filter', exact: true }).click();
  await expect.poll(() => reads.at(-1)?.has('exclude_kinds')).toBe(false);
  await expect.poll(() => new URL(page.url()).searchParams.has('kind_group')).toBe(false);
  await page.getByRole('button', { name: 'Clear ✕', exact: true }).click();
  await expect(orderControl(page)).toHaveValue('newest');
  await expect.poll(() => reads.at(-1)?.has('order')).toBe(false);
  await expect.poll(() => new URL(page.url()).searchParams.get('keep')).toBe('yes');
  await page.reload();
  await expect(groupControl(page)).toHaveValue('');
  await expect(orderControl(page)).toHaveValue('newest');
});

test('selected grouping holds a failed registry read instead of requesting the unfiltered collection', async ({ page }) => {
  const reads = await grouped(page);
  await page.route(/\/api\/workflows$/, route => route.fulfill({ status: 503, body: 'registry down' }));
  await mountPage(page, `${PATH}?kind_group=group%3Amachine`);
  await expect(page.getByRole('alert').filter({ hasText: "Couldn't load jobs:" })).toContainText('Job grouping unavailable');
  expect(reads).toHaveLength(0);
  await expect(page.getByText('No jobs match.', { exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Remove kind group filter', exact: true }).click();
  await expect.poll(() => reads.length).toBe(1);
  await expect(page.locator('table.data-table tbody tr')).toHaveCount(200);
});

test('an undeclared group keeps an explicitly empty inclusion instead of falling back to All', async ({ page }) => {
  const reads = await grouped(page);
  await mountPage(page, `${PATH}?kind_group=group%3Amissing`);
  await expect.poll(() => reads.at(-1)?.get('kinds')).toBe('[]');
  await expect(page.locator('header.exec-header p')).toHaveText('0 open');
  await expect(page.getByText('No jobs match.', { exact: true })).toBeVisible();
});

test('malformed listed steps produce a failed-read state instead of crashing or inventing None', async ({ page }) => {
  await install(page, [{ ...row, steps: {} }]);
  await mountPage(page, PATH);
  await expect(page.getByRole('alert').filter({ hasText: "Couldn't load jobs:" })).toBeVisible();
  await expect(page.locator('header.exec-header p')).toHaveText('Job count unknown — the read failed');
  await expect(page.locator('table.data-table')).toHaveCount(0);
  await expect(page.getByText('No jobs match.', { exact: true })).toHaveCount(0);
});

test('missing native steps are a failed read rather than None', async ({ page }) => {
  const { steps: _steps, ...withoutSteps } = row;
  await install(page, [withoutSteps]);
  await mountPage(page, PATH);
  await expect(page.getByRole('alert').filter({ hasText: "Couldn't load jobs:" })).toBeVisible();
  await expect(page.locator('header.exec-header p')).toHaveText('Job count unknown — the read failed');
  await expect(page.locator('table.data-table')).toHaveCount(0);
});

test('missing native holder is a failed read rather than Unassigned', async ({ page }) => {
  const holderStep = row.steps[0];
  if (!holderStep) throw new Error('Required test fixture step is absent');
  const { assignee_id: _holder, ...withoutHolder } = holderStep;
  await install(page, [{ ...row, steps: [withoutHolder] }]);
  await mountPage(page, PATH);
  await expect(page.getByRole('alert').filter({ hasText: "Couldn't load jobs:" })).toBeVisible();
  await expect(page.locator('header.exec-header p')).toHaveText('Job count unknown — the read failed');
  await expect(page.locator('table.data-table')).toHaveCount(0);
});

test('explicit native empty steps and null holder retain None and Unassigned', async ({ page }) => {
  await install(page, [{ ...row, steps: [] }, { ...row, id: 'b1111111-1111-4111-8111-111111111111',
    steps: [{ ...row.steps[1], assignee_id: null }] }]);
  await mountPage(page, PATH);
  await expect(page.locator('table.data-table tbody tr')).toHaveCount(2);
  await expect(page.locator('table.data-table tbody tr').first()).toContainText('None');
  await expect(page.locator('table.data-table tbody tr').last()).toContainText('Review the inspection · ready · Unassigned');
  await expect(page.getByRole('alert').filter({ hasText: "Couldn't load jobs:" })).toHaveCount(0);
});
