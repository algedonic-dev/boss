// A visitor at `/` gets the front door, not somebody else's dashboard.
//
// The regression this pins is what David actually saw: a guest session
// resolves, so My Day renders — and for someone with no assignments
// that is three empty employee panels ("Nothing in your personal
// queue", "Nothing waiting on your role's queue", a watchlist that
// fails to load) under a header reading "audit-readonly · 0.0 years ·
// visitor". Every one of those is true and none of it is for them.
//
// Two properties worth holding: the guest branch is taken at all, and
// the feedback track reports what is real rather than what flatters.

import { expect, test, type Page, type Route } from '@playwright/test';

const FEEDBACK = [
  // Nobody has picked this one up yet.
  { id: 'f1', kind: 'user-feedback', status: 'open', opened_on: '2026-08-15',
    subject: { subject_kind: 'custom', id: '/shop/FP-HAZY-6PK' }, simulated: false,
    steps: [{ spec_slug: 'triage', status: 'ready' }] },
  { id: 'f2', kind: 'user-feedback', status: 'open', opened_on: '2026-08-14',
    subject: { subject_kind: 'custom', id: '/ux/orders' }, simulated: false,
    steps: [{ spec_slug: 'triage', status: 'completed' },
            { spec_slug: 'build', status: 'active' }] },
  { id: 'f3', kind: 'user-feedback', status: 'closed', opened_on: '2026-08-11',
    subject: { subject_kind: 'custom', id: '/' }, simulated: false,
    steps: [{ spec_slug: 'closed', status: 'completed' }] },
  { id: 'f4', kind: 'user-feedback', status: 'open', opened_on: '2026-08-13',
    subject: { subject_kind: 'custom', id: '/shop' }, simulated: false,
    steps: [{ spec_slug: 'triage', status: 'completed' },
            { spec_slug: 'design-review', status: 'ready' }] },
  // Read and turned down — counted, never shown as progress.
  { id: 'f5', kind: 'user-feedback', status: 'closed', opened_on: '2026-08-10',
    subject: { subject_kind: 'custom', id: '/ux/jobs' }, simulated: false,
    steps: [{ spec_slug: 'declined', status: 'completed' }] },
  // The demo tenant's synthetic work must not inflate the numbers.
  { id: 'f6', kind: 'user-feedback', status: 'open', opened_on: '2026-08-15',
    subject: { subject_kind: 'custom', id: '/sim' }, simulated: true,
    steps: [{ spec_slug: 'triage', status: 'ready' }] },
];

async function guestSession(page: Page): Promise<void> {
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

  // Catch-all FIRST: Playwright matches in reverse registration order,
  // so everything registered below takes precedence over it.
  await page.route('**/api/**', (r) => json(r, []));
  // No employee matches, and the role is audit-readonly — which is
  // exactly how `classifyProbe` recognises the guest persona.
  await page.route(/\/api\/people$/, (r) => json(r, []));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'guest', role: 'audit-readonly' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/summary(\?|$)/, (r) => json(r, { counts: {}, total: 0 }));
  await page.route(/\/api\/jobs\?kind=user-feedback/, (r) =>
    json(r, { data: FEEDBACK, total: FEEDBACK.length }));
}

test('a guest lands on the brewery front door, not on My Day', async ({ page }) => {
  await guestSession(page);
  await page.goto('/');

  await expect(page.getByText('Welcome to Algedonic Ales')).toBeVisible();
  // The employee board's own words. If any of these come back, a
  // visitor is being shown an operator's empty queue again.
  await expect(page.getByText('Nothing in your personal queue')).toHaveCount(0);
  await expect(page.getByText("Nothing waiting on your role's queue")).toHaveCount(0);
  await expect(page.getByText("Couldn't load your watchlist")).toHaveCount(0);
});

test('the feedback track reports what is real, not what flatters', async ({ page }) => {
  await guestSession(page);
  await page.goto('/');

  const track = page.locator('.guest-track');
  await expect(track).toBeVisible();

  // Five simulated-free packets: one built, one turned down. Asserted
  // on the element's whole text — Svelte splits a text node at every
  // interpolation, so a regex spanning `{track.done}` matches none of
  // the fragments individually.
  const foot = await page.locator('.guest-track-foot').innerText();
  expect(foot).toMatch(/5 pieces of feedback so far/);
  expect(foot).toMatch(/1 of them\s+built and shipped/);
  expect(foot).toMatch(/1 we\s+read and didn't take up/);

  // The synthetic one never reaches the track.
  await expect(track.getByText('/sim')).toHaveCount(0);

  // Each packet stands at the stop its live step says it reached.
  await expect(track.getByText('/shop/FP-HAZY-6PK')).toBeVisible();
  await expect(track.getByText('/ux/orders')).toBeVisible();
  // The one we turned down is counted in the footer, never on the track.
  await expect(track.getByText('/ux/jobs')).toHaveCount(0);
});
