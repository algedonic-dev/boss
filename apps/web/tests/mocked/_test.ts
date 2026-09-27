// EVERY MOCKED SPEC FAILS ON AN UNCAUGHT PAGE ERROR (backlog 9045a570).
//
// Until this file the suite had no such rule. A dozen specs attached
// `page.on('pageerror')` for themselves and asserted the list empty;
// every other spec let a throw ride a green run. The runner's own log
// said so: on 2026-09-25 (run e845bdb5) `test:mocked` passed 521 while
// the dev-server printed a frontend `TypeError: Cannot read properties
// of undefined (reading 'toLocaleString')` forwarded from the browser —
// a page rendering a number it did not have, read by nobody. A check
// nobody reads is a check that is not running (CLAUDE.md, Diagnosis).
//
// So every spec takes `test` from here, not from '@playwright/test',
// and scripts/every-mocked-spec-fails-on-a-page-error.test.ts refuses a
// spec that does not. The fixture below is automatic: it listens on the
// test's browser CONTEXT — every page the context opens, popups
// included — from before the first goto, and when the test is done it
// fails it, naming each throw, its page and its stack.
//
// A throw a spec PLANTS is declared, by name, beside the test that
// plants it:
//
//   test.describe('the late-throw pin', () => {
//     test.use({ expectedPageErrors: [{ name: 'the planted late throw', pattern: /late plugin throw/ }] });
//     test('...', async ({ page }) => { ... });
//   });
//
// and an allowance that matches nothing fails the test too, so a
// declaration cannot outlive the throw it excused and go on excusing
// whatever arrives next under the same words.
//
// Scope: uncaught exceptions and unhandled rejections — what the
// browser calls a page error. console.error is not gated here;
// route-smoke.mocked.spec.ts reports it as noise, because the
// adversarial empty fixtures provoke benign error logs.

import { test as base } from '@playwright/test';

export * from '@playwright/test';

/** A throw a spec causes on purpose: a name a reader can search for,
 *  and the pattern its `Name: message` (or its stack) must match. */
export type ExpectedPageError = Readonly<{ name: string; pattern: RegExp }>;

type GuardFixtures = {
  /** The planted throws this test may see. Empty unless a spec says. */
  expectedPageErrors: ReadonlyArray<ExpectedPageError>;
  /** Automatic: fails the test on any page error not declared above. */
  pageErrorsFailTheSpec: void;
};

const indent = (s: string): string => s.split('\n').map((l) => `      ${l}`).join('\n');

export const test = base.extend<GuardFixtures>({
  expectedPageErrors: [[], { option: true }],
  pageErrorsFailTheSpec: [
    async ({ context, expectedPageErrors }, use) => {
      const unexpected: string[] = [];
      const seen = new Set<string>();
      context.on('weberror', (webError) => {
        const e = webError.error();
        const said = `${e.name}: ${e.message}`;
        const allowed = expectedPageErrors.find((x) => x.pattern.test(said) || x.pattern.test(e.stack ?? ''));
        if (allowed) {
          seen.add(allowed.name);
          return;
        }
        const where = webError.page()?.url() ?? '(no page)';
        unexpected.push(`${said}\n    on ${where}\n${indent(e.stack ?? '(no stack)')}`);
      });

      await use();

      const unmatched = expectedPageErrors.filter((x) => !seen.has(x.name));
      const problems = [
        ...(unexpected.length > 0
          ? [
              `${unexpected.length} uncaught page error(s) — fix the page, or, if this spec throws ` +
                `on purpose, declare it with test.use({ expectedPageErrors: [{ name, pattern }] }) ` +
                `(tests/mocked/_test.ts):\n\n${unexpected.join('\n\n')}`,
            ]
          : []),
        ...(unmatched.length > 0
          ? [
              `declared expected page error(s) never thrown — delete the declaration, or it will ` +
                `excuse the next throw that matches: ${unmatched.map((x) => `"${x.name}" ${x.pattern}`).join(', ')}`,
            ]
          : []),
      ];
      if (problems.length > 0) throw new Error(problems.join('\n\n'));
    },
    { auto: true },
  ],
});
