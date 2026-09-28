// EVERY MOCKED SPEC RUNS UNDER THE PAGE-ERROR GUARD (backlog 9045a570).
//
// tests/mocked/_test.ts fails a spec on any uncaught page error it did
// not declare — but only a spec that takes `test` from there. One that
// imports `test` from '@playwright/test' runs without the guard, green
// over whatever its page throws, which is the state the whole suite was
// in until this car: a frontend TypeError printed by the runner on a
// 521-pass run (e845bdb5), read by nobody.
//
// The guard's own effect is pinned in the browser by
// tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts; this holds
// the other half, that nothing opts out of it by its import line. Every
// helper and type still comes through _test.ts, which re-exports
// '@playwright/test', so the rule is one line: a spec imports nothing
// from '@playwright/test' directly. (An inline `import('@playwright/test')`
// TYPE reference imports no `test` and is allowed.)
import { expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const MOCKED = join(import.meta.dir, '../tests/mocked');
const specs = readdirSync(MOCKED).filter((f) => f.endsWith('.mocked.spec.ts')).sort();
const DIRECT = /\bfrom\s+['"]@playwright\/test['"]/;
const GUARDED = /\bimport\s*\{[^}]*\btest\b[^}]*\}\s*from\s*['"]\.\/_test['"]/;

test('the mocked directory has specs to hold (a moved directory must not pass this vacuously)', () => {
  expect(specs.length).toBeGreaterThan(100);
});

test('no mocked spec imports from @playwright/test directly', () => {
  const direct = specs.filter((f) => DIRECT.test(readFileSync(join(MOCKED, f), 'utf8')));
  expect(
    direct,
    "these specs import from '@playwright/test', so a page error in them rides a green run — " +
      "import from './_test' instead (it re-exports everything, and adds the guard)",
  ).toEqual([]);
});

test('every mocked spec takes its test from the guard', () => {
  const unguarded = specs.filter((f) => !GUARDED.test(readFileSync(join(MOCKED, f), 'utf8')));
  expect(unguarded, "these specs do not import { test } from './_test'").toEqual([]);
});

test('the guard itself is automatic, so importing it is enough', () => {
  const guard = readFileSync(join(MOCKED, '_test.ts'), 'utf8');
  expect(guard).toMatch(/pageErrorsFailTheSpec:\s*\[/);
  expect(guard).toMatch(/\{\s*auto:\s*true\s*\}/);
});
