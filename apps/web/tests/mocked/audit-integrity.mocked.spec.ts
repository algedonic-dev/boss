import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const ID = '65dd7f6f-4c09-45f7-9065-7b4702707dcd';
const checked = '2026-10-03T03:30:23.551906213Z';
const json = (r: Route, value: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(value) });
const receipt = {
  version: 1, checked_at: checked,
  chain: { state: 'intact', total_rows: 1127907, chain_break_count: 0,
    regression_count: 0, dangling_ref_count: 0, gap_count: 3, missing_ids: 45,
    gap_reading: 'sequence-burn' },
  drift: { state: 'covered', kinds: [] },
};
function packet(report: unknown = receipt, result = 'ok') {
  return { id: ID, kind: 'maintenance-audit-integrity', partition: 'real', simulated: false,
    opened_at: '2026-10-03T03:30:02.555056Z', status: 'closed',
    metadata: { outcome: result === 'ok' ? 'completed' : 'failed' },
    steps: [{ spec_slug: 'run', status: 'completed', completed_at: '2026-10-03T03:30:23.936011Z',
      metadata: { result, exit_status: result === 'ok' ? '0' : '2',
        output: 'Native raw evidence: sequence burns; chain verified.', audit_integrity: report } }] };
}
async function reads(page: Page, row: unknown = packet(), status = 200): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\?/, (r) => {
    if (new URL(r.request().url()).searchParams.get('kind') === 'maintenance-audit-integrity') {
      return json(r, { total: 1, data: [row] }, status);
    }
    return r.fallback();
  });
  await page.route(`/api/jobs/${ID}`, (r) => json(r, row));
}

test('shows independently recorded chain, drift and check age with raw evidence and packet navigation', async ({ page }) => {
  await reads(page);
  await mountPage(page, '/it/operate/audit');
  const section = page.getByRole('region', { name: 'Audit integrity' });
  await expect(section).toContainText('Chain intact');
  await expect(section).toContainText('Every emitted kind declared');
  await expect(section).toContainText('Last checked');
  await expect(section).toContainText('3 sequence gaps');
  await expect(section.getByRole('link', { name: 'Open integrity check' })).toHaveAttribute('href', `/jobs/${ID}`);
  await section.getByText('Raw evidence', { exact: true }).click();
  await expect(section).toContainText('Native raw evidence');
  await section.getByRole('link', { name: 'Open integrity check' }).click();
  await expect(page).toHaveURL(new RegExp(`/jobs/${ID}$`));
  await page.goBack();
  await expect(page.getByRole('region', { name: 'Audit integrity' })).toContainText('Chain intact');
});

test('a successful overall run can have undeclared event kinds', async ({ page }) => {
  await reads(page, packet({ ...receipt, drift: { state: 'undeclared', kinds: ['jobs.unregistered'] } }));
  await mountPage(page, '/it/operate/audit');
  await expect(page.getByRole('region', { name: 'Audit integrity' })).toContainText('Undeclared event kinds: jobs.unregistered');
  await expect(page.getByRole('region', { name: 'Audit integrity' })).not.toContainText('Every emitted kind declared');
});

test('a drift query failure is unknown even when the overall check exits zero', async ({ page }) => {
  await reads(page, packet({ ...receipt, drift: { state: 'unavailable', error: 'registry read refused' } }));
  await mountPage(page, '/it/operate/audit');
  await expect(page.getByRole('region', { name: 'Audit integrity' })).toContainText('Event-kind coverage unavailable: registry read refused');
});

test('historical missing report never turns an overall success into chain or drift proof', async ({ page }) => {
  await reads(page, packet(null));
  await mountPage(page, '/it/operate/audit');
  await expect(page.getByRole('region', { name: 'Audit integrity' })).toContainText('Check details unknown');
});

test('a refused maintenance read is unavailable rather than a clean verdict', async ({ page }) => {
  await reads(page, packet(), 403);
  await mountPage(page, '/it/operate/audit');
  await expect(page.getByRole('region', { name: 'Audit integrity' })).toContainText('Integrity check unavailable: HTTP 403');
});

test('a recorded broken chain remains a failed check', async ({ page }) => {
  await reads(page, packet({ ...receipt, chain: { ...receipt.chain, state: 'broken', chain_break_count: 1, gap_reading: 'possible-deletion' } }, 'failed'));
  await mountPage(page, '/it/operate/audit');
  await expect(page.getByRole('region', { name: 'Audit integrity' })).toContainText('Chain broken');
  await expect(page.getByRole('region', { name: 'Audit integrity' })).toContainText('Run failed');
});

test('a counted empty population states absence without a clean verdict', async ({ page }) => {
  await reads(page);
  await page.route(/\/api\/jobs\?/, (r) => json(r, { total: 0, data: [] }));
  await mountPage(page, '/it/operate/audit');
  const section = page.getByRole('region', { name: 'Audit integrity' });
  await expect(section).toContainText('No recorded real integrity checks.');
  await expect(section).not.toContainText('Chain intact');
});

test('malformed detail is unavailable and preserves the rest of the audit page', async ({ page }) => {
  await reads(page);
  await page.route(`/api/jobs/${ID}`, (r) => json(r, {}));
  await mountPage(page, '/it/operate/audit');
  await expect(page.getByRole('region', { name: 'Audit integrity' })).toContainText('Malformed integrity packet detail');
  await expect(page.getByRole('heading', { name: 'Audit Log', exact: true })).toBeVisible();
});

test('a newest unfinished run does not reuse an older clean observation', async ({ page }) => {
  const running = { ...packet(null), status: 'open', steps: [{ spec_slug: 'run', status: 'active', metadata: {} }] };
  await reads(page, running);
  await mountPage(page, '/it/operate/audit');
  const section = page.getByRole('region', { name: 'Audit integrity' });
  await expect(section).toContainText('Newest run is in progress');
  await expect(section).toContainText('Check details unknown');
  await expect(section).not.toContainText('Chain intact');
});
