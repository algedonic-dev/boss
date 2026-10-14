import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

// Synthetic stored name references preserve the current join while testing
// status behavior. This fixture decides no ID/name association policy.
async function install(page: Page, failed: boolean): Promise<void> {
  await installSmokeMocks(page);
  const reference = 'Status contract vendor';
  await page.route(/\/api\/inventory\/vendors$/, (route) => route.fulfill({ json: [{
    id: 'vnd-status-contract', name: reference, contact_name: null, contact_email: null,
    city: null, state: null, lead_time_days: 7, payment_terms: null, category: null,
  }] }));
  await page.route(/\/api\/inventory\/orders$/, (route) => route.fulfill(failed
    ? { status: 503, body: 'orders unavailable' }
    : { json: ['tenant-awaiting-import', 'received', 'closed'].map((status, index) => ({
        id: `PO-${index}`, vendor: reference, status, placed_on: null,
        expected_on: null, received_on: null, lines: [],
      })) }));
  await page.route(/\/api\/inventory\/vendor-invoices$/, (route) => route.fulfill(failed
    ? { status: 503, body: 'bills unavailable' }
    : { json: ['tenant-credit-review', 'paid'].map((status, index) => ({
        id: `BILL-${index}`, po_id: 'PO-0', vendor: reference, status,
        vendor_invoice_no: `N-${index}`, amount_cents: 1200, currency: 'USD',
        received_on: '2026-01-01', matched_on: null, approved_on: null, paid_on: null,
        discrepancy_cents: null, discrepancy_kind: null,
      })) }));
}

const metric = (page: Page, label: string) => page.locator('.detail-meta-item')
  .filter({ has: page.locator('.detail-meta-label', { hasText: new RegExp(`^${label}$`) }) })
  .locator('.detail-meta-value');

for (const failed of [false, true]) {
  test(`both vendor surfaces preserve ${failed ? 'unknown reads' : 'tenant-added statuses and native terminals'}`, async ({ page }) => {
    await install(page, failed);
    await mountPage(page, '/ux/vendors', { titleMatch: /1 vendors/ });
    const cells = page.locator('table tbody tr td');
    await expect(cells.nth(5)).toHaveText(failed ? '?' : '1/ 3');
    await expect(cells.nth(6)).toHaveText(failed ? '?' : '1');
    await expect(cells.nth(7)).toHaveText(failed ? '?' : '$12.00');
    await mountPage(page, '/ux/vendors/vnd-status-contract');
    await expect(metric(page, 'Open POs')).toHaveText(failed ? '?' : '1');
    await expect(metric(page, 'Unpaid bills')).toHaveText(failed ? '?' : '1');
    await expect(metric(page, 'Outstanding')).toHaveText(failed ? '?' : '$12.00');
    await expect(metric(page, 'Paid lifetime')).toHaveText(failed ? '?' : '$12.00');
  });
}
