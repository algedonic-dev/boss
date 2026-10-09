// A step's attached files are on its page, with a link to each
// (user-feedback 2b7116ca, 2026-09-30).
//
// David opened the print step of the recovery-sheet reprint packet
// (7fff1e63) to print the sheet: "There was no actual link to the PDF
// on my step". The step DID carry it — the reprint verb attaches the
// PDF with target_kind=step and reads it back hash-checked — but
// nothing in apps/web rendered a step's attachments: files.ts had
// listFilesFor and downloadHref and no module imported them. So the
// packet said "download the PDF attached to the print step" and the
// page offered nothing to download.
//
// The list is step CHROME, drawn by the full-page route for every
// kind: a plugin-less step, and a step a plugin renders (sign-off,
// which is what the print step is). And a read that fails says so,
// because an empty list and an unread one otherwise look the same.

import { readFileSync } from 'fs';
import { test, expect, type Page } from './_test';
import { mountPage } from './_helpers';
import { installApiFloor, servePeopleRows } from './_smokeMocks';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

const JOB_ID = '7fff1e63-711d-480b-85f2-4d261eeec41e';
const STEP_ID = '02adf8e3-c58e-4999-9818-cee030d7771b';
const FILE_ID = 'e3a920cd-9bca-430c-b93e-11d8e373da37';
const SHA = '39ea8f29'.padEnd(64, '0');

const PDF = {
  id: FILE_ID,
  target: { kind: 'step', id: STEP_ID },
  bucket: 'boss-files',
  object_key: `sha256/${SHA}`,
  sha256: SHA,
  size_bytes: 1_572_864,
  mime: 'application/pdf',
  filename: 'recovery-sheet.pdf',
  uploaded_by: 'emp-david',
  uploaded_at: '2026-09-30T19:00:00Z',
  deleted_at: null,
};

const EMP = {
  id: 'emp-bootstrap-admin', name: 'Bootstrap Admin', email: 'admin@boss',
  role: 'platform-admin', department: 'it', hire_date: '2023-01-01',
  status: 'active', location: 'hq', employment_type: 'full-time',
  skills: [], certifications: [],
};

function stepOf(kind: string) {
  return {
    id: STEP_ID,
    job_id: JOB_ID,
    kind,
    title: 'Print the recovery sheet',
    assignee_id: EMP.id,
    status: 'ready',
    sort_order: 1,
    blocked_by: [],
    sign_offs_required: [],
    sign_offs: [],
    completed_on: null,
    notes: null,
    fields: [],
    metadata: { authority_role: 'platform-admin', pdf_file_ref: FILE_ID },
  };
}

async function serve(page: Page, kind: string, files: (r: import('@playwright/test').Route) => Promise<void>) {
  const step = stepOf(kind);
  const job = {
    id: JOB_ID, kind: 'reprint-recovery-sheet', title: 'Reprint the recovery sheet',
    status: 'open', subject: { subject_kind: 'custom', id: 'recovery-sheet' },
    owner_id: EMP.id, metadata: {}, steps: [step],
  };
  await installApiFloor(page);
  await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  await page.route(/\/api\/people$/, (r) => r.fulfill({ json: [EMP] }));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    r.fulfill({ json: { username: EMP.email, employee_id: EMP.id, role: EMP.role } }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => r.fulfill({ json: job }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/${STEP_ID}$`), (r) => r.fulfill({ json: step }));
  await page.route(/\/api\/files\?/, files);
}

const answers = (json: unknown) => (r: import('@playwright/test').Route) => r.fulfill({ json });

async function expectThePdfLinked(page: Page) {
  const list = page.getByTestId('step-files');
  const link = list.getByRole('link', { name: 'recovery-sheet.pdf' });
  await expect(link).toBeVisible();
  await expect(link).toHaveAttribute('href', `/api/files/${FILE_ID}`);
  await expect(list).toContainText('1.5 MB');
  await expect(list).toContainText(SHA);
}

test('a plugin-less step lists its attached file with a download link', async ({ page }) => {
  let asked = '';
  await serve(page, 'task', async (r) => {
    asked = r.request().url();
    await r.fulfill({ json: [PDF] });
  });
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });
  await expectThePdfLinked(page);
  // It asked for THIS step's files, by kind and id.
  expect(asked).toContain(`target_kind=step&target_id=${STEP_ID}`);
});

test('a step a plugin renders lists its attached file too', async ({ page }) => {
  await serve(page, 'sign-off', answers([PDF]));
  await page.route('**/api/jobs/step-plugins', (r) =>
    r.fulfill({
      json: [{
        kind: 'sign-off', label: 'Sign-off', category: 'platform', version: 1,
        frontend_url: '/plugins/sign-off.js', owning_team: 'platform',
      }],
    }));
  await page.route('**/plugins/sign-off.js', (r) =>
    r.fulfill({
      contentType: 'application/javascript',
      body: readFileSync(new URL('../../../../infra/step-plugins/sign-off.js', import.meta.url), 'utf8'),
    }));
  // The bundle loads the passkey ceremony it shares with
  // incident-review.js by adding its script tag, and registers once that
  // has run — so this is that loader, through the real host.
  let ceremonyLoads = 0;
  await page.route('**/plugins/passkey-ceremony.js', (r) => {
    ceremonyLoads += 1;
    return r.fulfill({
      contentType: 'application/javascript',
      body: readFileSync(
        new URL('../../../../infra/step-plugins/passkey-ceremony.js', import.meta.url),
        'utf8',
      ),
    });
  });
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });
  // The plugin drew its surface, and the list stands beside it.
  await expect(page.getByRole('button', { name: 'Approve', exact: true })).toBeVisible();
  expect(ceremonyLoads).toBe(1);
  await expectThePdfLinked(page);
});

test('a failed read says so instead of showing nothing', async ({ page }) => {
  await serve(page, 'task', (r) => r.fulfill({ status: 500, body: 'boom' }));
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });
  const failed = page.getByTestId('step-files-failed');
  await expect(failed).toBeVisible();
  await expect(failed).toContainText('Could not read the files attached to this step');
  await expect(failed).toContainText('HTTP 500');
});

test('a step with no attachments draws no files section', async ({ page }) => {
  await serve(page, 'task', answers([]));
  await mountPage(page, `/jobs/${JOB_ID}/steps/${STEP_ID}`, { root: '.step-focus' });
  await expect(page.locator('.step-focus-title')).toHaveText('Print the recovery sheet');
  await expect(page.getByTestId('step-files')).toHaveCount(0);
  await expect(page.getByTestId('step-files-failed')).toHaveCount(0);
});
