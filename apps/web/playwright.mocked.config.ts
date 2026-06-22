// Mocked-backend Playwright config — the CI-gated frontend smoke layer.
//
// Unlike playwright.config.ts (which runs the suite against the live
// backend via the dev-server proxy + seeds a scratch stack in
// globalSetup), this config runs specs that intercept EVERY `/api/**`
// call in-browser. So it needs only the dev-server serving the SPA
// shell — no backend, no seeding — which makes it fast, deterministic,
// and safe to gate in the fast `web` CI job. See tests/mocked/_mockApi.ts.

import { defineConfig } from '@playwright/test';

const skipDevServer = process.env['PWTEST_SKIP_DEVSERVER'] === '1';

export default defineConfig({
  testDir: './tests/mocked',
  timeout: 30_000,
  retries: process.env['CI'] ? 1 : 0,
  reporter: [['list']],
  use: {
    baseURL: 'http://127.0.0.1:5174',
    headless: true,
    viewport: { width: 1280, height: 800 },
  },
  // No globalSetup — these specs mock the backend, so there is nothing
  // to seed.
  webServer: skipDevServer
    ? undefined
    : {
        command: 'bun src/dev-server.ts',
        url: 'http://127.0.0.1:5174/',
        reuseExistingServer: true,
        timeout: 60_000,
        // BOSS_SCRATCH is irrelevant (every /api call is mocked), but
        // 0 avoids the dev-server trying to reach scratch services.
        env: { BOSS_SCRATCH: '0' },
      },
  projects: [
    {
      name: 'chromium',
      use: {
        browserName: 'chromium',
        launchOptions: { args: ['--no-sandbox', '--disable-dev-shm-usage'] },
      },
    },
  ],
});
