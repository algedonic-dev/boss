import { describe, expect, test } from 'bun:test';
import { stepFocusHref } from './decideHref';

describe('stepFocusHref', () => {
  test('carries the My Day return address the step-focus route reads', () => {
    const href = stepFocusHref('j1', 's1');
    expect(href.startsWith('/ux/jobs/j1/steps/s1?')).toBe(true);
    const q = new URLSearchParams(href.split('?')[1]);
    // Leading slash matters: the router drops a `from` that is not an
    // in-app path, and Back silently falls to the job page.
    expect(q.get('from')).toBe('/ux/me');
    expect(q.get('from_label')).toBe('My Day');
  });

  test('ids are path-encoded, not spliced', () => {
    expect(stepFocusHref('a/b', 's 1')).toContain('/ux/jobs/a%2Fb/steps/s%201?');
  });
});
