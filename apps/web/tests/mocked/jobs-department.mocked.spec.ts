import { isPageWrite } from './_smokeMocks';
import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.jobs.path;
const roster = { data: [
  { code: 'finance', display_name: 'Finance' },
  { code: 'it', display_name: 'IT' },
  { code: 'sales', display_name: 'Sales' },
  { code: 'service', display_name: 'Service' },
], total: 4 };
const workflows = [
  { kind: 'ordinary', label: 'Ordinary', subject_kinds: ['custom'], metadata: { department: 'finance' } },
  { kind: 'shared', label: 'Shared', subject_kinds: ['custom'], metadata: {} },
];
const packets = Array.from({ length: 203 }, (_, index) => ({
  id: `11111111-1111-4111-8111-${String(index).padStart(12, '0')}`,
  kind: index < 201 ? 'ordinary' : 'shared', title: `Packet ${index}`,
  subject: { subject_kind: 'custom', id: `subject-${index}` },
  owner_id: 'emp-001', status: 'open', priority: 'standard',
  opened_on: '2026-10-01', closed_on: null, steps: [], tags: [],
  metadata: index === 200 ? { department: 'it' } : {},
}));
const control = (page: Page) => page.locator('.job-filters label').filter({
  has: page.locator('span', { hasText: /^Department$/ }),
}).locator('select');
const rows = (page: Page) => page.locator('table.data-table tbody tr');

async function install(page: Page, departmentBody: unknown = roster, registryBody: unknown = workflows) {
  await installSmokeMocks(page);
  const reads: URLSearchParams[] = [];
  const writes: string[] = [];
  page.on('request', request => {
    const path = new URL(request.url()).pathname;
    if (isPageWrite(request.method(), path)) writes.push(path);
  });
  await page.route(/\/api\/departments(\?|$)/, route => route.fulfill({
    status: departmentBody === 'unavailable' ? 503 : 200,
    contentType: 'application/json', body: JSON.stringify(departmentBody),
  }));
  await page.route(/\/api\/workflows$/, route => route.fulfill({
    status: registryBody === 'unavailable' ? 503 : 200,
    contentType: 'application/json', body: JSON.stringify(registryBody),
  }));
  await page.route(/\/api\/jobs\?/, route => {
    const query = new URL(route.request().url()).searchParams;
    reads.push(query);
    const department = query.get('department');
    const matches = packets.filter(packet => !department ||
      (typeof packet.metadata.department === 'string' && packet.metadata.department
        ? packet.metadata.department : workflows.find(row => row.kind === packet.kind)?.metadata.department) === department);
    const offset = Number(query.get('offset') ?? 0);
    return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ data: matches.slice(offset, offset + 200), total: matches.length }) });
  });
  return { reads, writes };
}

test('general department picker narrows the counted server collection and resets paging', async ({ page }) => {
  const seen = await install(page);
  await mountPage(page, PATH);
  await expect(rows(page)).toHaveCount(200);
  await expect(control(page).locator('option')).toHaveText(['All departments', 'Finance', 'IT', 'Sales', 'Service']);
  await page.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(rows(page)).toHaveCount(3);
  await control(page).selectOption('finance');
  await expect.poll(() => Object.fromEntries(seen.reads.at(-1) ?? [])).toEqual({ department: 'finance', status: 'open', limit: '200' });
  await expect(page.locator('header.exec-header p')).toHaveText('200 open');
  await expect(rows(page)).toHaveCount(200);
  await expect(page.getByRole('button', { name: 'Remove department filter', exact: true })).toHaveText('Department: Finance ✕');
  expect(seen.writes).toEqual([]);
});

test('a department link activates its actual jobs view and Back returns to the original list', async ({ page }) => {
  const seen = await install(page);
  await mountPage(page, PATH);
  await rows(page).first().getByRole('link', { name: 'Finance', exact: true }).click();
  await expect.poll(() => new URL(page.url()).pathname).toBe('/finance/jobs');
  await expect(page.locator('h1.exec-title')).toHaveText('Jobs');
  await expect(page.locator('.exec-eyebrow')).toHaveText('Finance');
  await expect(page.getByText('Packet 0', { exact: true })).toBeVisible();
  await page.goBack();
  await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
  await expect(rows(page)).toHaveCount(200);
  await expect(control(page)).toHaveValue('');
  expect(seen.writes).toEqual([]);
});

test('department deep link survives reload and removal and Clear preserve unrelated parameters', async ({ page }) => {
  const seen = await install(page);
  await mountPage(page, `${PATH}?department=it&keep=yes&status=`);
  await expect(rows(page)).toHaveCount(1);
  await expect(control(page)).toHaveValue('it');
  await expect(page.locator('h1.exec-title')).toHaveText('Filtered jobs');
  await page.reload();
  await expect(control(page)).toHaveValue('it');
  await expect(rows(page)).toHaveCount(1);
  await page.getByRole('button', { name: 'Remove department filter', exact: true }).click();
  await expect.poll(() => seen.reads.at(-1)?.has('department')).toBe(false);
  await expect.poll(() => new URL(page.url()).searchParams.get('keep')).toBe('yes');
  await control(page).selectOption('finance');
  await page.getByRole('button', { name: 'Clear ✕', exact: true }).click();
  await expect(control(page)).toHaveValue('');
  await expect.poll(() => new URL(page.url()).searchParams.has('department')).toBe(false);
  await expect.poll(() => new URL(page.url()).searchParams.get('keep')).toBe('yes');
  await expect(page.locator('h1.exec-title')).toHaveText('All jobs');
  expect(seen.writes).toEqual([]);
});

test('row department follows packet override then workflow, with confirmed undeclared rows blank', async ({ page }) => {
  const seen = await install(page);
  await mountPage(page, PATH);
  const finance = rows(page).first().getByRole('link', { name: 'Finance', exact: true });
  await expect(finance).toHaveAttribute('href', '/finance/jobs');
  await page.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(rows(page)).toHaveCount(3);
  await expect(rows(page).first().getByRole('link', { name: 'IT', exact: true })).toHaveAttribute('href', '/it/jobs');
  await expect(rows(page).nth(1).locator('td').nth(2)).toHaveText('');
  await expect(rows(page).nth(2).locator('td').nth(2)).toHaveText('');
  expect(seen.writes).toEqual([]);
});

for (const body of ['unavailable', {}, { data: roster.data, total: 5 }, { data: [], total: 0 }] as const) {
  test(`department registry ${JSON.stringify(body)} has an honest state`, async ({ page }) => {
    const seen = await install(page, body);
    await mountPage(page, PATH);
    if (typeof body === 'object' && 'total' in body && body.total === 0) {
      await expect(page.getByText('No departments are registered.', { exact: true })).toBeVisible();
      await expect(control(page)).toBeDisabled();
    } else {
      await expect(page.getByRole('alert').filter({ hasText: "Couldn't load departments:" })).toBeVisible();
      await expect(control(page)).toBeDisabled();
      await expect(rows(page).first().locator('td').nth(2)).toHaveText('Unknown');
    }
    expect(seen.writes).toEqual([]);
  });
}

test('dark workflow registry leaves department unknown, not a confirmed blank', async ({ page }) => {
  const seen = await install(page, roster, 'unavailable');
  await mountPage(page, PATH);
  await expect(rows(page)).toHaveCount(200);
  await expect(rows(page).first().locator('td').nth(2)).toHaveText('Unknown');
  await expect(page.getByRole('alert').filter({ hasText: "Couldn't load job kinds:" })).toBeVisible();
  expect(seen.writes).toEqual([]);
});

test('retry restores the registry and resolves unknown row departments without a business write', async ({ page }) => {
  const seen = await install(page, 'unavailable');
  await mountPage(page, PATH);
  await expect(rows(page).first().locator('td').nth(2)).toHaveText('Unknown');
  await page.route(/\/api\/departments(\?|$)/, route => route.fulfill({
    contentType: 'application/json', body: JSON.stringify(roster),
  }));
  await page.getByRole('button', { name: 'Retry department registry', exact: true }).click();
  await expect(control(page)).toBeEnabled();
  await expect(rows(page).first().getByRole('link', { name: 'Finance', exact: true })).toBeVisible();
  await expect(page.getByRole('alert').filter({ hasText: "Couldn't load departments:" })).toHaveCount(0);
  expect(seen.writes).toEqual([]);
});

test('an unregistered deep-linked department stays visible and can be removed', async ({ page }) => {
  const seen = await install(page);
  await mountPage(page, `${PATH}?department=historical-team`);
  await expect(control(page)).toHaveValue('historical-team');
  await expect(control(page).locator('option:checked')).toHaveText('Unregistered department: historical-team');
  await expect(page.locator('header.exec-header p')).toHaveText('0 open');
  await page.getByRole('button', { name: 'Remove department filter', exact: true }).click();
  await expect(rows(page)).toHaveCount(200);
  await expect.poll(() => seen.reads.at(-1)?.has('department')).toBe(false);
  expect(seen.writes).toEqual([]);
});

test('a selected department does not claim to be unregistered while the registry is unavailable', async ({ page }) => {
  const seen = await install(page, 'unavailable');
  await mountPage(page, `${PATH}?department=it`);
  await expect(page.getByRole('alert').filter({ hasText: "Couldn't load departments:" })).toBeVisible();
  await expect(control(page).locator('option:checked')).toHaveText('Department: it');
  await expect(control(page)).toBeDisabled();
  await expect(rows(page)).toHaveCount(1);
  await expect(rows(page).first().locator('td').nth(2)).toHaveText('Unknown');
  expect(seen.writes).toEqual([]);
});

test('a selected department remains neutral while the registry read is loading', async ({ page }) => {
  const seen = await install(page);
  let finishRead: (() => void) | undefined;
  const pending = new Promise<void>(resolve => { finishRead = resolve; });
  await page.route(/\/api\/departments(\?|$)/, async route => {
    await pending;
    await route.fulfill({ contentType: 'application/json', body: JSON.stringify(roster) });
  });
  await mountPage(page, `${PATH}?department=it`);
  try {
    await expect(page.getByText('Reading the department registry…', { exact: true })).toBeVisible();
    await expect(control(page).locator('option:checked')).toHaveText('Department: it');
    await expect(control(page)).toBeDisabled();
  } finally {
    finishRead?.();
  }
  await expect(control(page).locator('option:checked')).toHaveText('IT');
  await expect(control(page)).toBeEnabled();
  expect(seen.writes).toEqual([]);
});
