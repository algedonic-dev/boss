import { expect, test, type Locator } from './_test';
import { installSmokeMocks } from './_smokeMocks';
import { mountPage } from './_helpers';

// Exercise mounted worklist controls: their scoped frames currently
// override the shared field, even though isolated sample inputs pass.
async function fieldLook(field: Locator): Promise<void> {
  await expect(field).toBeVisible();
  await field.evaluate(el => (el as HTMLElement).blur());
  const look = () => field.evaluate(el => {
    const style = getComputedStyle(el);
    return { frame: `${style.borderTopWidth} ${style.borderTopStyle} ${style.borderTopColor}`,
      radius: style.borderTopLeftRadius, font: style.fontSize, padding: style.padding,
      outline: style.outlineStyle, halo: style.boxShadow };
  });
  expect.soft(await look()).toMatchObject({ frame: '2px solid rgb(14, 27, 46)',
    radius: '3px', font: '14px', padding: '9px 12px' });
  await field.focus();
  expect.soft(await look()).toMatchObject({ frame: '2px solid rgb(15, 110, 159)',
    outline: 'none', halo: 'rgba(217, 164, 0, 0.35) 0px 0px 0px 3px' });
  // Probe the already-declared shared CSS state, not a new validation rule.
  await field.evaluate(el => el.setAttribute('aria-invalid', 'true'));
  expect.soft(await look()).toMatchObject({ frame: '2px solid rgb(200, 40, 61)' });
  await field.evaluate(el => el.removeAttribute('aria-invalid'));
  await field.evaluate(el => (el as HTMLElement).blur());
  expect.soft(await look()).toMatchObject({ frame: '2px solid rgb(14, 27, 46)' });
}

const protocol = { kind: 'ad-hoc', version: 1, status: 'active', label: 'Ad hoc',
  description: '', category: 'operations', subject_kinds: ['custom'], steps: [] };

test('job filters inherit the shared field states and still send exact filter reads', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/workflows$/, r => r.fulfill({ json: [protocol] }));
  const queries: URLSearchParams[] = [];
  await page.route(/\/api\/jobs\?/, r => {
    queries.push(new URL(r.request().url()).searchParams);
    return r.fulfill({ json: { data: [], total: 0 } });
  });
  await mountPage(page, '/ux/jobs');
  await fieldLook(page.locator('.job-filter select').filter({ has: page.locator('option[value="ad-hoc"]') }));
  const subject = page.getByPlaceholder('An exact subject id');
  await fieldLook(subject);
  await subject.fill('field-filter-subject');
  await expect.poll(() => queries.some(query => query.get('subject_id') === 'field-filter-subject')).toBe(true);
  await expect(page.getByRole('button', { name: 'Remove subject filter' })).toBeVisible();
  await page.getByRole('button', { name: 'Remove subject filter' }).click();
  await expect(subject).toHaveValue('');
});

test('new-job input and select fields inherit the shared look without changing admission', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/workflows$/, r => r.fulfill({ json: [protocol] }));
  await page.route(/\/api\/jobs\?/, r => r.fulfill({ json: { data: [], total: 0 } }));
  await mountPage(page, '/ux/jobs');
  await page.getByRole('button', { name: 'Start a new Job', exact: true }).click();
  const form = page.locator('.new-job-form');
  const kind = form.locator('label:has(> span:text-is("Kind")) select');
  const subject = form.locator('label:has(> span:text-is("Subject id")) input');
  const subjectKind = form.locator('label:has(> span:text-is("Subject kind")) select');
  await fieldLook(kind);
  await fieldLook(subject);
  await kind.selectOption('ad-hoc');
  await subjectKind.selectOption('custom');
  await subject.fill('field-admission-subject');
  await expect(subjectKind).toHaveValue('custom');
  await expect(form.getByRole('button', { name: 'Create Job', exact: true })).toBeEnabled();
  await form.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(form).toHaveCount(0);
});

test('triage finding and route fields inherit the shared states and preserve drafts', async ({ page }) => {
  await installSmokeMocks(page);
  const fields = [{ name: 'disposition', field_type: 'build|decline', required: true },
    { name: 'finding', field_type: 'string', required: false }];
  const workflow = { ...protocol, kind: 'user-feedback', subject_kinds: ['custom'], steps: [
    { title: 'submitted', kind: 'trigger', ready_when: 'true', fields: [] },
    { title: 'triage', kind: 'task', ready_when: 'steps.submitted.done', fields },
    { title: 'build', kind: 'task', ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "build"', title_template: 'Build the change', fields: [] },
    { title: 'declined', kind: 'outcome', ready_when: 'steps.triage.done AND steps.triage.metadata.disposition = "decline"', title_template: 'Closed without action', fields: [] },
  ] };
  const packet = { id: 'worklist-field-feedback', kind: 'user-feedback', title: 'Worklist field feedback',
    status: 'open', owner_id: null, priority: 'standard', opened_on: '2026-10-11',
    subject: { subject_kind: 'custom', id: '/ux/jobs' }, metadata: { message: 'Worklist field feedback' },
    steps: [{ id: 'worklist-submitted', kind: 'trigger', status: 'completed', fields: [], metadata: {} },
      { id: 'worklist-triage', kind: 'task', status: 'ready', fields, metadata: { authority_role: 'platform-admin' } }] };
  await page.route(/\/api\/workflows$/, r => r.fulfill({ json: [workflow] }));
  await page.route(/\/api\/jobs\?kind=user-feedback/, r => r.fulfill({ json: { data: [packet], total: 1 } }));
  await mountPage(page, '/it?at=receiving', { titleMatch: /Department Map/ });
  const card = page.locator('[data-station-board="feedback"] article').filter({ hasText: 'Worklist field feedback' });
  const route = card.getByLabel('Route this item');
  await fieldLook(route);
  await route.selectOption('build');
  await expect(route).toHaveValue('build');
  await expect(card.getByRole('button', { name: 'Route', exact: true })).toBeEnabled();
  await card.getByRole('button', { name: 'Record finding', exact: true }).click();
  const finding = card.getByLabel('What did you find?');
  await fieldLook(finding);
  await finding.fill('A preserved finding draft');
  await expect(finding).toHaveValue('A preserved finding draft');
  expect(await finding.evaluate(el => getComputedStyle(el).resize)).toBe('vertical');
  await expect(card.getByRole('button', { name: 'Save finding', exact: true })).toBeEnabled();
});
