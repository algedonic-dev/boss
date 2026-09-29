import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// A failed Workflow-registry read is said beside the Kind filter, not
// only inside the new-job form (backlog 3b1ec06e, page audit 473f4f92
// gap 3). The held failure used to live in `formError` alone, which
// renders only while the form is open — so the page as it mounts showed
// a Kind select offering "All kinds" and no word that the registry read
// had failed: a filter that looked valid and empty. This pins the
// template's shape: the kinds failure has an arm of its own, OUTSIDE
// the `newJobOpen` block, wearing the one class that says "this read
// failed" (`.load-failed`, tests/mocked/_routes.ts). The mocked spec
// jobs-kinds-failed-read proves the paint; this proves where it lives.
describe('the jobs list says a failed kinds read with the form closed', () => {
  const src = readFileSync(join(import.meta.dir, 'JobsListPage.svelte'), 'utf8');
  // Whitespace collapsed: a phrase that wraps in the source is still
  // one phrase on the page.
  const markup = src.slice(src.indexOf('</script>')).replace(/\s+/g, ' ');

  const armAt = markup.indexOf('{#if kindsError}');
  const arm = markup.slice(armAt, markup.indexOf('{/if}', armAt));

  it('has an arm of its own for the held failure', () => {
    expect(armAt, 'the template has a {#if kindsError} arm').toBeGreaterThan(-1);
  });

  it('wears the shared failure marker and names the read that failed', () => {
    expect(arm).toContain('load-failed');
    expect(arm).toContain('role="alert"');
    expect(arm).toContain("Couldn't load job kinds: {kindsError}");
  });

  it('sits outside the new-job form, after the filters it explains', () => {
    const formAt = markup.indexOf('{#if newJobOpen}');
    const filtersAt = markup.indexOf('class="job-filters"');
    expect(formAt).toBeGreaterThan(-1);
    expect(filtersAt).toBeGreaterThan(-1);
    expect(armAt).toBeGreaterThan(filtersAt);
    expect(armAt).toBeLessThan(formAt);
  });
});
