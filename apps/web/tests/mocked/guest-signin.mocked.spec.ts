// The guest button on /login.
//
// It replaces demo mode, which minted an `audit-readonly` session for
// anyone arriving without a valid cookie. That was invisible, and the
// invisibility is what broke it: when a real admin's session expired,
// the next request minted a guest session over it and reissued the
// cookie under the same name, so the SPA still looked signed in while
// every write returned 403.
//
// So the two properties worth pinning are that the guest session only
// ever appears because somebody asked for it, and that a deployment
// which does not offer it never shows the control.

import { test, expect } from './_test';
import { mountPage } from './_helpers';
import { installApiFloor } from './_smokeMocks';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

/// /login renders outside the AppShell — it takes the whole viewport,
/// so waiting for `.app-shell` would fail on a page that is working.
const LOGIN_ROOT = '.login-card';

/// The guest identity is the deployment's, read off GET /api/auth/guest
/// (approved design c6f08b60 D2, item 26f5080a). A reserved RFC 2606
/// address, with a local part that is not `guest`, so a page still
/// spelling a constant cannot pass by coincidence.
const GUEST_EMAIL = 'visitor@example.test';

/// The address the page hardcoded until this car: one instance's guest,
/// shown to every adopter's visitors.
const INSTANCE_GUEST_EMAIL = 'guest@algedonic.dev';

async function stubAuth(
  page: import('@playwright/test').Page,
  opts: { guestEnabled: boolean; email?: string | null },
): Promise<void> {
  await installApiFloor(page);
  await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  // Not signed in — otherwise the page redirects to home instead of
  // rendering the form we are testing.
  await page.route('**/api/auth/me', (r) => r.fulfill({ status: 401, body: '' }));
  const email = opts.email === undefined ? GUEST_EMAIL : opts.email;
  await page.route('**/api/auth/guest', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    return route.fulfill({
      json: {
        enabled: opts.guestEnabled,
        ...(email === null ? {} : { email }),
        role: 'audit-readonly',
      },
    });
  });
}

const guestButton = (page: import('@playwright/test').Page) =>
  page.getByRole('button', { name: /browse as a guest/i });

test.describe('guest sign-in', () => {
  test('is offered when the deployment enables it', async ({ page }) => {
    await stubAuth(page, { guestEnabled: true });
    await mountPage(page, '/login', { root: LOGIN_ROOT });

    await expect(guestButton(page)).toBeVisible();
    // The identity is named on the button's own note rather than left
    // for the visitor to discover in a menu after the fact — and it is
    // the identity this deployment answered, not one instance's.
    await expect(page.locator('.guest-note code')).toHaveText(GUEST_EMAIL);
    await expect(page.getByText(INSTANCE_GUEST_EMAIL)).toHaveCount(0);
    await expect(page.getByText(/read-only/i)).toBeVisible();
  });

  // A gateway that names no guest address still offers the button; the
  // note then names no address rather than inventing one.
  test('names no address when the deployment answers none', async ({ page }) => {
    await stubAuth(page, { guestEnabled: true, email: null });
    await mountPage(page, '/login', { root: LOGIN_ROOT });

    await expect(guestButton(page)).toBeVisible();
    await expect(page.getByText(/read-only/i)).toBeVisible();
    await expect(page.locator('.guest-note code')).toHaveCount(0);
    await expect(page.getByText(INSTANCE_GUEST_EMAIL)).toHaveCount(0);
  });

  // A tenant running BOSS on their own company's data does not hand
  // out a session that reads every projection, and their people should
  // never be shown a button that answers 404.
  test('is absent when the deployment does not offer it', async ({ page }) => {
    await stubAuth(page, { guestEnabled: false });
    await mountPage(page, '/login', { root: LOGIN_ROOT });

    // The form is up, so the page rendered — the button is missing
    // because it was withheld, not because nothing loaded.
    await expect(page.getByRole('button', { name: /^sign in$/i })).toBeVisible();
    await expect(guestButton(page)).toHaveCount(0);
  });

  test('mints the session only on the click, and returns to ?next', async ({ page }) => {
    await stubAuth(page, { guestEnabled: true });

    let mintCalls = 0;
    await page.route('**/api/auth/guest', async (route) => {
      if (route.request().method() !== 'POST') return route.fallback();
      mintCalls += 1;
      return route.fulfill({
        json: {
          email: GUEST_EMAIL,
          employee_id: null,
          role: 'audit-readonly',
          access_tier: null,
        },
      });
    });

    await mountPage(page, '/login?next=%2Fux%2Fjobs', { root: LOGIN_ROOT });
    // Rendering the page must not have signed anyone in.
    expect(mintCalls).toBe(0);

    await guestButton(page).click();
    await page.waitForURL(/\/ux\/jobs/);
    expect(mintCalls).toBe(1);
  });
});
