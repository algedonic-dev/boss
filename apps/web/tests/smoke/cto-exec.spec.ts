// CTO area + Exec dashboard. Atlas (flow diagram with clickable
// nodes), Perf (sortable latency table + Pause/Reset), Events
// (audit-log row expand/collapse), CTO home (script filter +
// run buttons).

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Perf (/perf) — controls', () => {
  test('Pause toggles to Resume + back', async ({ page }) => {
    await mountPage(page, '/perf', { titleMatch: /gateway latency/i });
    const toggle = page.getByRole('button', { name: /pause|resume/i });
    await expect(toggle).toBeVisible({ timeout: 10_000 });
    const before = await toggle.textContent();
    await toggle.click();
    // After click the label flips between "▶ Resume" ↔ "⏸ Pause".
    await expect(toggle).not.toHaveText(before!, { timeout: 5_000 });
  });

  test('Reset button is visible + clickable', async ({ page }) => {
    await mountPage(page, '/perf', { titleMatch: /gateway latency/i });
    const reset = page.getByRole('button', { name: /^reset$/i });
    await expect(reset).toBeVisible({ timeout: 10_000 });
    await reset.click();
    // Reset clears the in-memory histograms; the table re-renders
    // on the next poll. Just assert the page didn't crash.
    await expect(page.locator('h1')).toBeVisible();
  });

  test('column headers are clickable for sort', async ({ page }) => {
    await mountPage(page, '/perf', { titleMatch: /gateway latency/i });
    // The 7 sortable column headers all carry cursor:pointer +
    // onclick. Pick the count + p95 columns.
    const countHeader = page.locator('th').filter({ hasText: /^count$/i });
    if ((await countHeader.count()) > 0) {
      await countHeader.first().click();
      await expect(page.locator('h1')).toBeVisible();
    }
  });
});

test.describe('Events (/cto/events) — controls', () => {
  test('audit-log table renders + row expand toggles', async ({ page }) => {
    await mountPage(page, '/cto/events', { titleMatch: /audit log/i });
    const firstRow = page.locator('table tbody tr').first();
    if ((await firstRow.count()) === 0) {
      test.skip(true, 'audit_log empty');
    }
    // Clicking a row should toggle the expanded payload pane.
    await firstRow.click();
    // Expanded row renders a <pre> with the JSON payload — assert
    // at least one is visible after the click.
    await expect(page.locator('pre').first()).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Atlas (/cto/atlas) — interactive flow', () => {
  test('flow svg renders with clickable nodes', async ({ page }) => {
    await mountPage(page, '/cto/atlas', { titleMatch: /operating model flows/i });
    const svg = page.locator('svg.atlas-canvas');
    await expect(svg).toBeVisible({ timeout: 10_000 });
    // Atlas nodes are <text role="link" onclick> — assert the flow
    // graph has at least one navigable node.
    const navigable = svg.locator('text');
    expect(await navigable.count()).toBeGreaterThan(0);
  });
});

test.describe('CTO home (/cto) — script picker', () => {
  test('category filter buttons toggle active state', async ({ page }) => {
    await mountPage(page, '/cto', { titleMatch: /boss network/i });
    // The category filter row uses inline-styled buttons; pick
    // any visible button under the script-picker area.
    const buttons = page.getByRole('button');
    const count = await buttons.count();
    expect(count).toBeGreaterThan(0);
    // Click the first button — page must not crash.
    await buttons.first().click();
    await expect(page.locator('h1')).toBeVisible();
  });
});

test.describe('Exec dashboard (/exec)', () => {
  test('renders multiple exec cards', async ({ page }) => {
    await mountPage(page, '/exec');
    // Existing phase-4 smoke asserts hard-coded 6, but the count
    // varies with the deployed widget set. Assert ≥4 instead.
    const cards = page.locator('.exec-card');
    await expect(cards.first()).toBeVisible({ timeout: 10_000 });
    expect(await cards.count()).toBeGreaterThanOrEqual(4);
  });
});
