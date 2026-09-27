// /it/registry/policy under a refused read — F5 of the review behind
// backlog 5763e52e (2026-09-27).
//
// The policy service refuses the rule table to a caller who may not read
// it, and its 403 body says WHY: an anonymous visitor, or a role whose
// Read on policy-rule is narrower than scope all. The page painted a bare
// `HTTP 403`, which sent the reader to re-derive a reason the service had
// already written. A refusal is shown with the service's own words.

import { expect, test } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PAGE = '/it/registry/policy';
const RULES = /\/api\/policy\/rules$/;
const REASON =
  'guest@algedonic.dev may not read the policy rules: an anonymous visitor reads only its own authority';

test('a refused rule read shows the service’s reason, not a bare status', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(RULES, (r) => r.fulfill({ status: 403, contentType: 'text/plain', body: REASON }));
  await mountPage(page, PAGE, { titleMatch: /Policy rules/ });

  const failure = page.locator(`${FAILURE_MARKER}[role=alert]`);
  await expect(failure).toHaveCount(1);
  await expect(failure).toHaveText(`Failed to load rules: HTTP 403: ${REASON}`);
});
