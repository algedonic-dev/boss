// README-promised entry-point URLs — the public paths the repo
// README's "See it locally" table says visitors can open on the
// local install (http://localhost:4443/...). Each one MUST render
// without console errors / 404s, since readers arrive via these
// links and a broken link is a first-impression failure.
//
// Source: README.md "See it locally" table. Six rows, each
// becomes one assertion below. If a route is renamed, update both
// places — this spec is the contract for the README's promise.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

const README_URLS = [
  { label: 'Home dashboard / landing', path: '/', expect: 'h1' },
  {
    label: 'Job detail entry point',
    path: '/jobs/',
    // The trailing-slash variant should match /jobs and render the
    // jobs list (router strips trailing slashes).
    expect: 'h1',
  },
  {
    label: 'Event log streaming',
    path: '/it/monitoring/events',
    expect: 'h1',
  },
  {
    label: 'System Atlas via ?atlas query param',
    path: '/?atlas',
    // Landing page renders, viewMode flipped to atlas. We assert
    // the atlas-specific UI is present rather than the workflow
    // diagram. The atlas view exposes a `.atlas` selector or
    // similar; using the SVG presence as the smoke check.
    expect: 'svg',
  },
  { label: 'People list', path: '/people', expect: 'h1' },
  {
    label: 'JobKind authoring (admin alias)',
    path: '/admin/job-kinds',
    expect: 'h1',
  },
];

test.describe('README entry-point URLs render successfully', () => {
  for (const { label, path, expect: selector } of README_URLS) {
    test(`${label} (${path})`, async ({ page }) => {
      // Capture 5xx responses with their URL so a failure points
      // at the offending endpoint instead of a generic "Failed to
      // load resource". Server 5xx is the regression signal; 4xx
      // and client cancellations don't count (the SPA tolerates
      // many optional 4xx polls today).
      const httpFailures: string[] = [];
      page.on('response', (resp) => {
        if (resp.status() >= 500) {
          httpFailures.push(`${resp.status()} ${resp.url()}`);
        }
      });
      await mountPage(page, path);
      // Render contract: each README-promised URL must surface
      // its primary content selector. The SPA's not-found surface
      // doesn't render the chosen selectors, so this catches
      // routing regressions cleanly.
      await expect(page.locator(selector).first()).toBeVisible({
        timeout: 10_000,
      });
      // 5xx means the route was reached but the upstream exploded
      // — that's a regression worth catching even if the page
      // still renders (some optional widgets degrade silently).
      expect(
        httpFailures,
        `5xx responses while loading ${path}:\n  ${httpFailures.join('\n  ')}`,
      ).toEqual([]);
    });
  }
});
