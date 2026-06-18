// DebugGear — operator-gated floating panel. Renders only when
// the session user's `role === 'platform-admin'` (per
// src/debug/DebugGear.svelte). Under the dev-server's default
// persona (emp-001 with a tenant role like `cto`) the gear must
// NOT render — that's the contract this spec asserts.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('DebugGear — operator gate', () => {
  test('hidden for non-allowlisted users', async ({ page }) => {
    await mountPage(page, '/me');
    // {#if allowed} wraps the entire .debug-gear block; an
    // unauthenticated session paints nothing.
    await expect(page.locator('.debug-gear')).toHaveCount(0);
  });
});
