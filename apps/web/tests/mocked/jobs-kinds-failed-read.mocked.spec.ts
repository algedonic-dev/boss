// A FAILED READ IS HELD, NOT RETRIED (backlog 06038ed8) — AND SAID
// (backlog 3b1ec06e).
//
// JobsListPage loads the Workflow registry for its Kind filter from a
// mount effect. Its guard — `kinds.length > 0 || kindsLoading` — was
// read INSIDE that effect, so both are effect dependencies: a failed
// read left kinds empty and reset kindsLoading, the effect re-ran, and
// it fetched again. Measured 2026-09-19 while counting the mocked
// suite's unanswered reads: 127 `/api/workflows` requests from ONE
// mount of /ux/jobs, the largest single entry in that run's miss
// summary. A failure is a state the page holds; only an operator
// gesture (focusing the Kind select) asks again.
//
// Holding it was half the job. The held failure was written only to
// the new-job form's error line, so with the form closed — the page as
// it mounts — the Kind select offered "All kinds" alone and nothing on
// the page said why: a filter that looked valid and empty (page audit
// 473f4f92, gap 3). This spec counted the reads and never asked what
// the page SAID. It now does, with the form closed, on every route the
// page mounts under (/ux/service and /ux/sales are JobsListPage with a
// department).

import { expect, test, type Route } from './_test';
import { mountPage, settledReads } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';

const KINDS_FAILED = `${FAILURE_MARKER}[role=alert]`;

test.describe('the jobs list when the Workflow registry read fails', () => {
  for (const path of ['/ux/jobs', '/ux/service', '/ux/sales']) {
    test(`${path}: reads /api/workflows once, holds the failure, and says it with the form closed`, async ({ page }) => {
      await installSmokeMocks(page);

      // Registered after the smoke mocks, so it wins: the registry read
      // fails the way an outage fails it.
      let reads = 0;
      await page.route(/\/api\/workflows$/, (r: Route) => {
        reads += 1;
        return r.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"registry down"}' });
      });

      await mountPage(page, path);
      // The list itself renders — the registry read is the Kind filter's,
      // not the page's. Wait for that read to ARRIVE, then give the old
      // retry loop a second to climb (it managed 127 reads in one mount).
      // A fixed second from mount let a late first read under gate load
      // read as Received 0 on a correct page (backlog 3571be7f).
      expect(await settledReads(page, () => reads, 1)).toBe(1);

      // The form is closed; the failure is said anyway, on the shared
      // marker, and it names the read that failed.
      await expect(page.locator('form.new-job-form')).toHaveCount(0);
      await expect(page.locator(KINDS_FAILED)).toHaveText("Couldn't load job kinds: HTTP 503");
      await expect(page.locator(KINDS_FAILED)).toBeVisible();
    });
  }

  test('the control: a registry that answers paints no kinds failure', async ({ page }) => {
    await installSmokeMocks(page);
    await mountPage(page, '/ux/jobs');
    await expect(page.locator('.job-filter select').first().locator('option').nth(1)).toBeAttached();
    await expect(page.getByText("Couldn't load job kinds")).toHaveCount(0);
  });
});
