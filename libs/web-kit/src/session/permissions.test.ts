// Route-visibility matrix. Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { canSeeRoute, ROUTE_ACCESS, type RouteName } from './permissions';

describe('canSeeRoute — platform-admin is the super-admin and sees every surface', () => {
  // Regression: `platform-admin` was missing from ROUTE_ACCESS, so the
  // sidebar collapsed to the always-on routes only — even though the
  // policy layer grants it Scope::All on every resource, and it's the
  // role the job-kind-design approve step requires. It must surface the
  // full set (esp. the admin/KB surfaces an operator needs to author).
  const surfaces: RouteName[] = [
    'job-kinds', 'policy', 'it-kb', 'it-design', 'it-step-plugins',
    'people', 'catalog', 'accounts', 'finance', 'exec', 'auth-admin',
  ];
  for (const r of surfaces) {
    test(`platform-admin can see "${r}"`, () => {
      expect(canSeeRoute('platform-admin', r)).toBe(true);
    });
  }

  test('platform-admin gets the same full grant as the C-suite', () => {
    expect(ROUTE_ACCESS['platform-admin']).toEqual(ROUTE_ACCESS['ceo']);
  });
});

describe('canSeeRoute — unknown roles fall through safely', () => {
  test('an unrecognized role sees only the always-on routes', () => {
    expect(canSeeRoute('totally-made-up-role', 'job-kinds')).toBe(false);
    expect(canSeeRoute('totally-made-up-role', 'workflows')).toBe(true);
    expect(canSeeRoute('totally-made-up-role', 'inbox')).toBe(true);
  });
});
