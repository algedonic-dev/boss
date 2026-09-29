// Every reader of the Class registry says when its read failed — one
// line, one wording (backlog 8d58d250, 2026-09-28).
//
// `classesFor` answers [] on error exactly as it does for a taxonomy
// with no rows, so a page reading only it drew an outage as an empty
// registry: roles and tiers fell to their codes, the sidebar lost its
// role's narrowing, a headcount counted every role, and nothing said
// why. The two marketing-asset pages grew the line first (e520c794,
// pinned in marketing-assets-honest-reads); this spec pins the rest,
// each through the page that draws it. Components that render once per
// row — TierChip, OrgTreeNode — say it through the page that hosts them,
// once, rather than once per chip. The AppShell's line is the sidebar's.
//
// The line is ui/ClassesReadFailed.svelte in web-kit, and every case
// below expects its exact wording: "Couldn't load the <what> — <url>:
// HTTP n. <fallback>".

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const paged = (data: ReadonlyArray<unknown>) => ({ data, total: data.length, limit: 500, offset: 0 });

const ACCOUNT_ID = 'acct-tiers';
const ACCOUNT_ROW = { id: ACCOUNT_ID, name: 'Tier Taproom', director: null, city: null, state: null, tier: 'gold', customer_since: null, territory_rep_id: null };
const ACCOUNTS_LIST = /\/api\/people\/accounts\?limit=1000$/;

/// The account page's reads, answered, so its Account section renders
/// (account-notes-author-names does the same).
async function accountPage(page: Page): Promise<void> {
  await page.route(new RegExp(`/api/people/accounts/${ACCOUNT_ID}$`), (r) =>
    json(r, ACCOUNT_ROW),
  );
  for (const re of [
    new RegExp(`/api/assets\\?account_id=${ACCOUNT_ID}&`),
    new RegExp(`/api/commerce/invoices\\?account_id=${ACCOUNT_ID}&`),
    new RegExp(`/api/jobs\\?subject_id=${ACCOUNT_ID}&`),
    new RegExp(`/api/shipping/shipments\\?account_id=${ACCOUNT_ID}&`),
  ]) {
    await page.route(re, (r) => json(r, paged([])));
  }
}

/// One invoice on the account above, for the invoice page's Account
/// section (finance-page's fixture shape).
async function invoicePage(page: Page): Promise<void> {
  await page.route(ACCOUNTS_LIST, (r) => json(r, paged([ACCOUNT_ROW])));
  await page.route(/\/api\/commerce\/invoices\/inv-tiers$/, (r) => json(r, {
    id: 'inv-tiers', account_id: ACCOUNT_ID, status: 'outstanding', payment_method: 'ach', amount_cents: 10_000,
    currency: 'USD', issued_on: '2026-09-01', due_on: '2026-09-30', paid_on: null, tax_cents: 0, tax_jurisdiction: null,
    line_items: [{ id: 'inv-tiers-l1', invoice_id: 'inv-tiers', revenue_category: 'sponsorship', amount_cents: 10_000, currency: 'USD', description: 'Hosting', ref_id: null }],
  }));
}

type Case = Readonly<{
  reader: string;
  path: string;
  subjectKind: string;
  what: string;
  fallback: string;
  setup?: (page: Page) => Promise<void>;
  /// What a viewer does after the page mounts to reach the reader.
  open?: (page: Page) => Promise<void>;
}>;

const ROLES = 'Roles show by code, not by their registry names.';
const TIERS = 'Tiers show by code, not by their registry names.';

const CASES: ReadonlyArray<Case> = [
  { reader: 'AppShell (the sidebar)', path: ROUTE_CATALOG.inbox.path, subjectKind: 'employee', what: 'roles',
    fallback: 'The sidebar shows every surface, not the ones your role declares.' },
  { reader: 'InboxPage', path: ROUTE_CATALOG.inbox.path, subjectKind: 'employee', what: 'roles', fallback: ROLES },
  { reader: 'QaPage', path: ROUTE_CATALOG.qa.path, subjectKind: 'employee', what: 'roles', fallback: ROLES },
  { reader: 'HrPage', path: ROUTE_CATALOG.hr.path, subjectKind: 'employee', what: 'roles',
    fallback: 'Roles show by code, and the counts include every role.' },
  { reader: 'PeopleList and its OrgTreeNode', path: ROUTE_CATALOG.people.path, subjectKind: 'employee',
    what: 'roles and statuses',
    fallback: 'Roles and statuses show by code, and the headcount includes every role.' },
  { reader: 'EmployeePage', path: `${ROUTE_CATALOG.people.path}/emp-001`, subjectKind: 'employee', what: 'roles', fallback: ROLES },
  { reader: 'PolicyPage (the roles EditPolicyFlyout is opened on)', path: ROUTE_CATALOG.policy.path,
    subjectKind: 'employee', what: 'roles', fallback: 'Roles holding no rules are not listed.' },
  { reader: 'AccountsList and its TierChip', path: ROUTE_CATALOG.accounts.path, subjectKind: 'account',
    what: 'account tiers', fallback: TIERS },
  { reader: "AccountPage's TierChip", path: `${ROUTE_CATALOG.accounts.path}/${ACCOUNT_ID}`, subjectKind: 'account',
    what: 'account tiers', fallback: TIERS, setup: accountPage },
  { reader: "InvoicePage's TierChip", path: '/ux/finance/inv-tiers', subjectKind: 'account',
    what: 'account tiers', fallback: TIERS, setup: invoicePage },
  { reader: "SupportPage's TierChip (Account Health)", path: ROUTE_CATALOG.support.path, subjectKind: 'account',
    what: 'account tiers', fallback: TIERS,
    setup: async (page) => {
      await page.route(ACCOUNTS_LIST, (r) => json(r, paged([ACCOUNT_ROW])));
    },
    open: (page) => page.getByRole('tab', { name: 'Account Health' }).click() },
  { reader: 'WatchlistPage', path: ROUTE_CATALOG.watchlist.path, subjectKind: 'account', what: 'account tiers', fallback: TIERS },
  { reader: 'AssetsList', path: ROUTE_CATALOG.assets.path, subjectKind: 'asset', what: 'asset phases',
    fallback: 'Phases show by code, not by their registry names.' },
];

const classesOf = (kind: string): RegExp => new RegExp(`/api/classes\\?subject_kind=${kind}$`);

/// The reader's own line, told apart from any other reader's on the same
/// page (the shell and the page both read the roles) by its fallback.
const lineOf = (page: Page, c: Case) =>
  page.locator(`${FAILURE_MARKER}[role=alert]`, { hasText: c.fallback });

for (const c of CASES) {
  test.describe(`${c.reader} at ${c.path}`, () => {
    test(`a refused (${c.subjectKind}) Class read draws the shared failure line`, async ({ page }) => {
      await installSmokeMocks(page);
      await c.setup?.(page);
      await page.route(classesOf(c.subjectKind), (r) => json(r, { error: 'classes down' }, 503));
      await mountPage(page, c.path);
      await c.open?.(page);

      await expect(lineOf(page, c)).toHaveText(
        `Couldn't load the ${c.what} — /api/classes?subject_kind=${c.subjectKind}: HTTP 503. ${c.fallback}`,
      );
    });

    test('an answered Class read draws no such line', async ({ page }) => {
      await installSmokeMocks(page);
      await c.setup?.(page);
      await mountPage(page, c.path);
      await c.open?.(page);
      await page.waitForLoadState('networkidle');

      await expect(page.locator(FAILURE_MARKER, { hasText: '/api/classes?subject_kind=' })).toHaveCount(0);
    });
  });
}
