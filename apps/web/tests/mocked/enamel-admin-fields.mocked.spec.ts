import { expect, test, type Locator } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

// Mount the remaining page-owned fields. A sample input would miss
// the scoped declarations that override the already-shared rule.
async function fieldLook(field: Locator): Promise<void> {
  await expect(field).toBeVisible();
  await field.evaluate(el => (el as HTMLElement).blur());
  const look = () => field.evaluate(el => {
    const css = getComputedStyle(el);
    return { frame: `${css.borderTopWidth} ${css.borderTopStyle} ${css.borderTopColor}`,
      radius: css.borderTopLeftRadius, font: css.fontSize, padding: css.padding,
      outline: css.outlineStyle, halo: css.boxShadow };
  });
  expect.soft(await look()).toMatchObject({ frame: '2px solid rgb(14, 27, 46)',
    radius: '3px', font: '14px', padding: '9px 12px' });
  await field.focus();
  expect.soft(await look()).toMatchObject({ frame: '2px solid rgb(15, 110, 159)',
    outline: 'none', halo: 'rgba(217, 164, 0, 0.35) 0px 0px 0px 3px' });
  await field.evaluate(el => el.setAttribute('aria-invalid', 'true'));
  expect.soft(await look()).toMatchObject({ frame: '2px solid rgb(200, 40, 61)' });
  await field.evaluate(el => el.removeAttribute('aria-invalid'));
  await field.evaluate(el => (el as HTMLElement).blur());
}

test('auth-admin fields inherit Enamel without changing credential drafts', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route('**/api/auth/me', r => r.fulfill({ json: {
    email: 'fixture-admin@example.invalid', role: 'platform-admin' } }));
  await mountPage(page, '/it/auth-admin');
  const email = page.locator('#onboard-email');
  const password = page.locator('#onboard-password');
  const reset = page.locator('#reset-email');
  await fieldLook(email);
  await fieldLook(password);
  await fieldLook(reset);
  await email.fill('fixture-user@example.invalid');
  await password.fill('fixture-password');
  await expect(email).toHaveValue('fixture-user@example.invalid');
  await expect(password).toHaveValue('fixture-password');
  expect(await email.evaluate(el => getComputedStyle(el).boxSizing)).toBe('border-box');
});

test('local login and reset fields inherit Enamel without submitting', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route('**/api/auth/me', r => r.fulfill({ status: 401, body: '' }));
  await page.route('**/api/auth/guest', r => r.fulfill({ json: { enabled: false } }));
  await page.route('**/api/auth/oidc/available', r => r.fulfill({ json: { enabled: false } }));
  await mountPage(page, '/login', { root: '.login-card' });
  const form = page.locator('.login-card form');
  await fieldLook(form.locator('input[type="email"]'));
  await fieldLook(form.locator('input[type="password"]'));
  await form.locator('input[type="email"]').fill('fixture-login@example.invalid');
  await expect(form.locator('input[type="email"]')).toHaveValue('fixture-login@example.invalid');
  await expect(form.getByRole('button', { name: /^sign in$/i })).toBeVisible();
  await page.getByRole('button', { name: 'Have a reset token?' }).click();
  await fieldLook(page.locator('#login-token'));
  await fieldLook(page.locator('#login-password'));
  await page.locator('#login-token').fill('synthetic-reset-draft');
  await expect(page.locator('#login-token')).toHaveValue('synthetic-reset-draft');
});

test('dispatcher trigger filter inherits Enamel and adds its chip', async ({ page }) => {
  await installSmokeMocks(page);
  await mountPage(page, '/it/registry/dispatcher');
  const filter = page.locator('select.dx-filter-select');
  await fieldLook(filter);
  const value = await filter.locator('option').last().getAttribute('value');
  const label = await filter.locator('option').last().textContent();
  expect(value).not.toBeNull();
  await filter.selectOption(value!);
  await expect(filter).toHaveValue('');
  await expect(page.locator('.dx-chip')).toContainText(label!);
  expect(await filter.evaluate(el => getComputedStyle(el).maxWidth)).toBe('340px');
});

test('monthly-close month inherits Enamel and retains the draft month', async ({ page }) => {
  await installSmokeMocks(page);
  await mountPage(page, '/ux/finance');
  await page.getByRole('button', { name: 'Monthly close package', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Monthly close package' });
  const month = dialog.getByLabel('Month');
  await fieldLook(month);
  await month.fill('2026-02');
  await expect(month).toHaveValue('2026-02');
  await expect(dialog.getByRole('button', { name: 'Download ZIP' })).toBeEnabled();
});

test('audit filters inherit Enamel and preserve filter and checkbox behavior', async ({ page }) => {
  await installSmokeMocks(page);
  await mountPage(page, '/it/operate/audit');
  const source = page.getByPlaceholder('e.g. jobs, assets');
  await fieldLook(source);
  await fieldLook(page.locator('.events-filter select').first());
  await source.fill('field-test-source');
  await expect(source).toHaveValue('field-test-source');
  expect(await source.evaluate(el => getComputedStyle(el).minWidth)).toBe('160px');
  const live = page.getByRole('checkbox', { name: 'Live (SSE)' });
  expect(await live.evaluate(el => {
    const css = getComputedStyle(el);
    return { padding: css.padding, radius: css.borderTopLeftRadius, min: css.minWidth };
  // Chromium's native checkbox appearance computes these as zero on
  // both the original and inherited-field trees; its layout stays put.
  })).toEqual({ padding: '0px', radius: '0px', min: '160px' });
  const checked = await live.isChecked();
  await live.setChecked(!checked);
  await expect(live).toBeChecked({ checked: !checked });
});

test('workflow search inherits Enamel and retains its filter draft', async ({ page }) => {
  await installSmokeMocks(page);
  await mountPage(page, '/it/registry');
  const search = page.locator('input.wf-search');
  await fieldLook(search);
  await search.fill('a-kind-that-is-not-in-the-fixture');
  await expect(page.getByText(/^No Workflows match/)).toBeVisible();
  expect(await search.evaluate(el => getComputedStyle(el).maxWidth)).toBe('520px');
});

test('Home workflow picker inherits Enamel and keeps the chosen kind', async ({ page }) => {
  await installSmokeMocks(page);
  await mountPage(page, '/ux/system-model');
  const picker = page.locator('#kind-picker');
  await fieldLook(picker);
  const value = await picker.locator('option').first().getAttribute('value');
  expect(value).not.toBeNull();
  await picker.selectOption(value!);
  await expect(picker).toHaveValue(value!);
});
