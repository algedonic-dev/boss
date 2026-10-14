// These are the actual scoped step/editor controls, not detached sample
// inputs: their old component specificity took back the shared field rule.
import { expect, test, type Locator, type Page } from './_test';
import { installSmokeMocks } from './_smokeMocks';
import { installAuthoringMocks, JOB_ID } from './_mockApi';
import { mountPage } from './_helpers';

async function fieldLook(field: Locator): Promise<void> {
  await expect(field).toBeVisible();
  await field.evaluate((el) => (el as HTMLElement).blur());
  const look = () => field.evaluate((el) => {
    const s = getComputedStyle(el);
    return { frame: `${s.borderTopWidth} ${s.borderTopStyle} ${s.borderTopColor}`,
      radius: s.borderTopLeftRadius, font: s.fontSize, padding: s.padding,
      outline: s.outlineStyle, halo: s.boxShadow };
  });
  expect(await look()).toMatchObject({ frame: '2px solid rgb(14, 27, 46)',
    radius: '3px', font: '14px', padding: '9px 12px' });
  await field.focus();
  expect(await look()).toMatchObject({ frame: '2px solid rgb(15, 110, 159)',
    outline: 'none', halo: 'rgba(217, 164, 0, 0.35) 0px 0px 0px 3px' });
  // Exercise the shared invalid CSS state, without inventing validation
  // behavior for the quantity/select controls that do not declare it.
  await field.evaluate((el) => el.setAttribute('aria-invalid', 'true'));
  expect(await look()).toMatchObject({ frame: '2px solid rgb(200, 40, 61)' });
  await field.evaluate((el) => el.removeAttribute('aria-invalid'));
  await field.evaluate((el) => (el as HTMLElement).blur());
  expect(await look()).toMatchObject({ frame: '2px solid rgb(14, 27, 46)' });
}

async function quantityPage(page: Page, surface: string, metadata: Record<string, unknown>): Promise<void> {
  await installSmokeMocks(page);
  const jobId = 'enamel-step-fixture';
  await page.route(/\/api\/jobs\/step-types$/, (r) => r.fulfill({ json: [
    { kind: 'quantity-fixture', label: 'Quantity', category: 'generic', ux: 'inline', description: '', surface },
  ] }));
  await page.route(new RegExp(`/api/jobs/${jobId}$`), (r) => r.fulfill({ json: {
    id: jobId, kind: 'quantity-fixture', title: 'Enamel quantity fixture', status: 'open',
    opened_on: '2026-10-11', due_on: null, closed_on: null, owner_id: 'emp-001',
    priority: 'standard', simulated: false, tags: [], subject: { subject_kind: 'custom', id: 'fixture' }, metadata: {},
    steps: [{ id: 'quantity-step', job_id: jobId, kind: 'quantity-fixture', title: 'Quantity', status: 'active',
      assignee_id: 'emp-001', sort_order: 0, blocked_by: [], sign_offs_required: [], sign_offs: [], metadata, notes: null }],
  } }));
  await mountPage(page, `/ux/jobs/${jobId}`);
}

test('receiving quantity keeps the shared normal, focus and invalid field look', async ({ page }) => {
  await quantityPage(page, 'receiving', { expected_items: [{ part_sku: 'fixture-part', qty: 5 }] });
  const quantity = page.locator('.step-receiving .step-line-items input[type="number"]');
  await fieldLook(quantity);
  await quantity.fill('3');
  await expect(quantity).toHaveValue('3');
  await expect(page.locator('.step-receiving .delta-short')).toContainText('-2');
});

test('consumed quantity keeps the shared field look and its numeric alignment', async ({ page }) => {
  await quantityPage(page, 'production-consume', { ingredients_consumed: [{ part_sku: 'fixture-part', qty: 5 }] });
  const quantity = page.locator('.step-production-consume .step-line-items input[type="number"]');
  await fieldLook(quantity);
  expect(await quantity.evaluate((el) => getComputedStyle(el).textAlign)).toBe('right');
  await quantity.fill('3');
  await expect(quantity).toHaveValue('3');
});

test('the selected step inspector shares the field look for text, select and textarea', async ({ page }) => {
  await installAuthoringMocks(page);
  await mountPage(page, `/it/registry/authoring/${JOB_ID}`, { titleMatch: /Authoring/i });
  await expect(page.locator('.jk-node')).toHaveCount(2);
  await page.locator('.jk-node').first().click();
  const inspector = page.locator('.jk-inspector');
  await expect(inspector).toBeVisible();
  for (const selector of ['.jk-field input[type="text"]', '.jk-field select', '.jk-field textarea']) {
    await fieldLook(inspector.locator(selector).first());
  }
  await expect(inspector.locator('input.mono').first()).toHaveValue('start');
});

test('the inspector terminal checkbox retains its geometry and interaction', async ({ page }) => {
  await installAuthoringMocks(page);
  await mountPage(page, `/it/registry/authoring/${JOB_ID}`, { titleMatch: /Authoring/i });
  await expect(page.locator('.jk-node')).toHaveCount(2);
  await page.locator('.jk-node').first().click();
  const checkbox = page.locator('.jk-inspector .jk-check input[type="checkbox"]');
  await expect(checkbox).not.toBeChecked();
  const box = await checkbox.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { width: r.width, height: r.height };
  });
  expect(box).toEqual({ width: 13, height: 13 });
  await checkbox.check();
  await expect(checkbox).toBeChecked();
  await expect(page.locator('.jk-inspector .jk-outcome')).toHaveValue('completed');
});
