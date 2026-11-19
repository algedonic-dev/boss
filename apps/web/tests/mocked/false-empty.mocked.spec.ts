// Packet 3fba9c35 — "Failures render as data: the false-empty sweep."
// A fetch failure must render as a FAILURE, visibly distinct from
// "loaded fine and truly empty". The triage board, HR onboarding and
// people roster instances were pinned here until their pages went with
// step 1 of design 01fa5bdf; what stays is the inbox:
//
//   Inbox — a failed /api/messages/inbox/{id} left `messages` empty,
//   so the header announced "Nothing is waiting on you" during an
//   outage. Failure now renders with the error and a Retry.

import { expect, test, type Page, type Route } from './_test';
import { inboxPage, servePeopleRows } from './_smokeMocks';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

// ---- inbox ------------------------------------------------------------

const MSG = {
  id: 'msg-1', sender_id: 'system', recipient_id: EMP.id, kind: 'direct',
  subject: 'A thing needs you', body: 'Please look at the thing.',
  sent_at: '2026-08-20T10:00:00Z', read_at: null, entity_ref: null,
};

async function inboxMocks(page: Page) {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
}

test('an inbox outage renders as a failure with Retry — never as "Nothing is waiting on you"', async ({ page }) => {
  await inboxMocks(page);
  let up = false;
  await page.route(/\/api\/messages\/inbox\//, (r) =>
    up ? json(r, inboxPage([MSG], r.request().url())) : json(r, 'message store down', 500));

  await page.goto('/inbox');
  await expect(page.locator('.load-failed')).toBeVisible();
  await expect(page.locator('.load-failed')).toContainText('HTTP 500');
  // The header must not claim an empty inbox during an outage.
  await expect(page.getByText('Nothing is waiting on you')).toHaveCount(0);

  // The outage clears; Retry recovers without a reload.
  up = true;
  await page.getByRole('button', { name: 'Retry' }).click();
  await expect(page.getByText('1 waiting on you')).toBeVisible();
  await expect(page.locator('.load-failed')).toHaveCount(0);
});

test('a truly empty inbox still reads as empty', async ({ page }) => {
  await inboxMocks(page);
  await page.route(/\/api\/messages\/inbox\//, (r) => json(r, inboxPage([], r.request().url())));

  await page.goto('/inbox');
  await expect(page.getByText('Nothing is waiting on you')).toBeVisible();
  await expect(page.locator('.load-failed')).toHaveCount(0);
});
