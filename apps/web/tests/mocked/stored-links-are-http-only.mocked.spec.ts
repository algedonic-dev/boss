// A URL read off a record is a link only when it is http(s) or a path
// on this site (backlog 4f1f7698, 2026-09-27).
//
// Svelte does not sanitise href, so the device page's
// `<a href={doc.url}>` ran a stored `javascript:` URL in the operator's
// session on one click — the same bug the diagnostic-call Join button
// had (4a359b51). Every dynamic href and src now goes through
// safeLinkHref (@boss/web-kit/links), and src/linkSinks.test.ts refuses
// one that does not; this spec drives the behaviour in a browser: the
// hostile document's title is drawn as text, a click on it neither runs
// the script nor leaves the page, and the https document beside it is
// still a link.

import { expect, test, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, body: unknown): Promise<void> =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });

const doc = (title: string, url: string) => ({
  kind: 'manual', title, url, version: '1', published: null, audience: 'operator',
});

const MODEL = {
  sku: 'FV-30', name: 'Fermenter 30 bbl', manufacturer: 'Acme', model_year: 2024, category: 'cellar',
  extras: null, physical: null, regulatory: null,
  commerce: {
    list_price_new_cents: 100_000, typical_refurb_price_cents: null, currency: 'USD',
    lead_time_days: null, tagline: '', description: '', use_cases: [], hero_image: null,
  },
  service: {
    preventive_maintenance_hours: 2, preventive_maintenance_interval_months: 6,
    calibration_interval_months: 12, required_skill_level: 2, depot_required: false,
    common_failure_modes: [], pm_checklist: [],
  },
  spare_parts: [], consumables: [],
  documents: [
    doc('Hostile manual', 'javascript:window.__ranStoredUrl=1'),
    doc('Protocol-relative manual', '//evil.example/manual.pdf'),
    doc('Operator manual', 'https://docs.example/fv-30.pdf'),
  ],
  end_of_support: null, current_firmware: null,
};

test.describe('a stored URL on a device document', () => {
  test('a javascript: URL renders as text and never navigates', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(/\/api\/catalog\/models\/FV-30$/, (r) => json(r, MODEL));
    await mountPage(page, '/ux/catalog/FV-30', { titleMatch: /Fermenter 30 bbl/ });

    const docs = page.locator('section', { has: page.getByRole('heading', { name: /^Documents/ }) });
    await expect(docs.getByText('Hostile manual')).toBeVisible();

    // Drawn as text: no link carries the title, and no anchor on the
    // page carries the scheme or the other site.
    await expect(docs.getByRole('link', { name: 'Hostile manual' })).toHaveCount(0);
    await expect(docs.getByRole('link', { name: 'Protocol-relative manual' })).toHaveCount(0);
    await expect(page.locator('a[href^="javascript:" i]')).toHaveCount(0);
    await expect(page.locator('a[href^="//"]')).toHaveCount(0);

    // Clicked, it runs nothing and goes nowhere.
    const before = page.url();
    await docs.getByText('Hostile manual').click();
    expect(await page.evaluate(() => (window as { __ranStoredUrl?: number }).__ranStoredUrl)).toBeUndefined();
    expect(page.url()).toBe(before);

    // The https document beside it is still a link, to where it said.
    await expect(docs.getByRole('link', { name: 'Operator manual' })).toHaveAttribute(
      'href', 'https://docs.example/fv-30.pdf',
    );
  });
});
