// /it/registry/policy — the page audit b2af346a (2026-09-27), pinned
// (backlog 07865aab: a control whose honesty nothing pins drifts back).
//
// What each test holds, and the packet that measured its failure:
//   720d6345  the matrix's rows and columns are the rules' own — 43 of 186
//             live rules sat on resources a hardcoded list of 13 never drew
//   656dc0de  the page opens on the viewer's own role, never 'ceo'
//   5a7ab4b1  a pending read is 'Loading rules…', not a matrix of dashes
//   5358ad72  a failed read withholds the matrix, first load and Refresh
//   47b7f120  a role holding no rules is selectable and says so
//   ba8411da  the flyout says when its department scopes could not be
//             read (from the departments registry since the collapse
//             of c87e3d6d), and always offers the rule's current scope
//   07865aab  a refused write shows the service's own words in the flyout

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { DEPARTMENT_CLASSES, DEPARTMENTS_ENDPOINT, installSmokeMocks, servePeopleRows } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PAGE = '/it/registry/policy';
const TITLE = { titleMatch: /Policy rules/ };
const RULES_READ = /\/api\/policy\/rules$/;
const RULE_WRITE = /\/api\/policy\/rules\/[^/]+$/;
const CLASSES = /\/api\/classes(\?|$)/;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

type Scope = string | { department: string };
const rule = (role: string, resource: string, action: string, scope: Scope = 'all') => ({
  id: `${role}:${resource}:${action}`, role, resource, action, scope, active: true,
});

// Three of these resources were never drawn by the old list of 13 —
// ledger, event and break-glass's step-signoff:platform-admin — and
// sign-off on the last is the grant the audit named first.
const RULES = [
  rule('platform-admin', 'job', 'read'),
  rule('platform-admin', 'ledger', 'update', { department: 'archive' }),
  rule('break-glass', 'step-signoff:platform-admin', 'sign-off'),
  rule('audit-readonly', 'event', 'read'),
];

// Two role Classes: one that holds rules, one that holds none.
const roleClass = (code: string, sort_order: number) => ({
  subject_kind: 'employee', code, display_name: code, parent_code: null,
  member_attribute: 'role', metadata: {}, sort_order, retired_at: null,
});
const CLASS_ROWS = [roleClass('platform-admin', 1), roleClass('engineering-agent', 2), ...DEPARTMENT_CLASSES];

const subtitle = (page: Page) => page.locator('.exec-header-text p');
const matrix = (page: Page) => page.locator('table.data-table');
const roleSelect = (page: Page) => page.getByLabel('Role:');

async function mockPolicy(page: Page, rules: unknown = RULES): Promise<void> {
  await installSmokeMocks(page);
  await page.route(CLASSES, (r) => json(r, CLASS_ROWS));
  await page.route(RULES_READ, (r) => json(r, rules));
}

test('the matrix draws every resource and action the rules carry (720d6345)', async ({ page }) => {
  await mockPolicy(page);
  await mountPage(page, PAGE, TITLE);

  await roleSelect(page).selectOption('break-glass');
  await expect(page.getByRole('heading', { name: 'break-glass — resource × action matrix' })).toBeVisible();
  await expect(matrix(page).locator('thead th')).toHaveText(['Resource', 'read', 'sign-off', 'update']);
  await expect(matrix(page).locator('tbody td.mono')).toHaveText([
    'event', 'job', 'ledger', 'step-signoff:platform-admin',
  ]);
  const grant = matrix(page).locator('tbody tr', { hasText: 'step-signoff:platform-admin' });
  await expect(grant.locator('td').nth(2).getByRole('button')).toHaveText('all');
});

test('with no session the page opens on the first role listed, never a demo role (656dc0de)', async ({ page }) => {
  await mockPolicy(page);
  await mountPage(page, PAGE, TITLE);

  await expect(roleSelect(page)).toHaveValue('audit-readonly');
  await expect(page.getByRole('heading', { name: 'audit-readonly — resource × action matrix' })).toBeVisible();
  await expect(roleSelect(page).locator('option[value="ceo"]')).toHaveCount(0);
});

test('a signed-in viewer whose role holds rules opens on that role (656dc0de)', async ({ page }) => {
  await mockPolicy(page);
  const me = {
    id: 'emp-admin', name: 'Admin', email: 'admin@algedonic.dev', role: 'platform-admin',
    department: 'it', hire_date: '2026-01-01', status: 'active', location: 'HQ',
    employment_type: 'full-time', skills: [], certifications: [],
  };
  await page.route(/\/api\/session$/, (r) => json(r, { employee_id: me.id, username: 'admin' }));
  await servePeopleRows(page, [me]);
  await mountPage(page, PAGE, TITLE);

  await expect(roleSelect(page)).toHaveValue('platform-admin');
  await expect(page.getByRole('heading', { name: 'platform-admin — resource × action matrix' })).toBeVisible();
});

test('a pending read says Loading and draws no matrix until it lands (5a7ab4b1)', async ({ page }) => {
  await mockPolicy(page);
  let release: () => void = () => {};
  const landed = new Promise<void>((resolve) => { release = resolve; });
  await page.route(RULES_READ, async (r) => { await landed; await json(r, RULES); });
  await mountPage(page, PAGE, TITLE);

  await expect(subtitle(page)).toHaveText('Loading rules…');
  await expect(matrix(page)).toHaveCount(0);

  release();
  await expect(subtitle(page)).toHaveText('4 active rules · 3 of 4 roles hold rules');
  await expect(matrix(page)).toHaveCount(1);
});

test('a failed first read withholds the matrix under its failure line (5358ad72)', async ({ page }) => {
  await mockPolicy(page);
  await page.route(RULES_READ, (r) => r.fulfill({ status: 500, contentType: 'text/plain', body: 'policy store down' }));
  await mountPage(page, PAGE, TITLE);

  await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
    'Failed to load rules: HTTP 500: policy store down',
  );
  await expect(subtitle(page)).toHaveText('Rule count unknown — the policy read failed');
  await expect(matrix(page)).toHaveCount(0);
  await expect(page.getByText('holds no rules in this table')).toHaveCount(0);
});

test('a failed Refresh withdraws the grants the previous read drew (5358ad72)', async ({ page }) => {
  await mockPolicy(page);
  await mountPage(page, PAGE, TITLE);
  await expect(matrix(page)).toHaveCount(1);

  await page.route(RULES_READ, (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'unavailable' }));
  await page.getByRole('button', { name: 'Refresh' }).click();

  await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText('Failed to load rules: HTTP 503: unavailable');
  await expect(matrix(page)).toHaveCount(0);
});

test('a role holding no rules is selectable and says so instead of a grid (47b7f120)', async ({ page }) => {
  await mockPolicy(page);
  await mountPage(page, PAGE, TITLE);

  await roleSelect(page).selectOption('engineering-agent');
  await expect(page.getByText('engineering-agent holds no rules in this table.')).toBeVisible();
  await expect(matrix(page)).toHaveCount(0);
});

test('the flyout offers the rule’s current scope and names a failed registry read (ba8411da)', async ({ page }) => {
  await mockPolicy(page);
  // Two registries, two halves: the role list still reads the role
  // Classes, and the flyout's department scopes read GET /api/departments
  // (the `(employee, department)` Classes are retired, c87e3d6d). Both
  // fail, and each half names its own.
  await page.route(CLASSES, (r) => r.fulfill({ status: 500, body: 'classes down' }));
  await page.route(DEPARTMENTS_ENDPOINT, (r) => r.fulfill({ status: 500, body: 'departments down' }));
  await mountPage(page, PAGE, TITLE);

  // The role list falls back to the roles the rules carry, and says why a
  // role holding none may be missing from it.
  await expect(page.getByText(
    "Couldn't load the roles — /api/classes?subject_kind=employee: HTTP 500. Roles holding no rules are not listed.",
  )).toBeVisible();
  await roleSelect(page).selectOption('platform-admin');

  const ledger = matrix(page).locator('tbody tr', { hasText: 'ledger' });
  await ledger.getByRole('button', { name: 'department:archive' }).click();

  const dialog = page.getByRole('dialog');
  await expect(dialog.getByLabel('Scope')).toHaveValue('department:archive');
  await expect(dialog.locator('option[value="department:archive"]')).toHaveText(
    'Department: archive (current; not in the departments registry)',
  );
  await expect(dialog.locator(FAILURE_MARKER)).toHaveText(
    'Department scopes could not be listed: the departments registry read failed.',
  );
});

test('a refused write shows the service’s own words in the flyout (07865aab)', async ({ page }) => {
  const REASON = 'audit-readonly may not update policy-rule: its update scope is none';
  await mockPolicy(page);
  await page.route(RULE_WRITE, (r) =>
    r.request().method() === 'PUT'
      ? r.fulfill({ status: 403, contentType: 'text/plain', body: REASON })
      : r.fallback());
  await mountPage(page, PAGE, TITLE);

  await matrix(page).locator('tbody tr', { hasText: 'event' }).getByRole('button', { name: 'all' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: 'Save rule' }).click();
  await expect(dialog.getByRole('alert')).toHaveText('Reason is required');

  await dialog.getByLabel('Reason for change').fill('narrowing the auditor');
  await dialog.getByRole('button', { name: 'Save rule' }).click();
  await expect(dialog.getByRole('alert')).toHaveText(`HTTP 403: ${REASON}`);
});
