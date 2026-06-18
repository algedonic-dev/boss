// Inbox — Compose modal, filter buttons, search input. Designed
// to pass even when audit_log has zero `messages.*` events;
// asserts on the controls themselves.

import { test, expect } from '@playwright/test';
import { mountPage, clickButton } from './_helpers';

test.describe('Inbox — controls', () => {
  test('Compose opens the modal and Cancel closes it', async ({ page }) => {
    await mountPage(page, '/inbox', { titleMatch: /messages/i });

    await clickButton(page, /^compose$/i);
    const modal = page.locator('.compose-modal');
    await expect(modal).toBeVisible({ timeout: 5_000 });

    // Send is disabled until all fields filled.
    const send = modal.getByRole('button', { name: /^send$/i });
    await expect(send).toBeDisabled();

    await clickButton(modal, /^cancel$/i);
    await expect(modal).toBeHidden({ timeout: 5_000 });
  });

  test('Compose ✕ button closes the modal', async ({ page }) => {
    await mountPage(page, '/inbox', { titleMatch: /messages/i });

    await clickButton(page, /^compose$/i);
    const modal = page.locator('.compose-modal');
    await expect(modal).toBeVisible({ timeout: 5_000 });

    await modal.locator('.debug-close').click();
    await expect(modal).toBeHidden({ timeout: 5_000 });
  });

  test('filter buttons toggle active state', async ({ page }) => {
    await mountPage(page, '/inbox', { titleMatch: /messages/i });

    // The four filter buttons are always rendered: All / Unread /
    // Direct / Signals. Click each in turn and assert the active
    // class flips.
    const filterGroup = page
      .locator('aside.catalog-filters .filter-group')
      .nth(1);
    const buttons = filterGroup.getByRole('button');
    const count = await buttons.count();
    expect(count).toBeGreaterThanOrEqual(4);
    for (let i = 0; i < count; i++) {
      const btn = buttons.nth(i);
      await btn.click();
      await expect(btn).toHaveClass(/filter-btn-active/);
    }
  });

  test('Search input persists what the user types', async ({ page }) => {
    await mountPage(page, '/inbox', { titleMatch: /messages/i });
    const search = page.locator('aside.catalog-filters input').first();
    await search.fill('zzz_no_message_zzz');
    await expect(search).toHaveValue('zzz_no_message_zzz');
  });

  test('Send becomes enabled when all fields filled', async ({ page }) => {
    await mountPage(page, '/inbox', { titleMatch: /messages/i });

    await clickButton(page, /^compose$/i);
    const modal = page.locator('.compose-modal');
    await expect(modal).toBeVisible({ timeout: 5_000 });

    const recipient = modal.locator('#inbox-to');
    const optCount = await recipient.locator('option').count();
    if (optCount < 2) {
      test.skip(true, 'roster empty (audit_log gap)');
    }
    const firstId = await recipient.locator('option').nth(1).getAttribute('value');
    await recipient.selectOption(firstId!);
    await modal.locator('#inbox-subject').fill('Smoke test subject');
    await modal.locator('#inbox-body').fill('Smoke test body');

    const send = modal.getByRole('button', { name: /^send$/i });
    await expect(send).toBeEnabled();
  });
});
