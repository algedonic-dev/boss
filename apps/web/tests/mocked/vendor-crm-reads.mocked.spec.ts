// /ux/vendors/{id} — a refused vendor CRM read is not an empty CRM.
//
// Backlog 865d3d51. `fetchCrmList` in src/vendors/api.ts turned a
// refusal, a non-list body or a network error of the account-team,
// contracts, contacts and interactions reads into `[]`, so a vendor CRM
// outage read "No contacts captured yet." — a fact the read never
// said. Each read now carries its ReadState (src/data/readState.ts) and
// the section it feeds names the failure instead of its empty sentence,
// the way the page already did for its PO and invoice reads (223ebcd6).

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const VID = 'vnd-hops-001';
const PATH = `/ux/vendors/${VID}`;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const VENDOR = {
  id: VID, name: 'Cascade Hop Farm', contact_name: 'Rhea Okafor', contact_email: 'rhea@example.com',
  city: 'Yakima', state: 'WA', lead_time_days: 14, payment_terms: 'Net 30', category: 'hop-supplier',
};

/// The four CRM reads: the path segment, the section it feeds, what the
/// failure line calls it, and the empty sentence it must NOT show.
const READS = [
  { path: 'account-team', section: /^Account team/, what: 'the account team', empty: 'No account-team assignments yet.' },
  { path: 'contracts', section: /^Active contracts/, what: 'contracts', empty: 'No active contracts on file.' },
  { path: 'contacts', section: /^Contacts/, what: 'contacts', empty: 'No contacts captured yet.' },
  { path: 'interactions', section: /^Interactions/, what: 'interactions', empty: 'No interactions logged yet.' },
] as const;

/// Every read answered and empty, except `refused`, which answers 500.
async function install(page: Page, refused: string | null): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/inventory\/vendors$/, (r) => json(r, [VENDOR]));
  await page.route(/\/api\/inventory\/orders$/, (r) => json(r, []));
  await page.route(/\/api\/inventory\/vendor-invoices$/, (r) => json(r, []));
  for (const read of READS) {
    await page.route(new RegExp(`/api/inventory/vendors/${VID}/${read.path}$`), (r) =>
      read.path === refused ? json(r, { error: 'inventory down' }, 500) : json(r, []),
    );
  }
}

const section = (page: Page, title: RegExp) =>
  page.locator('section', { has: page.getByRole('heading', { name: title }) });

test.describe('/ux/vendors/{id} — the vendor CRM reads', () => {
  test('answered and empty, each section says it has none, and nothing says a read failed', async ({ page }) => {
    await install(page, null);
    await mountPage(page, PATH);

    for (const read of READS) {
      await expect(section(page, read.section)).toContainText(read.empty);
    }
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  for (const read of READS) {
    test(`a 500 on ${read.path} names the failure, not "${read.empty}"`, async ({ page }) => {
      await install(page, read.path);
      await mountPage(page, PATH);

      const url = `/api/inventory/vendors/${VID}/${read.path}`;
      await expect(section(page, read.section).locator(FAILURE_MARKER)).toHaveText(
        `Couldn't load ${read.what} — ${url}: HTTP 500`,
      );
      await expect(section(page, read.section)).not.toContainText(read.empty);
      await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toContainText(
        `Couldn't load ${read.what} — ${url}: HTTP 500.`,
      );
      // The other three answered, so they keep their honest empty.
      for (const other of READS.filter((o) => o.path !== read.path)) {
        await expect(section(page, other.section)).toContainText(other.empty);
      }
    });
  }
});
